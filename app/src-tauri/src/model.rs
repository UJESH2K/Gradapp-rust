//! Types the app returns to the UI that gm-types does not (yet) define.
//!
//! Each of these crosses into the UI, so by the rule in gm-types they belong
//! there. They live here only until the integrator moves them; the field names
//! are what `app/ui/js/api.js` (and its mock) already expect, so moving them is
//! a pure relocation. See docs/devlog/app.md, "gm-types change requests".

use gm_types::{Backend, DeviceFacts, Invite, PortSpec, PortStatus, ProcStatus, Profile, UpdateInfo};
use serde::{Deserialize, Serialize};

/// `get_device`: the probe's facts and the port it selected.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceReport {
    pub facts: DeviceFacts,
    pub profile: Profile,
    /// The person forced a backend in Settings; `profile` already reflects it.
    pub overridden: bool,
}

/// `get_ports`: one row per known port with its state on this machine.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortEntry {
    pub spec: PortSpec,
    pub status: PortStatus,
    /// `spec.platforms` contains this machine's "<os>-<arch>".
    pub compatible: bool,
    /// The port the probe selected for this machine.
    pub selected: bool,
    /// Why it cannot run here, when `compatible` is false.
    pub incompatible_reason: Option<String>,
}

/// One machine in a mesh, as `mesh_snapshot` reports it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeshNode {
    pub node_id: String,
    pub display_name: String,
    pub gpu: String,
    pub backend: Backend,
    pub gpu_memory_mb: Option<u64>,
    pub gflops: Option<f64>,
    pub throughput_sps: Option<f64>,
    pub active: bool,
    pub training: bool,
    /// "downloading" | "loading" | "training" | "uploading"
    pub phase: Option<String>,
    pub progress: Option<f32>,
    pub completed_rounds: u32,
    pub tier: String,
    pub fitness: f32,
    pub port: Option<String>,
    pub torch: Option<String>,
    pub os: Option<String>,
    pub is_self: bool,
    #[serde(default)]
    pub warnings: Vec<String>,
}

/// A coordinator event (`/events`), shown in the live feed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeshEvent {
    pub at: f64,
    pub kind: String,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeshSnapshot {
    pub mesh_id: String,
    pub mesh_name: Option<String>,
    pub coordinator: String,
    pub version: String,
    pub torch_ready: bool,
    pub nodes: Vec<MeshNode>,
    pub events: Vec<MeshEvent>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoundMetric {
    pub round: u32,
    pub wall_clock_seconds: f64,
    pub straggler_gap_seconds: f64,
    pub aggregation_seconds: f64,
    pub speedup: f64,
    pub workers: u32,
    pub map50: Option<f64>,
    pub loss: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunSummary {
    pub id: String,
    pub name: String,
    /// "planning" | "running" | "waiting" | "completed" | "stopped" | "failed"
    pub status: String,
    pub dataset: String,
    pub base_model: String,
    pub rounds: u32,
    pub current_round: u32,
    pub workers: u32,
    pub speedup: Option<f64>,
    pub map50: Option<f64>,
    pub wall_clock_seconds: Option<f64>,
    pub started_at: Option<f64>,
    pub error: Option<String>,
    #[serde(default)]
    pub round_metrics: Vec<RoundMetric>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Dataset {
    pub id: String,
    pub name: String,
    pub images: u32,
    pub classes: u32,
    pub is_default: bool,
    pub size_mb: Option<f64>,
}

/// This machine hosting a mesh.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostInfo {
    pub mesh_id: String,
    pub mesh_name: String,
    pub coordinator: String,
    pub web: String,
    /// `gradapp://join?...` link, what the QR code encodes.
    pub invite_link: String,
    pub token: String,
}

/// Everything the shell needs on boot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppStatus {
    pub version: String,
    pub worker: ProcStatus,
    pub coordinator: ProcStatus,
    pub joined: Option<Invite>,
    pub hosting: Option<HostInfo>,
    pub update: Option<UpdateInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogEntry {
    pub at: f64,
    /// "worker" | "coordinator" | "installer" | "app"
    pub source: String,
    pub line: String,
}

/// Persisted in `<config>/GradApp/settings.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub device_name: String,
    pub start_at_login: bool,
    pub start_hidden: bool,
    pub close_to_tray: bool,
    pub auto_rejoin: bool,
    pub notifications: bool,
    /// "stable" | "beta"
    pub update_channel: String,
    pub data_dir: String,
    /// None = automatic (gm-probe decides).
    pub preferred_backend: Option<Backend>,
    pub last_invite: Option<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            device_name: String::new(),
            start_at_login: false,
            start_hidden: false,
            close_to_tray: true,
            auto_rejoin: true,
            notifications: true,
            update_channel: "stable".into(),
            data_dir: crate::backend::default_data_dir().to_string_lossy().into_owned(),
            preferred_backend: None,
            last_invite: None,
        }
    }
}
