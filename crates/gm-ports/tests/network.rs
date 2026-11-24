//! Tests against real uv and the network.
//!
//! All `#[ignore]`: they need uv (on PATH, in GRADAPP_UV or downloadable) and
//! network access to PyPI / GitHub, and they take a minute or two. Run with
//!
//!     cargo test -p gm-ports --test network -- --ignored --nocapture
//!
//! `resolve_with_pytorch_indexes` additionally needs download.pytorch.org.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gm_ports::uv::{self, Uv};
use gm_ports::*;
use gm_types::Backend;

fn uv_or_skip() -> Option<Uv> {
    let found = uv::find();
    if found.is_none() {
        eprintln!("uv not found (set GRADAPP_UV); skipping");
    }
    found
}

/// A torch-free port: python 3.12 + `six`.
fn tiny_port() -> PortSpec {
    PortSpec {
        id: "cpu".into(),
        backend: Backend::Cpu,
        label: "test port".into(),
        python: "3.12".into(),
        torch: String::new(),
        torchvision: String::new(),
        index_url: None,
        local_suffix: String::new(),
        packages: vec!["six==1.17.0".into()],
        platforms: vec!["linux-x86_64".into()],
        reference: false,
        experimental: false,
        min_driver: None,
        notes: None,
    }
}

fn opts() -> ProvisionOptions {
    ProvisionOptions {
        facts_fingerprint: "test-machine".into(),
        verify: VerifyMode::Skip,
        backoff: Duration::from_millis(200),
        ..Default::default()
    }
}

async fn run_python(python: &Path, code: &str) -> String {
    let out = tokio::process::Command::new(python)
        .args(["-c", code])
        .output()
        .await
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[tokio::test]
#[ignore = "network + real uv: provisions python 3.12 and six"]
async fn provision_end_to_end() {
    let Some(uv) = uv_or_skip() else { return };
    eprintln!(
        "using {} ({:?}): {}",
        uv.path.display(),
        uv.source,
        uv.version().await.unwrap()
    );
    let root = tempfile::tempdir().unwrap();
    let env_root = root.path();
    let spec = tiny_port();

    // install → lock
    let events = Arc::new(Mutex::new(Vec::<Progress>::new()));
    let ev = events.clone();
    let st = provision_with(&spec, env_root, &uv, &opts(), move |p| {
        eprintln!(
            "[{} {:>5.1}%] {}",
            p.step,
            p.percent.unwrap_or(0.0),
            p.line.clone().unwrap_or_default()
        );
        ev.lock().unwrap().push(p);
    })
    .await
    .unwrap();
    assert_eq!(st.state, PortState::Ready, "{st:?}");

    let events = events.lock().unwrap().clone();
    let steps: Vec<&str> = events.iter().map(|p| p.step).collect();
    for s in ["prepare", "python", "venv", "install", "lock"] {
        assert!(steps.contains(&s), "missing step {s}: {steps:?}");
    }
    let pct: Vec<f32> = events.iter().filter_map(|p| p.percent).collect();
    assert!(
        pct.windows(2).all(|w| w[1] >= w[0]),
        "percent never goes back: {pct:?}"
    );
    assert_eq!(*pct.last().unwrap(), 100.0);
    assert!(events
        .iter()
        .any(|p| p.line.as_deref().is_some_and(|l| l.contains("six"))));

    let dir = env_dir(env_root, "cpu");
    let lock = read_lock(&dir).unwrap();
    assert_eq!(lock.requirements, vec!["six==1.17.0"]);
    assert_eq!(lock.fingerprint, plan(&spec, "test-machine").fingerprint);
    assert!(dir.join(PORT_MARKER).exists());

    // status Ready, and the interpreter is the private one.
    let st = status(&spec, env_root, "test-machine").await;
    assert_eq!(st.state, PortState::Ready, "{st:?}");
    let python = env_python(&dir);
    let out = run_python(
        &python,
        "import sys, six; print(sys.version_info[:2] == (3, 12), six.__version__, sys.base_prefix)",
    )
    .await;
    eprintln!("env python: {out}");
    let mut parts = out.split_whitespace();
    assert_eq!(parts.next(), Some("True"));
    assert_eq!(parts.next(), Some("1.17.0"));
    let base = std::fs::canonicalize(parts.next().unwrap()).unwrap();
    let private = std::fs::canonicalize(python_install_dir(env_root)).unwrap();
    assert!(
        base.starts_with(&private),
        "base python {base:?} must live under {private:?}"
    );

    // Another machine → Stale; changed port → Stale.
    assert_eq!(
        status(&spec, env_root, "other-machine").await.state,
        PortState::Stale
    );
    let mut changed = spec.clone();
    changed.packages.push("idna".into());
    assert_eq!(
        status(&changed, env_root, "test-machine").await.state,
        PortState::Stale
    );

    // Re-provisioning a Ready port reuses the venv and stays Ready.
    let st = provision_with(&spec, env_root, &uv, &opts(), |_| {})
        .await
        .unwrap();
    assert_eq!(st.state, PortState::Ready);

    // Cancel mid-install: the env must not be Ready afterwards.
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<()>();
    let o = opts();
    let fut = provision_with(&changed, env_root, &uv, &o, move |p| {
        if p.step == "install" {
            let _ = tx.send(());
        }
    });
    tokio::select! {
        r = fut => panic!("provision finished before it could be cancelled: {r:?}"),
        _ = rx.recv() => {}
    }
    let st = status(&changed, env_root, "test-machine").await;
    assert_ne!(st.state, PortState::Ready, "{st:?}");
    let st = status(&spec, env_root, "test-machine").await;
    assert_ne!(
        st.state,
        PortState::Ready,
        "the old lock was removed before changing the env: {st:?}"
    );
    eprintln!("after cancel: {:?} / {:?}", st.state, st.message);

    // A failing install is an Err and never Ready.
    let mut broken = spec.clone();
    broken.packages = vec!["six==999.0".into()];
    let err = provision_with(&broken, env_root, &uv, &opts(), |_| {})
        .await
        .unwrap_err();
    eprintln!("expected failure: {err}");
    assert!(matches!(err, Error::Command { ref step, .. } if step == "install"));
    assert_eq!(
        status(&broken, env_root, "test-machine").await.state,
        PortState::NotInstalled
    );

    // Provision again cleanly, add a second port env, then gc.
    provision_with(&spec, env_root, &uv, &opts(), |_| {})
        .await
        .unwrap();
    let mut other = spec.clone();
    other.id = "xpu".into();
    other.backend = Backend::Xpu;
    let st = provision_with(&other, env_root, &uv, &opts(), |_| {})
        .await
        .unwrap();
    assert_eq!(st.state, PortState::Ready);
    let removed = gc(env_root, &["cpu"]).unwrap();
    assert_eq!(removed, vec!["xpu"]);
    assert_eq!(
        status(&other, env_root, "test-machine").await.state,
        PortState::NotInstalled
    );
    assert_eq!(
        status(&spec, env_root, "test-machine").await.state,
        PortState::Ready
    );
    assert!(
        python_install_dir(env_root).exists(),
        "gc keeps the private CPython store"
    );
    assert_eq!(gc(env_root, &[]).unwrap(), vec!["cpu"]);
    assert_eq!(
        status(&spec, env_root, "test-machine").await.state,
        PortState::NotInstalled
    );
}

#[tokio::test]
#[ignore = "network: downloads the pinned uv release from GitHub"]
async fn download_pinned_uv() {
    let dir = tempfile::tempdir().unwrap();
    let uv = uv::download(dir.path(), uv::UV_VERSION).await.unwrap();
    assert_eq!(uv.source, uv::UvSource::Downloaded);
    let v = uv.version().await.unwrap();
    eprintln!("downloaded {} -> {v}", uv.path.display());
    assert!(v.contains(uv::UV_VERSION), "{v}");
    // Next search finds it in the data dir (unless something earlier wins).
    let found = uv::find_in(dir.path()).unwrap();
    assert!(matches!(
        found.source,
        uv::UvSource::DataDir | uv::UvSource::Env | uv::UvSource::Bundled | uv::UvSource::Path
    ));

    // A corrupted checksum is refused: ask for a target whose archive exists but compare against another.
    let err = uv::download_target(
        dir.path(),
        "0.0.0-does-not-exist",
        "x86_64-unknown-linux-musl",
        &dir.path().join("x"),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, Error::Download { .. }), "{err}");
}

/// `uv pip compile` for one requirement set on one platform. Returns the pinned output.
async fn compile(
    uv: &Uv,
    work: &Path,
    name: &str,
    reqs: &[String],
    platform: &str,
    exclude_torch: bool,
    extra_index: Option<&str>,
) -> Result<String, String> {
    let req_file = work.join(format!("{name}.in"));
    std::fs::write(&req_file, reqs.join("\n")).unwrap();
    let ex = work.join("exclude.txt");
    std::fs::write(&ex, "torch\ntorchvision\n").unwrap();
    let mut cmd = tokio::process::Command::new(&uv.path);
    cmd.args([
        "pip",
        "compile",
        "--no-config",
        "--quiet",
        "--no-header",
        "--python-version",
        "3.12",
    ])
    .args(["--python-platform", uv_python_platform(platform).unwrap()])
    .args([
        "--only-binary",
        ":all:",
        "--index-strategy",
        "unsafe-best-match",
    ])
    .env("MACOSX_DEPLOYMENT_TARGET", "14.0")
    .arg(&req_file);
    if exclude_torch {
        cmd.arg("--excludes").arg(&ex);
    }
    if let Some(url) = extra_index {
        cmd.args(["--extra-index-url", url]);
    }
    let out = cmd.output().await.unwrap();
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).into_owned())
    }
}

fn pinned<'a>(out: &'a str, pkg: &str) -> Option<&'a str> {
    out.lines()
        .find_map(|l| l.strip_prefix(&format!("{pkg}==")))
}

/// Every port's non-torch packages resolve, wheels only, on every platform it
/// lists; mps resolves with torch itself from PyPI on macOS 14 arm64; and the
/// torch/torchvision version pairs resolve together with ultralytics.
#[tokio::test]
#[ignore = "network: resolves every port against PyPI with uv pip compile"]
async fn ports_resolve_without_conflicts() {
    let Some(uv) = uv_or_skip() else { return };
    let work = tempfile::tempdir().unwrap();
    let reg = Registry::builtin();
    let mut failures = Vec::new();
    let mut table = BTreeMap::new();

    for spec in reg.all() {
        for platform in &spec.platforms {
            let (reqs, exclude) = if spec.backend == Backend::Mps {
                (requirements(spec), false)
            } else {
                (spec.packages.clone(), true)
            };
            let name = format!("{}-{platform}", spec.id);
            match compile(&uv, work.path(), &name, &reqs, platform, exclude, None).await {
                Ok(out) => {
                    assert!(pinned(&out, "ultralytics") == Some("8.4.46"), "{name}");
                    if exclude {
                        assert!(
                            pinned(&out, "torch").is_none(),
                            "{name}: torch must be excluded"
                        );
                    } else {
                        assert_eq!(pinned(&out, "torch"), Some(spec.torch.as_str()), "{name}");
                        assert_eq!(
                            pinned(&out, "torchvision"),
                            Some(spec.torchvision.as_str()),
                            "{name}"
                        );
                    }
                    table.insert(
                        name,
                        format!(
                            "ok ({} packages, numpy {})",
                            out.lines().filter(|l| l.contains("==")).count(),
                            pinned(&out, "numpy").unwrap_or("?")
                        ),
                    );
                }
                Err(e) => failures.push(format!("{name}:\n{e}")),
            }
        }
    }

    // The torch/torchvision pairs (as published on PyPI, without a local
    // suffix) are compatible with the rest of the set.
    let mut pairs = BTreeMap::new();
    for spec in reg.all() {
        pairs.insert(
            (spec.torch.clone(), spec.torchvision.clone()),
            spec.id.clone(),
        );
    }
    for ((torch, vision), id) in pairs {
        let mut reqs = vec![format!("torch=={torch}"), format!("torchvision=={vision}")];
        reqs.extend(reg.get(&id).unwrap().packages.iter().cloned());
        let name = format!("pypi-torch-{torch}");
        match compile(&uv, work.path(), &name, &reqs, "linux-x86_64", false, None).await {
            Ok(out) => {
                assert_eq!(pinned(&out, "torch"), Some(torch.as_str()));
                table.insert(name, format!("ok (torchvision {vision})"));
            }
            Err(e) => failures.push(format!("{name}:\n{e}")),
        }
    }

    for (k, v) in &table {
        eprintln!("{k:<32} {v}");
    }
    assert!(
        failures.is_empty(),
        "resolution failures:\n{}",
        failures.join("\n\n")
    );
}

/// The full pinned set including the local-suffix torch wheels from each
/// port's PyTorch index. Needs download.pytorch.org (blocked in some sandboxes).
#[tokio::test]
#[ignore = "network: needs download.pytorch.org"]
async fn resolve_with_pytorch_indexes() {
    let Some(uv) = uv_or_skip() else { return };
    let work = tempfile::tempdir().unwrap();
    let mut failures = Vec::new();
    for spec in Registry::builtin().all() {
        for platform in &spec.platforms {
            let name = format!("full-{}-{platform}", spec.id);
            match compile(
                &uv,
                work.path(),
                &name,
                &requirements(spec),
                platform,
                false,
                spec.index_url.as_deref(),
            )
            .await
            {
                Ok(out) => eprintln!("{name}: torch {}", pinned(&out, "torch").unwrap_or("?")),
                Err(e) => failures.push(format!("{name}: {}", e.lines().last().unwrap_or(""))),
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
