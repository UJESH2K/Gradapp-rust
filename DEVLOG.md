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

## 2026-11-24 (night) — New home: UJESH2K/Gradapp-rust; the G logo in the window

### What
- The Rust app moves to its own repository, **github.com/UJESH2K/Gradapp-rust**
  (full history kept, including GradApp 1, whose engine v2 still runs).
  The updater feed (`tauri.conf.json`, `commands.rs`), `gm-update`'s default
  repo, `gmctl update-check` and the workspace `repository` now point there.
- New `README.md` for the Rust repo; the GradApp 1 README moved to
  `docs/README-v1.md`.
- The window's brand mark was still the GradMesh v5 hexagon-of-nodes. It is
  now the **G logo from `main`** (`desktop/assets/logo.png`, commit
  da38245 "Use the new G logo everywhere"), as `app/ui/brand/g-mark.png`;
  `app/ui/logo.png` (favicon) regenerated from `desktop/assets/icon.png`.
  App icons and tray icons were already generated from that artwork
  (checked byte for byte against `desktop/assets/`).

### Why
- The owner created a dedicated repo for the Rust version, and releases must
  come from the repo the installed app checks, or the update popup never fires.
- The hexagon mark is retired; one logo everywhere.

### Kept on purpose
- The join page's "Get GradApp" link still points at `UJESH2K/gradapp`
  releases: that page is byte-identical to v1's (fixture-tested), and today
  only that repo has installers. Switch it when v2.0.0 ships.

### Verified
- fmt, clippy `-D warnings`, `cargo test --workspace`: 179 passed, 0 failed.
- UI checked in Chrome: new mark renders, no console errors.

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
- End of day, whole workspace incl. the app: **179 passed, 0 failed** (6
  network tests ignored by default); `cargo clippy --workspace
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

### Paused at 16:30 (usage limit), resumed 23:05
The first pass stopped with the window still on placeholder data and the QA
pass not started. Both were resumed in parallel; see the next two sections.

### Later the same day: the app on the real crates
- **What:** every `// INTEGRATE:` stub in `app/src-tauri` replaced: probe
  (cached, preferred-backend override), ports from gm-ports with real
  installs (one at a time), cancellable discovery, join (address → health →
  token → port → supervised worker, milestones, `/leave` after stop), host
  (coordinator + join page :3000 + beacon), mesh/runs/datasets/live feed
  from the coordinator (`app/src-tauri/src/mesh.rs`), gm-update checks with
  the Tauri updater for install (beta via the release's own `latest.json`),
  daily log files in `<data>/logs`, engine bundled as `engine/`.
  Details: `docs/devlog/integration.md`.
- **Why:** the window was a simulation. Wiring also found two bugs: the
  Discover join link used `host=` (the real parser only reads `server=`) and
  auto-rejoin ignored an explicit "Stop contributing".
- **Verified:** all four gates green; live under Xvfb against the real
  coordinator: discovery, a real uv/CPython port install (torch-free test
  port), hosting end to end, join up to the worker spawn (it stops at
  `import torch`), leave, host stop, quit cleanup
  (`docs/screenshots/real-*.png`).

### Later the same day: independent QA review
- **What:** a reviewer that wrote none of the code went through every crate,
  the manifests and the workflows. 11 bugs fixed (3 high, 4 medium, 4 low),
  each with a test where one could run here. Highest:
  - wrong port for the platform (Intel Mac with prefer=cpu got `mps`;
    linux-arm64 NVIDIA on a pre-R580 driver got x86-only CUDA ports) → a
    platform check now blocks those with the fix shown;
  - `provision` didn't check a port's `platforms` before deleting/downloading;
  - on Unix, a dropped supervisor could leave grandchildren running → the
    whole process group is killed on drop.
  Also: `gmctl join` hang after a clean exit, `ports gc` deleting the host's
  `cpu` port, too-old uv on PATH used unchecked, beta tags failing the MSI
  build (pre-releases now bundle NSIS only), mesh token printed in Debug
  output, beacon panic on a /0 netmask.
  New cross-crate suite `crates/gmctl/tests/e2e.rs`: beacon + join surface
  find a mesh with its token in < 1.5 s, one mesh seen three ways listed
  once, 400+ probe → port checks, token never in argv, stop kills a
  grandchild that ignores SIGINT.
  Full table and the per-OS/GPU hardware checklist: `docs/devlog/qa-review.md`.
- **Why:** the crates were each tested by the agent that wrote them; a second
  pair of eyes was needed before real users on three OSes.
- **Known, not changed yet:** the join page token is readable by a web page
  via DNS rebinding (same as v1; fix in 2.1 with a Host-header check); on
  Windows the job object is assigned just after spawn; no lock against two
  processes provisioning the same port at once.

### Next
1. ~~Finish wiring `backend.rs` to the crates~~ (done); live-test join/host
   with a real torch port on the LAN.
2. ~~QA pass + `crates/gmctl/tests/e2e.rs`~~ (done). Fix the three known items above.
3. Push `v2-rust`, let `ci-v2.yml` compile on Windows/macOS, fix what it finds.
4. Tag `v2.0.0-beta.1`, install on the M1 Air, an NVIDIA PC and an Arc PC,
   then tag `v2.0.1-beta.1` to prove the in-app update popup end to end.
- Longer term: see the Roadmap in `docs/ARCHITECTURE.md`.
