//! The embedded manifests: parse, validate, and agree with the v1 engine files
//! they replace (desktop/engine/requirements-*.txt, version.py, hardware.py).

use std::collections::BTreeSet;
use std::path::PathBuf;

use gm_ports::*;
use gm_types::Backend;

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn engine(file: &str) -> String {
    std::fs::read_to_string(repo().join("desktop/engine").join(file)).unwrap()
}

/// Requirement lines of a requirements file, following `-r` includes.
fn requirement_lines(file: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in engine(file).lines() {
        let line = line.split('#').next().unwrap().trim();
        if line.is_empty() || line.starts_with("--") {
            continue;
        }
        if let Some(inc) = line.strip_prefix("-r ") {
            out.extend(requirement_lines(inc.trim()));
        } else {
            out.push(line.to_string());
        }
    }
    out
}

fn extra_index(file: &str) -> Option<String> {
    engine(file).lines().find_map(|l| {
        l.trim()
            .strip_prefix("--extra-index-url ")
            .map(|s| s.trim().to_string())
    })
}

#[test]
fn builtin_registry_is_valid() {
    let reg = Registry::builtin();
    reg.validate().unwrap();
    assert_eq!(reg.all().len(), BUILTIN_MANIFESTS.len());
}

#[test]
fn every_probe_port_id_exists_and_nothing_else() {
    let reg = Registry::builtin();
    let ids: BTreeSet<_> = reg.all().iter().map(|p| p.id.as_str()).collect();
    let want: BTreeSet<_> = PORT_IDS.iter().copied().collect();
    assert_eq!(ids, want);
    for id in PORT_IDS {
        assert_eq!(reg.require(id).unwrap().id, *id);
    }
    assert!(reg.get("cuda-cu999").is_none());
    assert!(matches!(reg.require("nope"), Err(Error::UnknownPort(_))));
}

#[test]
fn load_dir_matches_builtin() {
    let from_disk = Registry::load_dir(repo().join("ports")).unwrap();
    let mut a: Vec<_> = from_disk.all().to_vec();
    let mut b: Vec<_> = Registry::builtin().all().to_vec();
    a.sort_by(|x, y| x.id.cmp(&y.id));
    b.sort_by(|x, y| x.id.cmp(&y.id));
    assert_eq!(a, b);
}

#[test]
fn all_ports_on_python_312_with_the_v1_package_union() {
    let union: Vec<String> = requirement_lines("requirements-common.txt");
    assert!(
        union.iter().any(|l| l.starts_with("fastapi")),
        "control plane included via -r"
    );
    for spec in Registry::builtin().all() {
        assert_eq!(spec.python, "3.12", "{}", spec.id);
        assert_eq!(
            spec.packages, union,
            "{}: packages must equal control+common",
            spec.id
        );
    }
}

#[test]
fn reference_pins_follow_version_py() {
    let v = engine("version.py");
    let pin = |k: &str| {
        let line = v
            .lines()
            .find(|l| l.trim_start().starts_with(&format!("\"{k}\":")))
            .unwrap();
        line.split('"').nth(3).unwrap().to_string()
    };
    assert_eq!(pin("torch"), REFERENCE_TORCH);
    assert_eq!(pin("torchvision"), REFERENCE_TORCHVISION);
    assert_eq!(pin("ultralytics"), REFERENCE_ULTRALYTICS);
    for spec in Registry::builtin().all().iter().filter(|p| p.reference) {
        assert_eq!(spec.torch, REFERENCE_TORCH, "{}", spec.id);
        assert_eq!(spec.torchvision, REFERENCE_TORCHVISION, "{}", spec.id);
    }
}

#[test]
fn torch_pins_and_indexes_match_v1_requirement_files() {
    let reg = Registry::builtin();
    for (id, file) in [
        ("cuda-cu130", "requirements-train-cu130.txt"),
        ("cuda-cu126", "requirements-train-cu126.txt"),
        ("cuda-cu128", "requirements-train-cu128.txt"),
        ("xpu", "requirements-xpu.txt"),
        ("mps", "requirements-mps.txt"),
        ("cpu", "requirements-train-cpu.txt"),
    ] {
        let spec = reg.get(id).unwrap();
        let lines = requirement_lines(file);
        let reqs = requirements(spec);
        for name in ["torch==", "torchvision=="] {
            let v1 = lines.iter().find(|l| l.starts_with(name)).unwrap();
            assert!(reqs.contains(v1), "{id}: {v1} missing from {reqs:?}");
        }
        assert_eq!(spec.index_url, extra_index(file), "{id}");
    }
}

#[test]
fn port_facts_from_the_v1_comments() {
    let reg = Registry::builtin();
    let get = |id| reg.get(id).unwrap();

    // hardware.py DRIVER_* constants.
    let hw = engine("hardware.py");
    let driver = |name: &str| -> u32 {
        hw.lines()
            .find(|l| l.starts_with(&format!("{name} =")))
            .unwrap()
            .split('=')
            .nth(1)
            .unwrap()
            .trim()
            .parse()
            .unwrap()
    };
    assert_eq!(get("cuda-cu130").min_driver, Some(driver("DRIVER_CU130")));
    assert_eq!(get("cuda-cu128").min_driver, Some(driver("DRIVER_CU128")));
    let cu126 = get("cuda-cu126");
    assert_eq!(
        cu126.min_driver,
        Some(driver("DRIVER_CU126_WINDOWS").max(driver("DRIVER_CU126_LINUX")))
    );
    assert!(cu126.notes.as_deref().unwrap().contains("Blackwell"));
    assert!(cu126.notes.as_deref().unwrap().contains("R525 on Linux"));

    let cu128 = get("cuda-cu128");
    assert!(!cu128.reference);
    assert_eq!(
        (cu128.torch.as_str(), cu128.torchvision.as_str()),
        ("2.11.0", "0.26.0")
    );

    let mps = get("mps");
    assert_eq!(mps.index_url, None);
    assert_eq!(mps.local_suffix, "");
    assert_eq!(mps.platforms, vec!["macos-arm64"]);
    assert!(mps.notes.as_deref().unwrap().contains("macOS 14"));

    let rocm = get("rocm");
    assert!(rocm.experimental && !rocm.reference);
    assert_eq!(rocm.backend, Backend::Rocm);
    assert_eq!(rocm.platforms, vec!["linux-x86_64"]);
    assert_eq!(rocm.torch, REFERENCE_TORCH);

    for spec in reg.all() {
        assert_eq!(
            spec.min_driver.is_some(),
            spec.backend == Backend::Cuda,
            "{}",
            spec.id
        );
        assert!(spec.notes.is_some(), "{} has notes", spec.id);
    }
}

fn good() -> PortSpec {
    Registry::builtin().get("cuda-cu130").unwrap().clone()
}

fn problems(spec: &PortSpec) -> String {
    validate_spec(spec).join("\n")
}

#[test]
fn validation_catches_mistakes() {
    assert!(validate_spec(&good()).is_empty());

    let mut s = good();
    s.local_suffix = "+cu126".into();
    assert!(problems(&s).contains("does not match index_url"));

    let mut s = good();
    s.index_url = None;
    assert!(problems(&s).contains("needs an index_url"));

    let mut s = good();
    s.platforms.push("macos-x86_64".into());
    assert!(problems(&s).contains("unknown platform"));

    let mut s = good();
    s.torch = "2.12.0".into();
    assert!(problems(&s).contains("reference stack"));

    let mut s = good();
    s.reference = false; // the same pin off-reference is fine
    s.torch = "2.12.0".into();
    assert!(validate_spec(&s).is_empty(), "{}", problems(&s));

    let mut s = good();
    s.python = "3.9".into();
    assert!(problems(&s).contains("python"));

    let mut s = good();
    s.packages.push("torch>=2".into());
    assert!(problems(&s).contains("pinned by the torch"));

    let mut s = good();
    s.backend = Backend::Xpu;
    assert!(problems(&s).contains("must start with its backend"));

    let mut s = good();
    s.experimental = true;
    assert!(problems(&s).contains("experimental"));

    let mut s = good();
    s.min_driver = None;
    assert!(problems(&s).contains("min_driver"));

    let mut s = good();
    s.packages.retain(|p| !p.starts_with("ultralytics"));
    assert!(problems(&s).contains("ultralytics==8.4.46"));
}

#[test]
fn duplicate_ids_and_misnamed_files_are_rejected() {
    let a = BUILTIN_MANIFESTS[0];
    let reg = Registry::from_sources([a, a]).unwrap();
    let err = reg.validate().unwrap_err().to_string();
    assert!(err.contains("duplicate port id"), "{err}");

    let err = Registry::from_sources([("other.toml", a.1)])
        .unwrap_err()
        .to_string();
    assert!(err.contains("file name must match"), "{err}");

    let err = Registry::from_sources([("x.toml", "id = 3")]).unwrap_err();
    assert!(matches!(err, Error::Manifest { .. }));
}

#[test]
fn load_dir_override() {
    let dir = tempfile::tempdir().unwrap();
    let text = BUILTIN_MANIFESTS
        .iter()
        .find(|(n, _)| *n == "mps.toml")
        .unwrap()
        .1;
    std::fs::write(
        dir.path().join("mps.toml"),
        text.replace("PyTorch Metal (MPS) build", "dev mps"),
    )
    .unwrap();
    std::fs::write(dir.path().join("README.txt"), "ignored").unwrap();
    let reg = Registry::load_dir(dir.path()).unwrap();
    assert_eq!(reg.all().len(), 1);
    assert_eq!(reg.get("mps").unwrap().label, "dev mps");
    reg.validate().unwrap();
}

#[test]
fn platform_queries() {
    let reg = Registry::builtin();
    let mac: Vec<_> = reg
        .for_platform("macos-arm64")
        .map(|p| p.id.as_str())
        .collect();
    assert_eq!(mac, vec!["mps"]);
    let win_arm: Vec<_> = reg
        .for_platform("windows-arm64")
        .map(|p| p.id.as_str())
        .collect();
    assert_eq!(win_arm, vec!["cpu"]);
    if let Some(here) = current_platform() {
        assert!(PLATFORMS.contains(&here));
    }
}
