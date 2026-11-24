# gm-probe development log

Newest first. Same format as the root `DEVLOG.md`.

---

## 2026-11-24 — gm-probe: hardware detection and port selection (2.0.0-dev)

### What
- `crates/gm-probe` is a Rust port of `desktop/engine/hardware.py`:
  - `detect() -> DeviceFacts`: nvidia-smi (current query, with the
    pre-`compute_cap` fallback and the Windows `%SystemRoot%\System32` copy);
    on Windows one PowerShell CIM query (`Win32_VideoController`, CPU, OS, RAM;
    run with `CREATE_NO_WINDOW`, no wmic); on Linux `lspci -mm`, then
    `/sys/class/drm/card*/device/vendor`, plus `/proc/meminfo`, `/proc/cpuinfo`
    and `/etc/os-release`; on macOS `sysctl`, `sw_vers` and
    `system_profiler SPDisplaysDataType -json`. Every command has a timeout
    (8 s; 15 s for PowerShell/system_profiler). nvidia-smi and the OS query
    run in parallel.
  - `parse::*`: every parser is a pure function over the tool's text, tested
    with captured-style output in `tests/fixtures/{nvidia,windows,linux,macos}`
    (`[N/A]` fields, legacy K80 query, mixed Ada+Pascal, GB10 without memory,
    Intel iGPU + NVIDIA laptop, Arc A770 as a bare JSON object, Core Ultra with
    virtual adapters, AMD Radeon, BOM-prefixed "Microsoft Basic Display
    Adapter", M1 8 GB, M3 Max, Rosetta, Intel Mac, VMs, containers without lspci).
  - `select_profile` / `host_profile` (and `_with` variants taking
    `ProbeOptions`) reproduce `select_profile_for` / `host_profile`, with port
    ids in place of requirement files.
  - New, opt-in only: an AMD GPU on Linux with ROCm (`/opt/rocm` or
    `rocminfo`) gets the experimental `rocm` port when `Prefer::Rocm` is
    chosen or `GRADAPP_EXPERIMENTAL_ROCM=1`. `Auto` is unchanged.
  - `machine_fingerprint` (sha256 prefix over hostname/os/arch/cpu/GPU names)
    and `summary` (one line for logs and the UI).
- `tests/fixtures/gen_parity.py` imports hardware.py by path and records its
  verdict on 64 fabricated machines × several `prefer` values (103 cases) in
  `parity_cases.json`; `tests/parity.rs` replays them.

### Why
- The app must choose the same port the one-line join (still Python) would
  choose for the same machine; otherwise a v1 and a v2 install on one machine
  disagree. The parity test makes "the same" checkable and keeps it so.
- Parsers separate from command execution so each source is tested on every
  OS's output from a Linux CI box.

### Differences from hardware.py (deliberate)
- macOS `arch` is the chip's, not the process's: a Rosetta process reports
  `arch = arm64, translated = true`, because ports bring their own native
  CPython. The Rosetta rule itself is ported and parity-tested.
- `sysctl` is run without `-n` and parsed as `name: value`, so an unknown
  name on Intel Macs cannot shift the other values.
- `gpu_index` is `None` for cpu/mps profiles (Python: always `0`).
- ROCm presence is recorded as `driver = "rocm <version>"` on AMD GPUs until
  `DeviceFacts` has a field for it (`rocm_version(facts)` reads it).

### Verified
- `cargo test -p gm-probe`: 38 tests green; parity matches hardware.py on all
  103 cases for **every** profile field, text included (label, reason,
  blocked, fix, warnings). A deliberately corrupted fixture fails the test.
- `cargo clippy -p gm-probe --all-targets -- -D warnings` clean;
  `cargo doc -p gm-probe` warning-free.
- `detect()` on the Linux build container: 0.7 ms, CPU port, no GPU.
- Not verified: compiling for Windows/macOS targets (rustup could not download
  the std components here). Only the `CREATE_NO_WINDOW` block is
  `#[cfg(windows)]`; all other OS paths are selected at run time with `cfg!`
  and are type-checked on Linux.

### Next
- Run `detect()` on real Windows, macOS and NVIDIA/Intel machines and add
  their outputs as fixtures.
- Add a runtimes field (ROCm) to `DeviceFacts` and drop the driver marker.
