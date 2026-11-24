//! Finding meshes on this network.
//!
//! Four sources run at once and feed one deduplicating recorder:
//!
//! * **beacon** — GradApp 2 hosts multicast a [`BeaconPacket`] every ~2 s; the
//!   listener also sends a query that makes hosts announce immediately.
//! * **mDNS** — the coordinator claims `gradmesh.local` and registers
//!   `_gradmesh._tcp.local.` (coordinator/discovery.py); both are resolved with
//!   a pure-Rust responder client, plus the OS resolver as in v1.
//! * **sweep** — `GET /health` on the coordinator port across this machine's
//!   subnet (capped to the /24 around each address), 128 at a time.
//! * **manual** — addresses the person typed (anything `parse_address` takes).
//!
//! Every candidate is confirmed with an unauthenticated `/health`, then its
//! join port is asked for the token (`/gradapp.json`, then `/join.sh`).

use std::collections::HashSet;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4};
use std::time::{Duration, Instant};

use futures::stream::{FuturesUnordered, StreamExt};
use gm_types::{
    AppEvent, DiscoveredMesh, DiscoverySource, BEACON_GROUP, BEACON_PORT, COORDINATOR_PORT,
    WEB_PORT,
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::address::{parse_address_with, AddressDefaults};
use crate::beacon::{bind_beacon_listener, parse_beacon, BEACON_QUERY};
use crate::invite::{lan_client, probe_health, read_invite_with, HealthHit, InviteInfo};
use crate::lan::{lan_addresses, sweep_targets};

/// The name the coordinator claims over mDNS.
pub const MDNS_HOSTNAME: &str = "gradmesh.local";
/// The coordinator's service type (coordinator/discovery.py).
pub const MDNS_SERVICE: &str = "_gradmesh._tcp.local.";

/// Knobs for [`discover`]. `Default` is what the app uses on a real LAN;
/// tests inject ports and targets.
#[derive(Debug, Clone)]
pub struct DiscoverOptions {
    pub coordinator_port: u16,
    pub web_port: u16,

    pub beacon: bool,
    pub beacon_port: u16,
    pub beacon_group: Ipv4Addr,
    /// Send a query on start so beacon hosts answer at once.
    pub beacon_query: bool,
    /// Beacons from these mesh ids are ignored (e.g. the mesh this app hosts).
    pub ignore_mesh_ids: Vec<String>,

    pub mdns: bool,
    pub mdns_hostname: String,
    pub mdns_service: String,

    pub sweep: bool,
    /// Hosts to sweep. `None`: every host on each real adapter's subnet.
    pub sweep_targets: Option<Vec<String>>,
    pub concurrency: usize,
    /// Per-host timeout for the sweep (most hosts never answer).
    pub sweep_timeout: Duration,
    /// Timeout confirming a beacon / mDNS / manual candidate.
    pub confirm_timeout: Duration,

    pub manual: Vec<String>,

    /// Keep listening for beacons and mDNS at least this long, even when the
    /// sweep is done sooner (one beacon interval plus slack).
    pub listen_for: Duration,
    /// Ask each mesh's join port for its token.
    pub read_invites: bool,

    pub cancel: CancellationToken,
}

impl Default for DiscoverOptions {
    fn default() -> Self {
        Self {
            coordinator_port: COORDINATOR_PORT,
            web_port: WEB_PORT,
            beacon: true,
            beacon_port: BEACON_PORT,
            beacon_group: Ipv4Addr::from(BEACON_GROUP),
            beacon_query: true,
            ignore_mesh_ids: Vec::new(),
            mdns: true,
            mdns_hostname: MDNS_HOSTNAME.into(),
            mdns_service: MDNS_SERVICE.into(),
            sweep: true,
            sweep_targets: None,
            concurrency: 128,
            sweep_timeout: Duration::from_millis(700),
            confirm_timeout: Duration::from_millis(1500),
            manual: Vec::new(),
            listen_for: Duration::from_millis(2500),
            read_invites: true,
            cancel: CancellationToken::new(),
        }
    }
}

/// A confirmed candidate on its way to the recorder.
#[derive(Debug, Clone)]
struct Candidate {
    hit: HealthHit,
    source: DiscoverySource,
    web_port: u16,
    token: Option<String>,
    mesh_name: Option<String>,
}

enum Msg {
    Found(Candidate),
    Progress { scanned: u32, total: u32 },
}

/// v1: /^\d+\.\d+\.\d+\.\d+$/
fn is_ipv4_literal(host: &str) -> bool {
    let parts: Vec<&str> = host.split('.').collect();
    parts.len() == 4
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
}

/// The deduplicating recorder (v1 `record`). Public so its preference rule is
/// testable on its own.
#[derive(Debug, Default)]
pub struct MeshTable {
    meshes: Vec<DiscoveredMesh>,
    own: HashSet<String>,
}

impl MeshTable {
    pub fn new(own_addresses: impl IntoIterator<Item = String>) -> Self {
        let mut own: HashSet<String> = own_addresses.into_iter().collect();
        own.insert("127.0.0.1".into());
        own.insert("localhost".into());
        Self {
            meshes: Vec::new(),
            own,
        }
    }

    /// Record a sighting. Dedupes by mesh id: the first address wins, except
    /// that an IP replaces a name (gradmesh.local → 192.168.1.20), because an
    /// IP is the address every machine can use. Returns the stored entry when
    /// the table changed.
    pub fn record(&mut self, mesh: DiscoveredMesh) -> Option<&DiscoveredMesh> {
        let is_ip = is_ipv4_literal(&mesh.hostname);
        let mut mesh = mesh;
        mesh.local = mesh.local || self.own.contains(&mesh.hostname);
        match self.meshes.iter().position(|m| m.mesh_id == mesh.mesh_id) {
            Some(i) => {
                if !is_ip || is_ipv4_literal(&self.meshes[i].hostname) {
                    return None;
                }
                let old = &self.meshes[i];
                if mesh.token.is_none() {
                    mesh.token = old.token.clone();
                }
                if mesh.mesh_name.is_none() {
                    mesh.mesh_name = old.mesh_name.clone();
                }
                self.meshes[i] = mesh;
                Some(&self.meshes[i])
            }
            None => {
                self.meshes.push(mesh);
                self.meshes.last()
            }
        }
    }

    /// Apply an invite read for `hostname`; ignored if the entry moved to
    /// another address meanwhile.
    fn apply_invite(
        &mut self,
        mesh_id: &str,
        hostname: &str,
        invite: InviteInfo,
    ) -> Option<&DiscoveredMesh> {
        let entry = self
            .meshes
            .iter_mut()
            .find(|m| m.mesh_id == mesh_id && m.hostname == hostname)?;
        let mut changed = false;
        if invite.token.is_some() && invite.token != entry.token {
            entry.token = invite.token;
            changed = true;
        }
        if invite.mesh_name.is_some() && invite.mesh_name != entry.mesh_name {
            entry.mesh_name = invite.mesh_name;
            changed = true;
        }
        changed.then_some(&*entry)
    }

    /// Best first: this machine's mesh, then the busiest.
    pub fn into_sorted(self) -> Vec<DiscoveredMesh> {
        let mut out = self.meshes;
        out.sort_by(|a, b| {
            b.local
                .cmp(&a.local)
                .then(b.nodes_active.cmp(&a.nodes_active))
        });
        out
    }
}

fn to_mesh(c: Candidate) -> DiscoveredMesh {
    let h = &c.hit;
    DiscoveredMesh {
        mesh_id: h.health.mesh_id.clone(),
        hostname: h.hostname.clone(),
        coordinator: format!("http://{}:{}", h.hostname, h.port),
        web: format!("http://{}:{}", h.hostname, c.web_port),
        version: h.health.version.clone(),
        protocol: h.health.protocol,
        nodes_active: h.health.nodes_active,
        torch_ready: h.health.torch_ready,
        local: false,
        source: c.source,
        token: c.token,
        mesh_name: c.mesh_name,
        latency_ms: Some(h.latency_ms),
    }
}

/// Meshes reachable from here, best first.
///
/// `on_event` receives [`AppEvent::MeshFound`] as soon as a mesh is confirmed
/// (again, with the same `mesh_id`, when a better address or its token
/// arrives: treat it as an upsert) and [`AppEvent::ScanProgress`] for the
/// sweep. Cancelling `opts.cancel` (or dropping the future) stops every
/// source at once; on cancel the meshes found so far are returned.
pub async fn discover<F>(opts: DiscoverOptions, mut on_event: F) -> Vec<DiscoveredMesh>
where
    F: FnMut(AppEvent),
{
    let started = Instant::now();
    let client = lan_client();
    let cancel = opts.cancel.clone();
    let listen = cancel.child_token();
    let lans = lan_addresses();
    let mut table = MeshTable::new(lans.iter().map(|a| a.address.to_string()));

    let (tx, mut rx) = mpsc::channel::<Msg>(64);

    let manual = {
        let tx = tx.clone();
        let (client, opts) = (client.clone(), opts.clone());
        async move {
            let defaults = AddressDefaults {
                port: opts.coordinator_port,
                web_port: opts.web_port,
            };
            let mut probes = FuturesUnordered::new();
            for text in &opts.manual {
                let Some(invite) = parse_address_with(text, defaults) else {
                    continue;
                };
                let Some(port) = url::Url::parse(&invite.coordinator)
                    .ok()
                    .and_then(|u| u.port_or_known_default())
                else {
                    continue;
                };
                let client = client.clone();
                probes.push(async move {
                    let hit =
                        probe_health(&client, &invite.hostname, port, opts.confirm_timeout).await;
                    hit.map(|hit| Candidate {
                        hit,
                        source: DiscoverySource::Manual,
                        web_port: opts.web_port,
                        token: invite.token,
                        mesh_name: invite.mesh_name,
                    })
                });
            }
            while let Some(found) = probes.next().await {
                if let Some(c) = found {
                    let _ = tx.send(Msg::Found(c)).await;
                }
            }
        }
    };

    let sweep = {
        let tx = tx.clone();
        let (client, opts) = (client.clone(), opts.clone());
        let lans = lans.clone();
        async move {
            if !opts.sweep {
                return;
            }
            let targets: Vec<String> = match opts.sweep_targets {
                Some(t) => t,
                None => sweep_targets(&lans)
                    .into_iter()
                    .map(|ip| ip.to_string())
                    .collect(),
            };
            let total = targets.len() as u32;
            let _ = tx.send(Msg::Progress { scanned: 0, total }).await;
            let mut results = futures::stream::iter(targets)
                .map(|host| {
                    let client = client.clone();
                    async move {
                        probe_health(&client, &host, opts.coordinator_port, opts.sweep_timeout)
                            .await
                    }
                })
                .buffer_unordered(opts.concurrency.max(1));
            let mut scanned = 0u32;
            while let Some(hit) = results.next().await {
                scanned += 1;
                if let Some(hit) = hit {
                    let c = Candidate {
                        hit,
                        source: DiscoverySource::Sweep,
                        web_port: opts.web_port,
                        token: None,
                        mesh_name: None,
                    };
                    let _ = tx.send(Msg::Found(c)).await;
                }
                if scanned & 15 == 0 || scanned == total {
                    let _ = tx.send(Msg::Progress { scanned, total }).await;
                }
            }
        }
    };

    let beacon = {
        let tx = tx.clone();
        let (client, opts, listen) = (client.clone(), opts.clone(), listen.clone());
        let lans = lans.clone();
        async move {
            if opts.beacon {
                // Cancellation must also win while a found mesh is waiting
                // for room in the channel (the consumer stops reading on cancel).
                listen
                    .run_until_cancelled(listen_beacons(&client, &opts, &lans, &listen, &tx))
                    .await;
            }
        }
    };

    let mdns = {
        let tx = tx.clone();
        let (client, opts, listen) = (client.clone(), opts.clone(), listen.clone());
        async move {
            if opts.mdns {
                listen
                    .run_until_cancelled(resolve_mdns(&client, &opts, &tx))
                    .await;
            }
        }
    };
    drop(tx);

    // Beacon and mDNS listen until the sweep and manual probes are done and
    // at least `listen_for` has passed.
    let bounded = {
        let (listen, cancel) = (listen.clone(), cancel.clone());
        let listen_for = opts.listen_for;
        async move {
            cancel
                .run_until_cancelled(async { tokio::join!(manual, sweep) })
                .await;
            let until = started + listen_for;
            cancel
                .run_until_cancelled(tokio::time::sleep_until(until.into()))
                .await;
            listen.cancel();
        }
    };
    let producers = async { tokio::join!(bounded, beacon, mdns) };

    let read_invites = opts.read_invites;
    let consumer = async {
        let mut invites = FuturesUnordered::new();
        let mut closed = false;
        loop {
            if closed && invites.is_empty() {
                break;
            }
            tokio::select! {
                _ = cancel.cancelled() => break,
                msg = rx.recv(), if !closed => match msg {
                    None => closed = true,
                    Some(Msg::Progress { scanned, total }) => on_event(AppEvent::ScanProgress { scanned, total }),
                    Some(Msg::Found(c)) => {
                        let web_port = c.web_port;
                        if let Some(mesh) = table.record(to_mesh(c)) {
                            on_event(AppEvent::MeshFound { mesh: mesh.clone() });
                            if read_invites {
                                let (client, id, host) = (client.clone(), mesh.mesh_id.clone(), mesh.hostname.clone());
                                invites.push(async move {
                                    let info = read_invite_with(&client, &host, web_port).await;
                                    (id, host, info)
                                });
                            }
                        }
                    }
                },
                Some((id, host, info)) = invites.next(), if !invites.is_empty() => {
                    if let Some(mesh) = table.apply_invite(&id, &host, info) {
                        on_event(AppEvent::MeshFound { mesh: mesh.clone() });
                    }
                }
            }
        }
    };
    tokio::join!(producers, consumer);
    table.into_sorted()
}

/// Send a beacon query to the group (on every interface) and to broadcast.
async fn send_query(
    socket: &tokio::net::UdpSocket,
    opts: &DiscoverOptions,
    lans: &[crate::lan::LanAddress],
) {
    let query = BEACON_QUERY.as_bytes();
    let sock = socket2::SockRef::from(socket);
    let _ = sock.set_broadcast(true);
    let _ = sock.set_multicast_ttl_v4(1);
    for lan in lans.iter().filter(|l| !l.is_virtual) {
        if sock.set_multicast_if_v4(&lan.address).is_ok() {
            let _ = socket
                .send_to(
                    query,
                    SocketAddrV4::new(opts.beacon_group, opts.beacon_port),
                )
                .await;
        }
    }
    let _ = sock.set_multicast_if_v4(&Ipv4Addr::UNSPECIFIED);
    let _ = socket
        .send_to(
            query,
            SocketAddrV4::new(opts.beacon_group, opts.beacon_port),
        )
        .await;
    let _ = socket
        .send_to(
            query,
            SocketAddrV4::new(Ipv4Addr::BROADCAST, opts.beacon_port),
        )
        .await;
}

async fn listen_beacons(
    client: &reqwest::Client,
    opts: &DiscoverOptions,
    lans: &[crate::lan::LanAddress],
    listen: &CancellationToken,
    tx: &mpsc::Sender<Msg>,
) {
    let socket = match bind_beacon_listener(opts.beacon_port, opts.beacon_group) {
        Ok(s) => s,
        Err(e) => {
            tracing::debug!(
                "beacon listener unavailable on port {}: {e}",
                opts.beacon_port
            );
            return;
        }
    };
    if opts.beacon_query {
        send_query(&socket, opts, lans).await;
    }
    let mut seen: HashSet<(IpAddr, u16)> = HashSet::new();
    let mut probes = FuturesUnordered::new();
    let mut buf = vec![0u8; 4096];
    loop {
        tokio::select! {
            _ = listen.cancelled() => return,
            received = socket.recv_from(&mut buf) => {
                let Ok((n, from)) = received else { continue };
                let Some(packet) = parse_beacon(&buf[..n]) else { continue };
                if opts.ignore_mesh_ids.contains(&packet.mesh_id) {
                    continue;
                }
                let SocketAddr::V4(from) = from else { continue };
                let key = (IpAddr::V4(*from.ip()), packet.coordinator_port);
                if !seen.insert(key) {
                    continue;
                }
                let client = client.clone();
                let timeout = opts.confirm_timeout;
                probes.push(async move {
                    let host = from.ip().to_string();
                    (key, probe_health(&client, &host, packet.coordinator_port, timeout).await, packet)
                });
            }
            Some((key, hit, packet)) = probes.next(), if !probes.is_empty() => {
                match hit {
                    Some(hit) => {
                        let c = Candidate {
                            hit,
                            source: DiscoverySource::Beacon,
                            web_port: packet.web_port,
                            token: None,
                            mesh_name: packet.mesh_name,
                        };
                        let _ = tx.send(Msg::Found(c)).await;
                    }
                    // Not reachable (yet); let a later beacon retry.
                    None => { seen.remove(&key); }
                }
            }
        }
    }
}

/// Resolve `gradmesh.local` (mDNS responder + OS resolver) and browse for the
/// coordinator service. Runs until cancelled by the caller.
async fn resolve_mdns(client: &reqwest::Client, opts: &DiscoverOptions, tx: &mpsc::Sender<Msg>) {
    let (addr_tx, mut addr_rx) = mpsc::unbounded_channel::<(IpAddr, u16)>();

    // OS resolver, as in v1 (macOS, Windows 10+, nss-mdns Linux).
    let os = {
        let addr_tx = addr_tx.clone();
        let host = opts.mdns_hostname.clone();
        let port = opts.coordinator_port;
        async move {
            if let Ok(Ok(addrs)) = tokio::time::timeout(
                Duration::from_secs(3),
                tokio::net::lookup_host((host.as_str(), port)),
            )
            .await
            {
                for a in addrs.filter(|a| a.is_ipv4()) {
                    let _ = addr_tx.send((a.ip(), port));
                }
            }
        }
    };

    // Our own mDNS queries: works even where the OS resolver does not do mDNS.
    let daemon = mdns_sd::ServiceDaemon::new().ok();
    let own = {
        let addr_tx = addr_tx.clone();
        let host = format!("{}.", opts.mdns_hostname.trim_end_matches('.'));
        let service = opts.mdns_service.clone();
        let port = opts.coordinator_port;
        let receivers = daemon.as_ref().map(|d| {
            (
                d.resolve_hostname(&host, Some(5000)).ok(),
                d.browse(&service).ok(),
            )
        });
        async move {
            let Some((hosts, services)) = receivers else {
                return;
            };
            let hosts = async {
                let Some(rx) = hosts else { return };
                while let Ok(event) = rx.recv_async().await {
                    if let mdns_sd::HostnameResolutionEvent::AddressesFound(_, addrs) = event {
                        for ip in addrs.into_iter().filter(IpAddr::is_ipv4) {
                            let _ = addr_tx.send((ip, port));
                        }
                    }
                }
            };
            let services = async {
                let Some(rx) = services else { return };
                while let Ok(event) = rx.recv_async().await {
                    if let mdns_sd::ServiceEvent::ServiceResolved(info) = event {
                        for ip in info.get_addresses_v4() {
                            let _ = addr_tx.send((IpAddr::V4(*ip), info.get_port()));
                        }
                    }
                }
            };
            tokio::join!(hosts, services);
        }
    };
    drop(addr_tx);
    let _guard = DaemonGuard(daemon);

    let mut seen = HashSet::new();
    let mut probes = FuturesUnordered::new();
    let sources = async { tokio::join!(os, own) };
    tokio::pin!(sources);
    let mut sources_done = false;
    loop {
        tokio::select! {
            _ = &mut sources, if !sources_done => sources_done = true,
            Some((ip, port)) = addr_rx.recv() => {
                if seen.insert((ip, port)) {
                    let client = client.clone();
                    let timeout = opts.confirm_timeout;
                    probes.push(async move { probe_health(&client, &ip.to_string(), port, timeout).await });
                }
            }
            Some(hit) = probes.next(), if !probes.is_empty() => {
                if let Some(hit) = hit {
                    let c = Candidate { hit, source: DiscoverySource::Mdns, web_port: opts.web_port, token: None, mesh_name: None };
                    let _ = tx.send(Msg::Found(c)).await;
                }
            }
            else => break,
        }
    }
}

struct DaemonGuard(Option<mdns_sd::ServiceDaemon>);

impl Drop for DaemonGuard {
    fn drop(&mut self) {
        if let Some(d) = self.0.take() {
            let _ = d.shutdown();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mesh(id: &str, host: &str, nodes: u32) -> DiscoveredMesh {
        DiscoveredMesh {
            mesh_id: id.into(),
            hostname: host.into(),
            coordinator: format!("http://{host}:8000"),
            web: format!("http://{host}:3000"),
            version: "5.1.1".into(),
            protocol: 5,
            nodes_active: nodes,
            torch_ready: false,
            local: false,
            source: DiscoverySource::Sweep,
            token: None,
            mesh_name: None,
            latency_ms: Some(1),
        }
    }

    #[test]
    fn ip_literal_rule() {
        assert!(is_ipv4_literal("192.168.1.20"));
        assert!(!is_ipv4_literal("gradmesh.local"));
        assert!(!is_ipv4_literal("[::1]"));
        assert!(!is_ipv4_literal("1.2.3"));
    }

    #[test]
    fn dedupe_prefers_ip() {
        let mut t = MeshTable::new(vec!["192.168.1.5".to_string()]);
        let mut named = mesh("m1", "gradmesh.local", 1);
        named.token = Some("tok".into());
        assert!(t.record(named).is_some());
        // Another name does not replace a name.
        assert!(t.record(mesh("m1", "other.local", 1)).is_none());
        // An IP replaces a name and keeps what the name had learned.
        let e = t.record(mesh("m1", "192.168.1.20", 1)).unwrap().clone();
        assert_eq!(e.hostname, "192.168.1.20");
        assert_eq!(e.token.as_deref(), Some("tok"));
        // Neither a later IP nor a name replaces an IP.
        assert!(t.record(mesh("m1", "192.168.1.21", 1)).is_none());
        assert!(t.record(mesh("m1", "gradmesh.local", 1)).is_none());
        // Sorting: local first, then busiest.
        t.record(mesh("m2", "10.0.0.9", 9));
        t.record(mesh("m3", "192.168.1.5", 0));
        let ids: Vec<_> = t.into_sorted().into_iter().map(|m| m.mesh_id).collect();
        assert_eq!(ids, ["m3", "m2", "m1"]);
    }
}
