//! Running the system tools. Everything OS-specific is selected at run time
//! with `cfg!(...)` rather than `#[cfg]`, so all of it compiles (and is
//! type-checked) on every platform; only the Windows `CREATE_NO_WINDOW` flag
//! needs real conditional compilation.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use gm_types::{Arch, DeviceFacts, Gpu, Os, Vendor};

use crate::parse;

/// Default per-command timeout, as in hardware.py `_run`.
const TIMEOUT: Duration = Duration::from_secs(8);
/// PowerShell and system_profiler are slow to start; hardware.py gives them 15–20 s.
const SLOW_TIMEOUT: Duration = Duration::from_secs(15);

struct RunOutput {
    success: bool,
    stdout: String,
}

/// Runs a command with a hard timeout. `None` when it cannot start or hangs.
/// stdout is collected on a separate thread so a chatty tool cannot block on
/// a full pipe, and a grandchild holding the pipe open cannot block us.
fn run_raw(program: &Path, args: &[&str], timeout: Duration) -> Option<RunOutput> {
    let mut cmd = Command::new(program);
    cmd.args(args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = cmd.spawn().ok()?;
    let mut stdout = child.stdout.take()?;
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stdout.read_to_end(&mut buf);
        let _ = tx.send(buf);
    });

    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(15)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    };
    let remaining = deadline.saturating_duration_since(Instant::now()).max(Duration::from_millis(500));
    let buf = rx.recv_timeout(remaining).ok()?;
    Some(RunOutput { success: status.success(), stdout: String::from_utf8_lossy(&buf).into_owned() })
}

/// stdout of a successful command, like hardware.py `_run`.
fn run(program: impl AsRef<Path>, args: &[&str], timeout: Duration) -> Option<String> {
    run_raw(program.as_ref(), args, timeout).filter(|o| o.success).map(|o| o.stdout)
}

/// stdout whatever the exit code (sysctl exits 1 when one name is unknown).
fn run_any(program: impl AsRef<Path>, args: &[&str], timeout: Duration) -> Option<String> {
    run_raw(program.as_ref(), args, timeout).map(|o| o.stdout)
}

/// `shutil.which`: the first file called `name` (or `name.exe` on Windows) on PATH.
fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).find_map(|dir| {
        let candidates: &[String] =
            if cfg!(windows) { &[format!("{name}.exe"), name.to_string()] } else { &[name.to_string()] };
        candidates.iter().map(|c| dir.join(c)).find(|p| p.is_file())
    })
}

fn current_os() -> Os {
    if cfg!(target_os = "windows") {
        Os::Windows
    } else if cfg!(target_os = "macos") {
        Os::Macos
    } else if cfg!(target_os = "linux") {
        Os::Linux
    } else {
        Os::Other
    }
}

fn process_arch() -> Arch {
    match std::env::consts::ARCH {
        "x86_64" => Arch::X86_64,
        "aarch64" | "arm64" => Arch::Arm64,
        _ => Arch::Other,
    }
}

fn read(path: impl AsRef<Path>) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

fn nvidia_smi() -> Option<PathBuf> {
    which("nvidia-smi").or_else(|| {
        if !cfg!(windows) {
            return None;
        }
        // The driver installs it here even when it is not on PATH.
        let root = std::env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into());
        let candidate = PathBuf::from(root).join("System32").join("nvidia-smi.exe");
        candidate.is_file().then_some(candidate)
    })
}

/// Every NVIDIA GPU nvidia-smi can see. hardware.py `_nvidia_gpus`.
fn nvidia_gpus() -> Vec<Gpu> {
    let Some(smi) = nvidia_smi() else { return Vec::new() };
    let query = format!("--query-gpu={}", parse::NVIDIA_SMI_FIELDS);
    if let Some(raw) = run(&smi, &[&query, "--format=csv,noheader,nounits"], TIMEOUT) {
        return parse::parse_nvidia_smi(&raw, true);
    }
    // nvidia-smi from before the compute_cap field existed.
    let legacy = format!("--query-gpu={}", parse::NVIDIA_SMI_FIELDS_LEGACY);
    run(&smi, &[&legacy, "--format=csv,noheader,nounits"], TIMEOUT)
        .map(|raw| parse::parse_nvidia_smi(&raw, false))
        .unwrap_or_default()
}

fn hostname() -> String {
    let from_env = |k: &str| std::env::var(k).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    let found = if cfg!(windows) {
        from_env("COMPUTERNAME")
    } else {
        read("/proc/sys/kernel/hostname")
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .or_else(|| run("hostname", &[], TIMEOUT).map(|s| s.trim().to_string()).filter(|s| !s.is_empty()))
            .or_else(|| from_env("HOSTNAME"))
    };
    found.unwrap_or_else(|| "unknown".into())
}

fn cpu_threads() -> Option<u32> {
    std::thread::available_parallelism().ok().map(|n| n.get() as u32)
}

// ---------------------------------------------------------------------------
// Per OS
// ---------------------------------------------------------------------------

struct OsFacts {
    os_version: String,
    os_name_pretty: Option<String>,
    arch: Arch,
    translated: bool,
    cpu_model: Option<String>,
    cpu_threads: Option<u32>,
    ram_mb: Option<u64>,
    listed: Vec<Gpu>,
}

fn linux_rocm_version() -> Option<String> {
    let installed = Path::new("/opt/rocm").is_dir();
    let from_file = read("/opt/rocm/.info/version").and_then(|t| parse::parse_rocm_version(&t));
    if from_file.is_some() {
        return from_file;
    }
    if installed || run("rocminfo", &[], TIMEOUT).is_some() {
        return Some(String::new());
    }
    None
}

fn linux_display_devices() -> Vec<Gpu> {
    if let Some(raw) = run("lspci", &["-mm"], TIMEOUT) {
        let gpus = parse::parse_lspci_mm(&raw);
        if !gpus.is_empty() {
            return gpus;
        }
    }
    // No lspci, which is common in containers. sysfs still names the vendor.
    let Ok(dir) = std::fs::read_dir("/sys/class/drm") else { return Vec::new() };
    let entries: Vec<(String, String)> = dir
        .filter_map(Result::ok)
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let vendor = read(e.path().join("device").join("vendor"))?;
            Some((name, vendor))
        })
        .collect();
    parse::parse_drm_cards(&entries)
}

fn linux_facts() -> OsFacts {
    let (cpu_model, cpu_count) = read("/proc/cpuinfo").map(|t| parse::parse_cpuinfo(&t)).unwrap_or((None, None));
    let mut listed = linux_display_devices();
    if listed.iter().any(|g| g.vendor == Vendor::Amd) {
        if let Some(version) = linux_rocm_version() {
            // DeviceFacts has no runtime field; see select::rocm_version.
            let marker = if version.is_empty() { "rocm".to_string() } else { format!("rocm {version}") };
            for gpu in listed.iter_mut().filter(|g| g.vendor == Vendor::Amd) {
                gpu.driver = Some(marker.clone());
            }
        }
    }
    OsFacts {
        os_version: read("/proc/sys/kernel/osrelease").map(|s| s.trim().to_string()).unwrap_or_default(),
        os_name_pretty: read("/etc/os-release")
            .or_else(|| read("/usr/lib/os-release"))
            .and_then(|t| parse::parse_os_release(&t)),
        arch: process_arch(),
        translated: false,
        cpu_model,
        cpu_threads: cpu_threads().or(cpu_count),
        ram_mb: read("/proc/meminfo").and_then(|t| parse::parse_meminfo(&t)),
        listed,
    }
}

fn windows_facts() -> OsFacts {
    let inventory = run(
        "powershell",
        &["-NoProfile", "-NonInteractive", "-Command", parse::WINDOWS_INVENTORY_SCRIPT],
        SLOW_TIMEOUT,
    )
    .and_then(|raw| parse::parse_windows_inventory(&raw))
    .unwrap_or_default();
    OsFacts {
        os_version: inventory.os_version.clone().unwrap_or_default(),
        os_name_pretty: inventory.os_caption.clone(),
        arch: process_arch(),
        translated: false,
        cpu_model: inventory.cpu_model.clone(),
        cpu_threads: cpu_threads().or(inventory.cpu_threads),
        ram_mb: inventory.ram_mb,
        listed: inventory.gpus,
    }
}

fn macos_facts() -> OsFacts {
    let sysctl = run_any("sysctl", &parse::SYSCTL_NAMES, TIMEOUT).map(|t| parse::parse_sysctl(&t)).unwrap_or_default();
    let version = run("sw_vers", &["-productVersion"], TIMEOUT).map(|t| parse::parse_sw_vers(&t)).unwrap_or_default();
    let ram_mb = sysctl.memsize.map(|b| b / (1024 * 1024));
    let process = process_arch();
    let apple_silicon = process == Arch::Arm64 || sysctl.arm64;
    // `arch` is the chip, not this process: ports bring their own native
    // CPython, so an x86_64 build of this app under Rosetta still gets the
    // arm64 port. `translated` records the Rosetta process for the UI.
    let arch = if apple_silicon { Arch::Arm64 } else { process };
    let translated = sysctl.translated || (process == Arch::X86_64 && apple_silicon);

    let mut listed = run("system_profiler", &["SPDisplaysDataType", "-json"], SLOW_TIMEOUT)
        .map(|t| parse::parse_system_profiler(&t, ram_mb))
        .unwrap_or_default();
    if listed.is_empty() {
        listed = parse::apple_fallback_gpus(sysctl.brand.as_deref(), apple_silicon, ram_mb);
    }
    let cpu_model = match sysctl.brand {
        Some(b) if b.to_lowercase().contains("virtualapple") => Some("Apple Silicon".into()),
        other => other,
    };
    OsFacts {
        os_name_pretty: (!version.is_empty()).then(|| format!("macOS {version}")),
        os_version: version,
        arch,
        translated,
        cpu_model,
        cpu_threads: cpu_threads(),
        ram_mb,
        listed,
    }
}

/// Gathers everything [`select_profile`](crate::select_profile) needs.
///
/// Synchronous and safe to call from any thread (it spawns short-lived
/// helper threads of its own). Every external command has a timeout of 8 s
/// (15 s for PowerShell and system_profiler); a missing, failing or hanging
/// tool only means that source found nothing. nvidia-smi and the OS query
/// run in parallel, so a typical probe takes well under a second (Windows:
/// about one PowerShell start).
///
/// Differences from hardware.py `detect()` worth knowing:
/// * On macOS `arch` is the chip's architecture; a Rosetta process gets
///   `arch = Arm64, translated = true` (Python reported its own x86_64 build).
/// * On Linux, AMD GPUs carry `driver = "rocm <version>"` when ROCm is
///   installed (`/opt/rocm` or `rocminfo`); see [`rocm_version`](crate::rocm_version).
pub fn detect() -> DeviceFacts {
    let os = current_os();
    let (nvidia, os_facts, host) = std::thread::scope(|s| {
        let nvidia = s.spawn(nvidia_gpus);
        let host = s.spawn(hostname);
        let os_facts = match os {
            Os::Windows => windows_facts(),
            Os::Macos => macos_facts(),
            Os::Linux | Os::Other => linux_facts(),
        };
        (nvidia.join().unwrap_or_default(), os_facts, host.join().unwrap_or_else(|_| "unknown".into()))
    });
    let (gpus, _note) = parse::assemble_gpus(nvidia, os_facts.listed);
    DeviceFacts {
        os,
        os_version: os_facts.os_version,
        arch: os_facts.arch,
        translated: os_facts.translated,
        hostname: host,
        cpu_model: os_facts.cpu_model,
        cpu_cores: os_facts.cpu_threads,
        ram_mb: os_facts.ram_mb,
        gpus,
        os_name_pretty: os_facts.os_name_pretty,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_command_is_none() {
        assert!(run("gm-probe-definitely-not-a-command", &[], TIMEOUT).is_none());
    }

    #[cfg(unix)]
    #[test]
    fn hanging_command_times_out() {
        let start = Instant::now();
        assert!(run("sleep", &["5"], Duration::from_millis(200)).is_none());
        assert!(start.elapsed() < Duration::from_secs(3));
    }

    #[cfg(unix)]
    #[test]
    fn failing_command_is_none_but_run_any_keeps_stdout() {
        assert!(run("sh", &["-c", "echo hi; exit 1"], TIMEOUT).is_none());
        assert_eq!(run_any("sh", &["-c", "echo hi; exit 1"], TIMEOUT).unwrap().trim(), "hi");
    }

    #[test]
    fn detect_runs_here() {
        let facts = detect();
        assert_eq!(facts.os, current_os());
        assert!(!facts.hostname.is_empty());
        // Virtual adapters never reach the facts.
        assert!(facts.gpus.iter().all(|g| g.vendor != Vendor::Other));
    }
}
