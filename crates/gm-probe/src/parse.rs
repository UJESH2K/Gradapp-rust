//! Pure parsers: each takes the text a system tool printed and returns facts.
//!
//! Nothing here runs a command or touches the file system, so every parser is
//! tested against captured output in `tests/fixtures/`. They mirror the
//! parsing in hardware.py; where hardware.py had no equivalent (CPU model,
//! pretty OS name, Windows CIM inventory) the behaviour is documented on the
//! function.

use std::sync::OnceLock;

use gm_types::{Gpu, Vendor};
use regex::Regex;
use serde_json::Value;

use crate::select::intel_rank;

/// `--query-gpu` fields of a current nvidia-smi.
pub const NVIDIA_SMI_FIELDS: &str = "index,name,compute_cap,driver_version,memory.total";
/// Fallback fields for an nvidia-smi from before `compute_cap` existed (< R510).
pub const NVIDIA_SMI_FIELDS_LEGACY: &str = "index,name,driver_version,memory.total";

fn new_gpu(vendor: Vendor, name: &str, index: u32) -> Gpu {
    Gpu {
        vendor,
        integrated: guess_integrated(vendor, name),
        name: name.to_string(),
        index,
        compute_capability: None,
        driver: None,
        memory_mb: None,
        cores: None,
    }
}

/// Python `float(text)`, or `None`.
fn py_float(text: &str) -> Option<f64> {
    text.trim().parse::<f64>().ok()
}

/// Python `str.isdigit()` for ASCII input.
fn is_digits(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_digit())
}

/// Vendor from a marketing or PCI name. hardware.py `_vendor_from_name`.
pub fn vendor_from_name(name: &str) -> Vendor {
    let lowered = name.to_lowercase();
    if ["nvidia", "geforce", "quadro", "tesla", "rtx "]
        .iter()
        .any(|t| lowered.contains(t))
    {
        Vendor::Nvidia
    } else if lowered.contains("intel") || lowered.contains(" arc") {
        Vendor::Intel
    } else if ["amd", "radeon", "advanced micro devices"]
        .iter()
        .any(|t| lowered.contains(t))
    {
        Vendor::Amd
    } else if lowered.contains("apple") {
        Vendor::Apple
    } else {
        Vendor::Other
    }
}

/// Best guess whether a GPU shares system memory. Informational only (the UI
/// shows it); no port rule depends on it. Apple GPUs always are; Intel GPUs
/// are unless they are discrete Arc / data-center parts; AMD APU graphics are
/// recognised by name.
pub fn guess_integrated(vendor: Vendor, name: &str) -> bool {
    let lowered = name.to_lowercase();
    match vendor {
        Vendor::Apple => true,
        Vendor::Intel => intel_rank(name) < 2 && !lowered.contains("data center gpu"),
        Vendor::Amd => {
            const APU: [&str; 14] = [
                "radeon graphics",
                "radeon(tm) graphics",
                "vega 3",
                "vega 6",
                "vega 8",
                "vega 11",
                "renoir",
                "cezanne",
                "barcelo",
                "rembrandt",
                "phoenix",
                "raphael",
                "strix",
                "hawk point",
            ];
            APU.iter().any(|t| lowered.contains(t))
        }
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// NVIDIA
// ---------------------------------------------------------------------------

/// Parses `nvidia-smi --query-gpu=... --format=csv,noheader,nounits`.
///
/// `with_capability` says the [`NVIDIA_SMI_FIELDS`] query succeeded; when false
/// the output is from the [`NVIDIA_SMI_FIELDS_LEGACY`] query. As in
/// hardware.py, `[N/A]` in the capability or memory column means unknown, while
/// a `[N/A]` driver string is kept verbatim (the GPU still counts as having a
/// driver, with no parseable major).
pub fn parse_nvidia_smi(text: &str, with_capability: bool) -> Vec<Gpu> {
    let mut gpus: Vec<Gpu> = Vec::new();
    for line in text.trim().lines() {
        let parts: Vec<&str> = line.split(',').map(str::trim).collect();
        let (index, name, capability, driver, memory) = if with_capability && parts.len() >= 5 {
            (parts[0], parts[1], parts[2], parts[3], parts[4])
        } else if parts.len() >= 4 {
            (parts[0], parts[1], "", parts[2], parts[3])
        } else {
            continue;
        };
        let memory_mb = match py_float(memory) {
            Some(m) if m != 0.0 => {
                if !m.is_finite() {
                    continue; // int(float("inf")) raises in Python: the line is skipped.
                }
                (m >= 0.0).then_some(m as u64)
            }
            _ => None,
        };
        let fallback_index = gpus.len() as u32;
        let mut gpu = new_gpu(Vendor::Nvidia, name, 0);
        gpu.index = if is_digits(index) {
            index.parse().unwrap_or(fallback_index)
        } else {
            fallback_index
        };
        gpu.compute_capability = py_float(capability)
            .filter(|c| c.is_finite())
            .map(|c| c as f32);
        gpu.driver = (!driver.is_empty()).then(|| driver.to_string());
        gpu.memory_mb = memory_mb;
        gpus.push(gpu);
    }
    gpus
}

// ---------------------------------------------------------------------------
// Windows
// ---------------------------------------------------------------------------

/// What one PowerShell CIM query reports about a Windows machine.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct WindowsInventory {
    /// Every `Win32_VideoController`, virtual adapters included (vendor `Other`).
    pub gpus: Vec<Gpu>,
    pub cpu_model: Option<String>,
    pub cpu_threads: Option<u32>,
    /// `Win32_OperatingSystem.Caption`, e.g. "Microsoft Windows 11 Pro".
    pub os_caption: Option<String>,
    /// `Win32_OperatingSystem.Version`, e.g. "10.0.26100".
    pub os_version: Option<String>,
    pub ram_mb: Option<u64>,
}

/// The PowerShell script whose output [`parse_windows_inventory`] reads. One
/// PowerShell start (~0.5 s) instead of three; `wmic` is deprecated.
pub const WINDOWS_INVENTORY_SCRIPT: &str = "$ErrorActionPreference='SilentlyContinue';\
[Console]::OutputEncoding=[Text.Encoding]::UTF8;\
$g=@(Get-CimInstance Win32_VideoController | Select-Object Name,AdapterRAM,DriverVersion);\
$c=Get-CimInstance Win32_Processor | Select-Object -First 1 Name,NumberOfLogicalProcessors;\
$o=Get-CimInstance Win32_OperatingSystem | Select-Object Caption,Version,TotalVisibleMemorySize;\
[pscustomobject]@{gpus=$g;cpu=$c;os=$o} | ConvertTo-Json -Compress -Depth 4";

fn strip_bom(text: &str) -> &str {
    text.trim().trim_start_matches('\u{feff}')
}

fn json_str(value: &Value, key: &str) -> Option<String> {
    match value.get(key)? {
        Value::String(s) if !s.trim().is_empty() => Some(s.trim().to_string()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

fn json_u64(value: &Value, key: &str) -> Option<u64> {
    match value.get(key)? {
        Value::Number(n) => n
            .as_u64()
            .or_else(|| n.as_f64().filter(|f| *f >= 0.0).map(|f| f as u64)),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

fn video_controllers(data: &Value) -> Vec<Gpu> {
    let entries: Vec<&Value> = match data {
        Value::Array(items) => items.iter().collect(),
        Value::Object(_) => vec![data],
        _ => Vec::new(),
    };
    let mut gpus = Vec::new();
    for (index, entry) in entries.into_iter().enumerate() {
        let Some(name) = json_str(entry, "Name") else {
            continue;
        };
        let mut gpu = new_gpu(vendor_from_name(&name), &name, index as u32);
        gpu.driver = json_str(entry, "DriverVersion");
        // AdapterRAM is a uint32, so it saturates at 4 GB on bigger cards.
        gpu.memory_mb = json_u64(entry, "AdapterRAM")
            .filter(|b| *b > 0)
            .map(|b| b / (1024 * 1024));
        gpus.push(gpu);
    }
    gpus
}

/// Parses `Get-CimInstance Win32_VideoController | Select Name,AdapterRAM,DriverVersion | ConvertTo-Json`.
///
/// `ConvertTo-Json` prints a bare object for one adapter and an array for
/// several; both are accepted. `index` is the position in the CIM list, as in
/// hardware.py. Unparseable output yields no GPUs.
pub fn parse_win32_video_controllers(json: &str) -> Vec<Gpu> {
    serde_json::from_str::<Value>(strip_bom(json))
        .map(|v| video_controllers(&v))
        .unwrap_or_default()
}

/// Parses the output of [`WINDOWS_INVENTORY_SCRIPT`].
pub fn parse_windows_inventory(json: &str) -> Option<WindowsInventory> {
    let data: Value = serde_json::from_str(strip_bom(json)).ok()?;
    let cpu = data.get("cpu").unwrap_or(&Value::Null);
    let os = data.get("os").unwrap_or(&Value::Null);
    Some(WindowsInventory {
        gpus: data.get("gpus").map(video_controllers).unwrap_or_default(),
        cpu_model: json_str(cpu, "Name"),
        cpu_threads: json_u64(cpu, "NumberOfLogicalProcessors").map(|n| n as u32),
        os_caption: json_str(os, "Caption"),
        os_version: json_str(os, "Version"),
        ram_mb: json_u64(os, "TotalVisibleMemorySize").map(|kb| kb / 1024),
    })
}

// ---------------------------------------------------------------------------
// Linux
// ---------------------------------------------------------------------------

fn lspci_class_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#""(VGA compatible controller|3D controller|Display controller)""#)
            .expect("static regex")
    })
}

fn quoted_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#""([^"]*)""#).expect("static regex"))
}

/// Parses `lspci -mm`. Display-class devices only; the name is
/// `"<vendor> <device>"` as hardware.py builds it. Virtual adapters come back
/// with vendor `Other` (the caller drops them).
pub fn parse_lspci_mm(text: &str) -> Vec<Gpu> {
    let mut gpus = Vec::new();
    for line in text.lines() {
        if !lspci_class_re().is_match(line) {
            continue;
        }
        let fields: Vec<&str> = quoted_re()
            .captures_iter(line)
            .map(|c| c.get(1).map_or("", |m| m.as_str()))
            .collect();
        let name = if fields.len() >= 3 {
            fields[1..3].join(" ")
        } else {
            line.to_string()
        };
        let index = gpus.len() as u32;
        gpus.push(new_gpu(vendor_from_name(&name), name.trim(), index));
    }
    gpus
}

/// PCI vendor id (as in `/sys/class/drm/cardN/device/vendor`) → vendor.
pub fn pci_vendor(id: &str) -> Vendor {
    match id.trim().to_lowercase().as_str() {
        "0x10de" => Vendor::Nvidia,
        "0x8086" => Vendor::Intel,
        "0x1002" => Vendor::Amd,
        _ => Vendor::Other,
    }
}

fn vendor_word(vendor: Vendor) -> &'static str {
    match vendor {
        Vendor::Nvidia => "nvidia",
        Vendor::Intel => "intel",
        Vendor::Amd => "amd",
        Vendor::Apple => "apple",
        Vendor::Other => "unknown",
    }
}

/// GPUs from sysfs, for machines without lspci (containers).
///
/// `entries` are `(directory name under /sys/class/drm, contents of its
/// device/vendor file)`. Only `cardN` entries count; they are taken in
/// lexicographic order like hardware.py's `sorted(os.listdir(...))`.
pub fn parse_drm_cards(entries: &[(String, String)]) -> Vec<Gpu> {
    let mut cards: Vec<&(String, String)> = entries
        .iter()
        .filter(|(card, _)| card.strip_prefix("card").is_some_and(is_digits))
        .collect();
    cards.sort_by(|a, b| a.0.cmp(&b.0));
    cards
        .into_iter()
        .enumerate()
        .map(|(index, (card, id))| {
            let vendor = pci_vendor(id);
            new_gpu(
                vendor,
                &format!("{} GPU ({})", vendor_word(vendor), card),
                index as u32,
            )
        })
        .collect()
}

/// `MemTotal` from `/proc/meminfo`, in MB.
pub fn parse_meminfo(text: &str) -> Option<u64> {
    text.lines()
        .find(|l| l.starts_with("MemTotal:"))
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|kb| kb.parse::<u64>().ok())
        .map(|kb| kb / 1024)
}

/// CPU model and logical CPU count from `/proc/cpuinfo`.
///
/// x86 has `model name`; ARM boards often only have `Hardware` or `Model`.
pub fn parse_cpuinfo(text: &str) -> (Option<String>, Option<u32>) {
    let mut model = None;
    let mut fallback = None;
    let mut count = 0u32;
    for line in text.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim();
        match key {
            "processor" => count += 1,
            "model name" | "cpu model" if model.is_none() && !value.is_empty() => {
                model = Some(value.to_string())
            }
            "Hardware" | "Model" if fallback.is_none() && !value.is_empty() => {
                fallback = Some(value.to_string())
            }
            _ => {}
        }
    }
    (model.or(fallback), (count > 0).then_some(count))
}

/// `PRETTY_NAME` (else `NAME`) from `/etc/os-release`.
pub fn parse_os_release(text: &str) -> Option<String> {
    let value = |key: &str| {
        text.lines().find_map(|l| {
            let v = l.strip_prefix(key)?.strip_prefix('=')?;
            let v = v.trim().trim_matches('"').trim_matches('\'').trim();
            (!v.is_empty()).then(|| v.to_string())
        })
    };
    value("PRETTY_NAME").or_else(|| value("NAME"))
}

/// The ROCm version from `/opt/rocm/.info/version` ("6.4.1-120" → "6.4.1").
pub fn parse_rocm_version(text: &str) -> Option<String> {
    let first = text.lines().next()?.trim();
    let version = first.split('-').next().unwrap_or(first).trim();
    (!version.is_empty()).then(|| version.to_string())
}

// ---------------------------------------------------------------------------
// macOS
// ---------------------------------------------------------------------------

/// The four sysctl values the probe reads on macOS.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SysctlFacts {
    /// `hw.memsize`, bytes.
    pub memsize: Option<u64>,
    /// `machdep.cpu.brand_string`, e.g. "Apple M3 Max".
    pub brand: Option<String>,
    /// `sysctl.proc_translated == 1`: this process runs under Rosetta.
    pub translated: bool,
    /// `hw.optional.arm64 == 1`: the chip is Apple Silicon.
    pub arm64: bool,
}

/// The sysctl invocation [`parse_sysctl`] expects. Names are printed as
/// `name: value` (no `-n`) so a name an older or Intel macOS does not know
/// (it prints an error and exits non-zero) cannot shift the others.
pub const SYSCTL_NAMES: [&str; 4] = [
    "hw.memsize",
    "machdep.cpu.brand_string",
    "sysctl.proc_translated",
    "hw.optional.arm64",
];

/// Parses `sysctl hw.memsize machdep.cpu.brand_string sysctl.proc_translated hw.optional.arm64`.
pub fn parse_sysctl(text: &str) -> SysctlFacts {
    let mut facts = SysctlFacts::default();
    for line in text.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        match key.trim() {
            "hw.memsize" => facts.memsize = value.parse().ok(),
            "machdep.cpu.brand_string" if !value.is_empty() => {
                facts.brand = Some(value.to_string())
            }
            "sysctl.proc_translated" => facts.translated = value == "1",
            "hw.optional.arm64" => facts.arm64 = value == "1",
            _ => {}
        }
    }
    facts
}

/// `sw_vers -productVersion` → "14.5".
pub fn parse_sw_vers(text: &str) -> String {
    text.trim().to_string()
}

/// "Apple M1, 8-core GPU": the M1 Air shipped with 7 and 8 GPU cores, and the
/// difference shows up in throughput, so the name carries it. hardware.py `apple_gpu_name`.
pub fn apple_gpu_name(name: &str, cores: Option<u32>) -> String {
    match cores {
        Some(c) if c > 0 => format!("{name}, {c}-core GPU"),
        _ => name.to_string(),
    }
}

/// Parses `system_profiler SPDisplaysDataType -json`. hardware.py `_apple_gpus`.
///
/// Apple GPUs get the core count in the name and the unified memory
/// (`ram_mb`) as their memory. Other GPUs (Intel Macs) keep their name and
/// vendor. An empty result means "use [`apple_fallback_gpus`]".
pub fn parse_system_profiler(json: &str, ram_mb: Option<u64>) -> Vec<Gpu> {
    let Ok(data) = serde_json::from_str::<Value>(strip_bom(json)) else {
        return Vec::new();
    };
    let Some(entries) = data.get("SPDisplaysDataType").and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut gpus = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        let name = json_str(entry, "sppci_model")
            .or_else(|| json_str(entry, "_name"))
            .unwrap_or_else(|| "Apple GPU".into());
        if !name.to_lowercase().contains("apple") {
            gpus.push(new_gpu(vendor_from_name(&name), &name, index as u32));
            continue;
        }
        let cores = json_str(entry, "sppci_cores")
            .and_then(|c| py_float(&c))
            .filter(|c| c.is_finite() && *c >= 1.0)
            .map(|c| c as u32);
        let mut gpu = new_gpu(Vendor::Apple, &apple_gpu_name(&name, cores), index as u32);
        gpu.memory_mb = ram_mb;
        gpu.cores = cores;
        gpus.push(gpu);
    }
    gpus
}

/// The GPU of an Apple Silicon Mac whose system_profiler said nothing usable:
/// named after the chip (`machdep.cpu.brand_string`), or "Apple Silicon" when
/// the brand is missing or Rosetta's "VirtualApple".
pub fn apple_fallback_gpus(
    brand: Option<&str>,
    apple_silicon: bool,
    ram_mb: Option<u64>,
) -> Vec<Gpu> {
    if !apple_silicon {
        return Vec::new();
    }
    let chip = match brand.map(str::trim) {
        Some(b) if !b.is_empty() && !b.to_lowercase().contains("virtualapple") => b,
        _ => "Apple Silicon",
    };
    let mut gpu = new_gpu(Vendor::Apple, chip, 0);
    gpu.memory_mb = ram_mb;
    vec![gpu]
}

// ---------------------------------------------------------------------------
// Assembly
// ---------------------------------------------------------------------------

/// Combines nvidia-smi's GPUs with the OS's display list, as hardware.py's
/// `detect()` does:
///
/// * an NVIDIA card the OS lists while nvidia-smi saw nothing is kept with
///   `driver = None` (the driver is not installed);
/// * virtual adapters (vendor `Other`: remote desktop, basic display driver,
///   VM framebuffers) are dropped, and so are listed NVIDIA cards when
///   nvidia-smi already reported them;
/// * order is nvidia-smi GPUs, then driverless NVIDIA, then Intel/AMD/Apple.
///
/// The second value is a human note when an NVIDIA GPU lacks its driver.
pub fn assemble_gpus(nvidia: Vec<Gpu>, listed: Vec<Gpu>) -> (Vec<Gpu>, Option<String>) {
    let mut note = None;
    let mut out = nvidia;
    if out.is_empty() && listed.iter().any(|g| g.vendor == Vendor::Nvidia) {
        note = Some(
            "an NVIDIA GPU is present but nvidia-smi is not, so the driver is not installed"
                .to_string(),
        );
        out.extend(
            listed
                .iter()
                .filter(|g| g.vendor == Vendor::Nvidia)
                .cloned()
                .map(|mut g| {
                    g.driver = None;
                    g
                }),
        );
    }
    out.extend(
        listed
            .into_iter()
            .filter(|g| matches!(g.vendor, Vendor::Intel | Vendor::Amd | Vendor::Apple)),
    );
    (out, note)
}
