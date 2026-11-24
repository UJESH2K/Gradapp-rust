//! End-to-end tests against mock servers on 127.0.0.1. No real LAN needed:
//! discovery takes injected ports and target lists.

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::{Path, Query};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use gm_net::beacon::beacon_packet;
use gm_net::client::CoordinatorClient;
use gm_net::join::{shell_script, JoinConfig, JoinInfo, JoinSurface, AGENT_FILES};
use gm_net::{
    discover, download, probe_health, read_invite, Beacon, BeaconConfig, CancellationToken,
    DiscoverOptions, InviteKind, NetError,
};
use gm_types::{AppEvent, DiscoverySource};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const TOKEN: &str = "tok-123";

async fn spawn(router: Router) -> SocketAddr {
    spawn_on(Ipv4Addr::LOCALHOST, router).await
}

async fn spawn_on(ip: Ipv4Addr, router: Router) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind((ip, 0)).await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    addr
}

fn authed(headers: &HeaderMap, token: &Mutex<String>) -> bool {
    headers.get("x-mesh-token").and_then(|v| v.to_str().ok())
        == Some(token.lock().unwrap().as_str())
}

fn unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(json!({"detail": "Invalid or missing mesh token"})),
    )
        .into_response()
}

/// A stand-in for the FastAPI coordinator: the routes and auth gm-net uses.
async fn mock_coordinator(mesh_id: &'static str) -> SocketAddr {
    mock_coordinator_on(Ipv4Addr::LOCALHOST, mesh_id).await
}

async fn mock_coordinator_on(ip: Ipv4Addr, mesh_id: &'static str) -> SocketAddr {
    let token = Arc::new(Mutex::new(TOKEN.to_string()));
    let t = |token: &Arc<Mutex<String>>| token.clone();
    let router = Router::new()
        .route(
            "/health",
            get(move || async move {
                Json(json!({"status":"ok","version":"5.1.1","protocol":5,"mesh_id":mesh_id,"torch_ready":true,
                    "nodes_active":2,"mdns":{"name":"gradmesh.local"}}))
            }),
        )
        .route("/mesh", {
            let token = t(&token);
            get(move |h: HeaderMap| async move {
                if !authed(&h, &token) {
                    return unauthorized();
                }
                Json(json!({"mesh_id": mesh_id, "nodes": [], "metrics": {"nodes_active": 2}})).into_response()
            })
        })
        .route("/runs", {
            let (t1, t2) = (t(&token), t(&token));
            get(move |h: HeaderMap| async move {
                if !authed(&h, &t1) {
                    return unauthorized();
                }
                Json(json!({"runs":[{"id":"r1"},{"id":"r2"}]})).into_response()
            })
            .post(move |h: HeaderMap, Json(body): Json<Value>| async move {
                if !authed(&h, &t2) {
                    return unauthorized();
                }
                if body.get("model").is_none() {
                    return (StatusCode::UNPROCESSABLE_ENTITY,
                        Json(json!({"detail":[{"msg":"field required"},{"msg":"bad epochs"}]}))).into_response();
                }
                Json(json!({"id":"r3","echo":body})).into_response()
            })
        })
        .route("/runs/:id", {
            let token = t(&token);
            get(move |h: HeaderMap, Path(id): Path<String>| async move {
                if !authed(&h, &token) {
                    return unauthorized();
                }
                if id != "r1" {
                    return (StatusCode::NOT_FOUND, Json(json!({"detail":"Unknown run"}))).into_response();
                }
                Json(json!({"id": id, "status": "running"})).into_response()
            })
        })
        .route("/runs/:id/stop", {
            let token = t(&token);
            post(move |h: HeaderMap| async move {
                if !authed(&h, &token) {
                    return unauthorized();
                }
                Json(json!({"status":"stopped"})).into_response()
            })
        })
        .route("/datasets", {
            let token = t(&token);
            get(move |h: HeaderMap| async move {
                if !authed(&h, &token) {
                    return unauthorized();
                }
                Json(json!({"datasets":[], "default": null})).into_response()
            })
        })
        .route("/policy", {
            let (t1, t2) = (t(&token), t(&token));
            get(move |h: HeaderMap| async move {
                if !authed(&h, &t1) {
                    return unauthorized();
                }
                Json(json!({"policy":{"min_gflops":1.0},"defaults":{"min_gflops":1.0}})).into_response()
            })
            .put(move |h: HeaderMap, Json(body): Json<Value>| async move {
                if !authed(&h, &t2) {
                    return unauthorized();
                }
                Json(json!({"policy": body["values"]})).into_response()
            })
        })
        .route("/token/rotate", {
            let token = t(&token);
            post(move |h: HeaderMap| async move {
                if !authed(&h, &token) {
                    return unauthorized();
                }
                *token.lock().unwrap() = "tok-rotated".into();
                Json(json!({"token":"tok-rotated","removed_nodes":2})).into_response()
            })
        })
        .route("/events", {
            let token = t(&token);
            get(move |h: HeaderMap, Query(q): Query<std::collections::HashMap<String, String>>| async move {
                if !authed(&h, &token) {
                    return unauthorized();
                }
                let replay: usize = q.get("replay").and_then(|r| r.parse().ok()).unwrap_or(40);
                let mut body = String::new();
                for i in 0..replay.min(3) {
                    body += &format!("event: run.progress\ndata: {}\n\n", json!({"id":i+1,"kind":"run.progress","at":1.0,"data":{"i":i}}));
                }
                body += ": keepalive\n\n";
                body += &format!("event: stream.ready\ndata: {}\n\n", json!({"id":0,"kind":"stream.ready","at":1.0,"data":{}}));
                ([("content-type", "text/event-stream")], body).into_response()
            })
        });
    spawn_on(ip, router).await
}

/// A join port: `gradapp` serves /gradapp.json and /join.sh, `dashboard`
/// only /join.sh (GradMesh's Next.js dashboard), `none` nothing.
async fn mock_join(kind: &'static str) -> SocketAddr {
    let mut router = Router::new();
    if kind == "gradapp" {
        router = router.route(
            "/gradapp.json",
            get(|| async { Json(json!({"app":"gradapp","meshName":"Lab","token":"tok-json","coordinatorPort":8000})) }),
        );
    }
    if kind != "none" {
        router = router.route(
            "/join.sh",
            get(|| async { shell_script("http://h:3000", "http://h:8000", "tok-sh", AGENT_FILES) }),
        );
    }
    spawn(router).await
}

fn quiet_opts() -> DiscoverOptions {
    DiscoverOptions {
        beacon: false,
        mdns: false,
        sweep: false,
        listen_for: Duration::ZERO,
        ..DiscoverOptions::default()
    }
}

#[tokio::test]
async fn probe_reads_health_and_latency() {
    let coord = mock_coordinator("mesh-a").await;
    let client = gm_net::lan_client();
    let hit = probe_health(&client, "127.0.0.1", coord.port(), Duration::from_secs(1))
        .await
        .unwrap();
    assert_eq!(hit.health.mesh_id, "mesh-a");
    assert_eq!(hit.health.nodes_active, 2);
    assert!(hit.health.torch_ready);
    assert!(hit.latency_ms < 1000);
    // A join port is not a coordinator.
    let join = mock_join("gradapp").await;
    assert!(
        probe_health(&client, "127.0.0.1", join.port(), Duration::from_secs(1))
            .await
            .is_none()
    );
    // Closed port.
    let closed = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    assert!(
        probe_health(&client, "127.0.0.1", closed, Duration::from_millis(500))
            .await
            .is_none()
    );
}

#[tokio::test]
async fn read_invite_both_kinds() {
    let a = read_invite("127.0.0.1", mock_join("gradapp").await.port()).await;
    assert_eq!(
        (a.token.as_deref(), a.mesh_name.as_deref(), a.kind),
        (Some("tok-json"), Some("Lab"), Some(InviteKind::Gradapp))
    );
    let b = read_invite("127.0.0.1", mock_join("dashboard").await.port()).await;
    assert_eq!(
        (b.token.as_deref(), b.mesh_name, b.kind),
        (Some("tok-sh"), None, Some(InviteKind::Dashboard))
    );
    let c = read_invite("127.0.0.1", mock_join("none").await.port()).await;
    assert_eq!((c.token, c.kind), (None, None));
    assert!(c.web.starts_with("http://127.0.0.1:"));
}

#[tokio::test]
async fn discover_sweep_manual_dedupe_and_invite() {
    let coord = mock_coordinator("mesh-d").await;
    let join = mock_join("gradapp").await;
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = events.clone();
    let opts = DiscoverOptions {
        coordinator_port: coord.port(),
        web_port: join.port(),
        sweep: true,
        // Mock first and last; everything else refuses.
        sweep_targets: Some((1..=40).map(|d| format!("127.0.0.{d}")).collect()),
        manual: vec![format!("localhost:{}", coord.port())],
        ..quiet_opts()
    };
    let started = Instant::now();
    let found = discover(opts, move |e| sink.lock().unwrap().push(e)).await;
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "took {:?}",
        started.elapsed()
    );
    assert_eq!(found.len(), 1, "{found:?}");
    let m = &found[0];
    assert_eq!(m.mesh_id, "mesh-d");
    // Seen as localhost (manual) and 127.0.0.1 (sweep): the IP wins.
    assert_eq!(m.hostname, "127.0.0.1");
    assert_eq!(m.coordinator, format!("http://127.0.0.1:{}", coord.port()));
    assert_eq!(m.web, format!("http://127.0.0.1:{}", join.port()));
    assert!(m.local);
    assert_eq!(m.token.as_deref(), Some("tok-json"));
    assert_eq!(m.mesh_name.as_deref(), Some("Lab"));
    assert!(m.latency_ms.is_some());

    let events = events.lock().unwrap();
    assert!(matches!(
        events.first(),
        Some(AppEvent::ScanProgress {
            scanned: 0,
            total: 40
        })
    ));
    assert!(events.iter().any(|e| matches!(
        e,
        AppEvent::ScanProgress {
            scanned: 40,
            total: 40
        }
    )));
    assert!(events
        .iter()
        .any(|e| matches!(e, AppEvent::MeshFound { mesh } if mesh.token.is_some())));
}

#[tokio::test]
async fn sweep_full_slash24_is_fast() {
    let coord = mock_coordinator("mesh-s").await;
    let opts = DiscoverOptions {
        coordinator_port: coord.port(),
        web_port: 1, // nothing there: invite read fails fast
        sweep: true,
        sweep_targets: Some((1..255).map(|d| format!("127.0.0.{d}")).collect()),
        ..quiet_opts()
    };
    let started = Instant::now();
    let found = discover(opts, |_| {}).await;
    let took = started.elapsed();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].source, DiscoverySource::Sweep);
    assert!(
        took < Duration::from_secs(3),
        "254-host sweep took {took:?}"
    );
}

#[tokio::test]
async fn manual_gradapp_link_carries_token() {
    let coord = mock_coordinator("mesh-m").await;
    let link = gm_net::invite_link(
        &format!("http://127.0.0.1:{}", coord.port()),
        "from-link",
        Some("Linked"),
    );
    let opts = DiscoverOptions {
        manual: vec![link, "not a host!".into()],
        web_port: 1,
        ..quiet_opts()
    };
    let found = discover(opts, |_| {}).await;
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].source, DiscoverySource::Manual);
    assert_eq!(found[0].token.as_deref(), Some("from-link"));
    assert_eq!(found[0].mesh_name.as_deref(), Some("Linked"));
}

#[tokio::test]
async fn discover_is_cancellable() {
    let cancel = CancellationToken::new();
    let opts = DiscoverOptions {
        sweep: true,
        // Blackhole-ish targets with a long timeout and long listen window.
        sweep_targets: Some((1..255).map(|d| format!("10.255.255.{d}")).collect()),
        sweep_timeout: Duration::from_secs(30),
        listen_for: Duration::from_secs(30),
        beacon: true,
        beacon_port: free_udp_port(),
        mdns: true,
        // Names no real mesh uses: a coordinator on this LAN (gradmesh.local)
        // answered within the 300 ms and made `found` non-empty (QA review).
        mdns_hostname: "gm-qa-cancel-test.local".into(),
        mdns_service: "_gmqacancel._tcp.local.".into(),
        cancel: cancel.clone(),
        ..quiet_opts()
    };
    let c = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(300)).await;
        c.cancel();
    });
    let started = Instant::now();
    let found = discover(opts, |_| {}).await;
    assert!(found.is_empty());
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "cancel took {:?}",
        started.elapsed()
    );
}

fn free_udp_port() -> u16 {
    std::net::UdpSocket::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// Beacon → listener round trip. Multicast is not reliably routable inside CI
/// containers, so the sender also unicasts to 127.0.0.1 (`extra_targets`);
/// the listener path (bind, parse, confirm via /health, record) is the same.
#[tokio::test]
async fn beacon_round_trip_on_loopback() {
    let coord = mock_coordinator("mesh-b").await;
    let join = mock_join("gradapp").await;
    let port = free_udp_port();
    let packet = beacon_packet(
        "mesh-b",
        Some("Beaconed"),
        "5.1.1",
        2,
        coord.port(),
        join.port(),
    );
    let beacon = Beacon::start_with(
        packet,
        BeaconConfig {
            interval: Duration::from_millis(200),
            port,
            multicast: true,
            broadcast: false,
            extra_targets: vec![SocketAddr::from((Ipv4Addr::LOCALHOST, port))],
            answer_queries: false,
            ..BeaconConfig::default()
        },
    );
    let first = Arc::new(Mutex::new(None::<Duration>));
    let sink = first.clone();
    let started = Instant::now();
    let opts = DiscoverOptions {
        beacon: true,
        beacon_port: port,
        listen_for: Duration::from_millis(1200),
        ..quiet_opts()
    };
    let found = discover(opts, move |e| {
        if matches!(e, AppEvent::MeshFound { .. }) {
            sink.lock().unwrap().get_or_insert(started.elapsed());
        }
    })
    .await;
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].source, DiscoverySource::Beacon);
    assert_eq!(found[0].hostname, "127.0.0.1");
    assert_eq!(found[0].web, format!("http://127.0.0.1:{}", join.port()));
    assert_eq!(found[0].token.as_deref(), Some("tok-json"));
    let first = first.lock().unwrap().unwrap();
    assert!(
        first < Duration::from_secs(1),
        "first beacon hit after {first:?}"
    );

    // Ignoring our own mesh id.
    let opts = DiscoverOptions {
        beacon: true,
        beacon_port: port,
        listen_for: Duration::from_millis(600),
        ignore_mesh_ids: vec!["mesh-b".into()],
        ..quiet_opts()
    };
    assert!(discover(opts, |_| {}).await.is_empty());
    beacon.stop().await;
}

#[tokio::test]
async fn client_calls() {
    let coord = mock_coordinator("mesh-c").await;
    let base = format!("http://127.0.0.1:{}", coord.port());
    let c = CoordinatorClient::new(&base, Some(TOKEN.into()));
    assert_eq!(c.health().await.unwrap().mesh_id, "mesh-c");
    assert_eq!(c.mesh().await.unwrap()["mesh_id"], "mesh-c");
    assert_eq!(c.runs().await.unwrap().len(), 2);
    assert_eq!(c.run("r1").await.unwrap()["status"], "running");
    match c.run("nope").await {
        Err(NetError::Http {
            status: 404,
            detail,
        }) => assert_eq!(detail, "Unknown run"),
        other => panic!("{other:?}"),
    }
    assert_eq!(
        c.create_run(&json!({"model":"m"})).await.unwrap()["id"],
        "r3"
    );
    match c.create_run(&json!({})).await {
        Err(NetError::Http {
            status: 422,
            detail,
        }) => assert_eq!(detail, "field required; bad epochs"),
        other => panic!("{other:?}"),
    }
    assert_eq!(c.stop_run("r1").await.unwrap()["status"], "stopped");
    assert!(c.datasets().await.unwrap()["datasets"].is_array());
    assert_eq!(c.policy().await.unwrap()["policy"]["min_gflops"], 1.0);
    assert_eq!(
        c.put_policy(&json!({"min_gflops": 2.0})).await.unwrap()["policy"]["min_gflops"],
        2.0
    );
    assert!(matches!(c.run("../etc").await, Err(NetError::Blocked(_))));
    assert!(matches!(
        c.call(reqwest::Method::GET, "/register_node", None).await,
        Err(NetError::Blocked(_))
    ));

    // Events: SSE with replay, keepalive comment skipped.
    use futures::StreamExt;
    let events: Vec<_> = c.events(2).await.unwrap().collect().await;
    let kinds: Vec<_> = events
        .iter()
        .map(|e| e.as_ref().unwrap().kind().unwrap().to_string())
        .collect();
    assert_eq!(kinds, ["run.progress", "run.progress", "stream.ready"]);

    // follow_events reconnects (mock closes each time) until cancelled.
    let cancel = CancellationToken::new();
    let seen = Arc::new(Mutex::new(0usize));
    let (s2, c2) = (seen.clone(), cancel.clone());
    tokio::time::timeout(
        Duration::from_secs(5),
        c.follow_events(0, cancel.clone(), move |e| {
            if e.kind() == Some("stream.ready") {
                *s2.lock().unwrap() += 1;
                if *s2.lock().unwrap() >= 2 {
                    c2.cancel();
                }
            }
        }),
    )
    .await
    .expect("follow_events stops on cancel");
    assert_eq!(*seen.lock().unwrap(), 2);

    // Rotation stores the new token; the old one is now refused.
    let clone = c.clone();
    assert_eq!(c.rotate_token().await.unwrap(), "tok-rotated");
    assert_eq!(clone.token().as_deref(), Some("tok-rotated"));
    assert!(clone.mesh().await.is_ok());
    let stale = CoordinatorClient::new(&base, Some(TOKEN.into()));
    match stale.mesh().await {
        Err(e) => assert_eq!(
            (e.status(), e.to_string()),
            (Some(401), "Invalid or missing mesh token".into())
        ),
        Ok(_) => panic!("stale token accepted"),
    }
    // Nothing listening.
    let dead = CoordinatorClient::new("http://127.0.0.1:1", None);
    assert!(matches!(dead.health().await, Err(NetError::Unreachable(_))));
}

/// Raw HTTP/1.1 so paths reach the server exactly as written (an HTTP client
/// would normalise `..` away before sending).
async fn raw_get(addr: SocketAddr, method: &str, path: &str, host: &str) -> (u16, String, String) {
    let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
    s.write_all(
        format!("{method} {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n").as_bytes(),
    )
    .await
    .unwrap();
    let mut out = Vec::new();
    s.read_to_end(&mut out).await.unwrap();
    let text = String::from_utf8_lossy(&out).to_string();
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
    let status = head.split(' ').nth(1).unwrap().parse().unwrap();
    (status, head.to_ascii_lowercase(), body.to_string())
}

#[tokio::test]
async fn join_surface_routes_and_traversal() {
    let dir = tempfile::tempdir().unwrap();
    let engine = dir.path().join("engine");
    std::fs::create_dir_all(&engine).unwrap();
    std::fs::write(engine.join("worker.py"), "print('worker')\n").unwrap();
    std::fs::write(engine.join("requirements-common.txt"), "numpy\n").unwrap();
    std::fs::write(engine.join("secret.txt"), "SECRET").unwrap();
    std::fs::write(dir.path().join("outside.txt"), "SECRET").unwrap();

    let coord = mock_coordinator("mesh-j").await;
    let token = Arc::new(Mutex::new(Some("tok-j".to_string())));
    let t = token.clone();
    let mut cfg = JoinConfig::new(&engine, move || JoinInfo {
        token: t.lock().unwrap().clone(),
        coordinator_port: 8000,
        mesh_name: Some("Lab".into()),
        mesh_id: Some("mesh-j".into()),
        version: "2.0.0".into(),
        lan_address: "192.168.1.20".into(),
    });
    cfg.health_upstream = Some(format!("http://127.0.0.1:{}", coord.port()));
    cfg.public_port = Some(3000);
    let surface = JoinSurface::serve((Ipv4Addr::LOCALHOST, 0).into(), cfg)
        .await
        .unwrap();
    let addr = surface.local_addr();

    // /gradapp.json: v1 key order; Host header decides the advertised name.
    let (st, head, body) = raw_get(addr, "GET", "/gradapp.json", "10.0.0.7:3000").await;
    assert_eq!(st, 200);
    assert!(head.contains("content-type: application/json"));
    assert!(
        head.contains("cache-control: no-store")
            && head.contains("x-content-type-options: nosniff")
    );
    assert_eq!(
        body,
        r#"{"app":"gradapp","meshName":"Lab","meshId":"mesh-j","version":"2.0.0","coordinator":"http://10.0.0.7:8000","web":"http://10.0.0.7:3000","token":"tok-j","coordinatorPort":8000}"#
    );
    // localhost Host → LAN address.
    let (_, _, body) = raw_get(addr, "GET", "/join.sh", "localhost:3000").await;
    assert_eq!(
        body,
        shell_script(
            "http://192.168.1.20:3000",
            "http://192.168.1.20:8000",
            "tok-j",
            AGENT_FILES
        )
    );
    let (st, _, body) = raw_get(addr, "GET", "/join.ps1/", "10.0.0.7").await;
    assert_eq!(st, 200);
    assert!(
        body.contains("$MeshToken = 'tok-j'") && body.contains("$MeshWeb = 'http://10.0.0.7:3000'")
    );
    let (st, _, body) = raw_get(addr, "GET", "/", "10.0.0.7").await;
    assert_eq!(st, 200);
    assert!(body.contains("irm http://10.0.0.7:3000/join.ps1 | iex"));
    assert_eq!(
        raw_get(addr, "GET", "/api/health", "x").await.2,
        r#"{"status":"ok","app":"gradapp"}"#
    );

    // Agent files: allowlisted and present → 200 with v1 content types.
    let (st, head, body) = raw_get(addr, "GET", "/api/agent/worker.py", "x").await;
    assert_eq!((st, body.as_str()), (200, "print('worker')\n"));
    assert!(head.contains("content-type: text/x-python; charset=utf-8"));
    let (st, head, _) = raw_get(addr, "GET", "/api/agent/requirements-common.txt", "x").await;
    assert_eq!(st, 200);
    assert!(head.contains("content-type: text/plain; charset=utf-8"));
    // Allowlisted but missing → 404 "Missing".
    assert_eq!(
        raw_get(addr, "GET", "/api/agent/hardware.py", "x").await.2,
        "Missing\n"
    );

    for path in [
        "/api/agent/../../etc/passwd",
        "/api/agent/..%2f..%2fetc%2fpasswd",
        "/api/agent/%2e%2e%2f%2e%2e%2fetc%2fpasswd",
        "/api/agent/%2e%2e/outside.txt",
        "/api/agent/..%2Foutside.txt",
        "/api/agent/..%5coutside.txt",
        "/api/agent/%2Fetc%2Fpasswd",
        "/api/agent/secret.txt",
        "/api/agent/./worker.py",
        "/api/agent/worker.py%00",
        "/api/agent/%ff%fe",
        "/api/agent/",
        "/api/agent/x/../worker.py",
        "/etc/passwd",
    ] {
        let (st, _, body) = raw_get(addr, "GET", path, "x").await;
        assert_eq!(st, 404, "{path}");
        assert!(
            !body.contains("SECRET") && !body.contains("root:"),
            "{path}"
        );
    }

    let (st, _, _) = raw_get(addr, "POST", "/join.sh", "x").await;
    assert_eq!(st, 405);
    let (st, _, body) = raw_get(addr, "HEAD", "/join.sh", "x").await;
    assert_eq!((st, body.as_str()), (200, ""));

    // /health passthrough to the coordinator.
    let (st, _, body) = raw_get(addr, "GET", "/health", "x").await;
    assert_eq!(st, 200);
    assert!(body.contains("mesh-j"));

    // No token yet → 503 everywhere except /health; picked up per request.
    *token.lock().unwrap() = None;
    assert_eq!(raw_get(addr, "GET", "/gradapp.json", "x").await.0, 503);
    assert_eq!(
        raw_get(addr, "GET", "/api/agent/worker.py", "x").await.0,
        503
    );
    *token.lock().unwrap() = Some("tok-new".into());
    assert!(raw_get(addr, "GET", "/gradapp.json", "x")
        .await
        .2
        .contains("tok-new"));

    // read_invite against the real surface (both routes).
    let inv = read_invite("127.0.0.1", addr.port()).await;
    assert_eq!(
        (inv.token.as_deref(), inv.kind),
        (Some("tok-new"), Some(InviteKind::Gradapp))
    );

    surface.shutdown().await.unwrap();
    assert!(
        tokio::net::TcpStream::connect(addr).await.is_err(),
        "port still open after shutdown"
    );
}

#[tokio::test]
async fn download_streams_with_progress() {
    let payload: Arc<Vec<u8>> = Arc::new((0..1_500_000u32).map(|i| (i % 251) as u8).collect());
    let p = payload.clone();
    let server = spawn(
        Router::new()
            .route("/file.bin", get(move || async move { p.as_ref().clone() }))
            .route(
                "/redirect",
                get(|| async { axum::response::Redirect::temporary("/file.bin") }),
            ),
    )
    .await;
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("nested/dir/file.bin");
    let mut calls = Vec::new();
    let n = download(
        &format!("http://127.0.0.1:{}/redirect", server.port()),
        &target,
        |got, total| calls.push((got, total)),
    )
    .await
    .unwrap();
    assert_eq!(n, payload.len() as u64);
    assert_eq!(std::fs::read(&target).unwrap(), *payload);
    assert!(!dir.path().join("nested/dir/file.bin.part").exists());
    assert!(
        calls.windows(2).all(|w| w[0].0 < w[1].0),
        "progress is monotonic"
    );
    assert_eq!(
        calls.last().unwrap(),
        &(payload.len() as u64, Some(payload.len() as u64))
    );

    let missing = dir.path().join("missing.bin");
    let err = download(
        &format!("http://127.0.0.1:{}/nope", server.port()),
        &missing,
        |_, _| {},
    )
    .await
    .unwrap_err();
    assert_eq!(err.status(), Some(404));
    assert!(!missing.exists());
}

/// The real path: no unicast, the listener's query makes a host with a 60 s
/// interval announce at once over multicast/broadcast. Needs an interface
/// that loops multicast back; skipped (with a note) on a machine with no LAN
/// interface at all.
#[tokio::test]
async fn beacon_query_over_multicast() {
    if gm_net::lan_addresses().iter().all(|a| a.is_virtual) {
        eprintln!("skipped: no LAN interface for multicast");
        return;
    }
    // The beacon arrives from the LAN address, so the coordinator must listen there.
    let coord = mock_coordinator_on(Ipv4Addr::UNSPECIFIED, "mesh-q").await;
    let port = free_udp_port();
    let packet = beacon_packet("mesh-q", None, "5.1.1", 0, coord.port(), 1);
    let _beacon = Beacon::start_with(
        packet,
        BeaconConfig {
            interval: Duration::from_secs(60),
            port,
            ..BeaconConfig::default()
        },
    );
    // Let the first (immediate) tick pass so only the query can find it.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let started = Instant::now();
    let opts = DiscoverOptions {
        beacon: true,
        beacon_port: port,
        listen_for: Duration::from_millis(1000),
        ..quiet_opts()
    };
    let first = Arc::new(Mutex::new(None::<Duration>));
    let sink = first.clone();
    let found = discover(opts, move |_| {
        sink.lock().unwrap().get_or_insert(started.elapsed());
    })
    .await;
    assert_eq!(
        found.len(),
        1,
        "no beacon over multicast/broadcast after {:?}",
        started.elapsed()
    );
    assert_eq!(found[0].source, DiscoverySource::Beacon);
    let first = first.lock().unwrap().unwrap();
    assert!(
        first < Duration::from_millis(500),
        "query answered after {first:?}"
    );
}

/// Against a real Python coordinator (desktop/engine `uvicorn coordinator.app:app`):
/// `GM_NET_LIVE=http://127.0.0.1:8999 GM_NET_LIVE_TOKEN=... cargo test -p gm-net -- --ignored live`
/// Rotates the token of that coordinator.
#[tokio::test]
#[ignore]
async fn live_coordinator() {
    let base = std::env::var("GM_NET_LIVE").expect("GM_NET_LIVE");
    let token = std::env::var("GM_NET_LIVE_TOKEN").expect("GM_NET_LIVE_TOKEN");
    let port = url::Url::parse(&base).unwrap().port().unwrap();
    let c = CoordinatorClient::new(&base, Some(token));
    let health = c.health().await.unwrap();
    assert_eq!(health.protocol, gm_types::MESH_PROTOCOL);
    assert!(c.mesh().await.unwrap()["metrics"].is_object());
    c.runs().await.unwrap();
    assert!(c.datasets().await.is_ok());
    assert!(c.policy().await.unwrap()["policy"].is_object());
    assert!(c.put_policy(&json!({})).await.is_ok());
    match c.put_policy(&json!({"no_such_key": 1.0})).await {
        Err(NetError::Http {
            status: 400,
            detail,
        }) => assert!(detail.contains("no_such_key")),
        other => panic!("{other:?}"),
    }
    // SSE: replay then stream.ready.
    use futures::StreamExt;
    let mut events = Box::pin(c.events(5).await.unwrap());
    let ready = tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(e) = events.next().await {
            if e.unwrap().kind() == Some("stream.ready") {
                return true;
            }
        }
        false
    })
    .await
    .unwrap();
    assert!(ready);
    let new = c.rotate_token().await.unwrap();
    assert_eq!(c.token().as_deref(), Some(new.as_str()));
    assert!(c.mesh().await.is_ok());

    // mDNS: the coordinator registers gradmesh.local and _gradmesh._tcp.
    let started = Instant::now();
    let opts = DiscoverOptions {
        mdns: true,
        coordinator_port: port,
        listen_for: Duration::from_millis(2000),
        ..quiet_opts()
    };
    let first = Arc::new(Mutex::new(None::<Duration>));
    let sink = first.clone();
    let found = discover(opts, move |_| {
        sink.lock().unwrap().get_or_insert(started.elapsed());
    })
    .await;
    // Other meshes may share the LAN (a lab often runs several): look for ours.
    let ours = found
        .iter()
        .find(|m| m.mesh_id == health.mesh_id)
        .unwrap_or_else(|| panic!("{} not found via mDNS: {found:?}", health.mesh_id));
    assert_eq!(ours.source, DiscoverySource::Mdns);
    let ids: std::collections::HashSet<_> = found.iter().map(|m| &m.mesh_id).collect();
    assert_eq!(ids.len(), found.len(), "a mesh listed twice: {found:?}");
    eprintln!("mDNS hit after {:?}", first.lock().unwrap());

    // Everything at once, defaults (real /24 sweep of this machine's LAN).
    let started = Instant::now();
    let found = discover(
        DiscoverOptions {
            coordinator_port: port,
            ..DiscoverOptions::default()
        },
        |e| {
            if let AppEvent::MeshFound { mesh } = e {
                eprintln!(
                    "{:?}: {:?} via {:?}",
                    started.elapsed(),
                    mesh.hostname,
                    mesh.source
                );
            }
        },
    )
    .await;
    eprintln!(
        "full discovery: {} mesh(es) in {:?}",
        found.len(),
        started.elapsed()
    );
    assert!(found.iter().any(|m| m.mesh_id == health.mesh_id));
}
