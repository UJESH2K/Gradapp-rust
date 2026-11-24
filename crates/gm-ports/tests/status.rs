//! Status and gc against fake envs in a temp dir (no uv, no network).

use std::path::Path;

use gm_ports::*;

fn spec() -> PortSpec {
    Registry::builtin().get("cpu").unwrap().clone()
}

fn write_lock(dir: &Path, lock: &PortLock) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join(LOCK_FILE), serde_json::to_vec(lock).unwrap()).unwrap();
}

/// A "python" that starts (exit 0) or not.
fn fake_python(dir: &Path, works: bool) {
    let py = env_python(dir);
    std::fs::create_dir_all(py.parent().unwrap()).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(
            &py,
            if works {
                "#!/bin/sh\nexit 0\n"
            } else {
                "#!/bin/sh\nexit 1\n"
            },
        )
        .unwrap();
        std::fs::set_permissions(&py, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    #[cfg(windows)]
    {
        // Any real exe that exits 0 for these args is fine; cmd.exe is not.
        let _ = works;
        std::fs::write(&py, b"").unwrap();
    }
}

#[tokio::test]
async fn not_installed_when_nothing_is_there() {
    let root = tempfile::tempdir().unwrap();
    let st = status(&spec(), root.path(), "m").await;
    assert_eq!(st.state, PortState::NotInstalled);
    assert_eq!(st.port, "cpu");
    assert!(st.env_dir.ends_with("cpu"));
}

#[tokio::test]
async fn not_installed_without_lock_even_if_env_exists() {
    // An interrupted install: venv present, lock never written.
    let root = tempfile::tempdir().unwrap();
    let dir = env_dir(root.path(), "cpu");
    fake_python(&dir, true);
    let st = status(&spec(), root.path(), "m").await;
    assert_eq!(st.state, PortState::NotInstalled);
    assert!(st.message.unwrap().contains("incomplete"));
}

#[tokio::test]
async fn stale_when_lock_differs() {
    let root = tempfile::tempdir().unwrap();
    let dir = env_dir(root.path(), "cpu");
    fake_python(&dir, true);

    write_lock(&dir, &plan(&spec(), "another-machine"));
    let st = status(&spec(), root.path(), "m").await;
    assert_eq!(st.state, PortState::Stale);
    assert!(st.message.unwrap().contains("another machine"));

    let mut old = spec();
    old.torch = "2.12.0".into();
    write_lock(&dir, &plan(&old, "m"));
    let st = status(&spec(), root.path(), "m").await;
    assert_eq!(st.state, PortState::Stale);
    assert!(st.message.unwrap().contains("2.12.0+cpu"));

    std::fs::write(dir.join(LOCK_FILE), "{not json").unwrap();
    assert_eq!(
        status(&spec(), root.path(), "m").await.state,
        PortState::Stale
    );
}

#[tokio::test]
async fn lock_for_another_port_is_stale() {
    let root = tempfile::tempdir().unwrap();
    let dir = env_dir(root.path(), "cpu");
    fake_python(&dir, true);
    let other = Registry::builtin().get("xpu").unwrap().clone();
    write_lock(&dir, &plan(&other, "m"));
    assert_eq!(
        status(&spec(), root.path(), "m").await.state,
        PortState::Stale
    );
}

#[cfg(unix)]
#[tokio::test]
async fn ready_only_when_lock_matches_and_python_starts() {
    let root = tempfile::tempdir().unwrap();
    let dir = env_dir(root.path(), "cpu");
    write_lock(&dir, &plan(&spec(), "m"));

    // Lock matches, interpreter missing.
    assert_eq!(
        status(&spec(), root.path(), "m").await.state,
        PortState::NotInstalled
    );

    // Lock matches, interpreter broken (copied env).
    fake_python(&dir, false);
    let st = status(&spec(), root.path(), "m").await;
    assert_eq!(st.state, PortState::NotInstalled);
    assert!(st.message.unwrap().contains("will not start"));

    fake_python(&dir, true);
    let st = status(&spec(), root.path(), "m").await;
    assert_eq!(st.state, PortState::Ready, "{st:?}");
    assert_eq!(st.torch.as_deref(), Some("2.13.0+cpu"));
    assert_eq!(st.python_path.unwrap(), env_python(&dir).to_string_lossy());

    // A verification report supplies the device name and the real torch version.
    std::fs::write(
        dir.join(VERIFY_FILE),
        r#"{"torch":"2.13.0+cpu","device":"CPU","ok":true}"#,
    )
    .unwrap();
    assert_eq!(
        status(&spec(), root.path(), "m").await.device.as_deref(),
        Some("CPU")
    );
}

#[test]
fn gc_removes_only_other_port_envs() {
    let root = tempfile::tempdir().unwrap();
    let r = root.path();
    for id in ["cpu", "xpu", "cuda-cu126"] {
        let d = env_dir(r, id);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join(PORT_MARKER), id).unwrap();
    }
    // A venv without our marker (e.g. half-built) still counts as ours.
    std::fs::create_dir_all(r.join("mps")).unwrap();
    std::fs::write(r.join("mps/pyvenv.cfg"), "").unwrap();
    // Not ours: never touched.
    std::fs::create_dir_all(r.join("notes")).unwrap();
    std::fs::create_dir_all(r.join(PYTHON_DIR).join("cpython-3.12")).unwrap();
    std::fs::write(r.join("stray.txt"), "x").unwrap();

    let removed = gc(r, &["cpu"]).unwrap();
    assert_eq!(removed, vec!["cuda-cu126", "mps", "xpu"]);
    assert!(r.join("cpu").exists());
    assert!(r.join("notes").exists());
    assert!(r.join(PYTHON_DIR).join("cpython-3.12").exists());
    assert!(r.join("stray.txt").exists());

    assert!(gc(&r.join("missing"), &[]).unwrap().is_empty());
}

#[test]
fn env_layout() {
    let p = env_python(Path::new("/x/cpu"));
    if cfg!(windows) {
        assert!(p.ends_with("Scripts/python.exe"));
    } else {
        assert_eq!(p, Path::new("/x/cpu/bin/python"));
    }
    assert!(default_env_root().ends_with("ports"));
    let data = data_dir();
    if cfg!(target_os = "linux") {
        assert!(data.ends_with("gradapp"));
    } else {
        assert!(data.ends_with("GradApp"));
    }
}

#[test]
fn progress_becomes_ui_event() {
    let p = Progress {
        port: "cpu".into(),
        step: "install",
        percent: Some(50.0),
        line: Some("x".into()),
    };
    let ev: gm_types::AppEvent = p.into();
    let v = serde_json::to_value(ev).unwrap();
    assert_eq!(v["kind"], "port_progress");
    assert_eq!(v["step"], "install");
}
