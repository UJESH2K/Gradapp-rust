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

### Verified
- Workspace builds; `cargo test` green (see entries below as crates land).

### Next
- See the bottom of `docs/ARCHITECTURE.md` (Roadmap).
