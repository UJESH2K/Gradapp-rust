//! gm-ports: **ports**, the pinned per-backend runtimes GradApp trains on.
//!
//! A port is one self-contained PyTorch runtime for one backend family
//! (`cuda-cu130`, `xpu`, `mps`, ...). Its manifest (`ports/<id>.toml`, embedded
//! into this crate at compile time) names the exact CPython it runs on, the
//! exact torch/torchvision build and every other package. GradApp provisions
//! that CPython privately with [uv](https://github.com/astral-sh/uv), so the
//! machine's own Python is never used: the failure that dominated GradApp 1.
//!
//! The flow an app follows:
//!
//! 1. [`Registry::builtin`] → the port manifests; gm-probe picks a port id.
//! 2. [`plan`] → the [`PortLock`] this machine should end up with.
//! 3. [`status`] → `Ready` / `Stale` / `NotInstalled` against that plan.
//! 4. [`uv::ensure`] then [`provision`] → builds `<env_root>/<port-id>`,
//!    verifies a real kernel launch, writes `port.lock.json`.
//! 5. [`gc`] → removes envs of ports this machine no longer uses.
//!
//! See docs/PORTS.md.

mod error;
mod paths;
mod plan;
mod provision;
mod registry;
mod spawn;
mod status;
pub mod uv;
mod verify;

pub use error::{Error, Result};
pub use paths::{
    data_dir, default_env_root, env_dir, env_python, python_install_dir, LOCK_FILE, PORT_MARKER,
    PYTHON_DIR, VERIFY_FILE,
};
pub use plan::{
    local_machine_fingerprint, lock_fingerprint, machine_fingerprint, plan, requirements,
};
pub use provision::{
    disk_needed_bytes, provision, provision_with, Progress, ProvisionOptions, VerifyMode,
};
pub use registry::{
    current_platform, uv_python_platform, validate_spec, Registry, BUILTIN_MANIFESTS, PLATFORMS,
    PORT_IDS, REFERENCE_TORCH, REFERENCE_TORCHVISION, REFERENCE_ULTRALYTICS,
};
pub use status::{gc, read_lock, status};
pub use verify::{interpret as interpret_verify, VerifyReport, VERIFY_SNIPPET};

pub use gm_types::{PortLock, PortSpec, PortState, PortStatus};
