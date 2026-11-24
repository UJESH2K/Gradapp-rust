"""Generate parity_cases.json: hardware.py's verdict on many fabricated machines.

Run from anywhere (the paths are resolved from this file):

    python3 -I crates/gm-probe/tests/fixtures/gen_parity.py

It imports desktop/engine/hardware.py by path, builds HostFacts for every
branch of select_profile, runs both select_profile_for() and host_profile()
for each, and writes the results next to this script. tests/parity.rs replays
every case through gm-probe and asserts the same outcome field by field.
Re-run it whenever hardware.py changes, and commit the JSON.
"""

from __future__ import annotations

import importlib.util
import json
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[3]
ENGINE = REPO / "desktop" / "engine"

# hardware.py does `from version import REFERENCE_STACK`.
sys.path.insert(0, str(ENGINE))
_spec = importlib.util.spec_from_file_location("gm_hardware", ENGINE / "hardware.py")
hw = importlib.util.module_from_spec(_spec)
sys.modules[_spec.name] = hw  # dataclasses look their module up here
_spec.loader.exec_module(hw)

PORTS = {
    "requirements-train-cu130.txt": "cuda-cu130",
    "requirements-train-cu126.txt": "cuda-cu126",
    "requirements-train-cu128.txt": "cuda-cu128",
    "requirements-xpu.txt": "xpu",
    "requirements-mps.txt": "mps",
    "requirements-train-cpu.txt": "cpu",
}


def nv(name, cc, driver, index=0, mem=None):
    return hw.Gpu(vendor="nvidia", name=name, index=index, compute_capability=cc, driver=driver, memory_mb=mem)


def gpu(vendor, name, index=0, **extra):
    return hw.Gpu(vendor=vendor, name=name, index=index, **extra)


def host(os_name="linux", gpus=(), os_version=None, arch="x86_64", ram_mb=32768, translated=False):
    if os_version is None:
        os_version = {"linux": "6.8.0-45-generic", "windows": "10.0.26100", "macos": "15.1"}[os_name]
    return hw.HostFacts(
        os=os_name,
        os_version=os_version,
        arch=arch,
        python="3.12.7",
        hostname="case-host",
        cpu_count=16,
        ram_mb=ram_mb,
        gpus=list(gpus),
        translated=translated,
    )


def mac(chip="Apple M3 Max, 40-core GPU", version="15.1", ram_mb=65536, arch="arm64", translated=False, gpus=None):
    if gpus is None:
        gpus = [gpu("apple", chip, memory_mb=ram_mb, cores=40)] if chip else []
    return host("macos", gpus, os_version=version, arch=arch, ram_mb=ram_mb, translated=translated)


RTX5090 = lambda driver, index=0: nv("NVIDIA GeForce RTX 5090", 12.0, driver, index, 32607)  # noqa: E731
RTX4090 = lambda driver="580.95.05", index=0: nv("NVIDIA GeForce RTX 4090", 8.9, driver, index, 24564)  # noqa: E731
RTX2080TI = lambda driver, index=0: nv("NVIDIA GeForce RTX 2080 Ti", 7.5, driver, index, 11264)  # noqa: E731
GTX1080TI = lambda driver="580.95.05", index=0: nv("NVIDIA GeForce GTX 1080 Ti", 6.1, driver, index, 11264)  # noqa: E731
RTX3090 = lambda driver, index=0: nv("NVIDIA GeForce RTX 3090", 8.6, driver, index, 24576)  # noqa: E731
LAPTOP4060 = nv("NVIDIA GeForce RTX 4060 Laptop GPU", 8.9, "566.36", 0, 8188)
ARC_IGPU = gpu("intel", "Intel(R) Arc(TM) Graphics")
ARC_A770 = gpu("intel", "Intel(R) Arc(TM) A770 Graphics", memory_mb=16288)
ARC_A750 = gpu("intel", "Intel Corporation DG2 [Arc A750]", index=1)
IRIS_XE = gpu("intel", "Intel(R) Iris(R) Xe Graphics")
UHD630 = gpu("intel", "Intel(R) UHD Graphics 630")
METEOR = gpu("intel", "Intel Corporation Meteor Lake-P [Intel Arc Graphics]")
RX7900 = gpu("amd", "AMD Radeon RX 7900 XTX")

CASES = [
    # --- NVIDIA: Blackwell ------------------------------------------------------
    ("blackwell-driver-590", host("linux", [RTX5090("590.44.01")]), ["auto", "cuda"]),
    ("blackwell-driver-575-windows", host("windows", [RTX5090("575.64")]), ["auto"]),
    ("blackwell-driver-560-windows", host("windows", [RTX5090("560.94")]), ["auto"]),
    ("blackwell-driver-na", host("linux", [RTX5090("[N/A]")]), ["auto"]),
    ("blackwell-gb10-no-memory", host("linux", [nv("NVIDIA GB10", 12.1, "580.95.05")], arch="arm64"), ["auto"]),
    ("blackwell-driver-520-linux", host("linux", [RTX5090("520.61.05")]), ["auto"]),
    # --- NVIDIA: older architectures -------------------------------------------
    ("turing-r580", host("linux", [RTX2080TI("580.65.06")]), ["auto"]),
    ("turing-r575", host("windows", [RTX2080TI("575.51")]), ["auto"]),
    ("pascal-r580", host("windows", [GTX1080TI("581.29")]), ["auto"]),
    ("kepler-k80", host("linux", [nv("Tesla K80", 3.7, "470.256.02", 0, 11441), nv("Tesla K80", 3.7, "470.256.02", 1, 11441)]), ["auto", "cuda"]),
    ("maxwell-cc50-edge", host("linux", [nv("NVIDIA GeForce GTX 980", 5.0, "535.183.01", 0, 4096)]), ["auto"]),
    ("unknown-cc-r580", host("linux", [nv("NVIDIA GeForce RTX 3080", None, "580.95.05", 0, 10240)]), ["auto"]),
    ("unknown-cc-driver-na", host("linux", [nv("NVIDIA A100-SXM4-40GB", None, "[N/A]")]), ["auto"]),
    ("zero-cc", host("linux", [nv("NVIDIA Mystery", 0.0, "550.54")]), ["auto"]),
    # --- NVIDIA: driver thresholds ------------------------------------------------
    ("ampere-525-linux-ok", host("linux", [RTX3090("525.60.13")]), ["auto"]),
    ("ampere-525-windows-blocked", host("windows", [RTX3090("525.60")]), ["auto"]),
    ("ampere-527-windows-blocked", host("windows", [RTX3090("527.56")]), ["auto"]),
    ("ampere-528-windows-ok", host("windows", [RTX3090("528.02")]), ["auto"]),
    ("ampere-520-linux-blocked", host("linux", [RTX3090("520.61.05")]), ["auto"]),
    ("ampere-570-cu126", host("linux", [RTX3090("570.133.07")]), ["auto"]),
    # --- NVIDIA: mixed / ties -----------------------------------------------------
    ("mixed-pascal-ada", host("linux", [GTX1080TI(index=0), RTX4090(index=1)]), ["auto", "cuda"]),
    ("mixed-blackwell-pascal-575", host("windows", [GTX1080TI("575.64", 0), RTX5090("575.64", 1)]), ["auto"]),
    ("mixed-blackwell-pascal-560", host("windows", [GTX1080TI("560.94", 0), RTX5090("560.94", 1)]), ["auto"]),
    ("twin-3090-tie", host("linux", [RTX3090("580.95.05", 0), RTX3090("580.95.05", 1)]), ["auto"]),
    ("same-cc-more-memory-wins", host("linux", [nv("NVIDIA RTX A4000", 8.6, "580.95.05", 0, 16376), nv("NVIDIA RTX A6000", 8.6, "580.95.05", 1, 49140)]), ["auto"]),
    ("cc-known-vs-unknown", host("linux", [nv("NVIDIA Card A", None, "580.95.05", 0, 99999), nv("NVIDIA Card B", 7.0, "580.95.05", 1, 16384)]), ["auto"]),
    # --- NVIDIA without driver ------------------------------------------------------
    ("nvidia-no-driver-linux", host("linux", [nv("NVIDIA Corporation AD102 [GeForce RTX 4090]", None, None)]), ["auto", "cuda", "xpu", "mps", "rocm"]),
    ("nvidia-no-driver-plus-iris", host("windows", [nv("NVIDIA GeForce RTX 3050 Laptop GPU", None, None), IRIS_XE]), ["auto", "cuda", "xpu"]),
    ("nvidia-no-driver-plus-uhd", host("windows", [nv("NVIDIA GeForce GTX 1650", None, None), UHD630]), ["auto", "cuda"]),
    # --- Intel ----------------------------------------------------------------------
    ("arc-a770-windows", host("windows", [ARC_A770]), ["auto", "xpu", "cuda"]),
    ("core-ultra-arc", host("windows", [ARC_IGPU]), ["auto"]),
    ("meteor-lake-linux", host("linux", [METEOR]), ["auto"]),
    ("iris-xe", host("windows", [IRIS_XE]), ["auto", "xpu"]),
    ("uhd-630", host("windows", [UHD630]), ["auto", "xpu", "mps"]),
    ("iris-plus-a750", host("linux", [IRIS_XE, ARC_A750]), ["auto", "xpu"]),
    ("uhd-plus-meteor-plus-a770", host("windows", [UHD630, METEOR, ARC_A770]), ["auto"]),
    ("battlemage-b580", host("windows", [gpu("intel", "Intel(R) Arc(TM) B580 Graphics")]), ["auto"]),
    ("data-center-max", host("linux", [gpu("intel", "Intel(R) Data Center GPU Max 1550")]), ["auto"]),
    # --- Intel + NVIDIA laptop ----------------------------------------------------------
    ("arc-igpu-plus-4060-laptop", host("windows", [LAPTOP4060, ARC_IGPU]), ["auto", "cuda", "xpu", "mps", "cpu", "rocm"]),
    ("iris-plus-3050-linux", host("linux", [nv("NVIDIA GeForce RTX 3050 Laptop GPU", 8.6, "550.120"), IRIS_XE]), ["auto", "xpu"]),
    ("uhd-plus-old-driver-blocked", host("windows", [nv("NVIDIA GeForce GTX 1650", 7.5, "472.12"), UHD630]), ["auto", "xpu"]),
    # --- Apple ---------------------------------------------------------------------------
    ("m1-8gb-macos14", mac("Apple M1, 8-core GPU", "14.6.1", 8192), ["auto", "cuda", "cpu", "mps"]),
    ("m1-8gb-edge-8704", mac("Apple M1, 7-core GPU", "14.0", 8704), ["auto"]),
    ("m1-8705mb-no-warning", mac("Apple M1, 7-core GPU", "14.0", 8705), ["auto"]),
    ("m3-max-macos15", mac("Apple M3 Max, 40-core GPU", "15.1", 65536), ["auto", "xpu", "rocm"]),
    ("m2-macos13-blocked", mac("Apple M2, 10-core GPU", "13.6.7", 16384), ["auto", "cpu"]),
    ("macos-version-14", mac("Apple M2 Pro, 19-core GPU", "14", 32768), ["auto"]),
    ("macos-tahoe-26", mac("Apple M4, 10-core GPU", "26.0", 16384), ["auto"]),
    ("macos-empty-version", mac("Apple M1", "", 16384), ["auto"]),
    ("macos-no-gpu-listed", mac(None, "14.5", 16384), ["auto"]),
    ("mac-ram-2560-bankers", mac("Apple M1", "14.5", 2560), ["auto"]),
    ("mac-ram-4608-bankers", mac("Apple M1", "14.5", 4608), ["auto"]),
    ("mac-ram-7680", mac("Apple M1", "14.5", 7680), ["auto"]),
    ("mac-ram-unknown", mac("Apple M1", "14.5", None), ["auto"]),
    ("rosetta", mac("Apple M2, 10-core GPU", "14.5", 16384, arch="x86_64", translated=True), ["auto", "cpu"]),
    ("rosetta-no-gpu-listed", mac(None, "14.5", 16384, arch="x86_64", translated=True), ["auto"]),
    ("intel-mac", mac(None, "13.7", 16384, arch="x86_64", gpus=[gpu("intel", "Intel UHD Graphics 630"), gpu("amd", "AMD Radeon Pro 5500M", index=1)]), ["auto", "xpu", "cpu"]),
    # --- AMD / none / odd ---------------------------------------------------------------
    ("amd-only-linux", host("linux", [RX7900]), ["auto", "rocm", "cuda"]),
    ("amd-only-windows", host("windows", [RX7900]), ["auto"]),
    ("amd-plus-uhd", host("linux", [UHD630, RX7900]), ["auto"]),
    ("amd-plus-iris", host("linux", [RX7900, IRIS_XE]), ["auto"]),
    ("no-gpu-linux", host("linux", []), ["auto", "cuda", "xpu", "mps", "cpu", "rocm"]),
    ("no-gpu-windows", host("windows", []), ["auto"]),
    ("unknown-vendor-only", host("windows", [gpu("unknown", "Microsoft Basic Display Adapter")]), ["auto"]),
]


def dump(profile: "hw.Profile") -> dict:
    data = profile.as_dict()
    data["port"] = PORTS[profile.requirements]
    return data


def main() -> int:
    cases = []
    for name, facts, prefers in CASES:
        for prefer in prefers:
            cases.append(
                {
                    "name": "%s/%s" % (name, prefer),
                    "prefer": prefer,
                    "facts": facts.as_dict(),
                    "select": dump(hw.select_profile_for(facts, prefer)),
                    "host": dump(hw.host_profile(facts, prefer)),
                }
            )
    out = HERE / "parity_cases.json"
    payload = {
        "generator": "crates/gm-probe/tests/fixtures/gen_parity.py",
        "source": "desktop/engine/hardware.py",
        "reference_torch": hw.REFERENCE_STACK["torch"],
        "cases": cases,
    }
    out.write_text(json.dumps(payload, indent=1, sort_keys=True) + "\n", encoding="utf-8")
    print("wrote %d cases (%d machines) to %s" % (len(cases), len(CASES), out))
    return 0


if __name__ == "__main__":
    sys.exit(main())
