# GradApp development log

Every change to GradApp gets an entry here: **what** changed, **why**, and
**how it was checked**. Newest first. One entry per working session; add to
today's entry instead of opening a second one on the same day.

Format:

```
## YYYY-MM-DD — short title (version)
### What
### Why
### Verified
### Next
```

---

## 2026-11-24 — GradApp 2.0 begins: Electron → Rust (2.0.0-dev)

### What
- New branch `v2-rust`. The Electron app in `desktop/` (GradApp 1.0.0) is
  frozen as the reference implementation and stays buildable until 2.0 ships.
- New Cargo workspace at the repo root:
  - `crates/gm-types` — the shared contract (device facts, backends, ports,
    mesh protocol, update info, UI events).
  - `crates/gm-probe` — hardware detection and port selection (replaces
    `engine/hardware.py` for the app; the Python stays for the one-line join).
  - `crates/gm-ports` — **ports**: pinned per-backend runtimes provisioned
    with uv and a managed CPython.
  - `crates/gm-net` — discovery (UDP beacon, mDNS, /24 sweep), invites,
    coordinator client.
  - `crates/gm-update` — update check against GitHub Releases.
  - `crates/gm-supervisor` — child process supervision (worker, coordinator).
  - `crates/gmctl` — headless CLI over the same crates.
  - `app/` — the Tauri 2 desktop app with the GradMesh v5 interface.
- `ports/*.toml` — one manifest per backend.
- Docs: this log, `docs/ARCHITECTURE.md`, `docs/FUNCTIONALITY.md`, `docs/PORTS.md`.

### Why
- **Size and speed.** Electron ships a whole Chromium (~150 MB installers,
  ~300 MB RAM idle). Tauri uses the OS webview with a Rust core: installers
  around 10 MB, a fraction of the memory, and native async networking.
- **The recurring failure was the Python environment, not the GPU.** v1 used
  whatever Python the machine had (3.9? 3.14? a Rosetta build?) and PyTorch
  wheels only exist for some of them. Ports fix that: each port names the exact
  CPython it runs on and uv downloads that interpreter privately, so the
  machine's own Python is never involved and can never conflict.
- **One place per decision.** Device → port selection is a pure function in
  `gm-probe`, port contents are data in `ports/*.toml`, and both are tested.
- **Fewer moving parts to install.** No Node, no npm, no system Python needed
  on any machine. uv (a single Rust binary) is the only helper.
- **Updates in the app.** v1 only linked to the download page. v2 shows an
  "Update available" dialog and installs signed updates in place.

### Kept from v1 (deliberately)
- The Python engine (`desktop/engine`): worker, trainers, coordinator. PyTorch
  training stays Python. Porting the coordinator to Rust is phase 2.
- Wire compatibility: same ports (8000 coordinator, 3000 join), same
  `/health`, same `/gradapp.json` and `/join.sh` invite surfaces, so v1 apps,
  v2 apps and the one-line join all meet on one mesh.

### How it was built
Five agents worked in parallel, one per crate/app, each with its own tests,
then an integration pass. Each agent's detailed notes are in `docs/devlog/`:
[gm-probe](docs/devlog/gm-probe.md) · [gm-ports](docs/devlog/gm-ports.md) ·
[gm-net](docs/devlog/gm-net.md) · [gm-update + gm-supervisor](docs/devlog/gm-update-supervisor.md) ·
[app](docs/devlog/app.md).

### Decisions made today (and why)
- **Tauri 2, not a pure-Rust UI toolkit.** The v5 look (three.js station,
  radar, glass panels) needs a real web renderer; Tauri gives that with a Rust
  core and the OS webview, without shipping Chromium.
- **No frontend build step.** Plain ES modules + vendored three.js 0.171. One
  less toolchain on every machine and in CI.
- **Port selection is parity-tested against Python.** 103 machine/preference
  cases generated from `hardware.py` must give identical results in Rust, so
  the app and the one-line join never disagree about a machine.
- **Deliberate differences from Python:** Rosetta doesn't block (ports bring a
  native Python); `gpu_index` is `None` on cpu/mps; ROCm is opt-in only
  (`GRADAPP_EXPERIMENTAL_ROCM=1` or prefer=rocm) and uses the `rocm7.2` index,
  not yet validated on hardware.
- **One directory per port** instead of uninstalling torch in place when the
  machine's port changes.
- **uv 0.11.32 pinned**, downloaded from GitHub with sha256 check when absent.
- **Beacon + query packet.** Hosts multicast every 2 s and also answer a
  query datagram at once, so a listener sees a mesh in well under a second.
  The /24 sweep stays as the fallback for networks that block multicast.
- **Worker token moves from argv to `GRADMESH_TOKEN`**, so it no longer shows
  in Task Manager / `ps`.
- **Update check lists all releases** (not `/releases/latest`), so a v1
  hotfix can never hide a v2 release; ETag-cached, rate-limit safe. Stable
  ignores pre-releases; Beta includes them.
- **Release workflows split by tag:** `v1.*` → Electron (`release.yml`),
  `v2.*` → Tauri (`release-v2.yml`), which publishes only after `latest.json`
  covers all four platforms. `ci-v2.yml` runs fmt, clippy, tests on 3 OSes and
  re-checks the parity fixture.
- **Updater signing key generated** (public key in `tauri.conf.json`; the
  private key is kept out of git — see SETUP in the session notes / GitHub
  secrets `TAURI_SIGNING_PRIVATE_KEY`, `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`).

### Verified
- `cargo test` (default members): ~190 tests green; `cargo clippy --workspace
  --all-targets -D warnings` and `cargo fmt --check` clean; `cargo build -p
  gradapp` clean.
- gm-ports network tests (run with `--ignored`): end-to-end provision of a
  test port with a private CPython 3.12 via uv; pinned uv download +
  checksum; **all 17 port × platform pairs resolve without conflicts** against
  PyPI. The PyTorch-index resolution test needs a normal network.
- gm-net against the real Python coordinator: found by mDNS in ~0.5 s and by
  the sweep in ~50 ms; `gmctl discover` finds it in 2.5 s total. `/join.sh`
  and `/join.ps1` are byte-identical to v1 (fixtures generated from v1's own
  `beacon.js` under Node).
- The app runs under Xvfb with the real Tauri window; every screen and the
  update dialog screenshotted in `docs/screenshots/`.

### Not verified yet (needs real machines)
- Windows-only and macOS-only code paths have not been compiled (the
  windows/macOS Rust targets could not be downloaded in the build sandbox);
  the first `ci-v2.yml` run on GitHub is that check.
- No real GPU has run a port's verification kernel yet.

### Open at end of day
- `app/src-tauri/src/backend.rs` still has `// INTEGRATE:` stubs: the window
  shows simulated join/host/install. The crates behind them are finished, and
  `gmctl` already runs the real path (probe → port → worker). Integration had
  started (crates added as deps, `UpdateInfo.reason/manifest_url`,
  `AppEvent::MeshEvent`) when the session hit its usage limit.
- Independent QA review pass (cross-platform `cfg` review, e2e tests) — not
  started for the same reason.

### Next
1. Finish wiring `backend.rs` to the crates; live-test join/host on the LAN.
2. QA pass + `crates/gmctl/tests/e2e.rs`.
3. Push `v2-rust`, let `ci-v2.yml` compile on Windows/macOS, fix what it finds.
4. Tag `v2.0.0-beta.1`, install on the M1 Air, an NVIDIA PC and an Arc PC,
   then tag `v2.0.1-beta.1` to prove the in-app update popup end to end.
- Longer term: see the Roadmap in `docs/ARCHITECTURE.md`.
