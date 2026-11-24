//! Where GradApp keeps its data, and the layout of a port env.
//!
//! ```text
//! <data>/                      %LOCALAPPDATA%\GradApp | ~/Library/Application Support/GradApp | ~/.local/share/gradapp
//!   bin/uv(.exe)               uv, when GradApp downloaded it
//!   ports/                     the env root
//!     .python/                 uv-managed CPython builds (private, never on PATH)
//!     cuda-cu130/              one venv per port
//!       .gradapp-port          "this directory belongs to GradApp" (gc only touches these)
//!       port.lock.json         written last; present and matching == Ready
//!       port.verify.json       the last verification report
//! ```

use std::path::{Path, PathBuf};

pub const LOCK_FILE: &str = "port.lock.json";
pub const VERIFY_FILE: &str = "port.verify.json";
pub const PORT_MARKER: &str = ".gradapp-port";
pub const PYTHON_DIR: &str = ".python";

/// The per-user app data directory.
pub fn data_dir() -> PathBuf {
    let base = dirs::data_local_dir().unwrap_or_else(std::env::temp_dir);
    if cfg!(any(target_os = "windows", target_os = "macos")) {
        base.join("GradApp")
    } else {
        base.join("gradapp")
    }
}

/// Where port envs live by default: `<data>/ports`.
pub fn default_env_root() -> PathBuf {
    data_dir().join("ports")
}

pub fn env_dir(env_root: &Path, port_id: &str) -> PathBuf {
    env_root.join(port_id)
}

/// The interpreter inside a venv.
pub fn env_python(env_dir: &Path) -> PathBuf {
    if cfg!(windows) {
        env_dir.join("Scripts").join("python.exe")
    } else {
        env_dir.join("bin").join("python")
    }
}

/// uv's `UV_PYTHON_INSTALL_DIR` for this env root.
pub fn python_install_dir(env_root: &Path) -> PathBuf {
    env_root.join(PYTHON_DIR)
}
