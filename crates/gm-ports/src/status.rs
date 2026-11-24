//! Is a port's env usable, and cleaning up envs no longer needed.

use std::path::Path;
use std::time::Duration;

use gm_types::{PortLock, PortSpec, PortState, PortStatus};
use tokio::process::Command;

use crate::error::{Error, Result};
use crate::paths::{env_dir, env_python, LOCK_FILE, PORT_MARKER, VERIFY_FILE};
use crate::plan::plan;
use crate::verify::VerifyReport;

/// The lock in an env dir, if one is there and parses.
pub fn read_lock(env_dir: &Path) -> Option<PortLock> {
    let text = std::fs::read_to_string(env_dir.join(LOCK_FILE)).ok()?;
    serde_json::from_str(&text).ok()
}

pub(crate) fn read_verify(env_dir: &Path) -> Option<VerifyReport> {
    let text = std::fs::read_to_string(env_dir.join(VERIFY_FILE)).ok()?;
    serde_json::from_str(&text).ok()
}

/// Does the interpreter start? (An env copied from elsewhere, or whose base
/// CPython was deleted, has a python file that will not run.)
pub(crate) async fn python_starts(python: &Path) -> bool {
    if !python.is_file() {
        return false;
    }
    let mut cmd = Command::new(python);
    cmd.args(["-c", "import sys; print(sys.version_info[0])"])
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true);
    matches!(
        tokio::time::timeout(Duration::from_secs(60), cmd.output()).await,
        Ok(Ok(out)) if out.status.success()
    )
}

/// Status of `spec` under `env_root`, for the machine `fingerprint` identifies.
///
/// * `Ready`: `port.lock.json` matches [`plan`] and the env's python starts.
/// * `Stale`: a lock is there but describes something else (other pins, other
///   machine) or cannot be read.
/// * `NotInstalled`: anything else, including an interrupted install (the
///   lock is removed before an install starts and written only at the end).
pub async fn status(spec: &PortSpec, env_root: &Path, fingerprint: &str) -> PortStatus {
    let dir = env_dir(env_root, &spec.id);
    let python = env_python(&dir);
    let mut st = PortStatus {
        port: spec.id.clone(),
        state: PortState::NotInstalled,
        env_dir: dir.to_string_lossy().into_owned(),
        python_path: None,
        torch: None,
        device: None,
        message: None,
    };

    let lock_path = dir.join(LOCK_FILE);
    if !lock_path.is_file() {
        st.message = Some(if dir.exists() {
            "the environment is incomplete (no lock); it will be rebuilt".into()
        } else {
            "not installed".into()
        });
        return st;
    }
    let Some(lock) = read_lock(&dir) else {
        st.state = PortState::Stale;
        st.message = Some("port.lock.json cannot be read; the environment will be rebuilt".into());
        return st;
    };

    let want = plan(spec, fingerprint);
    if lock.port != want.port || lock.fingerprint != want.fingerprint {
        st.state = PortState::Stale;
        st.torch = Some(lock.torch.clone());
        st.message = Some(if lock.machine != want.machine {
            "the environment was built on another machine".into()
        } else if lock.requirements != want.requirements || lock.python != want.python {
            format!(
                "the port changed (installed torch {}, wanted {})",
                lock.torch, want.torch
            )
        } else {
            "the port lock does not match this version of GradApp".into()
        });
        return st;
    }

    if !python_starts(&python).await {
        st.message = Some("the environment's Python will not start; it will be rebuilt".into());
        return st;
    }

    let report = read_verify(&dir);
    st.state = PortState::Ready;
    st.python_path = Some(python.to_string_lossy().into_owned());
    st.torch = report
        .as_ref()
        .and_then(|r| r.torch.clone())
        .or(Some(lock.torch));
    st.device = report.and_then(|r| r.device);
    st
}

/// Remove every port env under `env_root` whose id is not in `keep`. Only
/// directories GradApp created (marker, lock or `pyvenv.cfg` inside) are
/// touched; dot-directories such as the private CPython store are kept.
/// Returns the removed port ids.
pub fn gc(env_root: &Path, keep: &[&str]) -> Result<Vec<String>> {
    let mut removed = Vec::new();
    let entries = match std::fs::read_dir(env_root) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(removed),
        Err(e) => return Err(Error::io(env_root, e)),
    };
    for entry in entries {
        let entry = entry.map_err(|e| Error::io(env_root, e))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let path = entry.path();
        if name.starts_with('.') || keep.contains(&name.as_str()) || !path.is_dir() {
            continue;
        }
        let ours = [PORT_MARKER, LOCK_FILE, "pyvenv.cfg"]
            .iter()
            .any(|f| path.join(f).exists());
        if !ours {
            continue;
        }
        std::fs::remove_dir_all(&path).map_err(|e| Error::io(&path, e))?;
        removed.push(name);
    }
    removed.sort();
    Ok(removed)
}
