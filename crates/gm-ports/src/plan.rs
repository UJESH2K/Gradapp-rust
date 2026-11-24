//! Turning a manifest into exact pins and a lock.

use std::time::{SystemTime, UNIX_EPOCH};

use gm_types::{Arch, DeviceFacts, Os, PortLock, PortSpec};
use sha2::{Digest, Sha256};

/// Exact requirement strings for a port: the torch pair first (with the build's
/// local suffix, e.g. `torch==2.13.0+cu130`), then every other package as written.
///
/// An empty `torch` / `torchvision` field leaves that pin out. Manifests in the
/// registry always set both ([`crate::validate_spec`] insists); the escape
/// hatch exists for torch-free test ports and a future control-plane-only port.
pub fn requirements(spec: &PortSpec) -> Vec<String> {
    let mut out = Vec::with_capacity(spec.packages.len() + 2);
    if !spec.torch.is_empty() {
        out.push(format!("torch=={}{}", spec.torch, spec.local_suffix));
    }
    if !spec.torchvision.is_empty() {
        out.push(format!(
            "torchvision=={}{}",
            spec.torchvision, spec.local_suffix
        ));
    }
    out.extend(spec.packages.iter().map(|p| p.trim().to_string()));
    out
}

/// sha256 over everything that, if it changed, means the env must be rebuilt:
/// port id, python, index, the requirement set (in order) and the machine.
pub fn lock_fingerprint(spec: &PortSpec, machine: &str) -> String {
    let mut h = Sha256::new();
    let mut field = |k: &str, v: &str| {
        h.update(k.as_bytes());
        h.update(b"=");
        h.update(v.as_bytes());
        h.update(b"\n");
    };
    field("v", "gradapp-port-lock/1");
    field("port", &spec.id);
    field("python", &spec.python);
    field("index", spec.index_url.as_deref().unwrap_or(""));
    for r in requirements(spec) {
        field("req", &r);
    }
    field("machine", machine);
    hex::encode(h.finalize())
}

/// The lock this machine should end up with for `spec`. `facts_fingerprint`
/// identifies the machine (see [`machine_fingerprint`]); an env copied or synced
/// from another machine then shows up as Stale instead of half working.
pub fn plan(spec: &PortSpec, facts_fingerprint: &str) -> PortLock {
    PortLock {
        port: spec.id.clone(),
        python: spec.python.clone(),
        torch: format!("{}{}", spec.torch, spec.local_suffix),
        torchvision: format!("{}{}", spec.torchvision, spec.local_suffix),
        index_url: spec.index_url.clone(),
        requirements: requirements(spec),
        fingerprint: lock_fingerprint(spec, facts_fingerprint),
        machine: facts_fingerprint.to_string(),
        created_unix: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
    }
}

fn machine_hash(hostname: &str, os: &str, arch: &str) -> String {
    let text = format!("{}|{}|{}", hostname.trim().to_lowercase(), os, arch);
    hex::encode(Sha256::digest(text.as_bytes()))[..16].to_string()
}

/// Machine fingerprint from probed facts: hostname, OS and CPU architecture.
///
/// For a *remote* machine's facts (e.g. a wheelhouse built elsewhere). For
/// this machine always use [`local_machine_fingerprint`], in both
/// `provision` and `status`: the two are **not** guaranteed equal. On
/// Windows `facts.hostname` is `%COMPUTERNAME%` (NetBIOS: upper case, cut to
/// 15 characters) while gethostname is the DNS name, and under Rosetta
/// `facts.arch` is the chip's arm64. Mixing them makes every lock Stale and
/// rebuilds the port on every start (QA review).
pub fn machine_fingerprint(facts: &DeviceFacts) -> String {
    let os = match facts.os {
        Os::Windows => "windows",
        Os::Macos => "macos",
        Os::Linux => "linux",
        Os::Other => "other",
    };
    let arch = match facts.arch {
        Arch::X86_64 => "x86_64",
        Arch::Arm64 => "arm64",
        Arch::Other => "other",
    };
    machine_hash(&facts.hostname, os, arch)
}

/// Machine fingerprint of the running machine, without a probe.
pub fn local_machine_fingerprint() -> String {
    let os = match std::env::consts::OS {
        "windows" => "windows",
        "macos" => "macos",
        "linux" => "linux",
        _ => "other",
    };
    let arch = match std::env::consts::ARCH {
        "x86_64" => "x86_64",
        "aarch64" => "arm64",
        _ => "other",
    };
    let host = gethostname::gethostname().to_string_lossy().into_owned();
    machine_hash(&host, os, arch)
}
