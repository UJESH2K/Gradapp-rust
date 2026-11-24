//! Regression tests from the QA review (docs/devlog/qa-review.md).

#[cfg(unix)]
use std::time::{Duration, Instant};

use gm_supervisor::{worker_spec, WorkerLaunch};
#[cfg(unix)]
use gm_supervisor::{ProcSpec, Supervisor};
#[cfg(unix)]
use gm_types::ProcRole;

/// The worker's token travels in the environment, not argv; a `{:?}` of the
/// launch or the spec (logs, panics, crash reports) must not print it either.
#[test]
fn debug_output_never_contains_the_token() {
    let launch = WorkerLaunch::new("py", "engine", "http://10.0.0.2:8000", "s3cret-mesh-token");
    let spec = worker_spec(&launch);
    for text in [
        format!("{launch:?}"),
        format!("{spec:?}"),
        format!("{spec:#?}"),
    ] {
        assert!(
            !text.contains("s3cret-mesh-token"),
            "token leaked in Debug: {text}"
        );
    }
    // Still useful for debugging.
    assert!(format!("{spec:?}").contains("GRADMESH_TOKEN"));
    assert!(format!("{launch:?}").contains("10.0.0.2"));
}

#[cfg(unix)]
fn alive(pid: i32) -> bool {
    // A zombie no longer runs; only count live processes.
    if let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        let state = stat.rsplit(')').next().unwrap_or("").trim_start();
        return !state.starts_with('Z') && !state.starts_with('X');
    }
    // SAFETY: signal 0 only checks existence.
    unsafe { libc::kill(pid, 0) == 0 }
}

/// When the app's runtime shuts down without an explicit `stop()` (window
/// closed, panic unwinding, task aborted), the supervision task is dropped.
/// `kill_on_drop` only reaches the direct child; the process group must go
/// too, or the worker's dataloader processes / uvicorn children keep running.
#[cfg(unix)]
#[test]
fn dropping_the_runtime_kills_the_whole_tree() {
    let dir = tempfile::tempdir().unwrap();
    let pidfile = dir.path().join("grandchild.pid");
    let script = format!(
        "sleep 300 & echo $! > '{}'; echo started; wait",
        pidfile.display()
    );
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .unwrap();
    let handle = rt.block_on(async {
        Supervisor::spawn(ProcSpec::new(ProcRole::Worker, "sh").arg("-c").arg(script))
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    let pid: i32 = loop {
        if let Some(pid) = std::fs::read_to_string(&pidfile)
            .ok()
            .and_then(|s| s.trim().parse().ok())
        {
            break pid;
        }
        assert!(Instant::now() < deadline, "grandchild never started");
        std::thread::sleep(Duration::from_millis(20));
    };
    assert!(alive(pid));
    // Keep the handle alive: only the runtime goes away.
    drop(rt);
    let deadline = Instant::now() + Duration::from_secs(5);
    while alive(pid) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    let survived = alive(pid);
    if survived {
        // SAFETY: clean up the leaked process before failing.
        unsafe { libc::kill(pid, libc::SIGKILL) };
    }
    drop(handle);
    assert!(
        !survived,
        "grandchild {pid} outlived the supervisor's runtime"
    );
}
