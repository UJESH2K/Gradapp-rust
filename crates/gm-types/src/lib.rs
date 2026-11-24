//! The shared contract between every GradApp 2 crate and the UI.
//!
//! Everything here is plain data with serde. Field names serialize in
//! `snake_case` so they line up with what the Python engine (hardware.py,
//! the coordinator's `/health`) already emits; the UI receives the same JSON.
//!
//! Rule: a type that crosses a crate boundary or reaches the UI lives here.
//! Crates may add private types of their own, never a second copy of these.

use serde::{Deserialize, Serialize};

/// Product version of the Rust app. Kept in step with the workspace version.
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Wire protocol spoken with the GradMesh coordinator. Mirrors
/// `engine/version.py::PROTOCOL`. Only moves when one side would misread the other.
pub const MESH_PROTOCOL: u32 = 5;

/// Engine release GradApp 2 is built against (engine/version.py `__version__`).
pub const ENGINE_VERSION: &str = "5.1.1";

/// Coordinator HTTP port (FastAPI) and the join/web port.
pub const COORDINATOR_PORT: u16 = 8000;
pub const WEB_PORT: u16 = 3000;
/// UDP port for the GradApp 2 LAN beacon (multicast group [`BEACON_GROUP`]).
pub const BEACON_PORT: u16 = 47_800;
pub const BEACON_GROUP: [u8; 4] = [239, 255, 71, 77];

// ---------------------------------------------------------------------------
// Device facts
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Os {
    Windows,
    Macos,
    Linux,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Arch {
    X86_64,
    Arm64,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Vendor {
    Nvidia,
    Intel,
    Apple,
    Amd,
    Other,
}

/// One GPU as the probe saw it. Matches hardware.py's `Gpu` dataclass.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Gpu {
    pub vendor: Vendor,
    pub name: String,
    pub index: u32,
    /// NVIDIA compute capability, e.g. 8.9 or 12.0.
    pub compute_capability: Option<f32>,
    /// Driver version string as reported by the vendor tool.
    pub driver: Option<String>,
    pub memory_mb: Option<u64>,
    /// Apple GPU core count, when known.
    #[serde(default)]
    pub cores: Option<u32>,
    /// Integrated (shares system memory) vs discrete.
    #[serde(default)]
    pub integrated: bool,
}

/// Everything about this machine that decides which port it runs.
/// Matches hardware.py's `HostFacts` plus a few fields the UI shows.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeviceFacts {
    pub os: Os,
    /// e.g. "14.5" on macOS, "10.0.26100" on Windows, kernel on Linux.
    pub os_version: String,
    pub arch: Arch,
    /// macOS: this process runs under Rosetta translation.
    #[serde(default)]
    pub translated: bool,
    pub hostname: String,
    pub cpu_model: Option<String>,
    pub cpu_cores: Option<u32>,
    pub ram_mb: Option<u64>,
    pub gpus: Vec<Gpu>,
    /// Linux distribution / Windows edition, informational.
    #[serde(default)]
    pub os_name_pretty: Option<String>,
}

// ---------------------------------------------------------------------------
// Backends and profiles
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Backend {
    Cuda,
    Xpu,
    Mps,
    Rocm,
    Cpu,
}

impl Backend {
    pub fn as_str(self) -> &'static str {
        match self {
            Backend::Cuda => "cuda",
            Backend::Xpu => "xpu",
            Backend::Mps => "mps",
            Backend::Rocm => "rocm",
            Backend::Cpu => "cpu",
        }
    }
}

/// Which port this machine should run and why. The Rust equivalent of
/// hardware.py's `Profile`; `port` replaces the requirements file name.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Profile {
    /// Port id, e.g. "cuda-cu130", "xpu", "mps", "cpu".
    pub port: String,
    pub backend: Backend,
    pub label: String,
    pub reason: String,
    /// torch version this port installs.
    pub torch: String,
    /// On the cross-vendor reference stack (results comparable).
    pub reference: bool,
    pub can_train: bool,
    pub gpu_index: Option<u32>,
    pub gpu_name: Option<String>,
    /// Hard stop: the machine cannot run this until the person fixes something.
    pub blocked: Option<String>,
    pub fix: Option<String>,
    pub warnings: Vec<String>,
}

// ---------------------------------------------------------------------------
// Ports
// ---------------------------------------------------------------------------

/// A port: one self-contained, pinned runtime for one backend family.
/// Loaded from `ports/<id>.toml`. See docs/PORTS.md.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PortSpec {
    pub id: String,
    pub backend: Backend,
    pub label: String,
    /// Exact CPython the port runs on, e.g. "3.12". Provisioned by uv, never
    /// the machine's own Python, which is what made v1 break.
    pub python: String,
    pub torch: String,
    pub torchvision: String,
    /// Extra package index for the torch wheels (PyTorch's per-backend index).
    pub index_url: Option<String>,
    /// Exact local-version suffix on the wheels, e.g. "+cu130"; empty on mps.
    #[serde(default)]
    pub local_suffix: String,
    /// Everything else, as pip requirement strings.
    #[serde(default)]
    pub packages: Vec<String>,
    /// Which OS/arch pairs this port can run on, e.g. ["windows-x86_64", "linux-x86_64"].
    pub platforms: Vec<String>,
    #[serde(default)]
    pub reference: bool,
    #[serde(default)]
    pub experimental: bool,
    /// Minimum driver major version (NVIDIA) for this port, if any.
    #[serde(default)]
    pub min_driver: Option<u32>,
    #[serde(default)]
    pub notes: Option<String>,
}

/// Resolved, frozen install plan for a port on this machine. Written next to
/// the environment as `port.lock.json`; a mismatch triggers a rebuild.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PortLock {
    pub port: String,
    pub python: String,
    pub torch: String,
    pub torchvision: String,
    pub index_url: Option<String>,
    pub requirements: Vec<String>,
    /// sha256 over the requirement set + python + machine fingerprint.
    pub fingerprint: String,
    pub machine: String,
    pub created_unix: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PortState {
    NotInstalled,
    Installing,
    Ready,
    Stale,
    Failed,
    Blocked,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PortStatus {
    pub port: String,
    pub state: PortState,
    pub env_dir: String,
    pub python_path: Option<String>,
    pub torch: Option<String>,
    pub device: Option<String>,
    pub message: Option<String>,
}

// ---------------------------------------------------------------------------
// Mesh
// ---------------------------------------------------------------------------

/// `GET /health` on a coordinator (unauthenticated).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MeshHealth {
    pub status: String,
    pub version: String,
    pub protocol: u32,
    pub mesh_id: String,
    #[serde(default)]
    pub torch_ready: bool,
    #[serde(default)]
    pub nodes_active: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiscoverySource {
    Beacon,
    Mdns,
    Sweep,
    Manual,
}

/// A mesh found on the network.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DiscoveredMesh {
    pub mesh_id: String,
    pub hostname: String,
    pub coordinator: String,
    pub web: String,
    pub version: String,
    pub protocol: u32,
    pub nodes_active: u32,
    pub torch_ready: bool,
    /// Runs on this machine.
    pub local: bool,
    pub source: DiscoverySource,
    pub token: Option<String>,
    pub mesh_name: Option<String>,
    /// Round-trip of the /health probe in milliseconds.
    pub latency_ms: Option<u32>,
}

/// What the GradApp 2 beacon multicasts every few seconds on the LAN.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BeaconPacket {
    /// Always "gradmesh".
    pub magic: String,
    pub mesh_id: String,
    pub mesh_name: Option<String>,
    pub coordinator_port: u16,
    pub web_port: u16,
    pub version: String,
    pub protocol: u32,
    pub nodes_active: u32,
}

/// Parsed from anything a person pastes: IP, hostname, URL or gradapp:// link.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Invite {
    pub hostname: String,
    pub coordinator: String,
    pub web: String,
    pub token: Option<String>,
    pub mesh_name: Option<String>,
}

/// One coordinator event (`GET /events`), in words, for the live feed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MeshEvent {
    /// Unix seconds.
    pub at: f64,
    /// The coordinator's kind, e.g. "node.joined", "round.completed".
    pub kind: String,
    pub text: String,
}

// ---------------------------------------------------------------------------
// Updates
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UpdateInfo {
    pub available: bool,
    pub current: String,
    pub latest: Option<String>,
    pub notes: Option<String>,
    pub url: Option<String>,
    pub published_at: Option<String>,
    /// The Tauri signed updater can install it in place (vs. link only).
    pub installable: bool,
    /// Why the check could not decide (offline, rate-limited, ...); `None`
    /// for a clean "up to date" or "update available".
    #[serde(default)]
    pub reason: Option<String>,
    /// `latest.json` of the offered release, when installable. The Beta
    /// channel points the Tauri updater here (the fixed
    /// `/releases/latest/download/latest.json` never serves pre-releases).
    #[serde(default)]
    pub manifest_url: Option<String>,
}

// ---------------------------------------------------------------------------
// Processes
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcRole {
    Worker,
    Coordinator,
    Installer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcState {
    Stopped,
    Starting,
    Running,
    Restarting,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProcStatus {
    pub role: ProcRole,
    pub state: ProcState,
    pub pid: Option<u32>,
    pub restarts: u32,
    pub last_line: Option<String>,
    pub exit_code: Option<i32>,
}

// ---------------------------------------------------------------------------
// Events pushed to the UI (Tauri event "gm://event")
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AppEvent {
    MeshFound {
        mesh: DiscoveredMesh,
    },
    ScanProgress {
        scanned: u32,
        total: u32,
    },
    PortProgress {
        port: String,
        step: String,
        percent: Option<f32>,
        line: Option<String>,
    },
    PortStatus {
        status: PortStatus,
    },
    Proc {
        status: ProcStatus,
    },
    Log {
        role: ProcRole,
        line: String,
    },
    Update {
        info: UpdateInfo,
    },
    UpdateProgress {
        downloaded: u64,
        total: Option<u64>,
    },
    Notice {
        level: String,
        title: String,
        body: String,
    },
    /// A new event on the mesh this app is in (joined or hosting).
    MeshEvent {
        event: MeshEvent,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn health_parses_coordinator_json() {
        let raw = r#"{"status":"ok","version":"5.1.1","protocol":5,"mesh_id":"m-1",
            "torch_ready":true,"nodes_active":3,"mdns":{"name":"gradmesh.local"}}"#;
        let h: MeshHealth = serde_json::from_str(raw).unwrap();
        assert_eq!(h.nodes_active, 3);
        assert_eq!(h.protocol, MESH_PROTOCOL);
    }

    #[test]
    fn event_is_tagged() {
        let e = AppEvent::ScanProgress {
            scanned: 1,
            total: 254,
        };
        let v = serde_json::to_value(&e).unwrap();
        assert_eq!(v["kind"], "scan_progress");
    }

    #[test]
    fn backend_strings() {
        assert_eq!(serde_json::to_string(&Backend::Cuda).unwrap(), "\"cuda\"");
        assert_eq!(Backend::Mps.as_str(), "mps");
    }
}
