# QA review: GradApp 2 crates, ports and v2 workflows

## 2026-11-24: independent review of the 2.0.0-dev crates

**Scope:** `gm-types` (read only), `gm-probe`, `gm-ports`, `gm-net`, `gm-update`,
`gm-supervisor`, `gmctl`, `ports/*.toml`, `.github/workflows/{ci-v2,release-v2,release}.yml`.
`app/` was not reviewed or edited; it belongs to the integration work.

**Method:** read every source file in scope. The Windows and macOS code
(`cfg(windows)`, Job Object FFI, PowerShell/CIM, sysctl and system_profiler
parsing) was checked by hand against windows-sys 0.59 and tokio 1.54 in
`~/.cargo/registry/src`, because those targets cannot be compiled here.
For each confirmed bug I wrote a failing test first, then made the smallest
fix. I also added cross-crate end-to-end tests (`crates/gmctl/tests/e2e.rs`)
and ran `gmctl` against the real Python coordinator.

### Findings

| # | Severity | Crate | Issue | Fix | Test |
|---|---|---|---|---|---|
| 1 | High | gm-probe | Selection could name a port with **no build for the machine's platform**, unblocked: an Intel Mac with `--prefer cpu` got `mps` (arm64 only); linux-arm64 NVIDIA (GH200, DGX Spark/GB10, Jetson) on a pre-R580 driver got `cuda-cu126`/`cuda-cu128` (x86 only); 32-bit ARM / riscv got `cpu`. The install would then fail minutes later inside uv. | `select.rs:701` lets the Intel-Mac/Rosetta rules run before the CPU shortcut. `gate_platform` (`select.rs:388`) blocks any profile whose port lacks the platform, and the fix suggests R580 when cu130 would fit. `host_profile` then falls back to `cpu` where that exists. New `PORT_PLATFORMS`, `platform()`, `port_runs_on()`. The parity test lists `intel-mac/cpu` and `rosetta/cpu` as deliberate deviations and asserts they block. | `select.rs` tests `intel_mac_asking_for_cpu_is_still_blocked`, `arm64_linux_nvidia_on_old_driver_…`, `unsupported_cpu_architecture_is_blocked`; e2e `every_probed_machine_resolves_to_an_installable_port_or_is_blocked` (fails with 68 problems if the gate is removed) |
| 2 | High | gm-ports | `provision` never checked `spec.platforms`, so nothing downstream stopped finding #1 or a wrongly named port. | `provision.rs:299` returns `Error::UnsupportedPlatform` before anything is deleted or downloaded. Added `Registry::require_on(id, platform)`. The ignored `tiny_port` now uses the current platform (it hard-coded linux-x86_64). | `tests/qa.rs` `provision_refuses_a_port_with_no_build_for_this_platform`, `require_on_checks_platforms`; e2e CLI `ports install mps` refused |
| 3 | High | gm-supervisor | **Process tree left running** on Unix whenever the supervision task is dropped instead of finishing (runtime shutdown, abort, app exit through Drop). `kill_on_drop` only kills the direct child; the worker's DataLoader processes and uvicorn children survived. Windows was already safe because closing the job kills the tree. | `impl Drop for Tree` (`os.rs:114`) sends SIGKILL to the process group. | `tests/qa.rs` `dropping_the_runtime_kills_the_whole_tree` (failed before: grandchild outlived the runtime) |
| 4 | Medium | gmctl | `gmctl join` **hung forever** after the worker exited cleanly (state `Stopped` was ignored). A lagged broadcast receiver could also miss the `Failed` event. | The loop now waits on `handle.wait()` (`main.rs:460`), drains the remaining lines, and reports the exit. | Manual review (bin-internal loop); supervisor end states covered by the e2e worker test |
| 5 | Medium | gmctl | `ports install` with an auto-picked port ignored `blocked` (it would install `mps` on an Intel Mac, or a driver-blocked CUDA port). `ports gc` by default kept only the worker port, so it deleted the host's `cpu` port on a machine whose GPU is blocked. | `pick_installable_port` (`main.rs:126`); `gc` keeps both the select and host ports (`main.rs:342`). | e2e `gmctl_version_probe_and_ports` |
| 6 | Medium | gm-ports | uv found on `PATH` or left in `<data>/bin` was used without a version check. A uv older than 0.8 has no `python install --no-bin`/`--no-registry`, so every provision fails on that machine. | `MIN_UV_VERSION = 0.8.0`, `parse_uv_version`, `is_usable`. `ensure` (`uv.rs:191`) downloads the pinned uv instead of a too-old one; `GRADAPP_UV` and bundled copies are still trusted. | `tests/qa.rs` `uv_version_parsing`, `old_uv_is_not_usable` |
| 7 | Medium | CI release-v2 | A **beta tag** (`v2.1.0-beta.1`, advertised in the header) fails the Windows build: WiX/MSI only accepts a numeric pre-release, and `publish` needs every build, so the release never leaves draft. | New `Bundle arguments` step: Windows pre-releases bundle NSIS only (the updater uses NSIS already). Also a guard that fails a tag build if the updater pubkey is the `REPLACE…` placeholder. | YAML parsed; the sed version extraction was run against `tauri.conf.json` |
| 8 | Low | gm-supervisor | `ProcSpec` and `WorkerLaunch` derived `Debug`, so any `{:?}` log, panic or crash report **printed the mesh token** (the token was carefully kept off argv). | Hand-written `Debug` that redacts the token and env vars named `*TOKEN*/*SECRET*/*PASSWORD*/*KEY*`. | `tests/qa.rs` `debug_output_never_contains_the_token` (failed before) |
| 9 | Low | gm-net | `directed_broadcast` computed `u32::MAX << 32` for a /0 netmask (some VPN/PPP adapters): a panic in debug builds, killing the beacon task; in release it sent the "broadcast" to the host's own address. | Only /1–/30 get a directed broadcast (`beacon.rs:179`). | `directed_broadcast_survives_prefix_zero` (failed before) |
| 10 | Low | gm-net | Manual addresses used `Url::port()`, which is `None` for the scheme default, so a coordinator on port 80 was silently skipped. The beacon task was the one producer not wrapped in the cancel token, so it could wait on a full channel after the consumer stopped on cancel. | `port_or_known_default()`; `run_until_cancelled(listen_beacons…)` (`discovery.rs:267,347`). | Existing cancel tests; review |
| 11 | Low (test) | gm-net | `discover_is_cancellable` **flaked in the full run**: it used the real `gradmesh.local`/`_gradmesh._tcp`, so a live coordinator on the LAN answered within 300 ms. `live_coordinator` assumed it was the only mesh on the network. | Unique mDNS names in the cancel test. The live test looks for its own mesh id and asserts no mesh is listed twice. | Whole gm-net suite run 3× with a real mDNS coordinator up; `live_coordinator` passes against a second coordinator |
| 12 | Low (doc) | gm-ports | `machine_fingerprint(&facts)` was documented as equal to `local_machine_fingerprint()`. It isn't: on Windows `%COMPUTERNAME%` is NetBIOS (cut to 15 chars) while gethostname is the DNS name, and Rosetta differs too. Using one for `provision` and the other for `status` would make every lock Stale and **rebuild the port on every start**. The app and gmctl currently use `local_…` everywhere. | Doc corrected (`plan.rs:84`). | n/a |
| 13 | Note | gm-net | The join surface hands the token to any client that can reach :3000, including a web page doing **DNS rebinding** (no Host check), so the boundary is "anyone browsing from the LAN", not "on the LAN". Same as v1 and GradMesh. | Not changed (design decision). Option: answer only when Host is an IP literal, `localhost`, `*.local` or this machine's name (`join.rs:258`). | n/a |
| 14 | Note | gm-supervisor (Windows) | `AssignProcessToJobObject` runs after `spawn`. The venv's `Scripts\python.exe` is a launcher that starts the real interpreter immediately, so the real python may land outside our job. CPython's launcher puts its own child in a kill-on-close job, so killing the launcher should still end it. | Not changed (untestable here). The robust fix is CREATE_SUSPENDED → assign → resume. | Hardware checklist below |
| 15 | Note | gm-ports | No lock between two concurrent provisions of the same port (app and `gmctl`, or a double click); `provision` also `remove_dir_all`s an env a running worker may be using. A lock written by `--skip-verify` looks the same as a verified one. | Not changed; the app should stop the worker and serialise installs. | n/a |
| 16 | Note | gm-net | For this machine's own mesh, "first IP wins" can keep `127.0.0.1` (from a manual `localhost`) instead of the LAN IP. Fine locally, wrong if it is used to build an invite for others. | Not changed; the app should build invites from `primary_lan_address()`. | Seen in the `gmctl discover --manual 127.0.0.1` smoke run |

Checked and found correct: the Job Object FFI against windows-sys 0.59
(`HANDLE = *mut c_void`; `CreateJobObjectW`, `SetInformationJobObject`,
`AssignProcessToJobObject` and `TerminateJobObject` signatures;
`JOBOBJECT_EXTENDED_LIMIT_INFORMATION` needs `Win32_System_Threading`, which
is enabled); `tokio::process::Child::raw_handle`, `Command::creation_flags`
and `process_group`; `CREATE_NO_WINDOW` in gm-probe (std `CommandExt`) and
gm-supervisor; `Scripts\python.exe` and `uv.exe`; `--no-registry` only on
Windows; the macOS `sysctl name: value` parsing; the system_profiler JSON
keys; join-surface path traversal (exact allowlist after decoding); the
`ready ⇔ lock matches plan and python starts` rule; `tauri-action@v0` inputs
(`projectPath`, `tauriScript`, `args`, `releaseId`, `includeUpdaterJson`,
`updaterJsonPreferNsis`); the tag/version check; bundle paths; the
`latest.json` platform check (universal macOS writes both darwin keys);
`release.yml` being limited to `v1.*` tags; libxdo not being needed
(tauri builds muda without default features).

### Tests added

- gm-probe: 4 unit tests and the parity deviation list.
- gm-ports: `tests/qa.rs` (4).
- gm-supervisor: `tests/qa.rs` (2).
- gm-net: 1 unit test, plus fixes to 2 tests.
- gmctl: `tests/e2e.rs` (7, plus 1 ignored):
  - Beacon + real `JoinSurface` + mock `/health`: found with its token in < 1.5 s, listed once.
  - The same mesh via beacon, manual and sweep: listed once.
  - Every parity machine plus 10 extra machines × every `prefer` × ROCm on/off (> 400 profiles) resolves to a manifest that exists and runs on that platform, with the same backend and torch, or is blocked with a reason.
  - gm-probe's platform table equals the manifests.
  - `worker_spec` runs a stand-in worker.py: token in env not argv, SIGINT "leaving" path, a SIGINT-ignoring grandchild killed by `stop()`.
  - Ignored: real uv builds a tiny port (private CPython 3.12 + six), then gm-supervisor runs that env's python and streams its output; `sys.base_prefix` is inside `<root>/.python`.
  - The `gmctl` binary: `version`, `probe --json`, `ports list/plan/status`, `install` of a foreign port refused, bad `--prefer` rejected, `discover --json`.

### Gates (2026-11-24, Linux x86_64)

- `cargo fmt --all --check`: clean.
- `cargo clippy --workspace --exclude gradapp --all-targets -- -D warnings`: clean.
- `cargo test --workspace --exclude gradapp`: **171 passed, 0 failed, 6 ignored**. Run with a real GradMesh coordinator (mDNS on) on :8000.
- `--ignored`:
  - gmctl e2e provisioning: pass.
  - gm-ports `provision_end_to_end`, `download_pinned_uv`, `ports_resolve_without_conflicts`: pass.
  - `resolve_with_pytorch_indexes`: fails only with `tunnel error` on download.pytorch.org (blocked here; mps from PyPI resolves).
  - gm-net `live_coordinator` against a private coordinator on :8999: pass (mDNS hit in 87 ms; full discovery listed both LAN meshes once each).
- `gmctl` smoke against the real :8000 coordinator: probe (cpu, no GPU), `ports list/plan/status`, `discover` (2.5 s, both meshes, no duplicates).

### Needs real hardware / OS (not verifiable in this container)

- [ ] **Windows 11 x64 + NVIDIA RTX 40/50, driver ≥ R580.**
  - `gmctl probe` picks cu130.
  - `ports install` verify passes.
  - `join`, then Ctrl+C: no `python.exe` left in Task Manager. This also tests the job-assignment race (#14).
  - No console window flashes (PowerShell probe, uv, worker).
- [ ] **Windows 11 x64 + NVIDIA on R570–R579 with a Blackwell card.** cu128 fallback warning; Windows Firewall prompt on UDP 47800 / TCP 3000.
- [ ] **Windows 11 x64 + Intel Arc A-series / Core Ultra iGPU.** xpu port and kernel verify; check that CIM JSON from Windows PowerShell 5.1 parses (single adapter = bare object; watch for the `{"value":[…],"Count":n}` array wrapper).
- [ ] **Windows 11 on ARM (Snapdragon X).** `cpu` port from `whl/cpu` `+cpu` wheels; uv aarch64-pc-windows-msvc download.
- [ ] **Windows hostname longer than 15 characters.** Port stays Ready across restarts (no rebuild loop; #12).
- [ ] **macOS 14/15 Apple Silicon (M1 8 GB, M3 Max).**
  - system_profiler/sysctl parse.
  - mps verify.
  - Universal app not "damaged" after download (unsigned/notarisation).
  - Updater installs in place.
- [ ] **macOS on Intel.** Blocked with the Intel-Mac message for every `--prefer`; `ports install` refused.
- [ ] **macOS universal app forced to run under Rosetta.** `arch = arm64, translated = true`; port still mps.
- [ ] **Ubuntu 22.04/24.04 x86_64 + NVIDIA (Ampere/Ada) on R535 and R580.** cu126 vs cu130.
- [ ] **Linux, Pascal or older card.** cu126; a Kepler card gets cpu.
- [ ] **Linux arm64 + NVIDIA (GH200 or DGX Spark).** cu130 on R580; blocked with the R580 fix on R570.
- [ ] **Linux x86_64 + Intel Arc.** xpu with only the compute runtime (no oneAPI).
- [ ] **Linux x86_64 + AMD Radeon with ROCm and `GRADAPP_EXPERIMENTAL_ROCM=1`.** The `whl/rocm7.2` index exists; HIP verify.
- [ ] **Raspberry Pi 5 (arm64), and 32-bit Pi OS.** cpu, and blocked respectively.
- [ ] **Two GradApp hosts plus a v1 host on one Wi-Fi with client isolation.** Beacon, mDNS and sweep each find each mesh once; time-to-first-mesh.
- [ ] **A real `v2.x.y-beta.1` tag.** Windows NSIS-only build, all four platforms in `latest.json`, the Beta channel offers it.
