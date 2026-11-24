//! Machine identity and a one-line description.

use gm_types::{Arch, DeviceFacts, Os, Vendor};
use sha2::{Digest, Sha256};

fn os_str(os: Os) -> &'static str {
    match os {
        Os::Windows => "windows",
        Os::Macos => "macos",
        Os::Linux => "linux",
        Os::Other => "other",
    }
}

fn arch_str(arch: Arch) -> &'static str {
    match arch {
        Arch::X86_64 => "x86_64",
        Arch::Arm64 => "arm64",
        Arch::Other => "other",
    }
}

fn vendor_str(vendor: Vendor) -> &'static str {
    match vendor {
        Vendor::Nvidia => "nvidia",
        Vendor::Intel => "intel",
        Vendor::Apple => "apple",
        Vendor::Amd => "amd",
        Vendor::Other => "other",
    }
}

/// Identifies this machine well enough to notice an environment (a port)
/// copied from another one. Same idea as setup_env.py `machine_fingerprint`:
/// the first 16 hex chars of sha256 over
/// `hostname(lowercase)|os|arch|cpu model|gpu names (sorted, comma-joined)`.
///
/// Stable across runs and reboots; it changes when the GPU set, CPU or
/// hostname does. Driver versions, RAM and OS version are deliberately left
/// out so a driver update does not look like a different machine.
pub fn machine_fingerprint(facts: &DeviceFacts) -> String {
    let mut gpus: Vec<&str> = facts.gpus.iter().map(|g| g.name.trim()).collect();
    gpus.sort_unstable();
    let parts = [
        facts.hostname.trim().to_lowercase(),
        os_str(facts.os).to_string(),
        arch_str(facts.arch).to_string(),
        facts.cpu_model.as_deref().unwrap_or("").trim().to_string(),
        gpus.join(","),
    ];
    let digest = Sha256::digest(parts.join("|").as_bytes());
    hex::encode(digest)[..16].to_string()
}

fn gb(mb: u64) -> String {
    let g = mb as f64 / 1024.0;
    if g >= 10.0 || (g - g.round()).abs() < 0.05 {
        format!("{:.0} GB", g)
    } else {
        format!("{:.1} GB", g)
    }
}

/// One line for logs and the UI, e.g.
/// `linux 6.8.0 x86_64 (rosetta) | AMD Ryzen 9 7950X, 32 threads | 64 GB RAM | GPU0 NVIDIA GeForce RTX 4090 [nvidia, cc 8.9, driver 580.95.05, 24 GB]`.
pub fn summary(facts: &DeviceFacts) -> String {
    let mut out = format!("{} {} {}", os_str(facts.os), facts.os_version, arch_str(facts.arch));
    if facts.translated {
        out.push_str(" (rosetta)");
    }
    let cpu = match (&facts.cpu_model, facts.cpu_cores) {
        (Some(m), Some(n)) => Some(format!("{m}, {n} threads")),
        (Some(m), None) => Some(m.clone()),
        (None, Some(n)) => Some(format!("{n} threads")),
        (None, None) => None,
    };
    if let Some(cpu) = cpu {
        out.push_str(" | ");
        out.push_str(&cpu);
    }
    if let Some(ram) = facts.ram_mb {
        out.push_str(&format!(" | {} RAM", gb(ram)));
    }
    if facts.gpus.is_empty() {
        out.push_str(" | no GPU");
    }
    for gpu in &facts.gpus {
        let mut extra = vec![vendor_str(gpu.vendor).to_string()];
        if let Some(cc) = gpu.compute_capability {
            extra.push(format!("cc {cc:.1}"));
        }
        match gpu.driver.as_deref() {
            Some(d) => extra.push(format!("driver {d}")),
            None if gpu.vendor == Vendor::Nvidia => extra.push("no driver".into()),
            None => {}
        }
        if let Some(mb) = gpu.memory_mb {
            extra.push(gb(mb));
        }
        out.push_str(&format!(" | GPU{} {} [{}]", gpu.index, gpu.name, extra.join(", ")));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use gm_types::Gpu;

    fn facts() -> DeviceFacts {
        DeviceFacts {
            os: Os::Linux,
            os_version: "6.8.0".into(),
            arch: Arch::X86_64,
            translated: false,
            hostname: "Rig-01".into(),
            cpu_model: Some("AMD Ryzen 9 7950X 16-Core Processor".into()),
            cpu_cores: Some(32),
            ram_mb: Some(64 * 1024),
            gpus: vec![Gpu {
                vendor: Vendor::Nvidia,
                name: "NVIDIA GeForce RTX 4090".into(),
                index: 0,
                compute_capability: Some(8.9),
                driver: Some("580.95.05".into()),
                memory_mb: Some(24564),
                cores: None,
                integrated: false,
            }],
            os_name_pretty: None,
        }
    }

    #[test]
    fn fingerprint_is_stable_and_ignores_drivers() {
        let a = facts();
        let fp = machine_fingerprint(&a);
        assert_eq!(fp.len(), 16);
        assert!(fp.chars().all(|c| c.is_ascii_hexdigit()));
        let mut b = a.clone();
        b.gpus[0].driver = Some("590.1".into());
        b.ram_mb = Some(1);
        b.hostname = "rig-01".into();
        assert_eq!(machine_fingerprint(&b), fp);
        b.gpus[0].name = "NVIDIA GeForce RTX 5090".into();
        assert_ne!(machine_fingerprint(&b), fp);
    }

    #[test]
    fn summary_line() {
        let s = summary(&facts());
        assert_eq!(
            s,
            "linux 6.8.0 x86_64 | AMD Ryzen 9 7950X 16-Core Processor, 32 threads | 64 GB RAM | \
             GPU0 NVIDIA GeForce RTX 4090 [nvidia, cc 8.9, driver 580.95.05, 24 GB]"
        );
        let mut none = facts();
        none.gpus.clear();
        assert!(summary(&none).ends_with("| no GPU"));
    }
}
