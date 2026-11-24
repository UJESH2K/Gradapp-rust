//! Regression tests from the QA review (docs/devlog/qa-review.md).

use gm_ports::uv::Uv;
use gm_ports::{current_platform, provision_with, Error, ProvisionOptions, Registry, VerifyMode};

/// A platform string that is not this machine's.
fn elsewhere() -> &'static str {
    if current_platform() == Some("macos-arm64") {
        "linux-x86_64"
    } else {
        "macos-arm64"
    }
}

#[tokio::test]
async fn provision_refuses_a_port_with_no_build_for_this_platform() {
    let mut spec = Registry::builtin().require("cpu").unwrap().clone();
    spec.platforms = vec![elsewhere().to_string()];
    let root = tempfile::tempdir().unwrap();
    // uv is never reached: the check comes before anything runs or is deleted.
    let uv = Uv::new(root.path().join("no-such-uv"));
    let opts = ProvisionOptions {
        verify: VerifyMode::Skip,
        retries: 0,
        ..Default::default()
    };
    let err = provision_with(&spec, root.path(), &uv, &opts, |_| {})
        .await
        .unwrap_err();
    assert!(
        matches!(err, Error::UnsupportedPlatform { .. }),
        "wanted UnsupportedPlatform, got {err}"
    );
    assert!(err.to_string().contains("cpu"));
    assert!(!root.path().join("cpu").exists());
}

#[test]
fn require_on_checks_platforms() {
    let reg = Registry::builtin();
    assert!(reg.require_on("cpu", "linux-arm64").is_ok());
    assert!(matches!(
        reg.require_on("cuda-cu126", "linux-arm64"),
        Err(Error::UnsupportedPlatform { .. })
    ));
    assert!(matches!(
        reg.require_on("nope", "linux-x86_64"),
        Err(Error::UnknownPort(_))
    ));
}

#[test]
fn uv_version_parsing() {
    use gm_ports::uv::parse_uv_version;
    assert_eq!(
        parse_uv_version("uv 0.11.32 (abc123 2026-10-01)"),
        Some((0, 11, 32))
    );
    assert_eq!(parse_uv_version("uv 0.8.0"), Some((0, 8, 0)));
    assert_eq!(
        parse_uv_version("uv 0.4.30 (Homebrew 2024-11-05)"),
        Some((0, 4, 30))
    );
    assert_eq!(parse_uv_version("ruff 0.1.0"), None);
    assert_eq!(parse_uv_version(""), None);
}

/// A uv older than 0.8 has no `python install --no-bin` / `--no-registry`,
/// so every provision would fail. `ensure` must not settle for it.
#[cfg(unix)]
#[tokio::test]
async fn old_uv_is_not_usable() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let fake = |name: &str, out: &str| {
        let p = dir.path().join(name);
        std::fs::write(&p, format!("#!/bin/sh\necho '{out}'\n")).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        Uv::new(p)
    };
    assert!(!gm_ports::uv::is_usable(&fake("old", "uv 0.4.30 (Homebrew 2024-11-05)")).await);
    assert!(!gm_ports::uv::is_usable(&fake("junk", "not uv")).await);
    assert!(gm_ports::uv::is_usable(&fake("new", "uv 0.11.32 (abc 2026-10-01)")).await);
    assert!(!gm_ports::uv::is_usable(&Uv::new(dir.path().join("missing"))).await);
}
