//! "GradApps on this network": this app's presence on the LAN, the list of
//! other GradApps, and invites between them (gm-net `PeerNode`).
//!
//! * Starts with the app; a 1 s loop keeps what it announces in step with the
//!   app (name, idle/contributing/hosting, mesh, GPU, port state) and with
//!   Settings › "Visible to other GradApps" and the blocked devices.
//! * Every peer and invite event goes to the window as `gm://event`. An
//!   incoming invite also raises an OS notification when the window is not in
//!   front (Settings › Notifications), and the window shows it as a dialog.
//! * Never joins by itself: `invite_respond("join")` runs the normal join
//!   flow with the invite, and only then tells the inviter "accepted".
//! * Degrades: if UDP 47800 or the invite port cannot be bound (port taken,
//!   firewall), the list or the invites are off and the window says so;
//!   mesh discovery (beacon/mDNS/sweep) is unaffected.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gm_net::peers::{random_id, OutgoingInvite};
use gm_net::{MeshOffer, PeerConfig, PeerNode};
use gm_types::*;
use serde::Serialize;
use tauri::{AppHandle, Manager, State};
use tauri_plugin_notification::NotificationExt as _;

use crate::backend::{emit, AppBackend};

type Res<T> = Result<T, String>;

/// Managed state: the running node, once started.
#[derive(Default)]
pub struct Peers {
    node: Mutex<Option<Arc<PeerNode>>>,
}

impl Peers {
    fn node(&self) -> Option<Arc<PeerNode>> {
        self.node.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }
}

/// `peers_state`: everything the "GradApps on this network" panel shows.
#[derive(Debug, Clone, Serialize)]
pub struct PeersView {
    /// The peer service runs.
    pub running: bool,
    /// UDP presence could be bound (else: nobody is listed; mesh discovery still works).
    pub listening: bool,
    /// Settings › Visible to other GradApps.
    pub visible: bool,
    /// TCP port of this app's invite endpoint; 0 = cannot receive invites.
    pub invite_port: u16,
    pub me: Option<PeerInfo>,
    pub peers: Vec<PeerInfo>,
    /// Invites waiting for an answer here (without their token).
    pub incoming: Vec<MeshInvite>,
    pub outgoing: Vec<OutgoingInvite>,
    pub blocked: usize,
}

/// One row of `peer_invite`'s answer.
#[derive(Debug, Clone, Serialize)]
pub struct InviteSent {
    pub device_id: String,
    pub nonce: Option<String>,
    pub error: Option<String>,
}

/// `<config>/GradApp/device-id`: a random id made once, kept next to settings.
fn device_id() -> String {
    let path: PathBuf = dirs::config_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("GradApp")
        .join("device-id");
    if let Ok(id) = std::fs::read_to_string(&path) {
        let id = id.trim();
        if !id.is_empty()
            && id.len() <= 64
            && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            return id.to_string();
        }
    }
    let id = random_id();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Err(error) = std::fs::write(&path, &id) {
        tracing::warn!(
            "could not save the device id in {}: {error}",
            path.display()
        );
    }
    id
}

fn env_port(key: &str) -> Option<u16> {
    std::env::var(key).ok().and_then(|v| v.trim().parse().ok())
}

/// The real LAN setup. `GRADAPP_PEER_PORT` / `GRADAPP_INVITE_PORT` move
/// presence and the invite endpoint for development.
fn config() -> PeerConfig {
    let mut cfg = PeerConfig::default();
    if let Some(port) = env_port("GRADAPP_PEER_PORT").filter(|p| *p != 0) {
        cfg.listen_port = port;
        cfg.send_port = port;
    }
    if let Some(port) = env_port("GRADAPP_INVITE_PORT") {
        cfg.invite_binds = vec![std::net::SocketAddr::from((
            std::net::Ipv4Addr::UNSPECIFIED,
            port,
        ))];
    }
    cfg
}

/// What this app announces, from the backend's current state.
async fn announce(app: &AppHandle, port_state: Option<PortState>) -> PeerInfo {
    let backend = app.state::<AppBackend>();
    let settings = backend.settings();
    let device = backend.device().await;
    let status = backend.status();
    let name = match settings.device_name.trim() {
        "" => device.facts.hostname.clone(),
        name => name.to_string(),
    };
    let (status, mesh_id, mesh_name) = if let Some(h) = &status.hosting {
        (
            PeerStatus::Hosting,
            Some(h.mesh_id.clone()),
            Some(h.mesh_name.clone()),
        )
    } else if backend.contributing() {
        (
            PeerStatus::Contributing,
            backend.joined_mesh_id(),
            status.joined.as_ref().and_then(|j| j.mesh_name.clone()),
        )
    } else {
        (PeerStatus::Idle, None, None)
    };
    let gpu = device
        .profile
        .gpu_name
        .clone()
        .or_else(|| device.facts.gpus.first().map(|g| g.name.clone()));
    PeerInfo {
        device_id: String::new(), // the node keeps its own
        name,
        os: device.facts.os,
        gpu,
        backend: Some(device.profile.backend),
        port_state,
        status,
        mesh_id,
        mesh_name,
        app_version: APP_VERSION.to_string(),
        invite_port: 0, // the node fills in its bound port
        address: None,
    }
}

async fn selected_port_state(app: &AppHandle) -> Option<PortState> {
    let backend = app.state::<AppBackend>();
    backend
        .ports()
        .await
        .into_iter()
        .find(|p| p.selected)
        .map(|p| p.status.state)
}

fn window_in_front(app: &AppHandle) -> bool {
    app.get_webview_window("main").is_some_and(|w| {
        w.is_visible().unwrap_or(false)
            && w.is_focused().unwrap_or(false)
            && !w.is_minimized().unwrap_or(false)
    })
}

/// Events from the node: to the window (invites without their token), plus
/// an OS notification for an incoming invite when the window is not in front.
fn on_event(app: &AppHandle, event: AppEvent) {
    if let AppEvent::InviteReceived { invite } = &event {
        let mut shown = invite.clone();
        shown.token.clear();
        emit(
            app,
            AppEvent::InviteReceived {
                invite: shown.clone(),
            },
        );
        let backend = app.state::<AppBackend>();
        backend.log(
            app,
            ProcRole::Worker,
            format!(
                "Invite from {} ({}) to contribute to {}",
                shown.from_name,
                shown.from_address.clone().unwrap_or_default(),
                shown.mesh_name
            ),
        );
        if !window_in_front(app) {
            if backend.settings().notifications {
                let _ = app
                    .notification()
                    .builder()
                    .title(format!("{} invites you to contribute", shown.from_name))
                    .body(format!(
                        "{} invites you to contribute to {}. Open GradApp to answer.",
                        shown.from_name, shown.mesh_name
                    ))
                    .show();
            }
            // Flash the taskbar/dock entry; the invite dialog is already on screen.
            if let Some(window) = app.get_webview_window("main") {
                if window.is_visible().unwrap_or(false) {
                    let _ = window
                        .request_user_attention(Some(tauri::UserAttentionType::Informational));
                }
            }
        }
        return;
    }
    emit(app, event);
}

/// Start presence and the invite endpoint, then keep them in step with the app.
pub fn start(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let port_state = selected_port_state(&app).await;
        let mut me = announce(&app, port_state).await;
        me.device_id = device_id();
        let settings = app.state::<AppBackend>().settings();
        let sink = app.clone();
        let node = PeerNode::start(config(), me, settings.visible_on_lan, move |e| {
            on_event(&sink, e)
        })
        .await;
        node.set_blocked(settings.blocked_peers.clone());
        tracing::info!(
            "peers: device {} · presence {} · invites {}",
            node.me().device_id,
            if node.presence_listening() {
                "listening"
            } else {
                "unavailable (UDP port taken or blocked)"
            },
            node.invite_addr()
                .map(|a| a.to_string())
                .unwrap_or_else(|| "unavailable".into())
        );
        let node = Arc::new(node);
        *app.state::<Peers>()
            .node
            .lock()
            .unwrap_or_else(|p| p.into_inner()) = Some(node.clone());

        // Keep the announcement current. The port state reads files, so it
        // is refreshed less often than the rest.
        let mut port_state = port_state;
        let mut port_checked = Instant::now();
        loop {
            tokio::time::sleep(Duration::from_secs(1)).await;
            if app.state::<Peers>().node().is_none() {
                return; // stopped
            }
            if port_checked.elapsed() > Duration::from_secs(15) {
                port_state = selected_port_state(&app).await;
                port_checked = Instant::now();
            }
            let settings = app.state::<AppBackend>().settings();
            node.set_visible(settings.visible_on_lan);
            node.set_blocked(settings.blocked_peers.clone());
            node.update(announce(&app, port_state).await);
        }
    });
}

/// On quit: say "bye" so the others drop this machine at once.
pub async fn stop(app: &AppHandle) {
    let node = app
        .state::<Peers>()
        .node
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .take();
    let Some(mut node) = node else { return };
    // The sync loop lets go of its copy within a second of seeing it gone.
    let until = Instant::now() + Duration::from_millis(1500);
    loop {
        match Arc::try_unwrap(node) {
            Ok(owned) => return owned.stop().await,
            Err(shared) if Instant::now() < until => {
                node = shared;
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            // Dropping cancels presence, which still says "bye" on its way out.
            Err(shared) => return drop(shared),
        }
    }
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn peers_state(
    peers: State<'_, Peers>,
    backend: State<'_, AppBackend>,
) -> Res<PeersView> {
    let settings = backend.settings();
    let Some(node) = peers.node() else {
        return Ok(PeersView {
            running: false,
            listening: false,
            visible: settings.visible_on_lan,
            invite_port: 0,
            me: None,
            peers: Vec::new(),
            incoming: Vec::new(),
            outgoing: Vec::new(),
            blocked: settings.blocked_peers.len(),
        });
    };
    Ok(PeersView {
        running: true,
        listening: node.presence_listening(),
        visible: node.visible(),
        invite_port: node.me().invite_port,
        me: Some(node.me()),
        peers: node.peers(),
        incoming: node
            .incoming()
            .into_iter()
            .map(|mut i| {
                i.token.clear();
                i
            })
            .collect(),
        outgoing: node.outgoing(),
        blocked: settings.blocked_peers.len(),
    })
}

/// Invite these GradApps into the mesh this app hosts, all at once.
#[tauri::command]
pub async fn peer_invite(
    app: AppHandle,
    peers: State<'_, Peers>,
    backend: State<'_, AppBackend>,
    device_ids: Vec<String>,
) -> Res<Vec<InviteSent>> {
    let node = peers
        .node()
        .ok_or("Peer discovery is not running on this computer.")?;
    let hosting = backend
        .status()
        .hosting
        .ok_or("Host a mesh first; invites bring machines into the mesh this computer hosts.")?;
    let offer = MeshOffer {
        mesh_name: hosting.mesh_name.clone(),
        mesh_id: hosting.mesh_id.clone(),
        coordinator: hosting.coordinator.clone(),
        token: hosting.token.clone(),
    };
    let sends = device_ids.into_iter().take(64).map(|id| {
        let (node, offer) = (node.clone(), offer.clone());
        async move {
            let result = node.invite(&id, &offer).await;
            InviteSent {
                device_id: id,
                nonce: result.as_ref().ok().cloned(),
                error: result.err(),
            }
        }
    });
    let results = futures::future::join_all(sends).await;
    let names: std::collections::HashMap<String, String> = node
        .peers()
        .into_iter()
        .map(|p| (p.device_id, p.name))
        .collect();
    for r in &results {
        let who = names.get(&r.device_id).cloned().unwrap_or_default();
        let line = match &r.error {
            None => format!("Invited {who} to {}", hosting.mesh_name),
            Some(e) => format!("Could not invite {who}: {e}"),
        };
        backend.log(&app, ProcRole::Coordinator, line);
    }
    Ok(results)
}

/// Answer an invite: "join" (runs the normal join; the inviter hears
/// "accepted" only if it worked), "decline", or "block" (decline and refuse
/// that device from now on).
#[tauri::command]
pub async fn invite_respond(
    app: AppHandle,
    peers: State<'_, Peers>,
    backend: State<'_, AppBackend>,
    nonce: String,
    action: String,
) -> Res<Option<Invite>> {
    let node = peers
        .node()
        .ok_or("Peer discovery is not running on this computer.")?;
    let invite = node
        .incoming()
        .into_iter()
        .find(|i| i.nonce == nonce)
        .ok_or("This invite has expired or was already answered.")?;
    match action.as_str() {
        "join" => {
            let link =
                gm_net::invite_link(&invite.coordinator, &invite.token, Some(&invite.mesh_name));
            let joined = backend.join(&app, &link).await?;
            node.respond(&nonce, InviteOutcome::Accepted, false);
            Ok(Some(joined))
        }
        "decline" => {
            node.respond(&nonce, InviteOutcome::Declined, false);
            Ok(None)
        }
        "block" => {
            node.respond(&nonce, InviteOutcome::Declined, true);
            let mut settings = backend.settings();
            if !settings.blocked_peers.contains(&invite.from_device) {
                settings.blocked_peers.push(invite.from_device.clone());
            }
            backend.save_settings(settings)?;
            backend.log(
                &app,
                ProcRole::Worker,
                format!(
                    "Blocked invites from {} ({})",
                    invite.from_name, invite.from_device
                ),
            );
            Ok(None)
        }
        other => Err(format!("Unknown answer “{other}”.")),
    }
}

#[tauri::command]
pub async fn peers_unblock_all(peers: State<'_, Peers>, backend: State<'_, AppBackend>) -> Res<()> {
    let mut settings = backend.settings();
    settings.blocked_peers.clear();
    backend.save_settings(settings)?;
    if let Some(node) = peers.node() {
        node.set_blocked(Vec::new());
    }
    Ok(())
}
