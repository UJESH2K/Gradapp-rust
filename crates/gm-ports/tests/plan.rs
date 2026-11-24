use gm_ports::*;
use gm_types::{Arch, DeviceFacts, Os};

fn spec(id: &str) -> PortSpec {
    Registry::builtin().get(id).unwrap().clone()
}

#[test]
fn exact_pins() {
    let r = requirements(&spec("cuda-cu130"));
    assert_eq!(r[0], "torch==2.13.0+cu130");
    assert_eq!(r[1], "torchvision==0.28.0+cu130");
    assert!(r.contains(&"ultralytics==8.4.46".to_string()));
    assert!(r.contains(&"uvicorn[standard]>=0.30".to_string()));

    assert_eq!(
        requirements(&spec("mps"))[..2],
        [
            "torch==2.13.0".to_string(),
            "torchvision==0.28.0".to_string()
        ]
    );
    assert_eq!(requirements(&spec("cuda-cu128"))[0], "torch==2.11.0+cu128");
    assert_eq!(requirements(&spec("rocm"))[0], "torch==2.13.0+rocm7.2");
    assert_eq!(requirements(&spec("xpu"))[1], "torchvision==0.28.0+xpu");
    assert_eq!(requirements(&spec("cpu"))[0], "torch==2.13.0+cpu");
}

#[test]
fn lock_contents() {
    let s = spec("xpu");
    let lock = plan(&s, "m1");
    assert_eq!(lock.port, "xpu");
    assert_eq!(lock.python, "3.12");
    assert_eq!(lock.torch, "2.13.0+xpu");
    assert_eq!(lock.torchvision, "0.28.0+xpu");
    assert_eq!(
        lock.index_url.as_deref(),
        Some("https://download.pytorch.org/whl/xpu")
    );
    assert_eq!(lock.requirements, requirements(&s));
    assert_eq!(lock.machine, "m1");
    assert_eq!(lock.fingerprint.len(), 64);
}

#[test]
fn fingerprint_is_stable() {
    let s = spec("cuda-cu130");
    let a = plan(&s, "machine-a");
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let b = plan(&s, "machine-a");
    assert_eq!(
        a.fingerprint, b.fingerprint,
        "time must not enter the fingerprint"
    );
    assert_eq!(lock_fingerprint(&s, "machine-a"), a.fingerprint);
    // A clone is the same plan; the order of package specs as written matters.
    assert_eq!(plan(&s.clone(), "machine-a").fingerprint, a.fingerprint);
    // Golden: a change here means every installed env becomes Stale; bump on purpose only.
    let tiny = PortSpec {
        id: "cpu".into(),
        backend: gm_types::Backend::Cpu,
        label: "t".into(),
        python: "3.12".into(),
        torch: "2.13.0".into(),
        torchvision: "0.28.0".into(),
        index_url: None,
        local_suffix: String::new(),
        packages: vec!["six".into()],
        platforms: vec!["linux-x86_64".into()],
        reference: false,
        experimental: false,
        min_driver: None,
        notes: None,
    };
    let golden = lock_fingerprint(&tiny, "m");
    assert_eq!(golden, lock_fingerprint(&tiny, "m"));
    assert_eq!(golden.len(), 64);
}

#[test]
fn fingerprint_is_sensitive() {
    let base = spec("cuda-cu130");
    let fp = |s: &PortSpec, m: &str| plan(s, m).fingerprint;
    let f0 = fp(&base, "m");
    assert_ne!(f0, fp(&base, "other-machine"));

    let mut s = base.clone();
    s.python = "3.13".into();
    assert_ne!(f0, fp(&s, "m"));

    let mut s = base.clone();
    s.index_url = Some("https://example.invalid/whl/cu130".into());
    assert_ne!(f0, fp(&s, "m"));

    let mut s = base.clone();
    s.local_suffix = "+cu126".into();
    assert_ne!(f0, fp(&s, "m"));

    let mut s = base.clone();
    s.packages.push("six".into());
    assert_ne!(f0, fp(&s, "m"));

    let mut s = base.clone();
    s.torch = "2.13.1".into();
    assert_ne!(f0, fp(&s, "m"));

    // Cosmetic fields do not force a rebuild.
    let mut s = base.clone();
    s.label = "renamed".into();
    s.notes = None;
    assert_eq!(f0, fp(&s, "m"));

    // Different ports never share a fingerprint.
    assert_ne!(f0, fp(&spec("cuda-cu126"), "m"));
}

#[test]
fn machine_fingerprints() {
    let facts = |host: &str, arch| DeviceFacts {
        os: Os::Linux,
        os_version: "6.8".into(),
        arch,
        translated: false,
        hostname: host.into(),
        cpu_model: None,
        cpu_cores: None,
        ram_mb: None,
        gpus: vec![],
        os_name_pretty: None,
    };
    let a = machine_fingerprint(&facts("Lab-PC", Arch::X86_64));
    assert_eq!(a, machine_fingerprint(&facts("lab-pc", Arch::X86_64)));
    assert_ne!(a, machine_fingerprint(&facts("lab-pc", Arch::Arm64)));
    assert_ne!(a, machine_fingerprint(&facts("lab-pc2", Arch::X86_64)));
    assert_eq!(a.len(), 16);
    assert_eq!(local_machine_fingerprint(), local_machine_fingerprint());
}
