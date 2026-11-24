//! Every parser against captured tool output, plus end-to-end checks that
//! the parsed facts select the expected port.

use gm_probe::parse::*;
use gm_probe::{select_profile_with, Prefer, ProbeOptions};
use gm_types::{Arch, DeviceFacts, Gpu, Os, Vendor};

macro_rules! fixture {
    ($path:literal) => {
        include_str!(concat!("fixtures/", $path))
    };
}

const OFF: ProbeOptions = ProbeOptions { experimental_rocm: false };

fn machine(os: Os, gpus: Vec<Gpu>) -> DeviceFacts {
    DeviceFacts {
        os,
        os_version: match os {
            Os::Macos => "14.6.1".into(),
            Os::Windows => "10.0.26100".into(),
            _ => "6.8.0".into(),
        },
        arch: if os == Os::Macos { Arch::Arm64 } else { Arch::X86_64 },
        translated: false,
        hostname: "fixture".into(),
        cpu_model: None,
        cpu_cores: Some(8),
        ram_mb: Some(16384),
        gpus,
        os_name_pretty: None,
    }
}

fn port(os: Os, gpus: Vec<Gpu>, prefer: Prefer) -> String {
    select_profile_with(&machine(os, gpus), prefer, &OFF).port
}

// --- nvidia-smi ----------------------------------------------------------------

#[test]
fn nvidia_blackwell() {
    let gpus = parse_nvidia_smi(fixture!("nvidia/rtx5090_r590.csv"), true);
    assert_eq!(gpus.len(), 1);
    let g = &gpus[0];
    assert_eq!((g.vendor, g.name.as_str(), g.index), (Vendor::Nvidia, "NVIDIA GeForce RTX 5090", 0));
    assert_eq!(g.compute_capability, Some(12.0));
    assert_eq!(g.driver.as_deref(), Some("590.44.01"));
    assert_eq!(g.memory_mb, Some(32607));
    assert!(!g.integrated);
    assert_eq!(port(Os::Linux, gpus, Prefer::Auto), "cuda-cu130");
}

#[test]
fn nvidia_mixed_generations() {
    let gpus = parse_nvidia_smi(fixture!("nvidia/mixed_ada_pascal.csv"), true);
    assert_eq!(gpus.iter().map(|g| g.index).collect::<Vec<_>>(), [0, 1]);
    assert_eq!(gpus[1].compute_capability, Some(6.1));
    let p = select_profile_with(&machine(Os::Linux, gpus), Prefer::Auto, &OFF);
    assert_eq!((p.port.as_str(), p.gpu_index), ("cuda-cu130", Some(0)));
    assert_eq!(p.warnings.len(), 1);
}

#[test]
fn nvidia_na_fields() {
    let gb10 = parse_nvidia_smi(fixture!("nvidia/gb10_memory_na.csv"), true);
    assert_eq!(gb10[0].memory_mb, None);
    assert_eq!(gb10[0].compute_capability, Some(12.1));

    let gpus = parse_nvidia_smi(fixture!("nvidia/na_fields.csv"), true);
    assert_eq!(gpus.len(), 2);
    assert_eq!(gpus[0].compute_capability, None);
    // A "[N/A]" driver is kept verbatim, as hardware.py does.
    assert_eq!(gpus[0].driver.as_deref(), Some("[N/A]"));
    assert_eq!(gpus[0].memory_mb, Some(40960));
    // The A100 with a known capability wins; driver 535 → CUDA 12.6.
    let p = select_profile_with(&machine(Os::Linux, gpus), Prefer::Auto, &OFF);
    assert_eq!((p.port.as_str(), p.gpu_index), ("cuda-cu126", Some(1)));
}

#[test]
fn nvidia_legacy_query() {
    let gpus = parse_nvidia_smi(fixture!("nvidia/legacy_k80.csv"), false);
    assert_eq!(gpus.len(), 2);
    assert!(gpus.iter().all(|g| g.compute_capability.is_none()));
    assert_eq!(gpus[1].driver.as_deref(), Some("470.256.02"));
    // Unknown capability on driver 470 (< 525): blocked on the CPU port.
    let p = select_profile_with(&machine(Os::Linux, gpus), Prefer::Auto, &OFF);
    assert_eq!(p.port, "cpu");
    assert!(p.blocked.is_some());
}

#[test]
fn nvidia_windows_crlf_and_noise() {
    let gpus = parse_nvidia_smi(fixture!("nvidia/laptop_4060_windows.csv"), true);
    assert_eq!(gpus[0].name, "NVIDIA GeForce RTX 4060 Laptop GPU");
    assert_eq!(gpus[0].memory_mb, Some(8188));
    assert!(parse_nvidia_smi(fixture!("nvidia/no_devices.txt"), true).is_empty());
    assert!(parse_nvidia_smi("", true).is_empty());
}

// --- Windows -----------------------------------------------------------------------

#[test]
fn windows_intel_plus_nvidia_laptop() {
    let listed = parse_win32_video_controllers(fixture!("windows/intel_nvidia_laptop.json"));
    assert_eq!(listed.len(), 2);
    assert_eq!((listed[0].vendor, listed[0].integrated), (Vendor::Intel, true));
    assert_eq!(listed[1].vendor, Vendor::Nvidia);
    assert_eq!(listed[1].memory_mb, Some(4095)); // AdapterRAM saturates at 4 GB.

    // nvidia-smi missing: the listed NVIDIA card is kept without a driver.
    let (gpus, note) = assemble_gpus(Vec::new(), listed.clone());
    assert!(note.is_some());
    assert_eq!(gpus[0].vendor, Vendor::Nvidia);
    assert_eq!(gpus[0].driver, None);
    // hardware.py only blocks a driverless NVIDIA card when no Intel GPU
    // exists. UHD is not an XPU part either, so this ends on the plain CPU
    // port ("no supported GPU"), not blocked.
    let p = select_profile_with(&machine(Os::Windows, gpus), Prefer::Auto, &OFF);
    assert_eq!(p.port, "cpu");
    assert!(p.blocked.is_none());

    // With nvidia-smi present, its GPU replaces the listed one.
    let smi = parse_nvidia_smi(fixture!("nvidia/laptop_4060_windows.csv"), true);
    let (gpus, note) = assemble_gpus(smi, listed);
    assert!(note.is_none());
    assert_eq!(gpus.len(), 2);
    assert_eq!(gpus[0].driver.as_deref(), Some("566.36"));
    // Ada on Windows driver 566 (< R580): CUDA 12.6.
    assert_eq!(port(Os::Windows, gpus.clone(), Prefer::Auto), "cuda-cu126");
    assert_eq!(port(Os::Windows, gpus, Prefer::Xpu), "xpu");
}

#[test]
fn windows_arc_a770_single_object() {
    let listed = parse_win32_video_controllers(fixture!("windows/arc_a770_single.json"));
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].name, "Intel(R) Arc(TM) A770 Graphics");
    assert!(!listed[0].integrated);
    assert_eq!(listed[0].driver.as_deref(), Some("32.0.101.6083"));
    assert_eq!(port(Os::Windows, listed, Prefer::Auto), "xpu");
}

#[test]
fn windows_core_ultra_drops_virtual_adapters() {
    let listed = parse_win32_video_controllers(fixture!("windows/core_ultra_virtual.json"));
    assert_eq!(listed.len(), 3);
    assert_eq!(listed[1].vendor, Vendor::Other);
    let (gpus, _) = assemble_gpus(Vec::new(), listed);
    assert_eq!(gpus.len(), 1);
    assert!(gpus[0].integrated);
    let p = select_profile_with(&machine(Os::Windows, gpus), Prefer::Auto, &OFF);
    assert_eq!((p.port.as_str(), p.gpu_name.as_deref()), ("xpu", Some("Intel(R) Arc(TM) Graphics")));
    assert!(p.warnings.is_empty());
}

#[test]
fn windows_amd_radeon() {
    let listed = parse_win32_video_controllers(fixture!("windows/amd_radeon.json"));
    assert_eq!(listed.iter().map(|g| g.vendor).collect::<Vec<_>>(), [Vendor::Amd, Vendor::Amd]);
    assert!(!listed[0].integrated && listed[1].integrated);
    let p = select_profile_with(&machine(Os::Windows, listed), Prefer::Rocm, &OFF);
    assert_eq!(p.port, "cpu"); // ROCm is Linux-only.
    assert_eq!(p.gpu_name.as_deref(), Some("AMD Radeon RX 7900 XTX"));
}

#[test]
fn windows_vm_basic_display_with_bom() {
    let listed = parse_win32_video_controllers(fixture!("windows/vm_basic_display.json"));
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].vendor, Vendor::Other);
    assert_eq!(listed[0].memory_mb, None);
    let (gpus, _) = assemble_gpus(Vec::new(), listed);
    assert!(gpus.is_empty());
    assert!(parse_win32_video_controllers("not json").is_empty());
    assert!(parse_win32_video_controllers("").is_empty());
}

#[test]
fn windows_inventory() {
    let inv = parse_windows_inventory(fixture!("windows/iris_xe_inventory.json")).unwrap();
    assert_eq!(inv.cpu_model.as_deref(), Some("11th Gen Intel(R) Core(TM) i7-1165G7 @ 2.80GHz"));
    assert_eq!(inv.cpu_threads, Some(8));
    assert_eq!(inv.os_caption.as_deref(), Some("Microsoft Windows 11 Home"));
    assert_eq!(inv.os_version.as_deref(), Some("10.0.22631"));
    assert_eq!(inv.ram_mb, Some(16109));
    let p = select_profile_with(&machine(Os::Windows, inv.gpus), Prefer::Auto, &OFF);
    assert_eq!(p.port, "xpu");
    assert_eq!(p.warnings.len(), 1); // Iris Xe is experimental.

    let inv = parse_windows_inventory(fixture!("windows/optimus_inventory.json")).unwrap();
    assert_eq!(inv.gpus.len(), 2);
    assert_eq!(inv.cpu_threads, Some(22));
    assert!(parse_windows_inventory("garbage").is_none());
}

// --- Linux ------------------------------------------------------------------------

#[test]
fn lspci_desktop_nvidia_with_amd_igpu() {
    let listed = parse_lspci_mm(fixture!("linux/lspci_desktop_nvidia.txt"));
    assert_eq!(listed.len(), 2);
    assert_eq!(listed[0].name, "NVIDIA Corporation AD102 [GeForce RTX 4090]");
    assert_eq!(listed[1].name, "Advanced Micro Devices, Inc. [AMD/ATI] Raphael");
    assert_eq!((listed[1].vendor, listed[1].integrated), (Vendor::Amd, true));
    // No nvidia-smi → driver not installed → blocked, AMD ignored.
    let (gpus, _) = assemble_gpus(Vec::new(), listed);
    let p = select_profile_with(&machine(Os::Linux, gpus), Prefer::Auto, &OFF);
    assert_eq!(p.port, "cpu");
    assert!(p.blocked.is_some() && p.fix.is_some());
}

#[test]
fn lspci_laptop_optimus() {
    let listed = parse_lspci_mm(fixture!("linux/lspci_laptop_optimus.txt"));
    assert_eq!(listed.len(), 2);
    assert_eq!(listed[1].vendor, Vendor::Nvidia); // "3D controller" counts.
    let (gpus, _) = assemble_gpus(Vec::new(), listed);
    // Driverless NVIDIA next to Iris Xe: the Intel GPU is tried instead.
    let p = select_profile_with(&machine(Os::Linux, gpus), Prefer::Auto, &OFF);
    assert_eq!(p.port, "xpu");
    assert_eq!(p.gpu_name.as_deref(), Some("Intel Corporation Alder Lake-P GT2 [Iris Xe Graphics]"));
}

#[test]
fn lspci_arc_a770_beats_uhd() {
    let listed = parse_lspci_mm(fixture!("linux/lspci_arc_a770.txt"));
    assert_eq!(listed.len(), 2); // "Display controller" counts too.
    assert!(listed[0].integrated && !listed[1].integrated);
    let p = select_profile_with(&machine(Os::Linux, listed), Prefer::Auto, &OFF);
    assert_eq!(p.gpu_name.as_deref(), Some("Intel Corporation DG2 [Arc A770]"));
}

#[test]
fn lspci_core_ultra() {
    let listed = parse_lspci_mm(fixture!("linux/lspci_core_ultra.txt"));
    assert_eq!(listed[0].vendor, Vendor::Intel);
    assert_eq!(port(Os::Linux, listed, Prefer::Auto), "xpu");
}

#[test]
fn lspci_amd_and_rocm_opt_in() {
    let mut listed = parse_lspci_mm(fixture!("linux/lspci_amd_radeon.txt"));
    assert_eq!(listed.len(), 1);
    assert!(!listed[0].integrated);
    assert_eq!(port(Os::Linux, listed.clone(), Prefer::Rocm), "cpu"); // no ROCm runtime
    let version = parse_rocm_version(fixture!("linux/rocm_version.txt")).unwrap();
    assert_eq!(version, "6.4.1");
    listed[0].driver = Some(format!("rocm {version}"));
    assert_eq!(port(Os::Linux, listed.clone(), Prefer::Auto), "cpu");
    assert_eq!(port(Os::Linux, listed.clone(), Prefer::Rocm), "rocm");
    let facts = machine(Os::Linux, listed);
    assert_eq!(gm_probe::rocm_version(&facts), Some("6.4.1"));
    let p = select_profile_with(&facts, Prefer::Auto, &ProbeOptions { experimental_rocm: true });
    assert_eq!(p.port, "rocm");
    assert!(p.reason.contains("6.4.1"));
}

#[test]
fn lspci_server_and_vm_adapters() {
    let server = parse_lspci_mm(fixture!("linux/lspci_server_aspeed.txt"));
    assert_eq!(server.iter().map(|g| g.vendor).collect::<Vec<_>>(), [Vendor::Other, Vendor::Nvidia]);
    let vm = parse_lspci_mm(fixture!("linux/lspci_vm.txt"));
    assert_eq!(vm.len(), 2);
    assert!(vm.iter().all(|g| g.vendor == Vendor::Other));
    assert!(assemble_gpus(Vec::new(), vm).0.is_empty());
}

#[test]
fn drm_cards_without_lspci() {
    let entries: Vec<(String, String)> = [
        ("card1", "0x10de\n"),
        ("card0-DP-1", "0x8086\n"),
        ("card0", "0x8086\n"),
        ("renderD128", "0x8086\n"),
        ("card10", "0x1002\n"),
        ("card2", "0x1af4\n"),
    ]
    .iter()
    .map(|(a, b)| (a.to_string(), b.to_string()))
    .collect();
    let gpus = parse_drm_cards(&entries);
    let names: Vec<&str> = gpus.iter().map(|g| g.name.as_str()).collect();
    // Lexicographic, like Python's sorted(): card10 before card2.
    assert_eq!(names, ["intel GPU (card0)", "nvidia GPU (card1)", "amd GPU (card10)", "unknown GPU (card2)"]);
    assert_eq!(gpus.iter().map(|g| g.index).collect::<Vec<_>>(), [0, 1, 2, 3]);
    assert_eq!(gpus[3].vendor, Vendor::Other);
}

#[test]
fn linux_system_files() {
    assert_eq!(parse_meminfo(fixture!("linux/meminfo.txt")), Some(63949));
    assert_eq!(parse_meminfo("garbage"), None);
    let (model, count) = parse_cpuinfo(fixture!("linux/cpuinfo_x86.txt"));
    assert_eq!(model.as_deref(), Some("AMD Ryzen 9 7950X 16-Core Processor"));
    assert_eq!(count, Some(2));
    let (model, count) = parse_cpuinfo(fixture!("linux/cpuinfo_rpi.txt"));
    assert_eq!(model.as_deref(), Some("Raspberry Pi 5 Model B Rev 1.0"));
    assert_eq!(count, Some(2));
    assert_eq!(parse_os_release(fixture!("linux/os-release.txt")).as_deref(), Some("Ubuntu 24.04.1 LTS"));
    assert_eq!(parse_os_release("NAME=Arch Linux\n").as_deref(), Some("Arch Linux"));
}

// --- macOS ------------------------------------------------------------------------

#[test]
fn macos_m1_8gb() {
    let sysctl = parse_sysctl(fixture!("macos/sysctl_m1.txt"));
    assert_eq!(sysctl.memsize, Some(8 * 1024 * 1024 * 1024));
    assert_eq!(sysctl.brand.as_deref(), Some("Apple M1"));
    assert!(sysctl.arm64 && !sysctl.translated);
    let ram = sysctl.memsize.map(|b| b / (1024 * 1024));
    let gpus = parse_system_profiler(fixture!("macos/system_profiler_m1.json"), ram);
    assert_eq!(gpus.len(), 1);
    let g = &gpus[0];
    assert_eq!((g.name.as_str(), g.cores, g.memory_mb), ("Apple M1, 8-core GPU", Some(8), Some(8192)));
    assert!(g.integrated);
    assert_eq!(parse_sw_vers(fixture!("macos/sw_vers.txt")), "14.6.1");

    let mut facts = machine(Os::Macos, gpus);
    facts.ram_mb = ram;
    let p = select_profile_with(&facts, Prefer::Auto, &OFF);
    assert_eq!((p.port.as_str(), p.gpu_name.as_deref()), ("mps", Some("Apple M1, 8-core GPU")));
    assert!(p.warnings[0].starts_with("8 GB of unified memory"));
}

#[test]
fn macos_m3_max() {
    let gpus = parse_system_profiler(fixture!("macos/system_profiler_m3_max.json"), Some(131072));
    assert_eq!(gpus[0].name, "Apple M3 Max, 40-core GPU");
    assert_eq!(gpus[0].cores, Some(40));
    let p = select_profile_with(&machine(Os::Macos, gpus), Prefer::Auto, &OFF);
    assert_eq!(p.port, "mps");
    assert!(p.warnings.is_empty() && p.blocked.is_none());
}

#[test]
fn macos_rosetta_and_fallback_gpu() {
    let sysctl = parse_sysctl(fixture!("macos/sysctl_rosetta.txt"));
    assert!(sysctl.translated && sysctl.arm64);
    assert!(parse_system_profiler(fixture!("macos/system_profiler_empty.json"), None).is_empty());
    assert!(parse_system_profiler("not json", None).is_empty());
    let gpus = apple_fallback_gpus(sysctl.brand.as_deref(), sysctl.arm64, Some(16384));
    assert_eq!(gpus[0].name, "Apple Silicon"); // VirtualApple is not a chip name.
    let gpus = apple_fallback_gpus(Some("Apple M2 Pro"), true, None);
    assert_eq!(gpus[0].name, "Apple M2 Pro");
    assert!(apple_fallback_gpus(Some("Apple M2"), false, None).is_empty());
}

#[test]
fn macos_intel_mac() {
    let sysctl = parse_sysctl(fixture!("macos/sysctl_intel_mac.txt"));
    assert!(!sysctl.arm64 && !sysctl.translated);
    let gpus = parse_system_profiler(fixture!("macos/system_profiler_intel_mac.json"), Some(16384));
    assert_eq!(gpus.iter().map(|g| g.vendor).collect::<Vec<_>>(), [Vendor::Intel, Vendor::Amd]);
    assert_eq!(gpus[1].memory_mb, None); // Only Apple GPUs get unified memory.
    let mut facts = machine(Os::Macos, gpus);
    facts.arch = Arch::X86_64;
    let p = select_profile_with(&facts, Prefer::Auto, &OFF);
    assert!(p.blocked.is_some());
    assert_eq!(p.port, "mps"); // _fit_os: the macOS CPU build is the stock wheel.
    assert_eq!(gm_probe::host_profile_with(&facts, Prefer::Auto, &OFF).port, "cpu");
}

#[test]
fn vendor_names() {
    assert_eq!(vendor_from_name("NVIDIA GeForce RTX 4090"), Vendor::Nvidia);
    assert_eq!(vendor_from_name("Quadro P4000"), Vendor::Nvidia);
    assert_eq!(vendor_from_name("Intel(R) Arc(TM) A770 Graphics"), Vendor::Intel);
    assert_eq!(vendor_from_name("AMD Radeon RX 7900 XTX"), Vendor::Amd);
    assert_eq!(vendor_from_name("Apple M1"), Vendor::Apple);
    assert_eq!(vendor_from_name("Microsoft Basic Display Adapter"), Vendor::Other);
    assert_eq!(apple_gpu_name("Apple M1", Some(7)), "Apple M1, 7-core GPU");
    assert_eq!(apple_gpu_name("Apple M1", None), "Apple M1");
}
