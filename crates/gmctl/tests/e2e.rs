//! Cross-crate end-to-end tests (QA review, docs/devlog/qa-review.md).
//!
//! Each test drives two or more crates together the way the app does:
//! gm-net discovery against a real gm-net join surface and beacon; gm-probe's
//! choice resolved through gm-ports' registry; gm-ports building an env that
//! gm-supervisor then runs; and the `gmctl` binary itself.
//!
//! `cargo test -p gmctl --test e2e -- --ignored` also runs the one that
//! downloads a CPython with real uv (needs PyPI + GitHub).

use std::net::{Ipv4Addr, SocketAddr};
use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::routing::get;
use axum::{Json, Router};
use gm_net::beacon::beacon_packet;
use gm_net::join::{JoinConfig, JoinInfo, JoinSurface};
use gm_net::{discover, Beacon, BeaconConfig, DiscoverOptions};
use gm_ports::Registry;
use gm_probe::{host_profile_with, select_profile_with, Prefer, ProbeOptions, Profile};
use gm_supervisor::{ProcSpec, Supervisor};
use gm_types::{
    AppEvent, Arch, Backend, DeviceFacts, DiscoverySource, Gpu, Os, ProcRole, ProcState, Vendor,
};
use serde_json::{json, Value};

// ---------------------------------------------------------------------------
// Discovery: beacon + join surface + coordinator /health
// ---------------------------------------------------------------------------

const MESH: &str = "mesh-e2e";
const TOKEN: &str = "tok-e2e-7f3a";

/// The coordinator's unauthenticated `/health`, as the FastAPI app answers it.
async fn mock_coordinator() -> SocketAddr {
    let router = Router::new().route(
        "/health",
        get(|| async {
            Json(json!({"status": "ok", "version": "5.1.1", "protocol": 5,
                        "mesh_id": MESH, "torch_ready": true, "nodes_active": 3}))
        }),
    );
    let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    addr
}

/// The real join surface a hosting GradApp serves on :3000 (here: any port).
async fn join_surface(coordinator_port: u16) -> JoinSurface {
    let engine = tempfile::tempdir().unwrap().keep();
    let cfg = JoinConfig::new(engine, move || JoinInfo {
        token: Some(TOKEN.into()),
        coordinator_port,
        mesh_name: Some("E2E Lab".into()),
        mesh_id: Some(MESH.into()),
        version: gm_types::APP_VERSION.into(),
        lan_address: "127.0.0.1".into(),
    });
    JoinSurface::serve((Ipv4Addr::LOCALHOST, 0).into(), cfg)
        .await
        .unwrap()
}

fn free_udp_port() -> u16 {
    std::net::UdpSocket::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// Unicast-to-loopback beacon: multicast routing differs between CI hosts,
/// the listener → confirm → invite path is identical.
fn loopback_beacon(coord: u16, web: u16, udp: u16) -> Beacon {
    Beacon::start_with(
        beacon_packet(MESH, Some("E2E Lab"), "5.1.1", 3, coord, web),
        BeaconConfig {
            interval: Duration::from_millis(250),
            port: udp,
            multicast: false,
            broadcast: false,
            extra_targets: vec![SocketAddr::from((Ipv4Addr::LOCALHOST, udp))],
            answer_queries: false,
            ..BeaconConfig::default()
        },
    )
}

fn hermetic(udp: u16) -> DiscoverOptions {
    DiscoverOptions {
        beacon: true,
        beacon_port: udp,
        beacon_query: false,
        mdns: false,
        sweep: false,
        listen_for: Duration::from_millis(1500),
        ..DiscoverOptions::default()
    }
}

#[tokio::test]
async fn beacon_discovery_finds_the_mesh_with_its_token_fast() {
    let coord = mock_coordinator().await;
    let join = join_surface(coord.port()).await;
    let udp = free_udp_port();
    let _beacon = loopback_beacon(coord.port(), join.local_addr().port(), udp);

    let with_token = Arc::new(Mutex::new(None::<Duration>));
    let sink = with_token.clone();
    let started = Instant::now();
    let found = discover(hermetic(udp), move |e| {
        if let AppEvent::MeshFound { mesh } = e {
            if mesh.token.as_deref() == Some(TOKEN) {
                sink.lock().unwrap().get_or_insert(started.elapsed());
            }
        }
    })
    .await;

    assert_eq!(found.len(), 1, "one mesh, listed once: {found:?}");
    let m = &found[0];
    assert_eq!(m.mesh_id, MESH);
    assert_eq!(m.source, DiscoverySource::Beacon);
    assert_eq!(m.coordinator, format!("http://127.0.0.1:{}", coord.port()));
    assert_eq!(
        m.web,
        format!("http://127.0.0.1:{}", join.local_addr().port())
    );
    assert_eq!(
        m.token.as_deref(),
        Some(TOKEN),
        "token read from the join surface"
    );
    assert_eq!(m.mesh_name.as_deref(), Some("E2E Lab"));
    assert_eq!((m.nodes_active, m.protocol), (3, gm_types::MESH_PROTOCOL));
    assert!(m.local, "127.0.0.1 is this machine");
    let t = with_token
        .lock()
        .unwrap()
        .expect("no MeshFound carried the token");
    assert!(t < Duration::from_millis(1500), "token arrived after {t:?}");
}

/// The same mesh seen by beacon, a typed address and the sweep is one entry.
#[tokio::test]
async fn one_mesh_seen_three_ways_is_listed_once() {
    let coord = mock_coordinator().await;
    let join = join_surface(coord.port()).await;
    let udp = free_udp_port();
    let _beacon = loopback_beacon(coord.port(), join.local_addr().port(), udp);
    let opts = DiscoverOptions {
        coordinator_port: coord.port(),
        web_port: join.local_addr().port(),
        sweep: true,
        sweep_targets: Some(vec!["127.0.0.1".into(), "127.0.0.2".into()]),
        manual: vec![format!("127.0.0.1:{}", coord.port()), "localhost".into()],
        ..hermetic(udp)
    };
    let mut events = 0;
    let found = discover(opts, |e| {
        if matches!(e, AppEvent::MeshFound { .. }) {
            events += 1;
        }
    })
    .await;
    let ids: Vec<_> = found.iter().map(|m| (&m.mesh_id, &m.hostname)).collect();
    assert_eq!(found.len(), 1, "listed more than once: {ids:?}");
    // localhost (a name) never replaces 127.0.0.1 (an IP).
    assert_eq!(found[0].hostname, "127.0.0.1");
    assert_eq!(found[0].token.as_deref(), Some(TOKEN));
    assert!(events >= 1);
}

// ---------------------------------------------------------------------------
// gm-probe → gm-ports: every selected port exists and runs on the machine
// ---------------------------------------------------------------------------

const PARITY: &str = include_str!("../../gm-probe/tests/fixtures/parity_cases.json");

fn facts(v: &Value) -> DeviceFacts {
    let vendor = |s: &str| match s {
        "nvidia" => Vendor::Nvidia,
        "intel" => Vendor::Intel,
        "amd" => Vendor::Amd,
        "apple" => Vendor::Apple,
        _ => Vendor::Other,
    };
    DeviceFacts {
        os: match v["os"].as_str().unwrap() {
            "windows" => Os::Windows,
            "macos" => Os::Macos,
            "linux" => Os::Linux,
            _ => Os::Other,
        },
        os_version: v["os_version"].as_str().unwrap().into(),
        arch: match v["arch"].as_str().unwrap() {
            "x86_64" => Arch::X86_64,
            "arm64" => Arch::Arm64,
            _ => Arch::Other,
        },
        translated: v["translated"].as_bool().unwrap(),
        hostname: "e2e".into(),
        cpu_model: None,
        cpu_cores: v["cpu_count"].as_u64().map(|c| c as u32),
        ram_mb: v["ram_mb"].as_u64(),
        gpus: v["gpus"]
            .as_array()
            .unwrap()
            .iter()
            .map(|g| Gpu {
                vendor: vendor(g["vendor"].as_str().unwrap()),
                name: g["name"].as_str().unwrap().into(),
                index: g["index"].as_u64().unwrap() as u32,
                compute_capability: g["compute_capability"].as_f64().map(|c| c as f32),
                driver: g["driver"].as_str().map(Into::into),
                memory_mb: g["memory_mb"].as_u64(),
                cores: g["cores"].as_u64().map(|c| c as u32),
                integrated: false,
            })
            .collect(),
        os_name_pretty: None,
    }
}

/// Machines the parity fixtures (hardware.py's own cases) do not cover:
/// arm64 Linux with NVIDIA on several drivers, Windows on ARM, 32-bit ARM,
/// ROCm opt-in, an Intel Mac asking for CPU.
fn extra_machines() -> Vec<(String, DeviceFacts)> {
    let nv = |name: &str, cc: f32, driver: &str| Gpu {
        vendor: Vendor::Nvidia,
        name: name.into(),
        index: 0,
        compute_capability: Some(cc),
        driver: Some(driver.into()),
        memory_mb: Some(96_000),
        cores: None,
        integrated: false,
    };
    let base = DeviceFacts {
        os: Os::Linux,
        os_version: "6.8.0".into(),
        arch: Arch::Arm64,
        translated: false,
        hostname: "e2e".into(),
        cpu_model: None,
        cpu_cores: Some(72),
        ram_mb: Some(480_000),
        gpus: vec![],
        os_name_pretty: None,
    };
    let mut out = vec![];
    for (name, cc, drv) in [
        ("NVIDIA GH200 480GB", 9.0, "580.95.05"),
        ("NVIDIA GH200 480GB", 9.0, "570.86.15"),
        ("NVIDIA GH200 480GB", 9.0, "535.104.05"),
        ("NVIDIA GB10", 12.1, "580.95.05"),
        ("NVIDIA GB10", 12.1, "575.51.03"),
        ("NVIDIA Jetson AGX Xavier", 7.2, "540.4.0"),
    ] {
        out.push((
            format!("linux-arm64 {name} {drv}"),
            DeviceFacts {
                gpus: vec![nv(name, cc, drv)],
                ..base.clone()
            },
        ));
    }
    out.push((
        "windows-arm64 Snapdragon".into(),
        DeviceFacts {
            os: Os::Windows,
            gpus: vec![Gpu {
                vendor: Vendor::Other,
                name: "Qualcomm Adreno X1-85".into(),
                index: 0,
                compute_capability: None,
                driver: None,
                memory_mb: None,
                cores: None,
                integrated: true,
            }],
            ..base.clone()
        },
    ));
    out.push((
        "linux-armv7 Raspberry Pi".into(),
        DeviceFacts {
            arch: Arch::Other,
            ..base.clone()
        },
    ));
    out.push((
        "linux-x86_64 Radeon ROCm".into(),
        DeviceFacts {
            arch: Arch::X86_64,
            gpus: vec![Gpu {
                vendor: Vendor::Amd,
                name: "AMD Radeon RX 7900 XTX".into(),
                index: 0,
                compute_capability: None,
                driver: Some("rocm 7.2.0".into()),
                memory_mb: None,
                cores: None,
                integrated: false,
            }],
            ..base.clone()
        },
    ));
    out.push((
        "macos-x86_64 Intel Mac".into(),
        DeviceFacts {
            os: Os::Macos,
            arch: Arch::X86_64,
            os_version: "13.6".into(),
            ..base
        },
    ));
    out
}

/// A usable profile names a manifest that exists, is the same backend and
/// torch, and lists this machine's platform. A blocked one says why.
fn check(
    registry: &Registry,
    case: &str,
    which: &str,
    f: &DeviceFacts,
    p: &Profile,
) -> Option<String> {
    if let Some(blocked) = &p.blocked {
        return (blocked.trim().is_empty())
            .then(|| format!("{case} [{which}]: blocked without a reason"));
    }
    let Some(spec) = registry.get(&p.port) else {
        return Some(format!("{case} [{which}]: port {} has no manifest", p.port));
    };
    let Some(platform) = gm_probe::platform(f) else {
        return Some(format!(
            "{case} [{which}]: {} chosen for a machine no port supports",
            p.port
        ));
    };
    if registry.require_on(&p.port, platform).is_err() {
        return Some(format!(
            "{case} [{which}]: {} has no build for {platform} ({:?})",
            p.port, spec.platforms
        ));
    }
    if spec.backend != p.backend && !(spec.backend == Backend::Mps && p.backend == Backend::Cpu) {
        return Some(format!(
            "{case} [{which}]: profile backend {:?} vs manifest {:?}",
            p.backend, spec.backend
        ));
    }
    (spec.torch != p.torch).then(|| {
        format!(
            "{case} [{which}]: profile torch {} vs manifest {}",
            p.torch, spec.torch
        )
    })
}

#[test]
fn every_probed_machine_resolves_to_an_installable_port_or_is_blocked() {
    let registry = Registry::builtin();
    registry.validate().unwrap();
    let data: Value = serde_json::from_str(PARITY).unwrap();
    let mut machines: Vec<(String, DeviceFacts, Prefer)> = data["cases"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| {
            (
                c["name"].as_str().unwrap().to_string(),
                facts(&c["facts"]),
                c["prefer"].as_str().unwrap().parse().unwrap(),
            )
        })
        .collect();
    for (name, f) in extra_machines() {
        for prefer in Prefer::ALL {
            machines.push((format!("{name}/{prefer}"), f.clone(), prefer));
        }
    }
    let mut problems = vec![];
    let mut checked = 0;
    for (name, f, prefer) in &machines {
        for options in [
            ProbeOptions {
                experimental_rocm: false,
            },
            ProbeOptions {
                experimental_rocm: true,
            },
        ] {
            problems.extend(check(
                &registry,
                name,
                "select",
                f,
                &select_profile_with(f, *prefer, &options),
            ));
            problems.extend(check(
                &registry,
                name,
                "host",
                f,
                &host_profile_with(f, *prefer, &options),
            ));
            checked += 2;
        }
    }
    assert!(checked > 400, "only {checked} profiles checked");
    assert!(
        problems.is_empty(),
        "{} problems:\n{}",
        problems.len(),
        problems.join("\n")
    );
}

/// gm-probe keeps a copy of each port's platforms (it cannot depend on
/// gm-ports); it must equal the manifests.
#[test]
fn probe_platform_table_matches_the_manifests() {
    let registry = Registry::builtin();
    for spec in registry.all() {
        let (_, platforms) = gm_probe::PORT_PLATFORMS
            .iter()
            .find(|(id, _)| *id == spec.id)
            .unwrap_or_else(|| panic!("gm-probe does not know port {}", spec.id));
        let mut a: Vec<&str> = platforms.to_vec();
        let mut b: Vec<&str> = spec.platforms.iter().map(String::as_str).collect();
        a.sort();
        b.sort();
        assert_eq!(a, b, "platforms of {}", spec.id);
    }
    assert_eq!(gm_probe::PORT_PLATFORMS.len(), registry.all().len());
    for id in gm_ports::PORT_IDS {
        assert!(registry.get(id).is_some());
    }
}

// ---------------------------------------------------------------------------
// gm-supervisor running the real worker launch spec
// ---------------------------------------------------------------------------

#[cfg(unix)]
fn alive(pid: i32) -> bool {
    if let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        let state = stat.rsplit(')').next().unwrap_or("").trim_start();
        return !state.starts_with('Z') && !state.starts_with('X');
    }
    // SAFETY: signal 0 only checks existence.
    unsafe { libc::kill(pid, 0) == 0 }
}

#[cfg(unix)]
async fn wait_for_line(h: &gm_supervisor::Handle, prefix: &str) -> String {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if let Some(l) = h
            .recent_lines()
            .into_iter()
            .find(|l| l.line.starts_with(prefix))
        {
            return l.line;
        }
        assert!(
            Instant::now() < deadline,
            "no line starting {prefix:?}; got {:?}",
            h.recent_lines()
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// worker_spec → a stand-in worker.py that starts a grandchild which ignores
/// SIGINT. Stop must run the polite path (SIGINT → "leaving") and still leave
/// no process of the tree behind; the token must reach the env, not argv.
#[cfg(unix)]
#[tokio::test]
async fn worker_spec_runs_and_stop_kills_the_whole_tree() {
    let python = ["python3", "python"].into_iter().find(|p| {
        Command::new(p)
            .arg("-V")
            .output()
            .is_ok_and(|o| o.status.success())
    });
    let Some(python) = python else {
        eprintln!("skipped: no python on PATH");
        return;
    };
    let engine = tempfile::tempdir().unwrap();
    std::fs::write(
        engine.path().join("worker.py"),
        r#"
import os, signal, subprocess, sys, time
print("argv " + " ".join(sys.argv[1:]))
print("token-env " + str(os.environ.get("GRADMESH_TOKEN") == "e2e-secret"))
print("token-argv " + str(any("e2e-secret" in a for a in sys.argv)))
child = subprocess.Popen([sys.executable, "-c",
    "import signal, time; signal.signal(signal.SIGINT, signal.SIG_IGN); signal.signal(signal.SIGTERM, signal.SIG_IGN); time.sleep(300)"])
print("grandchild %d" % child.pid)
try:
    while True:
        time.sleep(0.05)
except KeyboardInterrupt:
    print("leaving")
    sys.exit(0)
"#,
    )
    .unwrap();
    let mut launch =
        gm_supervisor::WorkerLaunch::new(python, engine.path(), "http://127.0.0.1:9", "e2e-secret");
    launch.backend = Some(Backend::Cpu);
    launch.name = Some("e2e box".into());
    let handle = Supervisor::spawn(gm_supervisor::worker_spec(&launch));
    let mut events = handle.subscribe();

    let pid: i32 = wait_for_line(&handle, "grandchild ").await["grandchild ".len()..]
        .parse()
        .unwrap();
    assert!(alive(pid));
    let argv = wait_for_line(&handle, "argv ").await;
    assert!(
        argv.contains("--server-url http://127.0.0.1:9 --backend cpu --discover --name e2e box"),
        "{argv}"
    );
    assert_eq!(wait_for_line(&handle, "token-env ").await, "token-env True");
    assert_eq!(
        wait_for_line(&handle, "token-argv ").await,
        "token-argv False"
    );

    let st = tokio::time::timeout(Duration::from_secs(15), handle.stop())
        .await
        .expect("stop hung");
    assert_eq!(st.state, ProcState::Stopped);
    assert_eq!(
        st.exit_code,
        Some(0),
        "worker should leave cleanly on SIGINT"
    );
    assert!(handle.recent_lines().iter().any(|l| l.line == "leaving"));
    let deadline = Instant::now() + Duration::from_secs(3);
    while alive(pid) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(25));
    }
    let survived = alive(pid);
    if survived {
        // SAFETY: clean up before failing.
        unsafe { libc::kill(pid, libc::SIGKILL) };
    }
    assert!(!survived, "grandchild {pid} survived stop()");
    // The status stream ended in Stopped too (what the UI sees).
    let mut last = None;
    while let Ok(ev) = events.try_recv() {
        if let gm_supervisor::Event::Status(s) = ev {
            last = Some(s.state);
        }
    }
    assert_eq!(last, Some(ProcState::Stopped));
}

/// gm-ports builds a tiny port with real uv and a private CPython, then
/// gm-supervisor runs a script in that env's python and streams its output.
#[tokio::test]
#[ignore = "downloads CPython and a wheel (GitHub + PyPI)"]
async fn provisioned_port_python_runs_under_the_supervisor() {
    let data = tempfile::tempdir().unwrap();
    let root = data.path().join("ports");
    let uv = gm_ports::uv::ensure(data.path()).await.expect("uv");
    let spec = gm_types::PortSpec {
        id: "cpu".into(),
        backend: Backend::Cpu,
        label: "e2e tiny port".into(),
        python: "3.12".into(),
        torch: String::new(),
        torchvision: String::new(),
        index_url: None,
        local_suffix: String::new(),
        packages: vec!["six==1.17.0".into()],
        platforms: vec![gm_ports::current_platform()
            .expect("supported platform")
            .into()],
        reference: false,
        experimental: false,
        min_driver: None,
        notes: None,
    };
    let fp = gm_ports::local_machine_fingerprint();
    let opts = gm_ports::ProvisionOptions {
        facts_fingerprint: fp.clone(),
        verify: gm_ports::VerifyMode::Skip,
        ..Default::default()
    };
    let mut steps = vec![];
    let st = gm_ports::provision_with(&spec, &root, &uv, &opts, |p| {
        if steps.last() != Some(&p.step) {
            steps.push(p.step);
        }
    })
    .await
    .expect("provision");
    assert_eq!(st.state, gm_types::PortState::Ready, "{st:?}");
    assert_eq!(steps, ["prepare", "python", "venv", "install", "lock"]);
    assert_eq!(
        gm_ports::status(&spec, &root, &fp).await.state,
        gm_types::PortState::Ready
    );
    let python = st.python_path.expect("python path");
    assert!(Path::new(&python).starts_with(&root));

    let handle = Supervisor::spawn(ProcSpec::new(ProcRole::Installer, &python).args([
        "-u",
        "-c",
        "import sys, six\nprint('py %d.%d' % sys.version_info[:2])\nprint('six ' + six.__version__)\nprint('base ' + sys.base_prefix)\nfor i in range(3): print('tick %d' % i)",
    ]));
    let st = tokio::time::timeout(Duration::from_secs(60), handle.wait())
        .await
        .expect("python hung");
    assert_eq!(
        (st.state, st.exit_code),
        (ProcState::Stopped, Some(0)),
        "{:?}",
        handle.recent_lines()
    );
    let lines: Vec<String> = handle.recent_lines().into_iter().map(|l| l.line).collect();
    assert!(lines.contains(&"py 3.12".to_string()), "{lines:?}");
    assert!(lines.contains(&"six 1.17.0".to_string()), "{lines:?}");
    assert!(lines.iter().filter(|l| l.starts_with("tick ")).count() == 3);
    let base = lines.iter().find_map(|l| l.strip_prefix("base ")).unwrap();
    assert!(
        Path::new(base).starts_with(root.join(gm_ports::PYTHON_DIR)),
        "the env must run on the private CPython, not the machine's: {base}"
    );
}

// ---------------------------------------------------------------------------
// The gmctl binary
// ---------------------------------------------------------------------------

fn gmctl(args: &[&str]) -> (bool, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_gmctl"))
        .args(args)
        .env_remove("GRADAPP_PORTS_DIR")
        .output()
        .unwrap();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn gmctl_version_probe_and_ports() {
    let (ok, out, err) = gmctl(&["version"]);
    assert!(ok, "{err}");
    assert!(
        out.contains(gm_types::APP_VERSION) && out.contains("protocol 5"),
        "{out}"
    );

    let (ok, out, err) = gmctl(&["probe", "--json"]);
    assert!(ok, "{err}");
    let v: Value = serde_json::from_str(&out).unwrap();
    let port = v["profile"]["port"].as_str().unwrap().to_string();
    assert!(
        Registry::builtin().get(&port).is_some(),
        "probe chose unknown port {port}"
    );
    assert!(v["facts"]["hostname"]
        .as_str()
        .is_some_and(|h| !h.is_empty()));

    let (ok, out, _) = gmctl(&["ports", "list"]);
    assert!(ok);
    for id in gm_ports::PORT_IDS {
        assert!(
            out.lines().any(|l| l.starts_with(id)),
            "{id} missing from:\n{out}"
        );
    }

    let (ok, out, err) = gmctl(&["ports", "plan", "cpu"]);
    assert!(ok, "{err}");
    let lock: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(lock["requirements"][0], "torch==2.13.0+cpu");
    assert_eq!(lock["index_url"], "https://download.pytorch.org/whl/cpu");

    let dir = tempfile::tempdir().unwrap();
    let ports_dir = dir.path().to_str().unwrap();
    let (ok, out, err) = gmctl(&["--ports-dir", ports_dir, "ports", "status", "cpu"]);
    assert!(ok, "{err}");
    let st: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(st["state"], "not_installed");

    // A port with no build for this machine is refused before uv runs.
    let foreign = if gm_ports::current_platform() == Some("macos-arm64") {
        "rocm"
    } else {
        "mps"
    };
    let (ok, _, err) = gmctl(&[
        "--ports-dir",
        ports_dir,
        "ports",
        "install",
        foreign,
        "--skip-verify",
    ]);
    assert!(!ok);
    assert!(err.contains("has no build for"), "{err}");
    assert!(!dir.path().join(foreign).exists());

    let (ok, _, err) = gmctl(&["--prefer", "metal", "probe"]);
    assert!(!ok && err.contains("metal"));
}

#[test]
fn gmctl_discover_prints_json() {
    let (ok, out, err) = gmctl(&["discover", "--seconds", "3", "--json"]);
    assert!(ok, "{err}");
    let meshes: Value = serde_json::from_str(&out).unwrap();
    assert!(meshes.is_array());
}
