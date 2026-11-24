//! Peers end to end on loopback: presence between two in-process "apps",
//! the query/answer, expiry and "bye", and the invite endpoint's rules.
//! Unicast to 127.0.0.1 stands in for multicast (CI containers do not always
//! loop multicast back); the multicast path has its own test that skips
//! itself where there is no LAN interface.

use std::net::{Ipv4Addr, SocketAddr};
use std::time::{Duration, Instant};

use axum::routing::get;
use axum::{Json, Router};
use gm_net::peers::{encode_peer, parse_peer, random_id, PEER_QUERY};
use gm_net::{MeshOffer, PeerConfig, PeerNode};
use gm_types::{AppEvent, Backend, InviteOutcome, MeshInvite, Os, PeerInfo, PeerStatus, PortState};
use serde_json::json;
use tokio::sync::mpsc;

fn free_udp_port() -> u16 {
    std::net::UdpSocket::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn me(id: &str, name: &str) -> PeerInfo {
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
        invite_port: 0,
        address: None,
    }
}

/// Loopback-only config: listens on `listen`, unicasts to `to`.
fn cfg(listen: u16, to: &[u16]) -> PeerConfig {
    PeerConfig {
        interval: Duration::from_millis(200),
        listen_port: listen,
        send_port: listen,
        multicast: false,
        broadcast: false,
        extra_targets: to
            .iter()
            .map(|p| SocketAddr::from((Ipv4Addr::LOCALHOST, *p)))
            .collect(),
        invite_binds: vec![SocketAddr::from((Ipv4Addr::LOCALHOST, 0))],
        ..PeerConfig::default()
    }
}

type Events = mpsc::UnboundedReceiver<AppEvent>;

async fn node(cfg: PeerConfig, id: &str, name: &str) -> (PeerNode, Events) {
    let (tx, rx) = mpsc::unbounded_channel();
    let node = PeerNode::start(cfg, me(id, name), true, move |e| {
        let _ = tx.send(e);
    })
    .await;
    (node, rx)
}

/// The first event matching `pred` within `within`.
async fn wait_for(
    rx: &mut Events,
    within: Duration,
    mut pred: impl FnMut(&AppEvent) -> bool,
) -> Option<AppEvent> {
    let until = Instant::now() + within;
    loop {
        let left = until.saturating_duration_since(Instant::now());
        match tokio::time::timeout(left, rx.recv()).await {
            Ok(Some(e)) if pred(&e) => return Some(e),
            Ok(Some(_)) => continue,
            _ => return None,
        }
    }
}

/// A coordinator `/health` on 127.0.0.1.
async fn health_server(mesh_id: &'static str, protocol: u32, nodes: u32) -> SocketAddr {
    let router = Router::new().route(
        "/health",
        get(move || async move {
            Json(json!({"status":"ok","version":"5.1.1","protocol":protocol,"mesh_id":mesh_id,"torch_ready":true,"nodes_active":nodes}))
        }),
    );
    let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    addr
}

fn offer(coord: SocketAddr, mesh_id: &str) -> MeshOffer {
    MeshOffer {
        mesh_name: "MSI's mesh".into(),
        mesh_id: mesh_id.into(),
        coordinator: format!("http://{coord}"),
        token: "secret-token".into(),
    }
}

fn seen(e: &AppEvent, id: &str) -> bool {
    matches!(e, AppEvent::PeerSeen { peer } if peer.device_id == id)
}

#[tokio::test]
async fn two_apps_see_each_other_and_invite() {
    let coord = health_server("mesh-e2e", gm_types::MESH_PROTOCOL, 3).await;
    let (pa, pb) = (free_udp_port(), free_udp_port());
    let (a, mut ea) = node(cfg(pa, &[pb]), "dev-a", "MSI").await;
    let (b, mut eb) = node(cfg(pb, &[pa]), "dev-b", "studio-m3").await;

    // Presence both ways, quickly, with the address and invite port.
    let started = Instant::now();
    let Some(AppEvent::PeerSeen { peer }) =
        wait_for(&mut ea, Duration::from_secs(2), |e| seen(e, "dev-b")).await
    else {
        panic!("A never saw B");
    };
    assert!(started.elapsed() < Duration::from_secs(1));
    assert_eq!(peer.address.as_deref(), Some("127.0.0.1"));
    assert_eq!(peer.invite_port, b.invite_addr().unwrap().port());
    assert_eq!(peer.name, "studio-m3");
    assert!(
        wait_for(&mut eb, Duration::from_secs(2), |e| seen(e, "dev-a"))
            .await
            .is_some()
    );
    assert_eq!(a.peers().len(), 1, "deduped by device id");

    // A starts hosting: B hears the new status (and never a token).
    let mut hosting = a.me();
    hosting.status = PeerStatus::Hosting;
    hosting.mesh_id = Some("mesh-e2e".into());
    hosting.mesh_name = Some("MSI's mesh".into());
    a.update(hosting);
    let Some(AppEvent::PeerSeen { peer }) = wait_for(
        &mut eb,
        Duration::from_secs(2),
        |e| matches!(e, AppEvent::PeerSeen { peer } if peer.status == PeerStatus::Hosting),
    )
    .await
    else {
        panic!("B never saw A hosting");
    };
    assert_eq!(peer.mesh_name.as_deref(), Some("MSI's mesh"));

    // A invites B.
    let nonce = a.invite("dev-b", &offer(coord, "mesh-e2e")).await.unwrap();
    assert!(wait_for(&mut ea, Duration::from_secs(1), |e| matches!(
        e,
        AppEvent::InviteResult {
            outcome: InviteOutcome::Sent,
            incoming: false,
            ..
        }
    ))
    .await
    .is_some());
    let Some(AppEvent::InviteReceived { invite }) =
        wait_for(&mut eb, Duration::from_secs(2), |e| {
            matches!(e, AppEvent::InviteReceived { .. })
        })
        .await
    else {
        panic!("B got no invite");
    };
    assert_eq!(invite.nonce, nonce);
    assert_eq!(invite.from_name, "MSI");
    assert_eq!(invite.from_device, "dev-a");
    assert_eq!(invite.from_address.as_deref(), Some("127.0.0.1"));
    assert_eq!(invite.nodes_active, Some(3), "probed /health first");
    assert_eq!(invite.token, "secret-token");
    assert_eq!(b.incoming().len(), 1);
    assert_eq!(a.outgoing().len(), 1);

    // B accepts: A hears it through the reply endpoint.
    let answered = b.respond(&nonce, InviteOutcome::Accepted, false).unwrap();
    assert_eq!(answered.coordinator, format!("http://{coord}"));
    assert!(b.incoming().is_empty());
    let Some(AppEvent::InviteResult { device_id, outcome, .. }) =
        wait_for(&mut ea, Duration::from_secs(2), |e| {
            matches!(e, AppEvent::InviteResult { incoming: false, outcome, .. } if *outcome != InviteOutcome::Sent)
        })
        .await
    else {
        panic!("A never heard the answer");
    };
    assert_eq!(device_id, "dev-b");
    assert_eq!(outcome, InviteOutcome::Accepted);
    assert!(a.outgoing().is_empty());
    assert!(
        b.respond(&nonce, InviteOutcome::Accepted, false).is_none(),
        "answered once"
    );

    // A second invite within 30 s is rate-limited by B.
    let err = a
        .invite("dev-b", &offer(coord, "mesh-e2e"))
        .await
        .unwrap_err();
    assert!(err.contains("try again"), "{err}");

    // B quits: its "bye" removes it from A's list at once.
    b.stop().await;
    let started = Instant::now();
    assert!(wait_for(&mut ea, Duration::from_secs(2), |e| matches!(
        e,
        AppEvent::PeerGone { device_id } if device_id == "dev-b"
    ))
    .await
    .is_some());
    assert!(started.elapsed() < Duration::from_secs(1));
    assert!(a.peers().is_empty());
    let err = a
        .invite("dev-b", &offer(coord, "mesh-e2e"))
        .await
        .unwrap_err();
    assert!(err.contains("no longer"), "{err}");
    a.stop().await;
}

#[tokio::test]
async fn accepted_by_presence_when_the_reply_is_lost() {
    let coord = health_server("mesh-p", gm_types::MESH_PROTOCOL, 1).await;
    let (pa, pb) = (free_udp_port(), free_udp_port());
    let (a, mut ea) = node(cfg(pa, &[pb]), "dev-a", "MSI").await;
    let (b, _eb) = node(cfg(pb, &[pa]), "dev-b", "B").await;
    wait_for(&mut ea, Duration::from_secs(2), |e| seen(e, "dev-b"))
        .await
        .unwrap();
    let nonce = a.invite("dev-b", &offer(coord, "mesh-p")).await.unwrap();
    // B joins without replying (e.g. the reply was dropped): its presence says so.
    let mut joined = b.me();
    joined.status = PeerStatus::Contributing;
    joined.mesh_id = Some("mesh-p".into());
    b.update(joined);
    let Some(AppEvent::InviteResult {
        nonce: n, outcome, ..
    }) = wait_for(&mut ea, Duration::from_secs(2), |e| {
        matches!(
            e,
            AppEvent::InviteResult {
                outcome: InviteOutcome::Accepted,
                ..
            }
        )
    })
    .await
    else {
        panic!("no acceptance from presence");
    };
    assert_eq!((n, outcome), (nonce, InviteOutcome::Accepted));
    // Already in the mesh: no second invite.
    let err = a
        .invite("dev-b", &offer(coord, "mesh-p"))
        .await
        .unwrap_err();
    assert!(err.contains("already"), "{err}");
    a.stop().await;
    b.stop().await;
}

async fn recv(socket: &tokio::net::UdpSocket, within: Duration) -> Option<Vec<u8>> {
    let mut buf = [0u8; 4096];
    match tokio::time::timeout(within, socket.recv_from(&mut buf)).await {
        Ok(Ok((n, _))) => Some(buf[..n].to_vec()),
        _ => None,
    }
}

#[tokio::test]
async fn query_is_answered_at_once() {
    let probe = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let probe_port = probe.local_addr().unwrap().port();
    let pa = free_udp_port();
    let (a, _ea) = node(
        PeerConfig {
            interval: Duration::from_secs(60),
            ..cfg(pa, &[probe_port])
        },
        "dev-a",
        "MSI",
    )
    .await;
    // Drain the start-up query and first announcement.
    let mut startup = Vec::new();
    while let Some(p) = recv(&probe, Duration::from_millis(300)).await {
        startup.push(p);
    }
    assert!(
        startup.iter().any(|p| p == PEER_QUERY.as_bytes()),
        "asks on start"
    );
    assert!(
        startup.iter().any(|p| parse_peer(p).is_some()),
        "announces on start"
    );

    let asked = Instant::now();
    let sender = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    sender
        .send_to(PEER_QUERY.as_bytes(), ("127.0.0.1", pa))
        .await
        .unwrap();
    let mut answered = None;
    while let Some(p) = recv(&probe, Duration::from_secs(1)).await {
        if let Some((p, bye)) = parse_peer(&p) {
            answered = Some((p, bye, asked.elapsed()));
            break;
        }
    }
    let (p, bye, after) = answered.expect("no answer to the query");
    assert_eq!(p.device_id, "dev-a");
    assert!(!bye);
    assert!(
        after < Duration::from_millis(500),
        "answered after {after:?}"
    );
    a.stop().await;
}

#[tokio::test]
async fn silent_peers_expire_and_bye_is_immediate() {
    let pa = free_udp_port();
    let (a, mut ea) = node(
        PeerConfig {
            ttl: Duration::from_millis(400),
            ..cfg(pa, &[])
        },
        "dev-a",
        "A",
    )
    .await;
    let ghost = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    ghost
        .send_to(
            &encode_peer(&me("ghost", "Ghost"), false),
            ("127.0.0.1", pa),
        )
        .unwrap();
    assert!(
        wait_for(&mut ea, Duration::from_secs(1), |e| seen(e, "ghost"))
            .await
            .is_some()
    );
    let heard = Instant::now();
    assert!(wait_for(&mut ea, Duration::from_secs(2), |e| matches!(
        e,
        AppEvent::PeerGone { device_id } if device_id == "ghost"
    ))
    .await
    .is_some());
    let after = heard.elapsed();
    assert!(
        after >= Duration::from_millis(350) && after < Duration::from_millis(1200),
        "expired after {after:?}"
    );

    // Back, then a bye: gone without waiting for the TTL.
    ghost
        .send_to(
            &encode_peer(&me("ghost", "Ghost"), false),
            ("127.0.0.1", pa),
        )
        .unwrap();
    wait_for(&mut ea, Duration::from_secs(1), |e| seen(e, "ghost"))
        .await
        .unwrap();
    ghost
        .send_to(&encode_peer(&me("ghost", "Ghost"), true), ("127.0.0.1", pa))
        .unwrap();
    let bye = Instant::now();
    wait_for(&mut ea, Duration::from_secs(1), |e| {
        matches!(e, AppEvent::PeerGone { .. })
    })
    .await
    .unwrap();
    assert!(bye.elapsed() < Duration::from_millis(300));
    a.stop().await;
}

fn invite_body(coord: &str, mesh_id: &str, from: &str) -> MeshInvite {
    MeshInvite {
        from_device: from.into(),
        from_name: "MSI".into(),
        mesh_name: "MSI's mesh".into(),
        mesh_id: mesh_id.into(),
        coordinator: coord.into(),
        token: "t0k".into(),
        sent_at: 0.0,
        nonce: random_id(),
        reply_port: 0,
        from_address: Some("1.2.3.4".into()),
        nodes_active: Some(999),
    }
}

#[tokio::test]
async fn invite_endpoint_rules() {
    let good = health_server("mesh-r", gm_types::MESH_PROTOCOL, 2).await;
    let old = health_server("mesh-old", 4, 1).await;
    let (b, mut eb) = node(
        PeerConfig {
            rate_limit: Duration::from_millis(300),
            max_pending: 2,
            vet_timeout: Duration::from_millis(600),
            ..cfg(free_udp_port(), &[])
        },
        "dev-b",
        "B",
    )
    .await;
    let url = format!("http://{}/v1/invite", b.invite_addr().unwrap());
    let client = gm_net::lan_client();
    let post_raw = |body: Vec<u8>| {
        let (client, url) = (client.clone(), url.clone());
        async move {
            let r = client
                .post(&url)
                .header("content-type", "application/json")
                .body(body)
                .send()
                .await
                .unwrap();
            let code = r.status().as_u16();
            (code, r.text().await.unwrap_or_default())
        }
    };
    let post = |invite: MeshInvite| post_raw(serde_json::to_vec(&invite).unwrap());
    let pause = || tokio::time::sleep(Duration::from_millis(320));

    // Malformed: not JSON, missing fields, oversized, public coordinator.
    assert_eq!(post_raw(b"{not json".to_vec()).await.0, 400);
    assert_eq!(post_raw(br#"{"from_device":"x"}"#.to_vec()).await.0, 400);
    assert_eq!(post_raw(vec![b' '; 64 * 1024]).await.0, 413);
    let public = invite_body("http://8.8.8.8:8000", "mesh-r", "dev-a");
    let (code, text) = post(public).await;
    assert_eq!(code, 400, "{text}");

    // A good invite from loopback (a private address) is accepted, with the
    // sender's own claims about address and size replaced.
    let first = invite_body(&format!("http://{good}"), "mesh-r", "dev-a");
    let (code, text) = post(first.clone()).await;
    assert_eq!(code, 202, "{text}");
    let Some(AppEvent::InviteReceived { invite }) =
        wait_for(&mut eb, Duration::from_secs(1), |e| {
            matches!(e, AppEvent::InviteReceived { .. })
        })
        .await
    else {
        panic!("no InviteReceived");
    };
    assert_eq!(invite.from_address.as_deref(), Some("127.0.0.1"));
    assert_eq!(invite.nodes_active, Some(2));

    // Same nonce again: idempotent, no second event. Another at once: rate-limited.
    assert_eq!(post(first.clone()).await.0, 202);
    let (code, text) = post(invite_body(&format!("http://{good}"), "mesh-r", "dev-a")).await;
    assert_eq!(code, 429, "{text}");
    assert!(text.contains("try again"), "{text}");

    // Unreachable coordinator, wrong protocol, wrong mesh id: dropped (422).
    pause().await;
    let dead = invite_body("http://127.0.0.1:9", "mesh-r", "dev-a");
    assert_eq!(post(dead).await.0, 422);
    pause().await;
    assert_eq!(
        post(invite_body(&format!("http://{old}"), "mesh-old", "dev-a"))
            .await
            .0,
        422
    );
    pause().await;
    assert_eq!(
        post(invite_body(
            &format!("http://{good}"),
            "other-mesh",
            "dev-a"
        ))
        .await
        .0,
        422
    );

    // Pending cap (2).
    pause().await;
    assert_eq!(
        post(invite_body(&format!("http://{good}"), "mesh-r", "dev-c"))
            .await
            .0,
        202
    );
    pause().await;
    let (code, text) = post(invite_body(&format!("http://{good}"), "mesh-r", "dev-d")).await;
    assert_eq!(code, 429, "{text}");
    assert!(text.contains("too many"), "{text}");
    assert_eq!(b.incoming().len(), 2);

    // Blocked device.
    let declined = b
        .respond(&first.nonce, InviteOutcome::Declined, true)
        .unwrap();
    assert_eq!(declined.from_device, "dev-a");
    pause().await;
    let (code, text) = post(invite_body(&format!("http://{good}"), "mesh-r", "dev-a")).await;
    assert_eq!(code, 403, "{text}");
    b.set_blocked(Vec::<String>::new());

    // Hidden: refused.
    b.set_visible(false);
    pause().await;
    assert_eq!(
        post(invite_body(&format!("http://{good}"), "mesh-r", "dev-e"))
            .await
            .0,
        403
    );
    b.set_visible(true);

    // Reply endpoint: unknown nonce, malformed.
    let reply = format!("http://{}/v1/invite-reply", b.invite_addr().unwrap());
    let r = client
        .post(&reply)
        .json(&json!({"nonce": random_id(), "from_device": "dev-a", "outcome": "accepted"}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 404);
    let r = client.post(&reply).body("[]").send().await.unwrap();
    assert_eq!(r.status().as_u16(), 400);
    b.stop().await;
}

#[tokio::test]
async fn unanswered_invites_time_out_on_both_sides() {
    let coord = health_server("mesh-t", gm_types::MESH_PROTOCOL, 1).await;
    let (pa, pb) = (free_udp_port(), free_udp_port());
    let quick = |listen, to| PeerConfig {
        invite_ttl: Duration::from_millis(500),
        ..cfg(listen, &[to])
    };
    let (a, mut ea) = node(quick(pa, pb), "dev-a", "A").await;
    let (b, mut eb) = node(quick(pb, pa), "dev-b", "B").await;
    wait_for(&mut ea, Duration::from_secs(2), |e| seen(e, "dev-b"))
        .await
        .unwrap();
    let nonce = a.invite("dev-b", &offer(coord, "mesh-t")).await.unwrap();
    let timed_out = |incoming: bool| move |e: &AppEvent| matches!(e, AppEvent::InviteResult { outcome: InviteOutcome::TimedOut, incoming: i, .. } if *i == incoming);
    let Some(AppEvent::InviteResult { nonce: n, .. }) =
        wait_for(&mut eb, Duration::from_secs(2), timed_out(true)).await
    else {
        panic!("invitee kept a stale invite");
    };
    assert_eq!(n, nonce);
    assert!(b.incoming().is_empty());
    assert!(wait_for(&mut ea, Duration::from_secs(2), timed_out(false))
        .await
        .is_some());
    assert!(a.outgoing().is_empty());
    a.stop().await;
    b.stop().await;
}

/// Hidden apps say "bye", stop announcing, and refuse invites; they still
/// see others.
#[tokio::test]
async fn hidden_app_disappears_and_refuses() {
    let coord = health_server("mesh-h", gm_types::MESH_PROTOCOL, 1).await;
    let (pa, pb) = (free_udp_port(), free_udp_port());
    let (a, mut ea) = node(cfg(pa, &[pb]), "dev-a", "A").await;
    let (b, mut eb) = node(cfg(pb, &[pa]), "dev-b", "B").await;
    wait_for(&mut ea, Duration::from_secs(2), |e| seen(e, "dev-b"))
        .await
        .unwrap();
    wait_for(&mut eb, Duration::from_secs(2), |e| seen(e, "dev-a"))
        .await
        .unwrap();
    b.set_visible(false);
    assert!(wait_for(&mut ea, Duration::from_secs(1), |e| matches!(
        e,
        AppEvent::PeerGone { device_id } if device_id == "dev-b"
    ))
    .await
    .is_some());
    // No presence from B for a while, though A keeps announcing to it.
    assert!(
        wait_for(&mut ea, Duration::from_millis(800), |e| seen(e, "dev-b"))
            .await
            .is_none()
    );
    assert_eq!(b.peers().len(), 1, "a hidden app still sees others");
    b.set_visible(true);
    assert!(
        wait_for(&mut ea, Duration::from_secs(1), |e| seen(e, "dev-b"))
            .await
            .is_some()
    );
    let _ = coord;
    a.stop().await;
    b.stop().await;
}

/// The real path: multicast + broadcast on a free port, both nodes on the
/// same port (as two GradApps on one LAN). Skipped where there is no LAN
/// interface, and on macOS CI VMs (no multicast loopback; see net.rs).
#[tokio::test]
async fn presence_over_multicast() {
    if gm_net::lan_addresses().iter().all(|a| a.is_virtual) {
        eprintln!("skipped: no LAN interface for multicast");
        return;
    }
    if cfg!(target_os = "macos") && std::env::var_os("GITHUB_ACTIONS").is_some() {
        eprintln!("skipped: macOS CI VM has no multicast loopback");
        return;
    }
    let port = free_udp_port();
    let real = || PeerConfig {
        interval: Duration::from_secs(60),
        listen_port: port,
        send_port: port,
        invite_binds: vec![SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0))],
        ..PeerConfig::default()
    };
    let (a, mut ea) = node(real(), "dev-a", "A").await;
    // Let A's own start-up packets pass; B's query must make A answer.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let started = Instant::now();
    let (b, _eb) = node(real(), "dev-b", "B").await;
    assert!(
        wait_for(&mut ea, Duration::from_secs(2), |e| seen(e, "dev-b"))
            .await
            .is_some(),
        "A did not hear B over multicast/broadcast"
    );
    assert!(started.elapsed() < Duration::from_millis(800));
    let found = wait_until_peer(&b, "dev-a").await;
    assert!(found, "B did not hear A answer its query");
    a.stop().await;
    b.stop().await;
}

async fn wait_until_peer(node: &PeerNode, id: &str) -> bool {
    let until = Instant::now() + Duration::from_secs(1);
    while Instant::now() < until {
        if node.peers().iter().any(|p| p.device_id == id) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    false
}
