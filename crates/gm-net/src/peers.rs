//! Peers: every running GradApp 2 on this network, and invites between them.
//!
//! * **Presence.** Each app sends a small JSON datagram ([`PeerInfo`] plus
//!   `"magic":"gradapp-peer","v":1`) every ~3 s on the beacon's UDP port and
//!   group (47800, 239.255.71.77, plus broadcast), idle or not, and answers a
//!   `{"magic":"gradapp-peer-query"}` at once. Listeners dedupe by
//!   `device_id` and forget a peer after ~10 s of silence (or at once on its
//!   `"bye"`). The packet never carries the mesh token. v2.0.0 listeners
//!   ignore it: it is not a `BeaconPacket` and not the beacon query.
//! * **Invites.** Each app runs a tiny HTTP endpoint (TCP 47801, or any free
//!   port; advertised as `invite_port`). A hosting app POSTs a
//!   [`MeshInvite`] to `/v1/invite`; the receiver accepts it only from a
//!   private/link-local/loopback address (or this machine's own address), at most once per 30 s per sender,
//!   with at most a few waiting, never from a blocked device, and only after
//!   the coordinator in it answered `/health` with our protocol. It never
//!   joins by itself: the person answers, and the answer goes back to
//!   `/v1/invite-reply` on the inviter, quoting the invite's random nonce.
//!
//! [`PeerNode`] bundles both; the app wires its events to the window.

use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use axum::body::Bytes;
use axum::extract::{ConnectInfo, DefaultBodyLimit, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use gm_types::{
    AppEvent, InviteOutcome, MeshInvite, PeerInfo, PeerStatus, BEACON_GROUP, BEACON_PORT,
    MESH_PROTOCOL,
};
use serde::{Deserialize, Serialize};
use tokio::net::UdpSocket;
use tokio::sync::{oneshot, watch};
use tokio_util::sync::CancellationToken;

use crate::beacon::{bind_beacon_listener, directed_broadcast, sender_socket, MAX_BEACON_BYTES};
use crate::invite::{lan_client, probe_health};
use crate::lan::{lan_addresses, LanAddress};

/// `"magic"` of a presence packet.
pub const PEER_MAGIC: &str = "gradapp-peer";
/// Presence packet format version.
pub const PEER_VERSION: u32 = 1;
/// Makes every GradApp that hears it announce itself now.
pub const PEER_QUERY: &str = r#"{"magic":"gradapp-peer-query"}"#;
/// How often presence is sent.
pub const PEER_INTERVAL: Duration = Duration::from_secs(3);
/// A peer silent this long is gone.
pub const PEER_TTL: Duration = Duration::from_secs(10);
/// Preferred TCP port of the invite endpoint (any free port if taken).
pub const INVITE_PORT: u16 = 47_801;
pub const INVITE_PATH: &str = "/v1/invite";
pub const REPLY_PATH: &str = "/v1/invite-reply";
/// One invite per sender address per this long.
pub const INVITE_RATE: Duration = Duration::from_secs(30);
/// Invites waiting for an answer, at most.
pub const MAX_PENDING: usize = 5;
/// An unanswered invite expires after this long (both sides).
pub const INVITE_TTL: Duration = Duration::from_secs(120);

const MAX_INVITE_BODY: usize = 8 * 1024;
const QUERY_HOLDOFF: Duration = Duration::from_millis(250);
const NAME_MAX: usize = 64;
const TEXT_MAX: usize = 96;
const ID_MAX: usize = 64;
const TOKEN_MAX: usize = 256;

// ---------------------------------------------------------------------------
// Wire format
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize)]
struct PeerPacket {
    magic: String,
    v: u32,
    /// Sent once when an app quits or hides: forget me now.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    bye: bool,
    #[serde(flatten)]
    peer: PeerInfo,
}

/// A fresh random id (device ids, nonces).
pub fn random_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

/// Printable, trimmed, at most `max` characters.
pub fn clean_text(text: &str, max: usize) -> String {
    text.chars()
        .filter(|c| !c.is_control())
        .collect::<String>()
        .trim()
        .chars()
        .take(max)
        .collect()
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= ID_MAX
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn clean_peer(mut p: PeerInfo) -> Option<PeerInfo> {
    if !valid_id(&p.device_id) {
        return None;
    }
    p.name = clean_text(&p.name, NAME_MAX);
    if p.name.is_empty() {
        p.name = "GradApp".into();
    }
    p.gpu = p
        .gpu
        .map(|g| clean_text(&g, TEXT_MAX))
        .filter(|g| !g.is_empty());
    p.mesh_id = p.mesh_id.filter(|m| valid_id(m));
    p.mesh_name = p
        .mesh_name
        .map(|m| clean_text(&m, TEXT_MAX))
        .filter(|m| !m.is_empty());
    p.app_version = clean_text(&p.app_version, 32);
    p.address = None;
    Some(p)
}

/// The presence datagram for `peer` (`bye`: the last one, on quit/hide).
pub fn encode_peer(peer: &PeerInfo, bye: bool) -> Vec<u8> {
    let peer = clean_peer(peer.clone()).unwrap_or_else(|| peer.clone());
    serde_json::to_vec(&PeerPacket {
        magic: PEER_MAGIC.into(),
        v: PEER_VERSION,
        bye,
        peer,
    })
    .unwrap_or_default()
}

/// Decode a presence datagram: `(peer, bye)`. `None` for anything else,
/// including beacons and queries. `address` is always `None` (the caller
/// sets it from the socket).
pub fn parse_peer(bytes: &[u8]) -> Option<(PeerInfo, bool)> {
    if bytes.len() > MAX_BEACON_BYTES {
        return None;
    }
    let packet: PeerPacket = serde_json::from_slice(bytes).ok()?;
    if packet.magic != PEER_MAGIC || packet.v < 1 {
        return None;
    }
    Some((clean_peer(packet.peer)?, packet.bye))
}

fn is_query(bytes: &[u8]) -> bool {
    bytes.trim_ascii() == PEER_QUERY.as_bytes()
}

/// Private (RFC 1918), link-local or loopback, IPv4 or IPv6 (ULA fc00::/7,
/// fe80::/10, ::1, and IPv4-mapped forms). Invites are accepted only from these.
pub fn is_private_lan(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_private() || v4.is_loopback() || v4.is_link_local(),
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => is_private_lan(IpAddr::V4(v4)),
            None => {
                let first = v6.segments()[0];
                v6.is_loopback() || (first & 0xfe00) == 0xfc00 || (first & 0xffc0) == 0xfe80
            }
        },
    }
}

/// One of this machine's own interface addresses (a GradApp on the same
/// computer is as trusted as loopback, whatever range the adapter is in).
pub fn is_own_address(ip: IpAddr) -> bool {
    let ip = match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(ip),
        v4 => v4,
    };
    ip.is_loopback()
        || if_addrs::get_if_addrs()
            .map(|all| all.iter().any(|i| i.ip() == ip))
            .unwrap_or(false)
}

/// Who may send an invite or a reply: this network or this machine.
pub fn is_trusted_sender(ip: IpAddr) -> bool {
    is_private_lan(ip) || is_own_address(ip)
}

/// A coordinator URL an invite may point at: http(s) on a private address,
/// this machine, or a `.local` name. Never a public host.
pub fn coordinator_is_lan(coordinator: &str) -> Option<(String, u16)> {
    let url = url::Url::parse(coordinator).ok()?;
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    let port = url.port_or_known_default()?;
    let ok = match url.host()? {
        url::Host::Ipv4(ip) => is_trusted_sender(IpAddr::V4(ip)),
        url::Host::Ipv6(ip) => is_trusted_sender(IpAddr::V6(ip)),
        url::Host::Domain(d) => d.to_ascii_lowercase().ends_with(".local"),
    };
    ok.then(|| (url.host_str().unwrap_or_default().to_string(), port))
}

fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

// ---------------------------------------------------------------------------
// Peer table
// ---------------------------------------------------------------------------

/// Peers by `device_id`, with when each was last heard.
#[derive(Debug)]
pub struct PeerTable {
    me: String,
    peers: HashMap<String, (PeerInfo, Instant)>,
}

impl PeerTable {
    pub fn new(own_device_id: impl Into<String>) -> Self {
        Self {
            me: own_device_id.into(),
            peers: HashMap::new(),
        }
    }

    /// Record a sighting. True when the peer is new or anything about it
    /// changed (worth a `PeerSeen`); our own packets are ignored.
    pub fn observe(&mut self, peer: PeerInfo, at: Instant) -> bool {
        if peer.device_id == self.me {
            return false;
        }
        match self.peers.get_mut(&peer.device_id) {
            Some((known, seen)) => {
                *seen = at;
                if *known == peer {
                    false
                } else {
                    *known = peer;
                    true
                }
            }
            None => {
                self.peers.insert(peer.device_id.clone(), (peer, at));
                true
            }
        }
    }

    pub fn remove(&mut self, device_id: &str) -> bool {
        self.peers.remove(device_id).is_some()
    }

    /// Forget peers not heard for `ttl`; returns their ids.
    pub fn expire(&mut self, now: Instant, ttl: Duration) -> Vec<String> {
        let gone: Vec<String> = self
            .peers
            .iter()
            .filter(|(_, (_, seen))| now.saturating_duration_since(*seen) > ttl)
            .map(|(id, _)| id.clone())
            .collect();
        for id in &gone {
            self.peers.remove(id);
        }
        gone
    }

    pub fn get(&self, device_id: &str) -> Option<&PeerInfo> {
        self.peers.get(device_id).map(|(p, _)| p)
    }

    /// Hosting first, then by name.
    pub fn list(&self) -> Vec<PeerInfo> {
        let mut out: Vec<PeerInfo> = self.peers.values().map(|(p, _)| p.clone()).collect();
        out.sort_by(|a, b| {
            (b.status == PeerStatus::Hosting)
                .cmp(&(a.status == PeerStatus::Hosting))
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
                .then_with(|| a.device_id.cmp(&b.device_id))
        });
        out
    }

    pub fn len(&self) -> usize {
        self.peers.len()
    }

    pub fn is_empty(&self) -> bool {
        self.peers.is_empty()
    }
}

// ---------------------------------------------------------------------------
// Node: presence + invite endpoint
// ---------------------------------------------------------------------------

/// Knobs for [`PeerNode`]. `Default` is the real LAN setup; tests use
/// loopback ports and unicast targets.
#[derive(Debug, Clone)]
pub struct PeerConfig {
    pub interval: Duration,
    pub ttl: Duration,
    /// UDP port presence is heard on.
    pub listen_port: u16,
    /// UDP port presence is sent to.
    pub send_port: u16,
    pub group: Ipv4Addr,
    pub multicast: bool,
    pub broadcast: bool,
    /// Extra unicast destinations (tests; peers behind a router).
    pub extra_targets: Vec<SocketAddr>,
    /// Addresses the invite endpoint tries, in order.
    pub invite_binds: Vec<SocketAddr>,
    pub rate_limit: Duration,
    pub max_pending: usize,
    pub invite_ttl: Duration,
    /// How long the receiver gives the invite's coordinator to answer `/health`.
    pub vet_timeout: Duration,
}

impl Default for PeerConfig {
    fn default() -> Self {
        Self {
            interval: PEER_INTERVAL,
            ttl: PEER_TTL,
            listen_port: BEACON_PORT,
            send_port: BEACON_PORT,
            group: Ipv4Addr::from(BEACON_GROUP),
            multicast: true,
            broadcast: true,
            extra_targets: Vec::new(),
            invite_binds: vec![
                SocketAddr::from((Ipv4Addr::UNSPECIFIED, INVITE_PORT)),
                SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0)),
            ],
            rate_limit: INVITE_RATE,
            max_pending: MAX_PENDING,
            invite_ttl: INVITE_TTL,
            vet_timeout: Duration::from_millis(2500),
        }
    }
}

/// The mesh an invite offers (the inviter's hosted mesh).
#[derive(Debug, Clone, PartialEq)]
pub struct MeshOffer {
    pub mesh_name: String,
    pub mesh_id: String,
    pub coordinator: String,
    pub token: String,
}

/// `POST /v1/invite-reply`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InviteReply {
    pub nonce: String,
    pub from_device: String,
    pub outcome: InviteOutcome,
}

/// What the receiving side's HTTP answer says.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Answer {
    status: String,
    #[serde(default)]
    detail: Option<String>,
}

/// An invite this app sent and is waiting on.
#[derive(Debug, Clone, Serialize)]
pub struct OutgoingInvite {
    pub nonce: String,
    pub device_id: String,
    pub mesh_id: String,
    pub sent_at: f64,
    #[serde(skip)]
    sent: Instant,
}

type Sink = Arc<dyn Fn(AppEvent) + Send + Sync>;

struct Me {
    info: PeerInfo,
    visible: bool,
}

struct NodeState {
    table: PeerTable,
    blocked: HashSet<String>,
    last_from: HashMap<IpAddr, Instant>,
    incoming: Vec<(MeshInvite, Instant)>,
    outgoing: HashMap<String, OutgoingInvite>,
    presence: bool,
}

struct Shared {
    cfg: PeerConfig,
    state: Mutex<NodeState>,
    me: watch::Sender<Me>,
    sink: Sink,
}

impl Shared {
    fn lock(&self) -> std::sync::MutexGuard<'_, NodeState> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn emit_all(&self, events: Vec<AppEvent>) {
        for e in events {
            (self.sink)(e);
        }
    }

    fn visible(&self) -> bool {
        self.me.borrow().visible
    }

    fn device_id(&self) -> String {
        self.me.borrow().info.device_id.clone()
    }
}

fn answer(code: StatusCode, status: &str, detail: impl Into<Option<String>>) -> Response {
    (
        code,
        Json(Answer {
            status: status.into(),
            detail: detail.into(),
        }),
    )
        .into_response()
}

/// This app on the LAN: presence (announce + listen) and the invite endpoint.
/// Stops (with a "bye") on [`PeerNode::stop`]; dropping it stops it silently.
pub struct PeerNode {
    shared: Arc<Shared>,
    cancel: CancellationToken,
    invite_addr: Option<SocketAddr>,
    http_stop: Option<oneshot::Sender<()>>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

impl PeerNode {
    /// Bind the invite endpoint (first free address of `cfg.invite_binds`;
    /// none: invites cannot be received and `invite_port` stays 0), then start
    /// presence. `me.invite_port` is set here. Must run inside Tokio.
    pub async fn start(
        cfg: PeerConfig,
        mut me: PeerInfo,
        visible: bool,
        sink: impl Fn(AppEvent) + Send + Sync + 'static,
    ) -> Self {
        let mut listener = None;
        for addr in &cfg.invite_binds {
            match tokio::net::TcpListener::bind(addr).await {
                Ok(l) => {
                    listener = Some(l);
                    break;
                }
                Err(e) => tracing::debug!("invite endpoint: {addr} unavailable: {e}"),
            }
        }
        let invite_addr = listener.as_ref().and_then(|l| l.local_addr().ok());
        me.invite_port = invite_addr.map(|a| a.port()).unwrap_or(0);
        if invite_addr.is_none() {
            tracing::warn!("invite endpoint could not bind; this app cannot receive invites");
        }
        // Bound here, not in the task, so nothing sent right after start is lost.
        let udp = match bind_beacon_listener(cfg.listen_port, cfg.group) {
            Ok(s) => Some(s),
            Err(e) => {
                tracing::warn!(
                    "peer presence: cannot listen on UDP {} ({e}); other GradApps will not be listed",
                    cfg.listen_port
                );
                None
            }
        };
        let own_id = me.device_id.clone();
        let (me_tx, me_rx) = watch::channel(Me { info: me, visible });
        let shared = Arc::new(Shared {
            state: Mutex::new(NodeState {
                table: PeerTable::new(own_id),
                blocked: HashSet::new(),
                last_from: HashMap::new(),
                incoming: Vec::new(),
                outgoing: HashMap::new(),
                presence: udp.is_some(),
            }),
            me: me_tx,
            sink: Arc::new(sink),
            cfg,
        });
        let cancel = CancellationToken::new();
        let mut tasks = Vec::new();
        let mut http_stop = None;
        if let Some(listener) = listener {
            let (stop, stopped) = oneshot::channel::<()>();
            http_stop = Some(stop);
            let router = Router::new()
                .route(INVITE_PATH, post(http_invite))
                .route(REPLY_PATH, post(http_reply))
                .layer(DefaultBodyLimit::max(MAX_INVITE_BODY))
                .with_state(shared.clone());
            tasks.push(tokio::spawn(async move {
                let _ = axum::serve(
                    listener,
                    router.into_make_service_with_connect_info::<SocketAddr>(),
                )
                .with_graceful_shutdown(async move {
                    let _ = stopped.await;
                })
                .await;
            }));
        }
        tasks.push(tokio::spawn(run_presence(
            shared.clone(),
            udp,
            me_rx,
            cancel.clone(),
        )));
        tasks.push(tokio::spawn(run_expiry(shared.clone(), cancel.clone())));
        Self {
            shared,
            cancel,
            invite_addr,
            http_stop,
            tasks,
        }
    }

    /// This app as announced (with its `invite_port`).
    pub fn me(&self) -> PeerInfo {
        self.shared.me.borrow().info.clone()
    }

    /// Where the invite endpoint listens, if it could bind.
    pub fn invite_addr(&self) -> Option<SocketAddr> {
        self.invite_addr
    }

    /// The UDP presence listener is up (false: blocked or port unavailable;
    /// discovery of meshes still works by sweep).
    pub fn presence_listening(&self) -> bool {
        self.shared.lock().presence
    }

    pub fn visible(&self) -> bool {
        self.shared.visible()
    }

    /// Announce a change (name, status, mesh, port state) at once. The invite
    /// port and device id stay what the node set.
    pub fn update(&self, mut info: PeerInfo) {
        self.shared.me.send_if_modified(|me| {
            info.invite_port = me.info.invite_port;
            info.device_id = me.info.device_id.clone();
            info.address = None;
            if me.info == info {
                false
            } else {
                me.info = info;
                true
            }
        });
    }

    /// Settings › "Visible to other GradApps". Hidden: no presence, a "bye"
    /// to everyone, no answers to queries, and invites are refused. Other
    /// apps are still heard.
    pub fn set_visible(&self, visible: bool) {
        self.shared.me.send_if_modified(|me| {
            let changed = me.visible != visible;
            me.visible = visible;
            changed
        });
    }

    pub fn set_blocked(&self, devices: impl IntoIterator<Item = String>) {
        self.shared.lock().blocked = devices.into_iter().collect();
    }

    pub fn peers(&self) -> Vec<PeerInfo> {
        self.shared.lock().table.list()
    }

    pub fn incoming(&self) -> Vec<MeshInvite> {
        self.shared
            .lock()
            .incoming
            .iter()
            .map(|(i, _)| i.clone())
            .collect()
    }

    pub fn outgoing(&self) -> Vec<OutgoingInvite> {
        self.shared.lock().outgoing.values().cloned().collect()
    }

    /// Invite `device_id` into `offer`. Emits `InviteResult` (`Sent` or
    /// `Failed`) either way; returns the nonce or why it failed.
    pub async fn invite(&self, device_id: &str, offer: &MeshOffer) -> Result<String, String> {
        let result = self.try_invite(device_id, offer).await;
        let event = match &result {
            Ok(nonce) => AppEvent::InviteResult {
                nonce: nonce.clone(),
                device_id: device_id.into(),
                incoming: false,
                outcome: InviteOutcome::Sent,
                detail: None,
            },
            Err(why) => AppEvent::InviteResult {
                nonce: String::new(),
                device_id: device_id.into(),
                incoming: false,
                outcome: InviteOutcome::Failed,
                detail: Some(why.clone()),
            },
        };
        (self.shared.sink)(event);
        result
    }

    async fn try_invite(&self, device_id: &str, offer: &MeshOffer) -> Result<String, String> {
        let peer = self
            .shared
            .lock()
            .table
            .get(device_id)
            .cloned()
            .ok_or("That GradApp is no longer on this network.")?;
        if peer.mesh_id.as_deref() == Some(offer.mesh_id.as_str()) {
            return Err(format!("{} is already in this mesh.", peer.name));
        }
        if peer.invite_port == 0 {
            return Err(format!(
                "{} cannot receive invites (its invite port is closed). Send it the invite link instead.",
                peer.name
            ));
        }
        let address = peer.address.clone().ok_or("No address for that GradApp.")?;
        let me = self.me();
        let invite = MeshInvite {
            from_device: me.device_id.clone(),
            from_name: me.name.clone(),
            mesh_name: clean_text(&offer.mesh_name, TEXT_MAX),
            mesh_id: offer.mesh_id.clone(),
            coordinator: offer.coordinator.clone(),
            token: offer.token.clone(),
            sent_at: now_secs(),
            nonce: random_id(),
            reply_port: me.invite_port,
            from_address: None,
            nodes_active: None,
        };
        let url = format!(
            "http://{}{INVITE_PATH}",
            host_port(&address, peer.invite_port)
        );
        let response = lan_client()
            .post(&url)
            .json(&invite)
            .timeout(self.shared.cfg.vet_timeout + Duration::from_secs(3))
            .send()
            .await
            .map_err(|e| {
                tracing::info!("invite to {url} failed: {e}");
                format!(
                    "Could not reach {}. Its firewall may be blocking GradApp, or it just closed.",
                    peer.name
                )
            })?;
        let code = response.status();
        let detail = response.json::<Answer>().await.ok().and_then(|a| a.detail);
        if !code.is_success() {
            return Err(
                detail.unwrap_or_else(|| format!("{} refused the invite ({code}).", peer.name))
            );
        }
        self.shared.lock().outgoing.insert(
            invite.nonce.clone(),
            OutgoingInvite {
                nonce: invite.nonce.clone(),
                device_id: device_id.into(),
                mesh_id: offer.mesh_id.clone(),
                sent_at: invite.sent_at,
                sent: Instant::now(),
            },
        );
        Ok(invite.nonce)
    }

    /// Answer an invite this app received: `Accepted` or `Declined`
    /// (`block`: also refuse that device from now on). The inviter is told
    /// (best effort, in the background). Returns the invite.
    pub fn respond(&self, nonce: &str, outcome: InviteOutcome, block: bool) -> Option<MeshInvite> {
        let invite = {
            let mut st = self.shared.lock();
            let at = st.incoming.iter().position(|(i, _)| i.nonce == nonce)?;
            let (invite, _) = st.incoming.remove(at);
            if block {
                st.blocked.insert(invite.from_device.clone());
            }
            invite
        };
        (self.shared.sink)(AppEvent::InviteResult {
            nonce: nonce.into(),
            device_id: invite.from_device.clone(),
            incoming: true,
            outcome,
            detail: None,
        });
        let reply = InviteReply {
            nonce: invite.nonce.clone(),
            from_device: self.shared.device_id(),
            outcome,
        };
        if let (Some(address), port) = (invite.from_address.clone(), invite.reply_port) {
            if port != 0 {
                let timeout = self.shared.cfg.vet_timeout;
                tokio::spawn(async move {
                    let url = format!("http://{}{REPLY_PATH}", host_port(&address, port));
                    if let Err(e) = lan_client()
                        .post(&url)
                        .json(&reply)
                        .timeout(timeout)
                        .send()
                        .await
                    {
                        tracing::info!("invite reply to {url} failed: {e}");
                    }
                });
            }
        }
        Some(invite)
    }

    /// Stop presence (saying "bye" if visible) and the invite endpoint.
    pub async fn stop(mut self) {
        self.cancel.cancel();
        if let Some(stop) = self.http_stop.take() {
            let _ = stop.send(());
        }
        for task in self.tasks.drain(..) {
            let _ = tokio::time::timeout(Duration::from_secs(2), task).await;
        }
    }
}

impl Drop for PeerNode {
    fn drop(&mut self) {
        self.cancel.cancel();
        if let Some(stop) = self.http_stop.take() {
            let _ = stop.send(());
        }
    }
}

fn host_port(address: &str, port: u16) -> String {
    if address.contains(':') && !address.starts_with('[') {
        format!("[{address}]:{port}")
    } else {
        format!("{address}:{port}")
    }
}

// ---------------------------------------------------------------------------
// Invite endpoint
// ---------------------------------------------------------------------------

async fn http_invite(
    State(shared): State<Arc<Shared>>,
    ConnectInfo(from): ConnectInfo<SocketAddr>,
    body: Bytes,
) -> Response {
    receive_invite(&shared, from.ip(), &body).await
}

async fn http_reply(
    State(shared): State<Arc<Shared>>,
    ConnectInfo(from): ConnectInfo<SocketAddr>,
    body: Bytes,
) -> Response {
    receive_reply(&shared, from.ip(), &body)
}

fn check_invite(invite: &MeshInvite) -> Result<(String, u16), String> {
    if !valid_id(&invite.from_device) || !valid_id(&invite.nonce) || invite.nonce.len() < 16 {
        return Err("missing or malformed id".into());
    }
    if invite.mesh_id.is_empty() || invite.mesh_id.len() > ID_MAX {
        return Err("missing mesh id".into());
    }
    if invite.token.is_empty() || invite.token.len() > TOKEN_MAX {
        return Err("missing token".into());
    }
    coordinator_is_lan(&invite.coordinator)
        .ok_or_else(|| "the coordinator must be on this network".to_string())
}

/// The invite endpoint's whole decision, separate from HTTP so it can be
/// tested with any source address.
async fn receive_invite(shared: &Shared, from: IpAddr, body: &[u8]) -> Response {
    if !is_trusted_sender(from) {
        return answer(
            StatusCode::FORBIDDEN,
            "refused",
            "Invites are accepted only from this network.".to_string(),
        );
    }
    if !shared.visible() {
        return answer(
            StatusCode::FORBIDDEN,
            "refused",
            "That GradApp is not accepting invites.".to_string(),
        );
    }
    let Ok(mut invite) = serde_json::from_slice::<MeshInvite>(body) else {
        return answer(StatusCode::BAD_REQUEST, "malformed", None);
    };
    let (host, port) = match check_invite(&invite) {
        Ok(target) => target,
        Err(why) => return answer(StatusCode::BAD_REQUEST, "malformed", why),
    };
    invite.from_name = clean_text(&invite.from_name, NAME_MAX);
    if invite.from_name.is_empty() {
        invite.from_name = "A GradApp".into();
    }
    invite.mesh_name = clean_text(&invite.mesh_name, TEXT_MAX);
    if invite.mesh_name.is_empty() {
        invite.mesh_name = format!("{}'s mesh", invite.from_name);
    }
    invite.from_address = Some(from.to_string());
    {
        let mut st = shared.lock();
        if st.blocked.contains(&invite.from_device) {
            return answer(
                StatusCode::FORBIDDEN,
                "refused",
                "That GradApp is not accepting invites from this device.".to_string(),
            );
        }
        if st.incoming.iter().any(|(i, _)| i.nonce == invite.nonce) {
            return answer(StatusCode::ACCEPTED, "pending", None);
        }
        let now = Instant::now();
        if let Some(last) = st.last_from.get(&from) {
            let wait = shared
                .cfg
                .rate_limit
                .saturating_sub(now.duration_since(*last));
            if !wait.is_zero() {
                return answer(
                    StatusCode::TOO_MANY_REQUESTS,
                    "rate_limited",
                    format!(
                        "Invited a moment ago; try again in {} s.",
                        wait.as_secs().max(1)
                    ),
                );
            }
        }
        if st.incoming.len() >= shared.cfg.max_pending {
            return answer(
                StatusCode::TOO_MANY_REQUESTS,
                "busy",
                "That GradApp has too many invites waiting.".to_string(),
            );
        }
        st.last_from.insert(from, now);
        st.last_from
            .retain(|_, t| now.duration_since(*t) < shared.cfg.rate_limit);
    }
    // Only show invites to a mesh that is really there and speaks our protocol.
    let Some(hit) = probe_health(&lan_client(), &host, port, shared.cfg.vet_timeout).await else {
        return answer(
            StatusCode::UNPROCESSABLE_ENTITY,
            "unreachable",
            format!(
                "The invited machine could not reach your coordinator at {}. Check this computer's firewall.",
                invite.coordinator
            ),
        );
    };
    if hit.health.protocol != MESH_PROTOCOL {
        return answer(
            StatusCode::UNPROCESSABLE_ENTITY,
            "protocol",
            format!(
                "Protocol mismatch: the mesh speaks {}, the invited GradApp {MESH_PROTOCOL}.",
                hit.health.protocol
            ),
        );
    }
    if hit.health.mesh_id != invite.mesh_id {
        return answer(
            StatusCode::UNPROCESSABLE_ENTITY,
            "mismatch",
            "The coordinator in the invite runs a different mesh.".to_string(),
        );
    }
    invite.nodes_active = Some(hit.health.nodes_active);
    {
        let mut st = shared.lock();
        if st.incoming.len() >= shared.cfg.max_pending
            || st.incoming.iter().any(|(i, _)| i.nonce == invite.nonce)
        {
            return answer(StatusCode::ACCEPTED, "pending", None);
        }
        st.incoming.push((invite.clone(), Instant::now()));
    }
    tracing::info!(
        "invite from {} ({from}) to {}",
        invite.from_name,
        invite.mesh_name
    );
    (shared.sink)(AppEvent::InviteReceived { invite });
    answer(StatusCode::ACCEPTED, "pending", None)
}

fn receive_reply(shared: &Shared, from: IpAddr, body: &[u8]) -> Response {
    if !is_trusted_sender(from) {
        return answer(StatusCode::FORBIDDEN, "refused", None);
    }
    let Ok(reply) = serde_json::from_slice::<InviteReply>(body) else {
        return answer(StatusCode::BAD_REQUEST, "malformed", None);
    };
    if !matches!(
        reply.outcome,
        InviteOutcome::Accepted | InviteOutcome::Declined | InviteOutcome::TimedOut
    ) {
        return answer(StatusCode::BAD_REQUEST, "malformed", None);
    }
    let found = {
        let mut st = shared.lock();
        match st.outgoing.get(&reply.nonce) {
            Some(o) if o.device_id == reply.from_device => st.outgoing.remove(&reply.nonce),
            _ => None,
        }
    };
    let Some(out) = found else {
        return answer(StatusCode::NOT_FOUND, "unknown", None);
    };
    (shared.sink)(AppEvent::InviteResult {
        nonce: out.nonce,
        device_id: out.device_id,
        incoming: false,
        outcome: reply.outcome,
        detail: None,
    });
    answer(StatusCode::OK, "ok", None)
}

// ---------------------------------------------------------------------------
// Presence
// ---------------------------------------------------------------------------

struct Announcer {
    per_iface: HashMap<Ipv4Addr, UdpSocket>,
    any: Option<UdpSocket>,
    warned: bool,
}

impl Announcer {
    fn new() -> Self {
        Self {
            per_iface: HashMap::new(),
            any: sender_socket(Ipv4Addr::UNSPECIFIED, None).ok(),
            warned: false,
        }
    }

    async fn send(&mut self, cfg: &PeerConfig, bytes: &[u8]) {
        let lans: Vec<LanAddress> = if cfg.multicast || cfg.broadcast {
            lan_addresses()
                .into_iter()
                .filter(|a| !a.is_virtual)
                .collect()
        } else {
            Vec::new()
        };
        self.per_iface
            .retain(|ip, _| lans.iter().any(|l| l.address == *ip));
        let mut errors = Vec::new();
        for lan in &lans {
            if let std::collections::hash_map::Entry::Vacant(slot) =
                self.per_iface.entry(lan.address)
            {
                match sender_socket(lan.address, Some(lan.address)) {
                    Ok(s) => {
                        slot.insert(s);
                    }
                    Err(e) => errors.push(format!("{}: {e}", lan.name)),
                }
            }
            let Some(socket) = self.per_iface.get(&lan.address) else {
                continue;
            };
            if cfg.multicast {
                if let Err(e) = socket
                    .send_to(bytes, SocketAddrV4::new(cfg.group, cfg.send_port))
                    .await
                {
                    errors.push(format!("multicast {}: {e}", lan.name));
                }
            }
            if cfg.broadcast {
                if let Some(bcast) = directed_broadcast(lan) {
                    let _ = socket
                        .send_to(bytes, SocketAddrV4::new(bcast, cfg.send_port))
                        .await;
                }
            }
        }
        if let Some(any) = &self.any {
            if cfg.broadcast {
                if let Err(e) = any
                    .send_to(bytes, SocketAddrV4::new(Ipv4Addr::BROADCAST, cfg.send_port))
                    .await
                {
                    errors.push(format!("broadcast: {e}"));
                }
            }
            for target in &cfg.extra_targets {
                if let Err(e) = any.send_to(bytes, target).await {
                    errors.push(format!("{target}: {e}"));
                }
            }
        }
        if !errors.is_empty() && !self.warned {
            self.warned = true;
            tracing::debug!(
                "presence send errors (reported once): {}",
                errors.join("; ")
            );
        }
    }
}

async fn run_presence(
    shared: Arc<Shared>,
    listener: Option<UdpSocket>,
    mut me: watch::Receiver<Me>,
    cancel: CancellationToken,
) {
    let cfg = shared.cfg.clone();
    let mut announcer = Announcer::new();
    // Ask everyone to announce now, so the list fills in one round trip.
    announcer.send(&cfg, PEER_QUERY.as_bytes()).await;

    let mut ticker = tokio::time::interval(cfg.interval.max(Duration::from_millis(50)));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut buf = vec![0u8; 4096];
    let mut last_sent: Option<Instant> = None;
    let mut was_visible = me.borrow().visible;
    loop {
        let mut announce = false;
        tokio::select! {
            _ = cancel.cancelled() => break,
            _ = ticker.tick() => announce = true,
            changed = me.changed() => {
                if changed.is_err() { break; }
                let (visible, bytes) = {
                    let m = me.borrow_and_update();
                    (m.visible, encode_peer(&m.info, true))
                };
                if was_visible && !visible {
                    announcer.send(&cfg, &bytes).await;
                }
                was_visible = visible;
                announce = visible;
            }
            got = async {
                match &listener {
                    Some(l) => l.recv_from(&mut buf).await,
                    None => std::future::pending().await,
                }
            } => {
                let Ok((n, from)) = got else { continue };
                let bytes = &buf[..n];
                if is_query(bytes) {
                    announce = last_sent.is_none_or(|t| t.elapsed() >= QUERY_HOLDOFF);
                } else if let Some((mut peer, bye)) = parse_peer(bytes) {
                    peer.address = Some(from.ip().to_string());
                    let events = observe(&shared, peer, bye);
                    shared.emit_all(events);
                }
            }
        }
        if announce {
            let (visible, bytes) = {
                let m = me.borrow();
                (m.visible, encode_peer(&m.info, false))
            };
            if visible {
                last_sent = Some(Instant::now());
                announcer.send(&cfg, &bytes).await;
            }
        }
    }
    if me.borrow().visible {
        let bytes = encode_peer(&me.borrow().info, true);
        announcer.send(&cfg, &bytes).await;
    }
}

fn observe(shared: &Shared, peer: PeerInfo, bye: bool) -> Vec<AppEvent> {
    let mut events = Vec::new();
    let mut st = shared.lock();
    if bye {
        if st.table.remove(&peer.device_id) {
            events.push(AppEvent::PeerGone {
                device_id: peer.device_id,
            });
        }
        return events;
    }
    // A peer that now reports our offered mesh has accepted, even if its
    // reply never arrived.
    if let Some(mesh) = peer.mesh_id.as_deref() {
        let joined: Vec<String> = st
            .outgoing
            .values()
            .filter(|o| o.device_id == peer.device_id && o.mesh_id == mesh)
            .map(|o| o.nonce.clone())
            .collect();
        for nonce in joined {
            st.outgoing.remove(&nonce);
            events.push(AppEvent::InviteResult {
                nonce,
                device_id: peer.device_id.clone(),
                incoming: false,
                outcome: InviteOutcome::Accepted,
                detail: None,
            });
        }
    }
    if st.table.observe(peer.clone(), Instant::now()) {
        events.insert(0, AppEvent::PeerSeen { peer });
    }
    events
}

/// Forget silent peers; time out invites nobody answered.
async fn run_expiry(shared: Arc<Shared>, cancel: CancellationToken) {
    let every = (shared.cfg.ttl / 4).clamp(Duration::from_millis(100), Duration::from_secs(1));
    loop {
        if cancel
            .run_until_cancelled(tokio::time::sleep(every))
            .await
            .is_none()
        {
            return;
        }
        let now = Instant::now();
        let ttl = shared.cfg.invite_ttl;
        let mut events = Vec::new();
        {
            let mut st = shared.lock();
            for id in st.table.expire(now, shared.cfg.ttl) {
                events.push(AppEvent::PeerGone { device_id: id });
            }
            st.incoming.retain(|(invite, at)| {
                let keep = now.duration_since(*at) < ttl;
                if !keep {
                    events.push(AppEvent::InviteResult {
                        nonce: invite.nonce.clone(),
                        device_id: invite.from_device.clone(),
                        incoming: true,
                        outcome: InviteOutcome::TimedOut,
                        detail: None,
                    });
                }
                keep
            });
            // The inviter waits a little longer than the invitee, so a late
            // answer still arrives first.
            let grace = ttl + (ttl / 4).min(Duration::from_secs(5));
            st.outgoing.retain(|_, o| {
                let keep = now.duration_since(o.sent) < grace;
                if !keep {
                    events.push(AppEvent::InviteResult {
                        nonce: o.nonce.clone(),
                        device_id: o.device_id.clone(),
                        incoming: false,
                        outcome: InviteOutcome::TimedOut,
                        detail: None,
                    });
                }
                keep
            });
        }
        shared.emit_all(events);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gm_types::{Backend, Os, PortState};

    pub(crate) fn peer(id: &str, name: &str) -> PeerInfo {
        PeerInfo {
            device_id: id.into(),
            name: name.into(),
            os: Os::Windows,
            gpu: Some("NVIDIA GeForce RTX 3050 Laptop GPU".into()),
            backend: Some(Backend::Cuda),
            port_state: Some(PortState::Ready),
            status: PeerStatus::Idle,
            mesh_id: None,
            mesh_name: None,
            app_version: "2.0.1".into(),
            invite_port: 47801,
            address: None,
        }
    }

    #[test]
    fn encode_parse_round_trip() {
        let p = peer("dev1", "MSI");
        let bytes = encode_peer(&p, false);
        let text = String::from_utf8(bytes.clone()).unwrap();
        assert!(text.contains(r#""magic":"gradapp-peer""#), "{text}");
        assert!(text.contains(r#""v":1"#));
        assert!(text.contains(r#""os":"windows""#));
        assert!(text.contains(r#""backend":"cuda""#));
        assert!(text.contains(r#""port_state":"ready""#));
        assert!(text.contains(r#""status":"idle""#));
        assert!(!text.contains("bye"));
        assert!(!text.contains("address"));
        assert!(!text.contains("token"));
        assert_eq!(parse_peer(&bytes), Some((p.clone(), false)));
        let (_, bye) = parse_peer(&encode_peer(&p, true)).unwrap();
        assert!(bye);
    }

    #[test]
    fn parse_rejects_others_and_cleans() {
        // A beacon, the beacon query and our own query are not presence.
        let beacon = crate::beacon::beacon_packet("m1", None, "5.1.1", 1, 8000, 3000);
        assert!(parse_peer(&serde_json::to_vec(&beacon).unwrap()).is_none());
        assert!(parse_peer(crate::beacon::BEACON_QUERY.as_bytes()).is_none());
        assert!(parse_peer(PEER_QUERY.as_bytes()).is_none());
        assert!(parse_peer(b"\x00\x01garbage").is_none());
        assert!(parse_peer(&vec![b' '; MAX_BEACON_BYTES + 1]).is_none());
        // Wrong magic, bad id.
        let ok = String::from_utf8(encode_peer(&peer("dev1", "x"), false)).unwrap();
        assert!(parse_peer(ok.replace("gradapp-peer", "gradmesh").as_bytes()).is_none());
        assert!(parse_peer(ok.replace("dev1", "../etc").as_bytes()).is_none());
        // The sender cannot choose the address we record; names are cleaned.
        let spoof = ok.replace(
            r#""name":"x""#,
            r#""name":"  evil\u0007name  ","address":"8.8.8.8""#,
        );
        let (p, _) = parse_peer(spoof.as_bytes()).unwrap();
        assert_eq!(p.address, None);
        assert_eq!(p.name, "evilname");
        // A newer version with extra fields still parses.
        let newer = ok.replace(r#""v":1"#, r#""v":2,"future":[1,2]"#);
        assert!(parse_peer(newer.as_bytes()).is_some());
    }

    /// v2.0.0 listeners must ignore presence: their parser and their query
    /// check both say no.
    #[test]
    fn old_listeners_ignore_presence() {
        let bytes = encode_peer(&peer("dev1", "MSI"), false);
        assert_eq!(crate::beacon::parse_beacon(&bytes), None);
        assert_ne!(PEER_QUERY, crate::beacon::BEACON_QUERY);
        assert_eq!(crate::beacon::parse_beacon(PEER_QUERY.as_bytes()), None);
    }

    #[test]
    fn private_addresses() {
        for ip in [
            "192.168.1.5",
            "10.0.0.2",
            "172.16.4.4",
            "172.31.255.1",
            "127.0.0.1",
            "169.254.3.3",
            "::1",
            "fd12::1",
            "fe80::1",
            "::ffff:192.168.1.9",
        ] {
            assert!(is_private_lan(ip.parse().unwrap()), "{ip}");
        }
        for ip in [
            "8.8.8.8",
            "172.32.0.1",
            "100.64.0.1",
            "1.1.1.1",
            "2001:4860::8888",
            "::ffff:8.8.8.8",
        ] {
            assert!(!is_private_lan(ip.parse().unwrap()), "{ip}");
        }
        assert_eq!(
            coordinator_is_lan("http://192.168.1.20:8000"),
            Some(("192.168.1.20".into(), 8000))
        );
        assert!(coordinator_is_lan("http://lab-pc.local:8000").is_some());
        assert!(coordinator_is_lan("http://example.com:8000").is_none());
        assert!(coordinator_is_lan("http://8.8.8.8:8000").is_none());
        assert!(coordinator_is_lan("file:///etc/passwd").is_none());
        // This machine's own adapters count, whatever their range.
        for own in if_addrs::get_if_addrs().unwrap() {
            assert!(is_trusted_sender(own.ip()), "{}", own.ip());
        }
        assert!(!is_trusted_sender("8.8.8.8".parse().unwrap()));
    }

    #[test]
    fn table_dedupes_and_expires() {
        let mut t = PeerTable::new("me");
        let t0 = Instant::now();
        assert!(!t.observe(peer("me", "Me"), t0), "own packets are ignored");
        assert!(t.observe(peer("a", "Alpha"), t0));
        assert!(!t.observe(peer("a", "Alpha"), t0), "a repeat is not news");
        let mut moved = peer("a", "Alpha");
        moved.status = PeerStatus::Contributing;
        assert!(t.observe(moved, t0 + Duration::from_secs(5)));
        let mut host = peer("b", "beta");
        host.status = PeerStatus::Hosting;
        assert!(t.observe(host, t0 + Duration::from_secs(1)));
        assert_eq!(t.len(), 2);
        let names: Vec<_> = t.list().into_iter().map(|p| p.name).collect();
        assert_eq!(names, ["beta", "Alpha"], "hosting first");
        // b silent for 11 s, a for 6 s.
        let gone = t.expire(t0 + Duration::from_secs(12), PEER_TTL);
        assert_eq!(gone, ["b"]);
        assert!(t.get("a").is_some());
        assert!(t.remove("a"));
        assert!(t.is_empty());
    }

    #[test]
    fn invite_checks() {
        let mut i = MeshInvite {
            from_device: "dev1".into(),
            from_name: "MSI".into(),
            mesh_name: "MSI's mesh".into(),
            mesh_id: "m-1".into(),
            coordinator: "http://192.168.1.20:8000".into(),
            token: "t".into(),
            sent_at: 0.0,
            nonce: random_id(),
            reply_port: 47801,
            from_address: None,
            nodes_active: None,
        };
        assert!(check_invite(&i).is_ok());
        i.coordinator = "http://evil.example:8000".into();
        assert!(check_invite(&i).is_err());
        i.coordinator = "http://192.168.1.20:8000".into();
        i.nonce = "short".into();
        assert!(check_invite(&i).is_err());
        i.nonce = random_id();
        i.token.clear();
        assert!(check_invite(&i).is_err());
    }

    #[test]
    fn text_is_cleaned() {
        assert_eq!(clean_text(" a\nb\u{1b}[31mc ", 10), "ab[31mc");
        assert_eq!(clean_text("ééééé", 3), "ééé");
        assert_eq!(random_id().len(), 32);
        assert_ne!(random_id(), random_id());
    }

    /// The endpoint's decision for a public source, without a socket.
    #[tokio::test]
    async fn public_sender_is_refused() {
        let node = PeerNode::start(
            PeerConfig {
                multicast: false,
                broadcast: false,
                listen_port: 0,
                invite_binds: vec![],
                ..PeerConfig::default()
            },
            peer("me", "Me"),
            true,
            |_| {},
        )
        .await;
        let body = serde_json::to_vec(&MeshInvite {
            from_device: "dev1".into(),
            from_name: "x".into(),
            mesh_name: "m".into(),
            mesh_id: "m-1".into(),
            coordinator: "http://192.168.1.20:8000".into(),
            token: "t".into(),
            sent_at: 0.0,
            nonce: random_id(),
            reply_port: 1,
            from_address: None,
            nodes_active: None,
        })
        .unwrap();
        let r = receive_invite(&node.shared, "8.8.8.8".parse().unwrap(), &body).await;
        assert_eq!(r.status(), StatusCode::FORBIDDEN);
        let r = receive_invite(&node.shared, "2001:4860::1".parse().unwrap(), &body).await;
        assert_eq!(r.status(), StatusCode::FORBIDDEN);
        let r = receive_reply(&node.shared, "8.8.8.8".parse().unwrap(), b"{}");
        assert_eq!(r.status(), StatusCode::FORBIDDEN);
        // Hidden: refused even from the LAN.
        node.set_visible(false);
        let r = receive_invite(&node.shared, "192.168.1.9".parse().unwrap(), &body).await;
        assert_eq!(r.status(), StatusCode::FORBIDDEN);
        assert_eq!(node.me().invite_port, 0, "no endpoint bound");
        node.stop().await;
    }
}
