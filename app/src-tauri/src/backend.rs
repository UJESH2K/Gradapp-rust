//! Everything the commands need from the rest of GradApp, in one place.
//!
//! Commands never talk to the crates directly; this file is the glue:
//!
//! * device: `gm_probe::detect` (once, on a blocking thread) + `select_profile`
//!   / `host_profile`, honouring the preferred-backend setting;
//! * ports: `gm_ports::Registry` + `status`, installs with `uv::ensure` +
//!   `provision_with`, one at a time (`install_lock`);
//! * discovery: `gm_net::discover`, cancellable;
//! * join: `parse_address` → `/health` → token → port Ready → worker under
//!   `gm_supervisor`, its lines and milestones forwarded to the window;
//! * host: coordinator under `gm_supervisor`, then the join surface on :3000
//!   and the LAN beacon;
//! * mesh data: `CoordinatorClient` (`/mesh`, `/runs`, `/datasets`) and the
//!   `/events` stream, turned into the interface's types by `mesh.rs`.

use std::collections::{HashMap, VecDeque};
use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use gm_net::{Beacon, CancellationToken, CoordinatorClient, JoinConfig, JoinInfo, JoinSurface};
use gm_ports::{ProvisionOptions, Registry, VerifyMode};
use gm_probe::Prefer;
use gm_supervisor::{Event as ProcEvent, FailureTracker, Handle, Milestone, Supervisor};
use gm_types::*;
use serde_json::Value;
use tauri::{AppHandle, Emitter, Manager};

use crate::mesh::{self, SelfInfo};
use crate::model::*;

pub const EVENT: &str = "gm://event";
const LOG_CAP: usize = 2000;
const FEED_CAP: usize = 200;
/// How long a starting coordinator gets to answer `/health`.
const COORDINATOR_BOOT: Duration = Duration::from_secs(90);

pub fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

fn config_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("GradApp")
        .join("settings.json")
}

fn stopped(role: ProcRole) -> ProcStatus {
    ProcStatus {
        role,
        state: ProcState::Stopped,
        pid: None,
        restarts: 0,
        last_line: None,
        exit_code: None,
    }
}

fn env_port(key: &str, default: u16) -> u16 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .filter(|p| *p != 0)
        .unwrap_or(default)
}

/// Ports a hosted mesh listens on. `GRADAPP_COORDINATOR_PORT` /
/// `GRADAPP_WEB_PORT` exist for development (a second mesh on one machine);
/// joiners find a non-default port through the beacon, which carries it.
pub fn host_ports() -> (u16, u16) {
    (
        env_port("GRADAPP_COORDINATOR_PORT", COORDINATOR_PORT),
        env_port("GRADAPP_WEB_PORT", WEB_PORT),
    )
}

fn prefer_of(backend: Option<Backend>) -> Prefer {
    match backend {
        None => Prefer::Auto,
        Some(Backend::Cuda) => Prefer::Cuda,
        Some(Backend::Xpu) => Prefer::Xpu,
        Some(Backend::Mps) => Prefer::Mps,
        Some(Backend::Rocm) => Prefer::Rocm,
        Some(Backend::Cpu) => Prefer::Cpu,
    }
}

fn is_ready(st: &PortStatus) -> bool {
    st.state == PortState::Ready && st.python_path.is_some()
}

fn web_port_of(invite: &Invite) -> u16 {
    url::Url::parse(&invite.web)
        .ok()
        .and_then(|u| u.port_or_known_default())
        .unwrap_or(WEB_PORT)
}

/// `gradapp://join?host=H&port=P&token=…` (the form GradApp 2.0-dev builds
/// put in links and QR codes) → the v1 form `gm_net::parse_address` reads,
/// `gradapp://join?server=http://H:P&token=…`. Anything else is returned as is.
pub fn normalize_invite(text: &str) -> String {
    let text = text.trim();
    if !text
        .get(..8)
        .is_some_and(|p| p.eq_ignore_ascii_case("gradapp:"))
    {
        return text.to_string();
    }
    let rest = text[8..].trim_start_matches('/');
    let Ok(url) = url::Url::parse(&format!("gradapp://{rest}")) else {
        return text.to_string();
    };
    let pairs: Vec<(String, String)> = url
        .query_pairs()
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    let get = |key: &str| {
        pairs
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
            .filter(|v| !v.is_empty())
    };
    if get("server").is_some() {
        return text.to_string();
    }
    let Some(host) = get("host") else {
        return text.to_string();
    };
    let port = get("port")
        .and_then(|p| p.parse::<u16>().ok())
        .unwrap_or(COORDINATOR_PORT);
    let host = if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]")
    } else {
        host
    };
    let mut link = url::Url::parse("gradapp://join").expect("static url");
    link.query_pairs_mut()
        .append_pair("server", &format!("http://{host}:{port}"));
    for key in ["token", "name"] {
        if let Some(value) = get(key) {
            link.query_pairs_mut().append_pair(key, &value);
        }
    }
    link.to_string()
}

/// The engine folder (worker.py, coordinator/): `GRADAPP_ENGINE_DIR`, the
/// bundled `engine/` resource, then `<repo>/desktop/engine` for `cargo run`.
pub fn engine_dir(app: &AppHandle) -> Result<PathBuf, String> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(dir) = std::env::var_os("GRADAPP_ENGINE_DIR").filter(|d| !d.is_empty()) {
        candidates.push(dir.into());
    }
    if let Ok(res) = app.path().resource_dir() {
        candidates.push(res.join("engine"));
    }
    candidates.push(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../desktop/engine"));
    candidates
        .into_iter()
        .find(|d| d.join("worker.py").is_file() && d.join("coordinator").join("app.py").is_file())
        .map(|d| d.canonicalize().unwrap_or(d))
        .ok_or_else(|| "GradApp's engine folder (worker.py) is missing from this installation. Reinstall GradApp.".to_string())
}

/// Where `logo.png` for the join page lives: the bundled `assets/`, or the UI folder in development.
fn assets_dir(app: &AppHandle) -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(res) = app.path().resource_dir() {
        candidates.push(res.join("assets"));
    }
    candidates.push(Path::new(env!("CARGO_MANIFEST_DIR")).join("../ui"));
    candidates
        .into_iter()
        .find(|d| d.join("logo.png").is_file())
}

/// `<state>/coordinator.json`: the coordinator's token, id and name.
fn read_mesh_state(state_dir: &Path) -> Option<Value> {
    serde_json::from_str(&std::fs::read_to_string(state_dir.join("coordinator.json")).ok()?).ok()
}

/// Set the mesh name before the coordinator starts. A missing file is created
/// with just the name; the coordinator fills in the token and id on load.
fn write_mesh_name(state_dir: &Path, name: &str) -> std::io::Result<()> {
    std::fs::create_dir_all(state_dir)?;
    let mut state = read_mesh_state(state_dir).unwrap_or_else(|| serde_json::json!({}));
    if let Some(obj) = state.as_object_mut() {
        obj.insert("mesh_name".into(), Value::String(name.to_string()));
    }
    let path = state_dir.join("coordinator.json");
    let tmp = state_dir.join("coordinator.json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(&state).unwrap_or_default())?;
    std::fs::rename(tmp, path)
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

struct WorkerSession {
    generation: u64,
    handle: Option<Handle>,
    cancel: CancellationToken,
    coordinator: String,
    token: String,
    mesh_id: String,
}

struct HostSession {
    handle: Handle,
    surface: Option<JoinSurface>,
    beacon: Option<Arc<Beacon>>,
    cancel: CancellationToken,
}

struct Feed {
    key: String,
    cancel: CancellationToken,
    events: VecDeque<MeshEvent>,
}

/// Where the mesh this app is in can be reached.
#[derive(Clone)]
struct MeshTarget {
    /// URL the app calls (127.0.0.1 for a mesh hosted here).
    api: String,
    /// URL shown to the person.
    display: String,
    token: String,
}

/// Shared state, managed by Tauri (`app.state::<AppBackend>()`).
pub struct AppBackend {
    inner: Mutex<Inner>,
    registry: Registry,
    facts: tokio::sync::OnceCell<DeviceFacts>,
    /// Held for a whole port install, so two never run at once.
    install_lock: Arc<tokio::sync::Mutex<()>>,
    /// Join/leave/host transitions happen one at a time.
    transition: tokio::sync::Mutex<()>,
}

struct Inner {
    settings: Settings,
    joined: Option<Invite>,
    hosting: Option<HostInfo>,
    worker: ProcStatus,
    coordinator: ProcStatus,
    logs: VecDeque<LogEntry>,
    update: Option<UpdateInfo>,
    installing: Option<String>,
    port_failures: HashMap<String, String>,
    discover: Option<(u64, CancellationToken)>,
    worker_session: Option<WorkerSession>,
    generation: u64,
    host_session: Option<HostSession>,
    node_id: Option<String>,
    feed: Option<Feed>,
}

impl AppBackend {
    pub fn load() -> Self {
        let settings = std::fs::read_to_string(config_path())
            .ok()
            .and_then(|raw| serde_json::from_str::<Settings>(&raw).ok())
            .unwrap_or_default();
        let registry = Registry::from_env_or_builtin().unwrap_or_else(|error| {
            tracing::error!(
                "GRADAPP_PORTS_DIR could not be loaded ({error}); using the built-in ports"
            );
            Registry::builtin()
        });
        Self {
            inner: Mutex::new(Inner {
                settings,
                joined: None,
                hosting: None,
                worker: stopped(ProcRole::Worker),
                coordinator: stopped(ProcRole::Coordinator),
                logs: VecDeque::new(),
                update: None,
                installing: None,
                port_failures: HashMap::new(),
                discover: None,
                worker_session: None,
                generation: 0,
                host_session: None,
                node_id: None,
                feed: None,
            }),
            registry,
            facts: tokio::sync::OnceCell::new(),
            install_lock: Arc::new(tokio::sync::Mutex::new(())),
            transition: tokio::sync::Mutex::new(()),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    // ------------------------------------------------------------- settings

    pub fn settings(&self) -> Settings {
        self.lock().settings.clone()
    }

    pub fn data_dir(&self) -> PathBuf {
        let dir = self.settings().data_dir;
        if dir.trim().is_empty() {
            gm_ports::data_dir()
        } else {
            PathBuf::from(dir)
        }
    }

    fn env_root(&self) -> PathBuf {
        self.data_dir().join("ports")
    }

    pub fn save_settings(&self, next: Settings) -> Result<Settings, String> {
        let path = config_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let raw = serde_json::to_string_pretty(&next).map_err(|e| e.to_string())?;
        std::fs::write(&path, raw)
            .map_err(|e| format!("Could not save settings to {}: {e}", path.display()))?;
        self.lock().settings = next.clone();
        Ok(next)
    }

    /// Settings from the window. The fields the backend owns (last mesh,
    /// rejoin flag) are kept, so a stale copy in the window cannot undo a join.
    pub fn save_settings_from_ui(&self, mut next: Settings) -> Result<Settings, String> {
        let current = self.settings();
        next.last_invite = current.last_invite;
        next.rejoin_on_start = current.rejoin_on_start;
        self.save_settings(next)
    }

    fn update_settings(&self, f: impl FnOnce(&mut Settings)) {
        let mut next = self.settings();
        f(&mut next);
        if let Err(error) = self.save_settings(next) {
            tracing::warn!("{error}");
        }
    }

    // ---------------------------------------------------------------- status

    pub fn status(&self) -> AppStatus {
        let inner = self.lock();
        AppStatus {
            version: APP_VERSION.to_string(),
            worker: inner.worker.clone(),
            coordinator: inner.coordinator.clone(),
            joined: inner.joined.clone(),
            hosting: inner.hosting.clone(),
            update: inner.update.clone(),
        }
    }

    pub fn contributing(&self) -> bool {
        self.lock().worker_session.is_some()
    }

    pub fn is_hosting(&self) -> bool {
        self.lock().hosting.is_some()
    }

    pub fn logs(&self) -> Vec<LogEntry> {
        self.lock().logs.iter().cloned().collect()
    }

    /// Record a line and push it to the window.
    pub fn log(&self, app: &AppHandle, role: ProcRole, line: impl Into<String>) {
        let line = line.into();
        let source = match role {
            ProcRole::Worker => "worker",
            ProcRole::Coordinator => "coordinator",
            ProcRole::Installer => "installer",
        };
        tracing::debug!(target: "gradapp_lib::proc", "[{source}] {line}");
        {
            let mut inner = self.lock();
            inner.logs.push_back(LogEntry {
                at: now(),
                source: source.into(),
                line: line.clone(),
            });
            while inner.logs.len() > LOG_CAP {
                inner.logs.pop_front();
            }
        }
        emit(app, AppEvent::Log { role, line });
    }

    fn set_proc(&self, app: &AppHandle, status: ProcStatus) {
        {
            let mut inner = self.lock();
            match status.role {
                ProcRole::Worker => inner.worker = status.clone(),
                ProcRole::Coordinator => inner.coordinator = status.clone(),
                ProcRole::Installer => {}
            }
        }
        emit(app, AppEvent::Proc { status });
        crate::tray::refresh(app);
    }

    fn set_proc_line(
        &self,
        app: &AppHandle,
        role: ProcRole,
        state: ProcState,
        line: impl Into<String>,
    ) {
        self.set_proc(
            app,
            ProcStatus {
                state,
                last_line: Some(line.into()),
                ..stopped(role)
            },
        );
    }

    // ---------------------------------------------------------------- device

    /// The probe runs once per launch, on a blocking thread (nvidia-smi,
    /// PowerShell and system_profiler can take seconds).
    pub async fn facts(&self) -> DeviceFacts {
        self.facts
            .get_or_init(|| async {
                let started = Instant::now();
                let facts = match tauri::async_runtime::spawn_blocking(gm_probe::detect).await {
                    Ok(facts) => facts,
                    Err(error) => {
                        tracing::error!("hardware probe panicked: {error}");
                        fallback_facts()
                    }
                };
                tracing::info!(
                    "probe: {} ({} ms)",
                    gm_probe::summary(&facts),
                    started.elapsed().as_millis()
                );
                facts
            })
            .await
            .clone()
    }

    pub async fn device(&self) -> DeviceReport {
        let facts = self.facts().await;
        let preferred = self.settings().preferred_backend;
        let auto = gm_probe::select_profile(&facts, Prefer::Auto);
        let mut profile = match preferred {
            Some(backend) => gm_probe::select_profile(&facts, prefer_of(Some(backend))),
            None => auto.clone(),
        };
        if preferred.is_some() && (profile.backend != auto.backend || profile.port != auto.port) {
            profile.warnings.insert(
                0,
                format!(
                    "Forced to {} in Settings; the automatic choice was {} ({}).",
                    profile.backend.as_str(),
                    auto.backend.as_str(),
                    auto.port
                ),
            );
        }
        DeviceReport {
            facts,
            profile,
            overridden: preferred.is_some(),
        }
    }

    /// The profile a coordinator host runs (gm-probe `host_profile`).
    async fn host_profile(&self) -> Profile {
        let facts = self.facts().await;
        gm_probe::host_profile(&facts, prefer_of(self.settings().preferred_backend))
    }

    // ----------------------------------------------------------------- ports

    pub async fn ports(&self) -> Vec<PortEntry> {
        let device = self.device().await;
        let platform = gm_ports::current_platform();
        let root = self.env_root();
        let fingerprint = gm_ports::local_machine_fingerprint();
        let (installing, failures) = {
            let inner = self.lock();
            (inner.installing.clone(), inner.port_failures.clone())
        };
        let rows = self.registry.all().iter().map(|spec| {
            let (root, fingerprint) = (&root, &fingerprint);
            let (installing, failures, device) = (&installing, &failures, &device);
            async move {
                let compatible = platform.is_some_and(|p| spec.platforms.iter().any(|x| x == p));
                let mut status = gm_ports::status(spec, root, fingerprint).await;
                if installing.as_deref() == Some(spec.id.as_str()) {
                    status.state = PortState::Installing;
                } else if status.state == PortState::NotInstalled {
                    if let Some(message) = failures.get(&spec.id) {
                        status.state = PortState::Failed;
                        status.message = Some(message.clone());
                    } else if !compatible {
                        status.state = PortState::Blocked;
                    }
                }
                PortEntry {
                    selected: spec.id == device.profile.port,
                    incompatible_reason: (!compatible).then(|| {
                        format!(
                            "Built for {}; this machine is {}.",
                            spec.platforms.join(", "),
                            platform.unwrap_or("an unsupported platform")
                        )
                    }),
                    compatible,
                    status,
                    spec: spec.clone(),
                }
            }
        });
        futures::future::join_all(rows).await
    }

    fn port_spec(&self, id: &str) -> Result<PortSpec, String> {
        self.registry
            .get(id)
            .cloned()
            .ok_or_else(|| format!("GradApp has no port called {id}."))
    }

    fn compatible(spec: &PortSpec) -> Result<(), String> {
        let platform = gm_ports::current_platform();
        if platform.is_some_and(|p| spec.platforms.iter().any(|x| x == p)) {
            Ok(())
        } else {
            Err(format!(
                "{} is built for {}; this machine is {}.",
                spec.label,
                spec.platforms.join(", "),
                platform.unwrap_or("an unsupported platform")
            ))
        }
    }

    /// Start installing (or repairing) a port; progress arrives as events.
    /// Refuses while another install runs.
    pub fn install_port(&self, app: &AppHandle, port_id: String) -> Result<PortStatus, String> {
        let spec = self.port_spec(&port_id)?;
        Self::compatible(&spec)?;
        let guard = self.install_lock.clone().try_lock_owned().map_err(|_| {
            let busy = self
                .lock()
                .installing
                .clone()
                .unwrap_or_else(|| "another port".into());
            format!("{busy} is installing. Wait for it to finish.")
        })?;
        let status = self.mark_installing(app, &spec);
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            let _guard = guard;
            let backend = app.state::<AppBackend>();
            let _ = backend.provision(&app, &spec).await;
        });
        Ok(status)
    }

    fn mark_installing(&self, app: &AppHandle, spec: &PortSpec) -> PortStatus {
        let status = PortStatus {
            port: spec.id.clone(),
            state: PortState::Installing,
            env_dir: gm_ports::env_dir(&self.env_root(), &spec.id)
                .to_string_lossy()
                .into_owned(),
            python_path: None,
            torch: None,
            device: None,
            message: None,
        };
        {
            let mut inner = self.lock();
            inner.installing = Some(spec.id.clone());
            inner.port_failures.remove(&spec.id);
        }
        emit(
            app,
            AppEvent::PortStatus {
                status: status.clone(),
            },
        );
        status
    }

    /// The port's status, installing it first when it is not Ready. Waits for
    /// an install that is already running instead of starting a second one.
    async fn ensure_port(&self, app: &AppHandle, spec: &PortSpec) -> Result<PortStatus, String> {
        Self::compatible(spec)?;
        let root = self.env_root();
        let fingerprint = gm_ports::local_machine_fingerprint();
        let status = gm_ports::status(spec, &root, &fingerprint).await;
        if is_ready(&status) {
            return Ok(status);
        }
        let _guard = self.install_lock.lock().await;
        let status = gm_ports::status(spec, &root, &fingerprint).await;
        if is_ready(&status) {
            return Ok(status);
        }
        self.mark_installing(app, spec);
        self.provision(app, spec).await
    }

    /// uv + private CPython + pinned packages + verify. The caller holds
    /// `install_lock` and has called `mark_installing`.
    async fn provision(&self, app: &AppHandle, spec: &PortSpec) -> Result<PortStatus, String> {
        let root = self.env_root();
        let data = self.data_dir();
        self.log(
            app,
            ProcRole::Installer,
            format!(
                "Installing port {} ({}, Python {})",
                spec.id, spec.label, spec.python
            ),
        );
        let progress = |step: &str, percent: f32, line: String| {
            emit(
                app,
                AppEvent::PortProgress {
                    port: spec.id.clone(),
                    step: step.into(),
                    percent: Some(percent),
                    line: Some(line),
                },
            );
        };
        progress("prepare", 0.01, "Looking for uv".into());
        let result = match gm_ports::uv::ensure(&data).await {
            Err(error) => Err(format!(
                "Could not get uv, the installer GradApp uses: {error}"
            )),
            Ok(uv) => {
                let version = uv.version().await.unwrap_or_default();
                let line = format!("uv {} at {}", version.trim(), uv.path.display());
                self.log(app, ProcRole::Installer, line.clone());
                progress("prepare", 0.02, line);
                // A torch-free port (test ports, a future control-plane port) has no kernel to check.
                let skip = spec.torch.is_empty()
                    || std::env::var_os("GRADAPP_SKIP_VERIFY").is_some_and(|v| v == "1");
                let opts = ProvisionOptions {
                    verify: if skip {
                        VerifyMode::Skip
                    } else {
                        VerifyMode::Backend
                    },
                    ..ProvisionOptions::default()
                };
                let sink_app = app.clone();
                gm_ports::provision_with(spec, &root, &uv, &opts, move |p| {
                    let line = p.line.clone();
                    let mut event: AppEvent = p.into();
                    if let AppEvent::PortProgress { percent, .. } = &mut event {
                        // gm-ports counts 0..=100; the window draws 0..=1.
                        *percent = percent.map(|x| (x / 100.0).clamp(0.0, 1.0));
                    }
                    emit(&sink_app, event);
                    if let Some(line) = line {
                        sink_app
                            .state::<AppBackend>()
                            .log(&sink_app, ProcRole::Installer, line);
                    }
                })
                .await
                .map_err(|error| error.to_string())
            }
        };
        self.lock().installing = None;
        match result {
            Ok(status) if is_ready(&status) => {
                self.lock().port_failures.remove(&spec.id);
                self.log(
                    app,
                    ProcRole::Installer,
                    format!(
                        "Port {} is ready ({})",
                        spec.id,
                        status.torch.clone().unwrap_or_default()
                    ),
                );
                emit(
                    app,
                    AppEvent::PortStatus {
                        status: status.clone(),
                    },
                );
                notice(
                    app,
                    "info",
                    format!("{} is ready", spec.label),
                    "This machine can join a mesh with it.",
                );
                Ok(status)
            }
            Ok(mut status) => {
                let message = status.message.clone().unwrap_or_else(|| {
                    format!("{} did not pass its check on this machine.", spec.label)
                });
                self.lock()
                    .port_failures
                    .insert(spec.id.clone(), message.clone());
                status.state = PortState::Failed;
                self.log(
                    app,
                    ProcRole::Installer,
                    format!("Port {} failed: {message}", spec.id),
                );
                emit(app, AppEvent::PortStatus { status });
                notice(
                    app,
                    "error",
                    format!("{} could not be verified", spec.label),
                    &message,
                );
                Err(message)
            }
            Err(message) => {
                self.lock()
                    .port_failures
                    .insert(spec.id.clone(), message.clone());
                let status = PortStatus {
                    port: spec.id.clone(),
                    state: PortState::Failed,
                    env_dir: gm_ports::env_dir(&root, &spec.id)
                        .to_string_lossy()
                        .into_owned(),
                    python_path: None,
                    torch: None,
                    device: None,
                    message: Some(message.clone()),
                };
                self.log(
                    app,
                    ProcRole::Installer,
                    format!("Port {} failed: {message}", spec.id),
                );
                emit(app, AppEvent::PortStatus { status });
                notice(
                    app,
                    "error",
                    format!("{} could not be installed", spec.label),
                    &message,
                );
                Err(message)
            }
        }
    }

    // ------------------------------------------------------------- discovery

    pub fn discover_start(&self, app: &AppHandle) {
        let (run, cancel) = {
            let mut inner = self.lock();
            if inner.discover.is_some() {
                return;
            }
            inner.generation += 1;
            let cancel = CancellationToken::new();
            inner.discover = Some((inner.generation, cancel.clone()));
            (inner.generation, cancel)
        };
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            let opts = gm_net::DiscoverOptions {
                cancel: cancel.clone(),
                ..Default::default()
            };
            let mut total = 0u32;
            let sink = app.clone();
            let found = gm_net::discover(opts, |event| {
                if let AppEvent::ScanProgress { total: t, .. } = &event {
                    total = *t;
                }
                emit(&sink, event);
            })
            .await;
            tracing::info!("discovery finished: {} mesh(es)", found.len());
            // Always end with a complete sweep so the window stops its spinner,
            // also when the sweep was skipped or cancelled.
            emit(
                &app,
                AppEvent::ScanProgress {
                    scanned: total,
                    total,
                },
            );
            let backend = app.state::<AppBackend>();
            let mut inner = backend.lock();
            // Only clear our own slot; a newer discovery has its own.
            if inner.discover.as_ref().is_some_and(|(id, _)| *id == run) {
                inner.discover = None;
            }
        });
    }

    pub fn discover_stop(&self) {
        if let Some((_, cancel)) = self.lock().discover.take() {
            cancel.cancel();
        }
    }

    // ------------------------------------------------------------- join/leave

    pub async fn join(&self, app: &AppHandle, text: &str) -> Result<Invite, String> {
        let _transition = self.transition.lock().await;
        let text = text.trim();
        if text.is_empty() {
            return Err("Paste an address or an invite link.".into());
        }
        let mut invite = gm_net::parse_address(&normalize_invite(text))
            .ok_or_else(|| format!("“{text}” is not an address GradApp understands."))?;
        let probe = CoordinatorClient::new(invite.coordinator.clone(), None)
            .with_timeout(Duration::from_secs(6));
        let health = probe.health().await.map_err(|e| {
            format!(
                "No GradMesh coordinator answered at {}. ({e})",
                invite.coordinator
            )
        })?;
        if health.protocol != MESH_PROTOCOL {
            self.log(
                app,
                ProcRole::Worker,
                format!(
                    "Warning: this mesh speaks protocol {}, this app {}.",
                    health.protocol, MESH_PROTOCOL
                ),
            );
        }

        // The token: from the link, from this app if it hosts that mesh, else from the host's join port.
        let own = self
            .lock()
            .hosting
            .clone()
            .filter(|h| h.mesh_id == health.mesh_id);
        if let Some(own) = own {
            invite.token = invite.token.or(Some(own.token));
            invite.mesh_name = invite.mesh_name.or(Some(own.mesh_name));
        }
        if invite.token.is_none() || invite.mesh_name.is_none() {
            let info = gm_net::read_invite(&invite.hostname, web_port_of(&invite)).await;
            invite.token = invite.token.or(info.token);
            invite.mesh_name = invite.mesh_name.or(info.mesh_name);
        }
        let token = invite
            .token
            .clone()
            .ok_or("This host did not share its mesh token. Paste the invite link from the host's Invite screen.")?;

        let device = self.device().await;
        if let Some(blocked) = &device.profile.blocked {
            return Err(match &device.profile.fix {
                Some(fix) => format!("{blocked} {fix}"),
                None => blocked.clone(),
            });
        }
        let spec = self.port_spec(&device.profile.port)?;

        self.stop_worker(app).await;
        let link = gm_net::invite_link(&invite.coordinator, &token, invite.mesh_name.as_deref());
        self.update_settings(|s| {
            s.last_invite = Some(link);
            s.rejoin_on_start = true;
        });
        let cancel = CancellationToken::new();
        let generation = {
            let mut inner = self.lock();
            inner.generation += 1;
            inner.joined = Some(invite.clone());
            inner.node_id = None;
            inner.worker_session = Some(WorkerSession {
                generation: inner.generation,
                handle: None,
                cancel: cancel.clone(),
                coordinator: invite.coordinator.clone(),
                token: token.clone(),
                mesh_id: health.mesh_id.clone(),
            });
            inner.generation
        };
        let name = invite
            .mesh_name
            .clone()
            .unwrap_or_else(|| invite.hostname.clone());
        self.log(
            app,
            ProcRole::Worker,
            format!(
                "Joining {name} at {} (mesh {}) on port {}",
                invite.coordinator, health.mesh_id, spec.id
            ),
        );
        self.set_proc_line(
            app,
            ProcRole::Worker,
            ProcState::Starting,
            format!("Checking port {}", spec.id),
        );
        self.refresh_feed(app);

        let job = WorkerJob {
            generation,
            cancel,
            coordinator: invite.coordinator.clone(),
            token,
            profile: device.profile,
            spec,
        };
        let app = app.clone();
        tauri::async_runtime::spawn(async move { run_worker(app, job).await });
        Ok(invite)
    }

    /// Stop contributing because the person asked: no auto-rejoin next start.
    pub async fn leave(&self, app: &AppHandle) {
        let _transition = self.transition.lock().await;
        self.update_settings(|s| s.rejoin_on_start = false);
        if self.stop_worker(app).await {
            self.log(app, ProcRole::Worker, "Left the mesh.");
        }
    }

    /// Stop the worker (if any), then tell the coordinator it left. Returns
    /// whether there was one.
    async fn stop_worker(&self, app: &AppHandle) -> bool {
        let (session, node_id) = {
            let mut inner = self.lock();
            inner.joined = None;
            (inner.worker_session.take(), inner.node_id.take())
        };
        let Some(session) = session else { return false };
        session.cancel.cancel();
        if let Some(handle) = &session.handle {
            // SIGINT first: worker.py says goodbye itself; killed after the grace period.
            if tokio::time::timeout(Duration::from_secs(12), handle.stop())
                .await
                .is_err()
            {
                tracing::warn!("worker did not stop within 12 s");
            }
        }
        // Then /leave, so a worker that could not say goodbye (Windows has no
        // polite stop, or it was killed) frees its shard now rather than at
        // the heartbeat timeout. Sent after the stop so the worker cannot
        // re-register in between; a second leave is a no-op for the coordinator.
        if let Some(node_id) = node_id {
            post_leave(&session.coordinator, &session.token, &node_id).await;
        }
        self.set_proc(app, stopped(ProcRole::Worker));
        self.refresh_feed(app);
        true
    }

    fn worker_current(&self, generation: u64) -> bool {
        self.lock()
            .worker_session
            .as_ref()
            .is_some_and(|s| s.generation == generation)
    }

    // ------------------------------------------------------------------ host

    pub async fn host_start(
        &self,
        app: &AppHandle,
        mesh_name: Option<String>,
    ) -> Result<HostInfo, String> {
        let _transition = self.transition.lock().await;
        if let Some(existing) = self.lock().hosting.clone() {
            return Ok(existing);
        }
        match self.start_hosting(app, mesh_name).await {
            Ok(info) => Ok(info),
            Err(error) => {
                self.log(
                    app,
                    ProcRole::Coordinator,
                    format!("Could not host: {error}"),
                );
                self.set_proc_line(app, ProcRole::Coordinator, ProcState::Failed, error.clone());
                Err(error)
            }
        }
    }

    async fn start_hosting(
        &self,
        app: &AppHandle,
        mesh_name: Option<String>,
    ) -> Result<HostInfo, String> {
        let (port, web_port) = host_ports();
        let lan = gm_net::lan_client();
        if let Some(hit) =
            gm_net::probe_health(&lan, "127.0.0.1", port, Duration::from_millis(800)).await
        {
            return Err(format!(
                "A GradMesh coordinator (mesh {}) already runs on port {port} of this computer. Stop it, or join it from Discover.",
                hit.health.mesh_id
            ));
        }
        let profile = self.host_profile().await;
        if let Some(blocked) = &profile.blocked {
            return Err(blocked.clone());
        }
        let spec = self.port_spec(&profile.port)?;
        self.set_proc_line(
            app,
            ProcRole::Coordinator,
            ProcState::Starting,
            format!("Checking port {}", spec.id),
        );
        let status = self.ensure_port(app, &spec).await?;
        let python = status
            .python_path
            .clone()
            .ok_or("The port has no Python interpreter.")?;
        let engine = engine_dir(app)?;

        let settings = self.settings();
        let fallback_name = {
            let device = settings.device_name.trim().to_string();
            let who = if device.is_empty() {
                self.facts().await.hostname
            } else {
                device
            };
            format!("{who}'s mesh")
        };
        let name = mesh_name
            .map(|n| n.trim().to_string())
            .filter(|n| !n.is_empty())
            .unwrap_or(fallback_name);
        let state_dir = self.data_dir().join("mesh");
        write_mesh_name(&state_dir, &name).map_err(|e| {
            format!(
                "Could not write the mesh state in {}: {e}",
                state_dir.display()
            )
        })?;

        let mut launch = gm_supervisor::CoordinatorLaunch::new(python, &engine, &state_dir);
        launch.port = port;
        launch.web_port = web_port;
        self.log(
            app,
            ProcRole::Coordinator,
            format!(
                "Starting the coordinator on port {port} with {} (state in {})",
                spec.id,
                state_dir.display()
            ),
        );
        let handle = Supervisor::spawn(gm_supervisor::coordinator_spec(&launch));
        let cancel = CancellationToken::new();
        {
            let app = app.clone();
            let (handle, cancel) = (handle.clone(), cancel.clone());
            tauri::async_runtime::spawn(async move { forward(app, handle, cancel, None).await });
        }

        // Wait for /health from *our* coordinator.
        let local = format!("http://127.0.0.1:{port}");
        let client =
            CoordinatorClient::new(local.clone(), None).with_timeout(Duration::from_secs(2));
        let started = Instant::now();
        let health = loop {
            if handle.is_finished() || handle.status().state == ProcState::Failed {
                let tail: Vec<String> = handle
                    .recent_lines()
                    .into_iter()
                    .rev()
                    .take(3)
                    .map(|l| l.line)
                    .collect();
                cancel.cancel();
                handle.stop().await;
                return Err(format!(
                    "The coordinator exited while starting. {}",
                    tail.into_iter().rev().collect::<Vec<_>>().join(" / ")
                ));
            }
            if let Ok(health) = client.health().await {
                break health;
            }
            if started.elapsed() > COORDINATOR_BOOT {
                cancel.cancel();
                handle.stop().await;
                return Err(format!(
                    "The coordinator did not answer on port {port} within {} s.",
                    COORDINATOR_BOOT.as_secs()
                ));
            }
            tokio::time::sleep(Duration::from_millis(400)).await;
        };
        let state = read_mesh_state(&state_dir).unwrap_or(Value::Null);
        let token = state
            .get("mesh_token")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or("The coordinator did not write its mesh token.")?;
        if state.get("mesh_id").and_then(Value::as_str) != Some(health.mesh_id.as_str()) {
            tracing::warn!("mesh id on disk differs from /health ({})", health.mesh_id);
        }

        let lan_ip = gm_net::primary_lan_address();
        let coordinator = format!("http://{lan_ip}:{port}");
        let web = format!("http://{lan_ip}:{web_port}");

        // Join surface (:3000): v1 apps and the one-line join read the token here.
        let info_dir = state_dir.clone();
        let fixed = (health.mesh_id.clone(), name.clone(), token.clone());
        let mut cfg = JoinConfig::new(&engine, move || {
            // Read per request, so a rotated token is served at once.
            let state = read_mesh_state(&info_dir).unwrap_or(Value::Null);
            let get = |k: &str| state.get(k).and_then(Value::as_str).map(str::to_string);
            JoinInfo {
                token: get("mesh_token").or(Some(fixed.2.clone())),
                coordinator_port: port,
                mesh_name: get("mesh_name").or(Some(fixed.1.clone())),
                mesh_id: get("mesh_id").or(Some(fixed.0.clone())),
                version: ENGINE_VERSION.to_string(),
                lan_address: gm_net::primary_lan_address().to_string(),
            }
        });
        cfg.assets_dir = assets_dir(app);
        cfg.health_upstream = Some(local.clone());
        let surface = match JoinSurface::serve(
            SocketAddr::from((Ipv4Addr::UNSPECIFIED, web_port)),
            cfg,
        )
        .await
        {
            Ok(surface) => {
                self.log(
                    app,
                    ProcRole::Coordinator,
                    format!("Join page on {web} (/gradapp.json, /join.sh, /join.ps1)"),
                );
                Some(surface)
            }
            Err(error) => {
                let message = format!("Port {web_port} is taken ({error}), so GradApp 1 and the one-line join cannot read the invite. GradApp 2 still finds this mesh.");
                self.log(app, ProcRole::Coordinator, message.clone());
                notice(app, "warn", "Join page not started", &message);
                None
            }
        };

        let beacon = Arc::new(Beacon::start(
            gm_net::beacon_packet(
                &health.mesh_id,
                Some(&name),
                &health.version,
                health.nodes_active,
                port,
                web_port,
            ),
            gm_net::beacon::BEACON_INTERVAL,
        ));
        self.log(
            app,
            ProcRole::Coordinator,
            format!(
                "Announcing {name} on the LAN (beacon {}:{BEACON_PORT})",
                Ipv4Addr::from(BEACON_GROUP)
            ),
        );

        // Keep the beacon's node count current.
        {
            let (beacon, cancel, client) = (beacon.clone(), cancel.clone(), client.clone());
            let (mesh_id, name) = (health.mesh_id.clone(), name.clone());
            tauri::async_runtime::spawn(async move {
                let mut last = health.nodes_active;
                while cancel
                    .run_until_cancelled(tokio::time::sleep(Duration::from_secs(5)))
                    .await
                    .is_some()
                {
                    if let Ok(h) = client.health().await {
                        if h.nodes_active != last {
                            last = h.nodes_active;
                            beacon.update(gm_net::beacon_packet(
                                &mesh_id,
                                Some(&name),
                                &h.version,
                                last,
                                port,
                                web_port,
                            ));
                        }
                    }
                }
            });
        }

        let info = HostInfo {
            mesh_id: health.mesh_id.clone(),
            mesh_name: name.clone(),
            invite_link: gm_net::invite_link(&coordinator, &token, Some(&name)),
            coordinator,
            web,
            token,
        };
        {
            let mut inner = self.lock();
            inner.hosting = Some(info.clone());
            inner.host_session = Some(HostSession {
                handle,
                surface,
                beacon: Some(beacon),
                cancel,
            });
        }
        self.log(
            app,
            ProcRole::Coordinator,
            format!(
                "Hosting {name} at {} (mesh {})",
                info.coordinator, info.mesh_id
            ),
        );
        crate::tray::refresh(app);
        self.refresh_feed(app);
        Ok(info)
    }

    pub async fn host_stop(&self, app: &AppHandle) {
        let _transition = self.transition.lock().await;
        let (session, hosted) = {
            let mut inner = self.lock();
            (inner.host_session.take(), inner.hosting.take())
        };
        // A worker training on this machine's own mesh has nothing left to train for.
        if let Some(hosted) = &hosted {
            let own = self
                .lock()
                .worker_session
                .as_ref()
                .is_some_and(|s| s.mesh_id == hosted.mesh_id);
            if own {
                self.stop_worker(app).await;
            }
        }
        if let Some(session) = session {
            session.cancel.cancel();
            if let Some(beacon) = session.beacon {
                match Arc::try_unwrap(beacon) {
                    Ok(beacon) => beacon.stop().await,
                    Err(shared) => drop(shared), // the updater task drops its copy on cancel; Drop stops it
                }
            }
            if let Some(surface) = session.surface {
                let _ = surface.shutdown().await;
            }
            if tokio::time::timeout(Duration::from_secs(10), session.handle.stop())
                .await
                .is_err()
            {
                tracing::warn!("coordinator did not stop within 10 s");
            }
        }
        self.set_proc(app, stopped(ProcRole::Coordinator));
        if hosted.is_some() {
            self.log(app, ProcRole::Coordinator, "Stopped hosting.");
        }
        self.refresh_feed(app);
    }

    /// Stop everything on quit.
    pub async fn shutdown(&self, app: &AppHandle) {
        self.discover_stop();
        self.stop_worker(app).await;
        self.host_stop(app).await;
    }

    // ------------------------------------------------------------- mesh data

    fn mesh_target(&self) -> Option<MeshTarget> {
        let inner = self.lock();
        if let (Some(joined), Some(session)) = (&inner.joined, &inner.worker_session) {
            // Our own mesh: talk to it over loopback.
            if let Some(h) = inner
                .hosting
                .as_ref()
                .filter(|h| h.mesh_id == session.mesh_id)
            {
                return Some(MeshTarget {
                    api: local_url(&h.coordinator),
                    display: h.coordinator.clone(),
                    token: h.token.clone(),
                });
            }
            return Some(MeshTarget {
                api: joined.coordinator.clone(),
                display: joined.coordinator.clone(),
                token: session.token.clone(),
            });
        }
        inner.hosting.as_ref().map(|h| MeshTarget {
            api: local_url(&h.coordinator),
            display: h.coordinator.clone(),
            token: h.token.clone(),
        })
    }

    fn client(target: &MeshTarget) -> CoordinatorClient {
        CoordinatorClient::new(target.api.clone(), Some(target.token.clone()))
            .with_timeout(Duration::from_secs(5))
    }

    async fn self_info(&self) -> SelfInfo {
        let facts = self.facts().await;
        let device = self.device().await;
        let (node_id, contributing, name) = {
            let inner = self.lock();
            (
                inner.node_id.clone(),
                inner.worker_session.is_some(),
                inner.settings.device_name.trim().to_string(),
            )
        };
        let port = self.registry.get(&device.profile.port);
        SelfInfo {
            node_id,
            contributing,
            names: [name, facts.hostname.clone()]
                .into_iter()
                .filter(|n| !n.is_empty())
                .collect(),
            port: Some(device.profile.port.clone()),
            torch: port.map(|p| format!("{}{}", p.torch, p.local_suffix)),
        }
    }

    pub async fn mesh_snapshot(&self) -> Result<Option<MeshSnapshot>, String> {
        let Some(target) = self.mesh_target() else {
            return Ok(None);
        };
        let raw = Self::client(&target)
            .mesh()
            .await
            .map_err(|e| format!("The mesh did not answer: {e}"))?;
        let me = self.self_info().await;
        let mut snapshot = mesh::snapshot(&raw, &target.display, &me);
        snapshot.events = self
            .lock()
            .feed
            .as_ref()
            .map(|f| f.events.iter().cloned().collect())
            .unwrap_or_default();
        Ok(Some(snapshot))
    }

    pub async fn runs(&self) -> Result<Vec<RunSummary>, String> {
        let Some(target) = self.mesh_target() else {
            return Ok(Vec::new());
        };
        let client = Self::client(&target);
        let list = client
            .runs()
            .await
            .map_err(|e| format!("Could not read runs: {e}"))?;
        // The list has summaries; per-round metrics need each run's view.
        let details = list.iter().take(12).map(|summary| {
            let client = client.clone();
            let id = summary
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            async move {
                if id.is_empty() {
                    return None;
                }
                client.run(&id).await.ok()
            }
        });
        let details = futures::future::join_all(details).await;
        Ok(list
            .iter()
            .enumerate()
            .map(
                |(i, summary)| match details.get(i).and_then(Option::as_ref) {
                    Some(view) => mesh::run(view),
                    None => mesh::run(summary),
                },
            )
            .collect())
    }

    pub async fn datasets(&self) -> Result<Vec<Dataset>, String> {
        let Some(target) = self.mesh_target() else {
            return Ok(Vec::new());
        };
        let raw = Self::client(&target)
            .datasets()
            .await
            .map_err(|e| format!("Could not read datasets: {e}"))?;
        Ok(mesh::datasets(&raw))
    }

    /// Follow `/events` of the mesh this app is in; restart when that changes.
    fn refresh_feed(&self, app: &AppHandle) {
        let target = self.mesh_target();
        let key = target.as_ref().map(|t| format!("{}|{}", t.api, t.token));
        let mut inner = self.lock();
        if inner.feed.as_ref().map(|f| &f.key) == key.as_ref() {
            return;
        }
        if let Some(old) = inner.feed.take() {
            old.cancel.cancel();
        }
        let (Some(target), Some(key)) = (target, key) else {
            return;
        };
        let cancel = CancellationToken::new();
        inner.feed = Some(Feed {
            key: key.clone(),
            cancel: cancel.clone(),
            events: VecDeque::new(),
        });
        drop(inner);
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            let client = CoordinatorClient::new(target.api.clone(), Some(target.token.clone()));
            let sink = app.clone();
            client
                .follow_events(60, cancel, move |sse| {
                    let Some(event) = mesh::describe(&sse.data) else {
                        return;
                    };
                    let backend = sink.state::<AppBackend>();
                    {
                        let mut inner = backend.lock();
                        let Some(feed) = inner.feed.as_mut().filter(|f| f.key == key) else {
                            return;
                        };
                        // A reconnect replays recent events; keep one copy.
                        if feed
                            .events
                            .iter()
                            .any(|e| e.at == event.at && e.kind == event.kind)
                        {
                            return;
                        }
                        feed.events.push_back(event.clone());
                        while feed.events.len() > FEED_CAP {
                            feed.events.pop_front();
                        }
                    }
                    emit(&sink, AppEvent::MeshEvent { event });
                })
                .await;
        });
    }

    // --------------------------------------------------------------- updates

    pub fn set_update(&self, info: Option<UpdateInfo>) {
        self.lock().update = info;
    }
}

/// `http://<lan>:8000` → `http://127.0.0.1:8000` for a mesh hosted here.
fn local_url(coordinator: &str) -> String {
    match url::Url::parse(coordinator) {
        Ok(mut u) => {
            let _ = u.set_host(Some("127.0.0.1"));
            u.as_str().trim_end_matches('/').to_string()
        }
        Err(_) => coordinator.to_string(),
    }
}

/// `POST /leave {"node_id"}` (worker.py `agent.leave`). Not on the
/// CoordinatorClient allowlist (it is a worker route), so sent directly with
/// the same pooled LAN client and token header. Best effort.
async fn post_leave(coordinator: &str, token: &str, node_id: &str) {
    let result = gm_net::lan_client()
        .post(format!("{}/leave", coordinator.trim_end_matches('/')))
        .header(gm_net::client::TOKEN_HEADER, token)
        .json(&serde_json::json!({ "node_id": node_id }))
        .timeout(Duration::from_secs(3))
        .send()
        .await;
    match result {
        Ok(r) => tracing::info!("POST /leave for {node_id}: {}", r.status()),
        Err(error) => tracing::info!("POST /leave for {node_id} failed: {error}"),
    }
}

struct WorkerJob {
    generation: u64,
    cancel: CancellationToken,
    coordinator: String,
    token: String,
    profile: Profile,
    spec: PortSpec,
}

/// Port Ready (installing it if needed) → worker under the supervisor →
/// forward its output until it stops or the person leaves.
async fn run_worker(app: AppHandle, job: WorkerJob) {
    let backend = app.state::<AppBackend>();
    let fail = |message: String| {
        if backend.worker_current(job.generation) {
            backend.log(&app, ProcRole::Worker, message.clone());
            backend.set_proc_line(&app, ProcRole::Worker, ProcState::Failed, message.clone());
            notice(&app, "error", "Could not start contributing", &message);
        }
    };
    let status = match backend.ensure_port(&app, &job.spec).await {
        Ok(status) => status,
        Err(error) => return fail(format!("Port {} is not ready: {error}", job.spec.id)),
    };
    if job.cancel.is_cancelled() || !backend.worker_current(job.generation) {
        return;
    }
    let Some(python) = status.python_path.clone() else {
        return fail("The port has no Python interpreter.".into());
    };
    let engine = match engine_dir(&app) {
        Ok(dir) => dir,
        Err(error) => return fail(error),
    };
    let mut launch = gm_supervisor::WorkerLaunch::new(python, engine, &job.coordinator, &job.token);
    let name = backend.settings().device_name.trim().to_string();
    launch.name = (!name.is_empty()).then_some(name);
    launch.backend = Some(job.profile.backend);
    launch.gpu_index = job.profile.gpu_index;
    let handle = Supervisor::spawn(gm_supervisor::worker_spec(&launch));
    let kept = {
        let mut inner = backend.lock();
        match inner
            .worker_session
            .as_mut()
            .filter(|s| s.generation == job.generation && !job.cancel.is_cancelled())
        {
            Some(session) => {
                session.handle = Some(handle.clone());
                true
            }
            None => false,
        }
    };
    if !kept {
        // The person left while the port was installing.
        handle.stop().await;
        return;
    }
    backend.log(
        &app,
        ProcRole::Worker,
        format!(
            "Worker started with {} ({})",
            job.spec.id,
            status.torch.clone().unwrap_or_default()
        ),
    );
    forward(app.clone(), handle, job.cancel, Some(job.generation)).await;
}

/// Forward a supervised process's lines and status to the window. For the
/// worker (`generation` set), also read its milestones.
async fn forward(
    app: AppHandle,
    handle: Handle,
    cancel: CancellationToken,
    generation: Option<u64>,
) {
    let backend = app.state::<AppBackend>();
    let role = handle.role();
    let mut events = handle.subscribe();
    let mut failures = FailureTracker::new();
    backend.set_proc(&app, handle.status());
    loop {
        let event = tokio::select! {
            _ = cancel.cancelled() => return,
            event = events.recv() => event,
        };
        let current = generation.is_none_or(|g| backend.worker_current(g));
        if !current {
            return;
        }
        match event {
            Ok(ProcEvent::Line(line)) => {
                backend.log(&app, role, line.line.clone());
                if role == ProcRole::Worker {
                    failures.observe(&line.line, line.stream == gm_supervisor::Stream::Stderr);
                    if let Some(milestone) = gm_supervisor::classify(&line.line) {
                        on_milestone(&app, &backend, milestone);
                    }
                }
            }
            Ok(ProcEvent::Status(status)) => {
                let terminal = matches!(status.state, ProcState::Stopped | ProcState::Failed);
                if status.state == ProcState::Failed {
                    let why = failures
                        .error()
                        .map(str::to_string)
                        .or_else(|| status.last_line.clone())
                        .unwrap_or_else(|| "it kept exiting".into());
                    let title = if role == ProcRole::Worker {
                        "The worker stopped"
                    } else {
                        "The coordinator stopped"
                    };
                    notice(&app, "error", title, &why);
                }
                backend.set_proc(&app, status);
                if terminal {
                    return;
                }
            }
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
            Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
        }
    }
}

fn on_milestone(app: &AppHandle, backend: &AppBackend, milestone: Milestone) {
    match &milestone {
        Milestone::Joined { server, node_id } => {
            backend.lock().node_id = Some(node_id.clone());
            notice(
                app,
                "info",
                "Contributing",
                &format!("Joined {server} as {node_id}."),
            );
        }
        Milestone::TokenRejected => {
            notice(
                app,
                "error",
                "The mesh token was rejected",
                milestone.detail().unwrap_or_default().as_str(),
            );
        }
        Milestone::Disconnected | Milestone::Reconnected => {
            if let Some(detail) = milestone.detail() {
                backend.log(app, ProcRole::Worker, detail);
            }
        }
        _ => {}
    }
}

pub fn emit(app: &AppHandle, event: AppEvent) {
    if let Err(error) = app.emit(EVENT, event) {
        tracing::warn!("emit failed: {error}");
    }
}

pub fn notice(app: &AppHandle, level: &str, title: impl Into<String>, body: &str) {
    emit(
        app,
        AppEvent::Notice {
            level: level.into(),
            title: title.into(),
            body: body.into(),
        },
    );
}

/// Only if the probe thread itself died: enough to show the screen.
fn fallback_facts() -> DeviceFacts {
    DeviceFacts {
        os: if cfg!(windows) {
            Os::Windows
        } else if cfg!(target_os = "macos") {
            Os::Macos
        } else {
            Os::Linux
        },
        os_version: String::new(),
        arch: if cfg!(target_arch = "aarch64") {
            Arch::Arm64
        } else {
            Arch::X86_64
        },
        translated: false,
        hostname: gethostname(),
        cpu_model: None,
        cpu_cores: std::thread::available_parallelism()
            .ok()
            .map(|n| n.get() as u32),
        ram_mb: None,
        gpus: vec![],
        os_name_pretty: None,
    }
}

fn gethostname() -> String {
    std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("COMPUTERNAME"))
        .ok()
        .or_else(|| {
            std::fs::read_to_string("/etc/hostname")
                .ok()
                .map(|s| s.trim().to_string())
        })
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "this-computer".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_host_links_are_read() {
        let invite = gm_net::parse_address(&normalize_invite(
            "gradapp://join?host=10.0.0.5&port=8100&token=a_b-c&name=Lab%20mesh",
        ))
        .unwrap();
        assert_eq!(invite.coordinator, "http://10.0.0.5:8100");
        assert_eq!(invite.token.as_deref(), Some("a_b-c"));
        assert_eq!(invite.mesh_name.as_deref(), Some("Lab mesh"));
        let v1 = "gradapp://join?server=http%3A%2F%2F10.0.0.5%3A8000&token=t&name=x";
        assert_eq!(normalize_invite(v1), v1);
        assert_eq!(normalize_invite(" 192.168.1.2 "), "192.168.1.2");
        assert_eq!(
            gm_net::parse_address(&normalize_invite("gradapp://join?host=box.local"))
                .unwrap()
                .coordinator,
            "http://box.local:8000"
        );
    }

    #[test]
    fn local_url_swaps_host() {
        assert_eq!(
            local_url("http://192.168.1.20:8000"),
            "http://127.0.0.1:8000"
        );
        assert_eq!(local_url("http://box.local:8100/"), "http://127.0.0.1:8100");
    }

    #[test]
    fn mesh_name_written_and_kept() {
        let dir = std::env::temp_dir().join(format!("gradapp-meshstate-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        write_mesh_name(&dir, "Lab").unwrap();
        assert_eq!(read_mesh_state(&dir).unwrap()["mesh_name"], "Lab");
        std::fs::write(
            dir.join("coordinator.json"),
            r#"{"mesh_token":"t","mesh_id":"m","mesh_name":"Old"}"#,
        )
        .unwrap();
        write_mesh_name(&dir, "New").unwrap();
        let state = read_mesh_state(&dir).unwrap();
        assert_eq!(
            (state["mesh_token"].as_str(), state["mesh_name"].as_str()),
            (Some("t"), Some("New"))
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn prefer_maps_every_backend() {
        assert_eq!(prefer_of(None), Prefer::Auto);
        assert_eq!(prefer_of(Some(Backend::Rocm)), Prefer::Rocm);
        assert_eq!(prefer_of(Some(Backend::Cpu)), Prefer::Cpu);
    }
}
