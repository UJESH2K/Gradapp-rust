//! Everything the commands need from the rest of GradApp, in one place.
//!
//! The crates that do the real work (gm-probe, gm-ports, gm-net, gm-update,
//! gm-supervisor) are being written in parallel. Every call into them is a
//! function here marked `// INTEGRATE:` with the call it should become; until
//! then each returns honest placeholder data (a CPU-only machine, the built-in
//! port list, a simulated install) so the app builds, runs and can be clicked
//! through. Commands never talk to those crates directly, so wiring them is a
//! change to this file only.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use gm_types::*;
use tauri::{AppHandle, Emitter, Manager};

use crate::model::*;

pub const EVENT: &str = "gm://event";
const LOG_CAP: usize = 2000;

pub fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

pub fn default_data_dir() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("GradApp")
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

/// Shared state, managed by Tauri (`app.state::<AppBackend>()`).
pub struct AppBackend {
    inner: Mutex<Inner>,
    discovering: AtomicBool,
}

struct Inner {
    settings: Settings,
    joined: Option<Invite>,
    hosting: Option<HostInfo>,
    worker: ProcStatus,
    coordinator: ProcStatus,
    logs: VecDeque<LogEntry>,
    update: Option<UpdateInfo>,
    port_states: Vec<(String, PortState)>,
}

impl AppBackend {
    pub fn load() -> Self {
        let settings = std::fs::read_to_string(config_path())
            .ok()
            .and_then(|raw| serde_json::from_str::<Settings>(&raw).ok())
            .unwrap_or_default();
        Self {
            inner: Mutex::new(Inner {
                settings,
                joined: None,
                hosting: None,
                worker: stopped(ProcRole::Worker),
                coordinator: stopped(ProcRole::Coordinator),
                logs: VecDeque::new(),
                update: None,
                port_states: Vec::new(),
            }),
            discovering: AtomicBool::new(false),
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
        !matches!(
            self.lock().worker.state,
            ProcState::Stopped | ProcState::Failed
        )
    }

    pub fn logs(&self) -> Vec<LogEntry> {
        self.lock().logs.iter().cloned().collect()
    }

    /// Record a line and push it to the UI.
    pub fn log(&self, app: &AppHandle, role: ProcRole, line: impl Into<String>) {
        let line = line.into();
        let source = match role {
            ProcRole::Worker => "worker",
            ProcRole::Coordinator => "coordinator",
            ProcRole::Installer => "installer",
        };
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

    // ---------------------------------------------------------------- device

    pub fn device(&self) -> DeviceReport {
        // INTEGRATE: `let facts = gm_probe::detect();`
        let facts = placeholder_facts();
        let preferred = self.settings().preferred_backend;
        // INTEGRATE: `let profile = gm_probe::select_profile(&facts, &self.port_specs(), preferred);`
        let mut profile = placeholder_profile(&facts);
        if let Some(backend) = preferred {
            if backend != profile.backend {
                profile.warnings.push(format!(
                    "Forced to {} in Settings; the automatic choice was {}.",
                    backend.as_str(),
                    profile.backend.as_str()
                ));
            }
        }
        DeviceReport {
            facts,
            profile,
            overridden: preferred.is_some(),
        }
    }

    // ----------------------------------------------------------------- ports

    pub fn port_specs(&self) -> Vec<PortSpec> {
        // INTEGRATE: `gm_ports::load_manifests(&ports_dir())` (ports/*.toml, bundled as resources).
        builtin_ports()
    }

    pub fn ports(&self) -> Vec<PortEntry> {
        let device = self.device();
        let platform = platform_key(&device.facts);
        let data = PathBuf::from(self.settings().data_dir);
        let states = self.lock().port_states.clone();
        self.port_specs()
            .into_iter()
            .map(|spec| {
                let compatible = spec.platforms.iter().any(|p| p == &platform);
                // INTEGRATE: `gm_ports::status(&spec, &data)` reads port.lock.json + verifies the env.
                let state = states
                    .iter()
                    .find(|(id, _)| id == &spec.id)
                    .map(|(_, s)| *s)
                    .unwrap_or(if compatible {
                        PortState::NotInstalled
                    } else {
                        PortState::Blocked
                    });
                let status = PortStatus {
                    port: spec.id.clone(),
                    state,
                    env_dir: data
                        .join("ports")
                        .join(&spec.id)
                        .to_string_lossy()
                        .into_owned(),
                    python_path: None,
                    torch: (state == PortState::Ready)
                        .then(|| format!("{}{}", spec.torch, spec.local_suffix)),
                    device: None,
                    message: None,
                };
                PortEntry {
                    selected: spec.id == device.profile.port,
                    incompatible_reason: (!compatible).then(|| {
                        format!(
                            "Built for {}; this machine is {platform}.",
                            spec.platforms.join(", ")
                        )
                    }),
                    compatible,
                    status,
                    spec,
                }
            })
            .collect()
    }

    fn set_port_state(&self, port: &str, state: PortState) {
        let mut inner = self.lock();
        inner.port_states.retain(|(id, _)| id != port);
        inner.port_states.push((port.to_string(), state));
    }

    /// Start installing (or repairing) a port; progress arrives as events.
    pub fn install_port(&self, app: &AppHandle, port_id: String) -> Result<PortStatus, String> {
        let entry = self
            .ports()
            .into_iter()
            .find(|entry| entry.spec.id == port_id)
            .ok_or_else(|| format!("Unknown port {port_id}"))?;
        if !entry.compatible {
            return Err(entry
                .incompatible_reason
                .unwrap_or_else(|| "Not for this machine".into()));
        }
        self.set_port_state(&port_id, PortState::Installing);
        let mut status = entry.status.clone();
        status.state = PortState::Installing;
        emit(
            app,
            AppEvent::PortStatus {
                status: status.clone(),
            },
        );

        // INTEGRATE: `gm_ports::install(&spec, &data_dir, |progress| emit PortProgress)` on a
        // blocking task. The simulation below emits the same event sequence.
        let app = app.clone();
        let spec = entry.spec.clone();
        let started = status.clone();
        tauri::async_runtime::spawn(async move {
            let status = started;
            let backend = app.state::<AppBackend>();
            let steps: [(&str, f32, String); 6] = [
                ("uv", 0.05, "uv 0.9 found in the app bundle".into()),
                (
                    "python",
                    0.18,
                    format!("Downloading CPython {} (managed by uv)", spec.python),
                ),
                ("venv", 0.3, format!("Created {}", status.env_dir)),
                (
                    "torch",
                    0.72,
                    format!("Installing torch {}{}", spec.torch, spec.local_suffix),
                ),
                (
                    "packages",
                    0.9,
                    format!("Installing {} packages", spec.packages.len().max(1)),
                ),
                (
                    "verify",
                    1.0,
                    format!("Test kernel ran on {}", spec.backend.as_str()),
                ),
            ];
            for (step, percent, line) in steps {
                tokio::time::sleep(Duration::from_millis(600)).await;
                backend.log(&app, ProcRole::Installer, line.clone());
                emit(
                    &app,
                    AppEvent::PortProgress {
                        port: spec.id.clone(),
                        step: step.into(),
                        percent: Some(percent),
                        line: Some(line),
                    },
                );
            }
            backend.set_port_state(&spec.id, PortState::Ready);
            let mut done = status.clone();
            done.state = PortState::Ready;
            done.torch = Some(format!("{}{}", spec.torch, spec.local_suffix));
            emit(&app, AppEvent::PortStatus { status: done });
        });
        Ok(status)
    }

    // ------------------------------------------------------------- discovery

    pub fn discover_start(&self, app: &AppHandle) {
        if self.discovering.swap(true, Ordering::SeqCst) {
            return;
        }
        // INTEGRATE: `gm_net::discover(DiscoverOptions::default(), tx)` → for each
        // DiscoveredMesh emit MeshFound; sweep progress → ScanProgress. Stop on discover_stop.
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            let total = 254;
            for scanned in (0..=total).step_by(32) {
                if !app.state::<AppBackend>().discovering.load(Ordering::SeqCst) {
                    break;
                }
                emit(
                    &app,
                    AppEvent::ScanProgress {
                        scanned: scanned.min(total),
                        total,
                    },
                );
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
            let hosting = app.state::<AppBackend>().status().hosting;
            if let Some(host) = hosting {
                emit(
                    &app,
                    AppEvent::MeshFound {
                        mesh: DiscoveredMesh {
                            mesh_id: host.mesh_id,
                            hostname: hostname(),
                            coordinator: host.coordinator,
                            web: host.web,
                            version: ENGINE_VERSION.into(),
                            protocol: MESH_PROTOCOL,
                            nodes_active: 1,
                            torch_ready: true,
                            local: true,
                            source: DiscoverySource::Beacon,
                            token: Some(host.token),
                            mesh_name: Some(host.mesh_name),
                            latency_ms: Some(1),
                        },
                    },
                );
            }
            app.state::<AppBackend>()
                .discovering
                .store(false, Ordering::SeqCst);
        });
    }

    pub fn discover_stop(&self) {
        self.discovering.store(false, Ordering::SeqCst);
    }

    // ------------------------------------------------------------- join/leave

    pub fn join(&self, app: &AppHandle, invite: &str) -> Result<Invite, String> {
        // INTEGRATE: `gm_net::parse_invite(invite)` then `gm_net::health(&invite).await` to
        // confirm the coordinator answers and speaks MESH_PROTOCOL.
        let parsed = parse_invite(invite)?;
        let device = self.device();
        if let Some(blocked) = &device.profile.blocked {
            return Err(blocked.clone());
        }
        self.lock().joined = Some(parsed.clone());
        let mut settings = self.settings();
        settings.last_invite = Some(invite.to_string());
        let _ = self.save_settings(settings);

        // INTEGRATE: `supervisor.start(ProcRole::Worker, port_python, ["-m","worker", ...])`;
        // its status changes and stdout lines arrive as Proc and Log events.
        self.set_proc(
            app,
            ProcStatus {
                state: ProcState::Starting,
                ..stopped(ProcRole::Worker)
            },
        );
        self.log(
            app,
            ProcRole::Worker,
            format!(
                "Joining {} on port {}",
                parsed.coordinator, device.profile.port
            ),
        );
        let app2 = app.clone();
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(Duration::from_millis(1200)).await;
            let backend = app2.state::<AppBackend>();
            if backend.lock().joined.is_none() {
                return;
            }
            backend.set_proc(
                &app2,
                ProcStatus {
                    state: ProcState::Running,
                    pid: Some(std::process::id()),
                    last_line: Some("Connected. Waiting for work.".into()),
                    ..stopped(ProcRole::Worker)
                },
            );
            backend.log(&app2, ProcRole::Worker, "Connected. Waiting for work.");
        });
        Ok(parsed)
    }

    pub fn leave(&self, app: &AppHandle) {
        // INTEGRATE: `supervisor.stop(ProcRole::Worker).await`
        self.lock().joined = None;
        self.set_proc(app, stopped(ProcRole::Worker));
        self.log(app, ProcRole::Worker, "Left the mesh.");
    }

    // ------------------------------------------------------------------ host

    pub fn host_start(
        &self,
        app: &AppHandle,
        mesh_name: Option<String>,
    ) -> Result<HostInfo, String> {
        if let Some(existing) = self.lock().hosting.clone() {
            return Ok(existing);
        }
        // INTEGRATE: `supervisor.start(ProcRole::Coordinator, ...)` with the engine's coordinator,
        // then `gm_net::Beacon::start(packet)` and `gm_net::make_invite(...)` for the link.
        let host = hostname();
        let ip = "127.0.0.1"; // INTEGRATE: gm_net::lan_ip()
        let token = format!("{:016x}", (now() * 1000.0) as u64 ^ 0x9e37_79b9_7f4a_7c15);
        let name = mesh_name
            .filter(|n| !n.trim().is_empty())
            .unwrap_or_else(|| format!("{host}'s mesh"));
        let info = HostInfo {
            mesh_id: format!("m-{}", &token[..8]),
            mesh_name: name.clone(),
            coordinator: format!("http://{ip}:{COORDINATOR_PORT}"),
            web: format!("http://{ip}:{WEB_PORT}"),
            invite_link: format!(
                "gradapp://join?host={ip}&port={COORDINATOR_PORT}&token={token}&name={}",
                name.replace(' ', "%20")
            ),
            token,
        };
        self.lock().hosting = Some(info.clone());
        self.set_proc(
            app,
            ProcStatus {
                state: ProcState::Running,
                pid: Some(std::process::id()),
                ..stopped(ProcRole::Coordinator)
            },
        );
        self.log(
            app,
            ProcRole::Coordinator,
            format!("Coordinator listening on {}", info.coordinator),
        );
        Ok(info)
    }

    pub fn host_stop(&self, app: &AppHandle) {
        // INTEGRATE: `supervisor.stop(ProcRole::Coordinator).await` and stop the beacon.
        self.lock().hosting = None;
        self.set_proc(app, stopped(ProcRole::Coordinator));
        self.log(app, ProcRole::Coordinator, "Coordinator stopped.");
    }

    // ------------------------------------------------------------- mesh data

    pub fn mesh_snapshot(&self) -> Option<MeshSnapshot> {
        let (joined, hosting) = {
            let inner = self.lock();
            (inner.joined.clone(), inner.hosting.clone())
        };
        let coordinator = joined
            .as_ref()
            .map(|j| j.coordinator.clone())
            .or(hosting.as_ref().map(|h| h.coordinator.clone()))?;
        // INTEGRATE: `gm_net::client(&coordinator).mesh().await` (GET /mesh + /events); map the
        // coordinator's node JSON into MeshNode.
        let device = self.device();
        let me = MeshNode {
            node_id: "self".into(),
            display_name: self
                .settings()
                .device_name
                .trim()
                .to_string()
                .chars()
                .take(60)
                .collect::<String>()
                .if_empty(hostname()),
            gpu: device
                .profile
                .gpu_name
                .clone()
                .unwrap_or_else(|| "CPU".into()),
            backend: device.profile.backend,
            gpu_memory_mb: None,
            gflops: None,
            throughput_sps: None,
            active: self.contributing(),
            training: false,
            phase: None,
            progress: None,
            completed_rounds: 0,
            tier: "full".into(),
            fitness: 0.5,
            port: Some(device.profile.port.clone()),
            torch: Some(device.profile.torch.clone()),
            os: device.facts.os_name_pretty.clone(),
            is_self: true,
            warnings: device.profile.warnings.clone(),
        };
        Some(MeshSnapshot {
            mesh_id: hosting
                .as_ref()
                .map(|h| h.mesh_id.clone())
                .unwrap_or_else(|| "m-remote".into()),
            mesh_name: hosting
                .map(|h| h.mesh_name)
                .or(joined.and_then(|j| j.mesh_name)),
            coordinator,
            version: ENGINE_VERSION.into(),
            torch_ready: true,
            nodes: vec![me],
            events: vec![],
        })
    }

    pub fn runs(&self) -> Vec<RunSummary> {
        // INTEGRATE: `gm_net::client(&coordinator).runs().await` (GET /runs, /runs/{id}/rounds).
        Vec::new()
    }

    pub fn datasets(&self) -> Vec<Dataset> {
        // INTEGRATE: `gm_net::client(&coordinator).datasets().await` (GET /datasets).
        Vec::new()
    }

    // --------------------------------------------------------------- updates

    pub fn set_update(&self, info: Option<UpdateInfo>) {
        self.lock().update = info;
    }
}

pub fn emit(app: &AppHandle, event: AppEvent) {
    if let Err(error) = app.emit(EVENT, event) {
        tracing::warn!("emit failed: {error}");
    }
}

trait IfEmpty {
    fn if_empty(self, other: String) -> String;
}
impl IfEmpty for String {
    fn if_empty(self, other: String) -> String {
        if self.is_empty() {
            other
        } else {
            self
        }
    }
}

// ---------------------------------------------------------------------------
// Placeholders. Delete each with its INTEGRATE call above.
// ---------------------------------------------------------------------------

pub fn hostname() -> String {
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

fn this_os() -> Os {
    if cfg!(target_os = "windows") {
        Os::Windows
    } else if cfg!(target_os = "macos") {
        Os::Macos
    } else if cfg!(target_os = "linux") {
        Os::Linux
    } else {
        Os::Other
    }
}

fn this_arch() -> Arch {
    if cfg!(target_arch = "x86_64") {
        Arch::X86_64
    } else if cfg!(target_arch = "aarch64") {
        Arch::Arm64
    } else {
        Arch::Other
    }
}

pub fn platform_key(facts: &DeviceFacts) -> String {
    let os = match facts.os {
        Os::Windows => "windows",
        Os::Macos => "macos",
        Os::Linux => "linux",
        Os::Other => "other",
    };
    let arch = match facts.arch {
        Arch::X86_64 => "x86_64",
        Arch::Arm64 => "arm64",
        Arch::Other => "other",
    };
    format!("{os}-{arch}")
}

fn placeholder_facts() -> DeviceFacts {
    DeviceFacts {
        os: this_os(),
        os_version: String::new(),
        arch: this_arch(),
        translated: false,
        hostname: hostname(),
        cpu_model: None,
        cpu_cores: std::thread::available_parallelism()
            .ok()
            .map(|n| n.get() as u32),
        ram_mb: None,
        gpus: vec![],
        os_name_pretty: Some(std::env::consts::OS.to_string()),
    }
}

fn placeholder_profile(facts: &DeviceFacts) -> Profile {
    let mps = facts.os == Os::Macos && facts.arch == Arch::Arm64;
    let (port, backend, label) = if mps {
        ("mps", Backend::Mps, "Apple Silicon · Metal")
    } else {
        ("cpu", Backend::Cpu, "CPU only")
    };
    Profile {
        port: port.into(),
        backend,
        label: label.into(),
        reason: "Placeholder until gm-probe is wired: no GPU detection has run yet.".into(),
        torch: "2.9.1".into(),
        reference: true,
        can_train: mps,
        gpu_index: None,
        gpu_name: None,
        blocked: None,
        fix: None,
        warnings: vec![],
    }
}

#[allow(clippy::too_many_arguments)] // temporary mirror, removed when backend.rs reads gm-ports
fn spec(
    id: &str,
    backend: Backend,
    label: &str,
    index: Option<&str>,
    suffix: &str,
    platforms: &[&str],
    reference: bool,
    experimental: bool,
    min_driver: Option<u32>,
    notes: &str,
) -> PortSpec {
    PortSpec {
        id: id.into(),
        backend,
        label: label.into(),
        python: "3.12".into(),
        torch: "2.9.1".into(),
        torchvision: "0.24.1".into(),
        index_url: index.map(Into::into),
        local_suffix: suffix.into(),
        packages: vec![
            "ultralytics==8.3.228".into(),
            "numpy<3".into(),
            "httpx".into(),
        ],
        platforms: platforms.iter().map(|s| s.to_string()).collect(),
        reference,
        experimental,
        min_driver,
        notes: Some(notes.into()),
    }
}

/// Mirrors ports/*.toml until gm-ports loads them.
fn builtin_ports() -> Vec<PortSpec> {
    vec![
        spec(
            "cuda-cu130",
            Backend::Cuda,
            "NVIDIA · CUDA 13.0",
            Some("https://download.pytorch.org/whl/cu130"),
            "+cu130",
            &["windows-x86_64", "linux-x86_64", "linux-arm64"],
            true,
            false,
            Some(580),
            "Turing (RTX 20) and newer, including Blackwell.",
        ),
        spec(
            "cuda-cu126",
            Backend::Cuda,
            "NVIDIA · CUDA 12.6 (older drivers)",
            Some("https://download.pytorch.org/whl/cu126"),
            "+cu126",
            &["windows-x86_64", "linux-x86_64"],
            false,
            false,
            Some(560),
            "For drivers older than 580 and Pascal/Volta cards.",
        ),
        spec(
            "xpu",
            Backend::Xpu,
            "Intel Arc · XPU",
            Some("https://download.pytorch.org/whl/xpu"),
            "+xpu",
            &["windows-x86_64", "linux-x86_64"],
            true,
            false,
            None,
            "Arc A/B series and Core Ultra with Arc Graphics.",
        ),
        spec(
            "mps",
            Backend::Mps,
            "Apple Silicon · Metal",
            None,
            "",
            &["macos-arm64"],
            true,
            false,
            None,
            "M1 or later on macOS 14+.",
        ),
        spec(
            "rocm",
            Backend::Rocm,
            "AMD · ROCm 6.4",
            Some("https://download.pytorch.org/whl/rocm6.4"),
            "+rocm6.4",
            &["linux-x86_64"],
            false,
            true,
            None,
            "Experimental. Radeon RX 7000 / Instinct on Linux.",
        ),
        spec(
            "cpu",
            Backend::Cpu,
            "CPU only",
            Some("https://download.pytorch.org/whl/cpu"),
            "+cpu",
            &[
                "windows-x86_64",
                "linux-x86_64",
                "linux-arm64",
                "macos-arm64",
                "macos-x86_64",
            ],
            false,
            false,
            None,
            "Joins and is measured; receives no training work.",
        ),
    ]
}

/// Accepts an IP, host[:port], http(s) URL or gradapp://join?host=..&port=..&token=..&name=..
fn parse_invite(raw: &str) -> Result<Invite, String> {
    let text = raw.trim();
    if text.is_empty() {
        return Err("Paste an address or an invite link.".into());
    }
    let mut host = String::new();
    let mut port = COORDINATOR_PORT;
    let mut token = None;
    let mut name = None;
    if let Some(query) = text.strip_prefix("gradapp://join?") {
        for pair in query.split('&') {
            let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
            let v = v.replace("%20", " ");
            match k {
                "host" => host = v,
                "port" => {
                    port = v
                        .parse()
                        .map_err(|_| "The invite's port is not a number.")?
                }
                "token" => token = Some(v),
                "name" => name = Some(v),
                _ => {}
            }
        }
    } else {
        let stripped = text
            .trim_start_matches("http://")
            .trim_start_matches("https://");
        let authority = stripped.split('/').next().unwrap_or("");
        match authority.rsplit_once(':') {
            Some((h, p)) if p.chars().all(|c| c.is_ascii_digit()) && !p.is_empty() => {
                host = h.to_string();
                port = p.parse().unwrap_or(COORDINATOR_PORT);
            }
            _ => host = authority.to_string(),
        }
    }
    if host.is_empty() || host.contains(char::is_whitespace) {
        return Err(format!("“{text}” is not an address GradApp understands."));
    }
    Ok(Invite {
        coordinator: format!("http://{host}:{port}"),
        web: format!("http://{host}:{WEB_PORT}"),
        hostname: host,
        token,
        mesh_name: name,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invite_forms() {
        assert_eq!(
            parse_invite("192.168.1.20").unwrap().coordinator,
            "http://192.168.1.20:8000"
        );
        assert_eq!(
            parse_invite("http://box.local:8001/x").unwrap().coordinator,
            "http://box.local:8001"
        );
        let link = parse_invite("gradapp://join?host=10.0.0.5&port=8000&token=abc&name=Lab%20mesh")
            .unwrap();
        assert_eq!(link.token.as_deref(), Some("abc"));
        assert_eq!(link.mesh_name.as_deref(), Some("Lab mesh"));
        assert!(parse_invite("  ").is_err());
    }
}
