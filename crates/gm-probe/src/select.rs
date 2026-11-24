//! Device facts → port. A line-by-line port of `select_profile`,
//! `select_profile_for`, `host_profile` and `_fit_os` from `hardware.py`.
//!
//! Everything here is a pure function of its arguments (the one exception is
//! [`ProbeOptions::from_env`], which only [`select_profile`] and
//! [`host_profile`] call). The parity test pins the outcome to the Python.

use std::fmt;
use std::str::FromStr;
use std::sync::OnceLock;

use gm_types::{Arch, Backend, DeviceFacts, Gpu, Os, Profile, Vendor};
use regex::Regex;
use serde::{Deserialize, Serialize};

/// The reference torch every reference port installs (`version.py` REFERENCE_STACK).
pub const REFERENCE_TORCH: &str = "2.13.0";
/// The torch the CUDA 12.8 fallback port installs (Blackwell on a pre-R580 driver).
pub const TORCH_CU128_FALLBACK: &str = "2.11.0";

/// Minimum NVIDIA driver major for the CUDA 13.0 build (R580 branch).
pub const DRIVER_CU130: u64 = 580;
/// Minimum NVIDIA driver major for the CUDA 12.8 build (R570 branch).
pub const DRIVER_CU128: u64 = 570;
/// Minimum NVIDIA driver major for CUDA 12.x minor-version compatibility, Linux.
pub const DRIVER_CU126_LINUX: u64 = 525;
/// Minimum NVIDIA driver major for CUDA 12.x minor-version compatibility, Windows.
pub const DRIVER_CU126_WINDOWS: u64 = 528;

/// Where the "install a newer driver" fixes point.
pub const NVIDIA_DRIVERS_URL: &str = "https://www.nvidia.com/Download/index.aspx";
/// Where the Intel XPU fix points.
pub const INTEL_DRIVERS_URL: &str =
    "https://www.intel.com/content/www/us/en/download/785597/intel-arc-iris-xe-graphics-windows.html";

/// An 8 GB Mac reports 8192 MB; leave room for rounding in what sysctl returns.
pub const SMALL_UNIFIED_MEMORY_MB: u64 = 8 * 1024 + 512;

/// Env var that opts `Prefer::Auto` into the experimental ROCm port.
pub const EXPERIMENTAL_ROCM_ENV: &str = "GRADAPP_EXPERIMENTAL_ROCM";

// ---------------------------------------------------------------------------
// Prefer / options
// ---------------------------------------------------------------------------

/// Which backend family the person asked for. `Auto` lets the rules decide.
///
/// Forcing a family is for a machine with two kinds of GPU that should
/// contribute the one not picked by default, e.g. a laptop with an Intel Arc
/// iGPU next to an NVIDIA card.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Prefer {
    #[default]
    Auto,
    Cuda,
    Xpu,
    Mps,
    Cpu,
    /// Experimental: AMD GPUs on Linux with ROCm installed.
    Rocm,
}

impl Prefer {
    /// Every value, in CLI order.
    pub const ALL: [Prefer; 6] =
        [Prefer::Auto, Prefer::Cuda, Prefer::Xpu, Prefer::Mps, Prefer::Cpu, Prefer::Rocm];

    /// The lowercase name used on the command line and in hardware.py.
    pub fn as_str(self) -> &'static str {
        match self {
            Prefer::Auto => "auto",
            Prefer::Cuda => "cuda",
            Prefer::Xpu => "xpu",
            Prefer::Mps => "mps",
            Prefer::Cpu => "cpu",
            Prefer::Rocm => "rocm",
        }
    }
}

impl fmt::Display for Prefer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Returned by `"..".parse::<Prefer>()` for an unknown backend name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreferParseError(pub String);

impl fmt::Display for PreferParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unknown backend '{}' (expected auto, cuda, xpu, mps, cpu or rocm)", self.0)
    }
}

impl std::error::Error for PreferParseError {}

impl FromStr for Prefer {
    type Err = PreferParseError;

    /// Case-insensitive; an empty string means `Auto`, like hardware.py.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let lowered = s.trim().to_ascii_lowercase();
        if lowered.is_empty() {
            return Ok(Prefer::Auto);
        }
        Prefer::ALL
            .into_iter()
            .find(|p| p.as_str() == lowered)
            .ok_or_else(|| PreferParseError(s.to_string()))
    }
}

/// Switches that are not facts about the machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ProbeOptions {
    /// Let `Prefer::Auto` choose the experimental ROCm port on a Linux
    /// machine whose only usable GPU is AMD with ROCm installed. Off by
    /// default so `Auto` stays identical to hardware.py.
    pub experimental_rocm: bool,
}

impl ProbeOptions {
    /// Reads `GRADAPP_EXPERIMENTAL_ROCM=1` (also accepts `true`/`yes`).
    pub fn from_env() -> Self {
        let on = std::env::var(EXPERIMENTAL_ROCM_ENV)
            .map(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes"))
            .unwrap_or(false);
        ProbeOptions { experimental_rocm: on }
    }
}

// ---------------------------------------------------------------------------
// Public entry points
// ---------------------------------------------------------------------------

/// The port this machine should run as a worker: hardware.py `select_profile_for`.
///
/// Reads [`ProbeOptions::from_env`]; use [`select_profile_with`] for a fully
/// pure call.
pub fn select_profile(facts: &DeviceFacts, prefer: Prefer) -> Profile {
    select_profile_with(facts, prefer, &ProbeOptions::from_env())
}

/// [`select_profile`] with explicit options. Pure.
pub fn select_profile_with(facts: &DeviceFacts, prefer: Prefer, options: &ProbeOptions) -> Profile {
    fit_os(select_raw(facts, prefer, options), facts)
}

/// The port for a coordinator host: hardware.py `host_profile`.
///
/// The host needs PyTorch even when its own GPU cannot train, because it
/// aggregates every round. So a blocked GPU profile becomes the CPU build with
/// the block (and its fix) kept as a warning. The exception is an Intel Mac or
/// a Rosetta process, where nothing installs at all: that profile is returned
/// as it is (and, as in Python, without the macOS CPU-port rename).
pub fn host_profile(facts: &DeviceFacts, prefer: Prefer) -> Profile {
    host_profile_with(facts, prefer, &ProbeOptions::from_env())
}

/// [`host_profile`] with explicit options. Pure.
pub fn host_profile_with(facts: &DeviceFacts, prefer: Prefer, options: &ProbeOptions) -> Profile {
    let profile = select_raw(facts, prefer, options);
    if let Some(blocked) = &profile.blocked {
        if facts.os == Os::Macos && facts.arch != Arch::Arm64 {
            // Nothing installs on an Intel Mac, not even for aggregation.
            return profile;
        }
        let mut warnings = profile.warnings.clone();
        warnings.push(match &profile.fix {
            Some(fix) => format!("{blocked} {fix}"),
            None => blocked.clone(),
        });
        let fallback = cpu(
            "the GPU cannot be used yet, so the host installs the CPU build for aggregation".into(),
            None,
            None,
            None,
            warnings,
        );
        return fit_os(fallback, facts);
    }
    fit_os(profile, facts)
}

/// The ROCm version detection recorded for this machine, if any.
///
/// [`detect`](crate::detect) marks ROCm on Linux by setting the AMD GPUs'
/// `driver` to `"rocm <version>"` (or just `"rocm"`), because `DeviceFacts`
/// has no field for installed runtimes yet. Returns the text after `rocm`.
pub fn rocm_version(facts: &DeviceFacts) -> Option<&str> {
    facts.gpus.iter().filter(|g| g.vendor == Vendor::Amd).find_map(|g| {
        let driver = g.driver.as_deref()?;
        driver.strip_prefix("rocm").map(str::trim)
    })
}

// ---------------------------------------------------------------------------
// Rules
// ---------------------------------------------------------------------------

fn intel_xpu_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?i)arc|dg2|battlemage|\bbmg\b|alchemist|meteor ?lake|lunar ?lake|arrow ?lake|panther ?lake|data center gpu",
        )
        .expect("static regex")
    })
}

fn intel_experimental_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)iris\s*\(?r?\)?\s*xe|iris xe").expect("static regex"))
}

fn intel_discrete_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"arc.*\b[ab]\d{3}").expect("static regex"))
}

/// hardware.py `_intel_profile`'s rank: discrete Arc 2, other supported 1, else 0.
pub(crate) fn intel_rank(name: &str) -> u8 {
    let name = name.to_lowercase();
    if intel_discrete_re().is_match(&name) || name.contains("dg2") || name.contains("battlemage") {
        2
    } else if intel_xpu_re().is_match(&name) {
        1
    } else {
        0
    }
}

/// Leading integer of a driver string ("580.95.05" → 580). `_driver_major`.
fn driver_major(driver: Option<&str>) -> Option<u64> {
    let driver = driver?;
    let digits: String = driver.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return None;
    }
    Some(digits.parse::<u64>().unwrap_or(u64::MAX))
}

/// `_version_tuple(v) < (14,)` reduces to "first numeric component < 14".
fn macos_major(version: &str) -> u64 {
    let first = version.split(['.', '-']).next().unwrap_or("");
    if !first.is_empty() && first.chars().all(|c| c.is_ascii_digit()) {
        first.parse::<u64>().unwrap_or(u64::MAX)
    } else {
        0
    }
}

/// Python's `round()` (half to even) for the GB figure in the 8 GB warning.
fn round_half_even(x: f64) -> i64 {
    x.round_ties_even() as i64
}

/// Python truthiness of a driver string.
fn truthy(s: Option<&str>) -> Option<&str> {
    s.filter(|s| !s.is_empty())
}

fn arch_str(arch: Arch) -> &'static str {
    match arch {
        Arch::X86_64 => "x86_64",
        Arch::Arm64 => "arm64",
        Arch::Other => "unknown",
    }
}

fn cpu(
    reason: String,
    gpu: Option<&Gpu>,
    blocked: Option<String>,
    fix: Option<String>,
    warnings: Vec<String>,
) -> Profile {
    Profile {
        port: "cpu".into(),
        backend: Backend::Cpu,
        label: "PyTorch CPU build".into(),
        reason,
        torch: REFERENCE_TORCH.into(),
        reference: true,
        can_train: false,
        gpu_index: None,
        gpu_name: gpu.map(|g| g.name.clone()),
        blocked,
        fix,
        warnings,
    }
}

fn mps(reason: String, gpu_name: String, blocked: Option<String>, fix: Option<String>, warnings: Vec<String>) -> Profile {
    Profile {
        port: "mps".into(),
        backend: Backend::Mps,
        label: "PyTorch macOS build (Metal)".into(),
        reason,
        torch: REFERENCE_TORCH.into(),
        reference: true,
        // hardware.py leaves can_train at its default (True) even when blocked.
        can_train: true,
        gpu_index: None,
        gpu_name: Some(gpu_name),
        blocked,
        fix,
        warnings,
    }
}

/// `_fit_os`: PyTorch's CPU index has no +cpu wheels for macOS; the stock
/// wheel is the CPU build there, so the CPU port id becomes "mps".
fn fit_os(mut profile: Profile, facts: &DeviceFacts) -> Profile {
    if facts.os == Os::Macos && profile.port == "cpu" {
        profile.port = "mps".into();
        profile.label = "PyTorch macOS build".into();
    }
    profile
}

/// First element with the greatest key (Python `max` keeps the first on ties;
/// Rust's `Iterator::max_by` keeps the last).
fn first_max_by<'a, T, K: PartialOrd>(items: &[&'a T], key: impl Fn(&T) -> K) -> &'a T {
    let mut best = items[0];
    let mut best_key = key(best);
    for item in &items[1..] {
        let k = key(item);
        if k > best_key {
            best = item;
            best_key = k;
        }
    }
    best
}

fn nvidia_profile(gpus: &[&Gpu], os: Os) -> Profile {
    // Train on the most capable card. A machine mixing a Pascal and a Blackwell
    // card has no single build that covers both, so the stronger one wins.
    let best = first_max_by(gpus, |g| (g.compute_capability.unwrap_or(0.0), g.memory_mb.unwrap_or(0)));
    let capability = best.compute_capability;
    let driver_text = best.driver.as_deref();
    let driver = driver_major(driver_text);
    let driver_s = driver_text.unwrap_or("None");
    let cc_text = match capability {
        Some(c) if c != 0.0 => format!("compute {c:.1}"),
        _ => "unknown compute capability".to_string(),
    };

    let mut warnings = Vec::new();
    let mut distinct: Vec<Option<f32>> = Vec::new();
    for g in gpus {
        if !distinct.contains(&g.compute_capability) {
            distinct.push(g.compute_capability);
        }
    }
    if distinct.len() > 1 {
        warnings.push(format!(
            "this machine has GPUs of different generations; GradMesh trains on GPU {} ({})",
            best.index, best.name
        ));
    }

    let cuda = |port: &str, label: &str, reason: String, torch: &str, reference: bool, warnings: Vec<String>| Profile {
        port: port.into(),
        backend: Backend::Cuda,
        label: label.into(),
        reason,
        torch: torch.into(),
        reference,
        can_train: true,
        gpu_index: Some(best.index),
        gpu_name: Some(best.name.clone()),
        blocked: None,
        fix: None,
        warnings,
    };
    let driver_fix = || {
        format!("Install NVIDIA driver 580 or newer from {NVIDIA_DRIVERS_URL}, reboot, then run this again.")
    };

    if let Some(c) = capability {
        if c < 5.0 {
            return cpu(
                format!("{} at {} is older than any current PyTorch CUDA build supports", best.name, cc_text),
                Some(best),
                None,
                None,
                warnings,
            );
        }
    }

    let minimum_cu126 = if os == Os::Windows { DRIVER_CU126_WINDOWS } else { DRIVER_CU126_LINUX };
    if let Some(d) = driver {
        if d < minimum_cu126 {
            return cpu(
                format!("NVIDIA driver {driver_s} is too old for CUDA 12"),
                Some(best),
                Some(format!(
                    "The NVIDIA driver on this machine ({driver_s}) is too old for any current PyTorch build."
                )),
                Some(driver_fix()),
                warnings,
            );
        }
    }

    let blackwell = matches!(capability, Some(c) if c >= 12.0);
    if blackwell {
        match driver {
            None => {
                return cuda(
                    "cuda-cu130",
                    "PyTorch CUDA 13.0 build",
                    format!("{} is {} (Blackwell), which needs CUDA 12.8 or newer", best.name, cc_text),
                    REFERENCE_TORCH,
                    true,
                    warnings,
                )
            }
            Some(d) if d >= DRIVER_CU130 => {
                return cuda(
                    "cuda-cu130",
                    "PyTorch CUDA 13.0 build",
                    format!("{} is {} (Blackwell), which needs CUDA 12.8 or newer", best.name, cc_text),
                    REFERENCE_TORCH,
                    true,
                    warnings,
                )
            }
            Some(d) if d >= DRIVER_CU128 => {
                let mut warnings = warnings;
                warnings.push(format!(
                    "this machine is on torch 2.11 rather than the reference {REFERENCE_TORCH}, because its driver is \
                     older than R580. Update the NVIDIA driver to keep cross-vendor results comparable."
                ));
                return cuda(
                    "cuda-cu128",
                    "PyTorch CUDA 12.8 build (fallback)",
                    format!("{} is Blackwell and driver {} predates CUDA 13.0", best.name, driver_s),
                    TORCH_CU128_FALLBACK,
                    false,
                    warnings,
                );
            }
            Some(_) => {
                return cpu(
                    format!("Blackwell needs driver {DRIVER_CU128} or newer, this machine has {driver_s}"),
                    Some(best),
                    Some(format!("{} needs NVIDIA driver 570 or newer; this machine has {}.", best.name, driver_s)),
                    Some(driver_fix()),
                    warnings,
                );
            }
        }
    }

    // Turing (7.5) onward runs CUDA 13.0 when the driver allows it. Older
    // architectures were dropped from CUDA 13, so they stay on 12.6.
    if matches!(capability, Some(c) if c >= 7.5) && matches!(driver, Some(d) if d >= DRIVER_CU130) {
        return cuda(
            "cuda-cu130",
            "PyTorch CUDA 13.0 build",
            format!("{} is {} with driver {}", best.name, cc_text, driver_s),
            REFERENCE_TORCH,
            true,
            warnings,
        );
    }
    let with_driver = truthy(driver_text).map(|d| format!(" with driver {d}")).unwrap_or_default();
    cuda(
        "cuda-cu126",
        "PyTorch CUDA 12.6 build",
        format!("{} is {}{}", best.name, cc_text, with_driver),
        REFERENCE_TORCH,
        true,
        warnings,
    )
}

fn intel_profile(gpus: &[&Gpu]) -> Profile {
    // Prefer a discrete card over an integrated one when both are present.
    let best = first_max_by(gpus, |g| intel_rank(&g.name));
    Profile {
        port: "xpu".into(),
        backend: Backend::Xpu,
        label: "PyTorch Intel XPU build".into(),
        reason: format!("{} trains on the Intel XPU backend", best.name),
        torch: REFERENCE_TORCH.into(),
        reference: true,
        can_train: true,
        // XPU devices are numbered among Intel GPUs only, so the first is 0.
        gpu_index: Some(0),
        gpu_name: Some(best.name.clone()),
        blocked: None,
        fix: Some(format!(
            "If the XPU runtime does not start, install the latest Intel graphics driver from {INTEL_DRIVERS_URL} \
             (on Linux: Intel's compute runtime packages, intel-opencl-icd and level-zero)."
        )),
        warnings: Vec::new(),
    }
}

fn rocm_profile(gpu: &Gpu, version: &str) -> Profile {
    let shown = if version.is_empty() { String::new() } else { format!(" {version}") };
    Profile {
        port: "rocm".into(),
        backend: Backend::Rocm,
        label: "PyTorch ROCm build (experimental)".into(),
        reason: format!("{} trains on ROCm{} (experimental, opted in)", gpu.name, shown),
        torch: REFERENCE_TORCH.into(),
        reference: false,
        can_train: true,
        // HIP numbers AMD GPUs from 0 and exposes them as torch "cuda" devices.
        gpu_index: Some(0),
        gpu_name: Some(gpu.name.clone()),
        blocked: None,
        fix: None,
        warnings: vec![format!(
            "ROCm support is experimental: this machine is off the reference stack, so its results are not \
             guaranteed to match NVIDIA, Intel and Apple machines exactly. Unset {EXPERIMENTAL_ROCM_ENV} or pick \
             another backend to leave it."
        )],
    }
}

fn apple_warnings(facts: &DeviceFacts) -> Vec<String> {
    let mut warnings = Vec::new();
    if let Some(ram) = facts.ram_mb {
        if ram > 0 && ram <= SMALL_UNIFIED_MEMORY_MB {
            warnings.push(format!(
                "{} GB of unified memory is shared by macOS, every open app and the GPU. GradMesh sizes this Mac's \
                 batches to what Metal grants and stops training from paging to the SSD; quit browsers and other \
                 large apps before a run.",
                round_half_even(ram as f64 / 1024.0)
            ));
        }
    }
    warnings
}

/// hardware.py `select_profile` (before `_fit_os`), plus the opt-in ROCm rule.
fn select_raw(facts: &DeviceFacts, prefer: Prefer, options: &ProbeOptions) -> Profile {
    let of = |v: Vendor| facts.gpus.iter().filter(move |g| g.vendor == v);
    let nvidia: Vec<&Gpu> = of(Vendor::Nvidia).filter(|g| g.driver.is_some()).collect();
    let nvidia_without_driver: Vec<&Gpu> = of(Vendor::Nvidia).filter(|g| g.driver.is_none()).collect();
    let intel: Vec<&Gpu> = of(Vendor::Intel).collect();
    let apple: Vec<&Gpu> = of(Vendor::Apple).collect();

    if prefer == Prefer::Cpu {
        return cpu("CPU was requested explicitly".into(), None, None, None, Vec::new());
    }

    // --- Apple ---------------------------------------------------------------
    if facts.os == Os::Macos {
        let gpu_name = apple.first().map(|g| g.name.clone()).unwrap_or_else(|| "Apple Silicon".into());
        if facts.arch != Arch::Arm64 && facts.translated {
            return mps(
                "Apple Silicon, but this Python is an Intel build running under Rosetta".into(),
                gpu_name,
                Some(
                    "This Mac has Apple Silicon, but the Python that ran this is an Intel (x86_64) build running \
                     under Rosetta, and PyTorch publishes no Intel-Mac builds. The GPU is fine; the Python is the \
                     wrong one."
                        .into(),
                ),
                Some(
                    "Install the Apple Silicon Python with Homebrew (brew install python@3.12, which lives in \
                     /opt/homebrew) or the python.org macOS installer, then run the join command again. If \
                     Terminal itself is set to open using Rosetta, turn that off in its Get Info window first."
                        .into(),
                ),
                Vec::new(),
            );
        }
        if facts.arch != Arch::Arm64 {
            return cpu(
                "Intel Macs have no current PyTorch builds".into(),
                None,
                Some(
                    "PyTorch stopped publishing builds for Intel Macs after version 2.2, so this Mac cannot run the \
                     GradMesh training stack."
                        .into(),
                ),
                Some(
                    "Contribute an Apple Silicon Mac, or a Windows or Linux machine with an NVIDIA or Intel GPU."
                        .into(),
                ),
                Vec::new(),
            );
        }
        if macos_major(&facts.os_version) < 14 {
            return mps(
                format!("Apple Silicon on macOS {}", facts.os_version),
                gpu_name,
                Some(format!(
                    "PyTorch {REFERENCE_TORCH} needs macOS 14 Sonoma or newer; this Mac runs {}.",
                    facts.os_version
                )),
                Some("Update macOS from System Settings > General > Software Update, then run this again.".into()),
                Vec::new(),
            );
        }
        let chip = apple.first().map(|g| g.name.as_str()).unwrap_or(arch_str(facts.arch));
        return mps(
            format!("Apple Silicon ({chip}) trains on the Metal backend"),
            gpu_name,
            None,
            None,
            apple_warnings(facts),
        );
    }

    // --- Explicit Intel request ---------------------------------------------
    if prefer == Prefer::Xpu && !intel.is_empty() {
        return intel_profile(&intel);
    }

    let auto_or_cuda = matches!(prefer, Prefer::Auto | Prefer::Cuda);

    // --- NVIDIA ----------------------------------------------------------------
    if !nvidia.is_empty() && auto_or_cuda {
        return nvidia_profile(&nvidia, facts.os);
    }

    if !nvidia_without_driver.is_empty() && auto_or_cuda && intel.is_empty() {
        return cpu(
            "an NVIDIA GPU is present but its driver is not installed".into(),
            Some(nvidia_without_driver[0]),
            Some("This machine has an NVIDIA GPU, but nvidia-smi is missing, so the driver is not installed.".into()),
            Some(format!(
                "Install the NVIDIA driver from {NVIDIA_DRIVERS_URL} (on Linux: your distribution's nvidia-driver \
                 package), reboot, then run this again."
            )),
            Vec::new(),
        );
    }

    // --- Intel -----------------------------------------------------------------
    if !intel.is_empty() && matches!(prefer, Prefer::Auto | Prefer::Xpu) {
        let supported: Vec<&Gpu> = intel.iter().copied().filter(|g| intel_xpu_re().is_match(&g.name)).collect();
        let experimental: Vec<&Gpu> =
            intel.iter().copied().filter(|g| intel_experimental_re().is_match(&g.name)).collect();
        if !supported.is_empty() || prefer == Prefer::Xpu {
            return intel_profile(if supported.is_empty() { &intel } else { &supported });
        }
        if !experimental.is_empty() {
            let mut profile = intel_profile(&experimental);
            profile.warnings.push(format!(
                "{} is not on Intel's supported list for PyTorch XPU. It is installed and verified; if the runtime \
                 does not start, the machine joins without a GPU.",
                experimental[0].name
            ));
            return profile;
        }
    }

    match prefer {
        Prefer::Cuda => {
            return cpu(
                "CUDA was requested but no NVIDIA GPU with a driver was found".into(),
                None,
                None,
                None,
                Vec::new(),
            )
        }
        Prefer::Xpu => {
            return cpu("XPU was requested but no Intel GPU was found".into(), None, None, None, Vec::new())
        }
        Prefer::Mps => {
            return cpu("MPS is only available on Apple Silicon Macs".into(), None, None, None, Vec::new())
        }
        _ => {}
    }

    // --- AMD -------------------------------------------------------------------
    let amd: Vec<&Gpu> = of(Vendor::Amd).collect();
    if let Some(first_amd) = amd.first() {
        // New in 2.0, opt-in only: ROCm on Linux. `Auto` reaches this only with
        // the experimental switch, so default behaviour matches hardware.py.
        let opted_in = prefer == Prefer::Rocm || (prefer == Prefer::Auto && options.experimental_rocm);
        if opted_in && facts.os == Os::Linux {
            if let Some(version) = rocm_version(facts) {
                return rocm_profile(first_amd, version);
            }
        }
        return cpu(
            format!("{} is an AMD GPU, which GradMesh 5 does not train on yet", first_amd.name),
            Some(first_amd),
            None,
            None,
            Vec::new(),
        );
    }
    cpu("no supported GPU was detected".into(), None, None, None, Vec::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gpu(vendor: Vendor, name: &str, driver: Option<&str>) -> Gpu {
        Gpu {
            vendor,
            name: name.into(),
            index: 0,
            compute_capability: None,
            driver: driver.map(Into::into),
            memory_mb: None,
            cores: None,
            integrated: false,
        }
    }

    fn linux(gpus: Vec<Gpu>) -> DeviceFacts {
        DeviceFacts {
            os: Os::Linux,
            os_version: "6.8.0".into(),
            arch: Arch::X86_64,
            translated: false,
            hostname: "box".into(),
            cpu_model: None,
            cpu_cores: Some(16),
            ram_mb: Some(65536),
            gpus,
            os_name_pretty: None,
        }
    }

    const OFF: ProbeOptions = ProbeOptions { experimental_rocm: false };
    const ON: ProbeOptions = ProbeOptions { experimental_rocm: true };

    #[test]
    fn prefer_parses() {
        assert_eq!("ROCm".parse::<Prefer>().unwrap(), Prefer::Rocm);
        assert_eq!("".parse::<Prefer>().unwrap(), Prefer::Auto);
        assert!("metal".parse::<Prefer>().is_err());
        for p in Prefer::ALL {
            assert_eq!(p.to_string().parse::<Prefer>().unwrap(), p);
        }
    }

    #[test]
    fn rocm_is_opt_in_only() {
        let facts = linux(vec![gpu(Vendor::Amd, "AMD Radeon RX 7900 XTX", Some("rocm 6.4.1"))]);
        let auto = select_profile_with(&facts, Prefer::Auto, &OFF);
        assert_eq!(auto.port, "cpu");
        assert!(!auto.can_train);

        let forced = select_profile_with(&facts, Prefer::Rocm, &OFF);
        assert_eq!(forced.port, "rocm");
        assert_eq!(forced.backend, Backend::Rocm);
        assert!(forced.can_train && !forced.reference);
        assert_eq!(forced.gpu_name.as_deref(), Some("AMD Radeon RX 7900 XTX"));

        let env = select_profile_with(&facts, Prefer::Auto, &ON);
        assert_eq!(env.port, "rocm");
        assert_eq!(host_profile_with(&facts, Prefer::Auto, &ON).port, "rocm");
    }

    #[test]
    fn rocm_needs_the_runtime_and_linux() {
        let no_runtime = linux(vec![gpu(Vendor::Amd, "AMD Radeon RX 7900 XTX", None)]);
        assert_eq!(select_profile_with(&no_runtime, Prefer::Rocm, &ON).port, "cpu");

        let mut windows = linux(vec![gpu(Vendor::Amd, "AMD Radeon RX 7900 XTX", Some("rocm 6.4"))]);
        windows.os = Os::Windows;
        assert_eq!(select_profile_with(&windows, Prefer::Rocm, &ON).port, "cpu");
    }

    #[test]
    fn rocm_env_does_not_beat_nvidia_or_intel() {
        let mut nv = gpu(Vendor::Nvidia, "NVIDIA GeForce RTX 4090", Some("580.95.05"));
        nv.compute_capability = Some(8.9);
        let facts = linux(vec![nv, gpu(Vendor::Amd, "AMD Radeon RX 7900 XTX", Some("rocm"))]);
        assert_eq!(select_profile_with(&facts, Prefer::Auto, &ON).port, "cuda-cu130");
        assert_eq!(select_profile_with(&facts, Prefer::Rocm, &ON).port, "rocm");
        assert_eq!(rocm_version(&facts), Some(""));
    }

    #[test]
    fn helpers() {
        assert_eq!(driver_major(Some("580.95.05")), Some(580));
        assert_eq!(driver_major(Some("[N/A]")), None);
        assert_eq!(macos_major("13.6.7"), 13);
        assert_eq!(macos_major("26.0"), 26);
        assert_eq!(macos_major(""), 0);
        assert_eq!(round_half_even(2.5), 2);
        assert_eq!(round_half_even(7.5), 8);
        assert_eq!(round_half_even(8.0), 8);
        assert_eq!(intel_rank("Intel(R) Arc(TM) A770 Graphics"), 2);
        assert_eq!(intel_rank("Intel(R) Arc(TM) Graphics"), 1);
        assert_eq!(intel_rank("Intel(R) UHD Graphics 630"), 0);
    }
}
