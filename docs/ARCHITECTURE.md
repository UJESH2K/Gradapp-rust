# GradApp 2 architecture

GradApp turns the GPUs of every machine on one network (NVIDIA, Intel Arc,
Apple Silicon; AMD experimental) into one federated-training mesh. Version 2
replaces the Electron shell with a Rust core and a Tauri 2 window, and
replaces "use whatever Python the machine has" with **ports**.

This file describes *how it is built*. What it does, screen by screen and
command by command, is in [FUNCTIONALITY.md](FUNCTIONALITY.md). Why each change
was made, in order, is in [../DEVLOG.md](../DEVLOG.md).

## The picture

```
┌──────────────────────── GradApp (one per machine) ────────────────────────┐
│  app/ui  (HTML/CSS/ES modules, three.js station scene, v5 design system)  │
│      │ invoke(...)                       ▲ event "gm://event" (AppEvent)  │
│  app/src-tauri  (commands.rs → backend.rs: the only glue layer)           │
│      │                                                                    │
│  ┌───┴──────┬───────────┬────────────┬─────────────┬──────────────┐       │
│  gm-probe   gm-ports    gm-net       gm-update     gm-supervisor  │       │
│  facts →    port spec → discovery,   GitHub +      spawns/keeps   │       │
│  profile    uv + CPython beacon,     Tauri         alive the      │       │
│  (pure)     env + verify client,     latest.json   Python children│       │
│                         join surface                │             │       │
│                                                     ▼             │       │
│        ports/<id>/  (private CPython + pinned torch)  ── runs ──▶ engine/worker.py
│                                                     └──────────▶ engine/coordinator (host only)
└───────────────────────────────────────────────────────────────────────────┘
                     LAN / WiFi:  UDP beacon 239.255.71.77:47800
                                  coordinator :8000  (/health, API)
                                  join surface :3000 (/gradapp.json, /join.sh, /join.ps1)
```

All crates share one vocabulary: `crates/gm-types`. Anything that crosses a
crate boundary or reaches the UI is defined there exactly once.

## Crates

| Crate | Responsibility | Replaces (v1) |
|---|---|---|
| `gm-types` | Device facts, backends, profiles, port specs/locks/status, mesh health, invites, update info, process status, UI events | ad-hoc JS objects |
| `gm-probe` | Detect OS/CPU/RAM/GPUs/drivers; choose the port (pure function, parity-tested against `hardware.py`) | `engine/hardware.py` (for the app) |
| `gm-ports` | Port registry (`ports/*.toml`), lock files, uv management, provisioning and verification | `engine/setup_env.py install`, `src/main/python.js` |
| `gm-net` | LAN addresses, beacon, mDNS, /24 sweep, invites, coordinator client, join surface on :3000 | `src/main/network.js`, `beacon.js`, `api.js` |
| `gm-update` | Release check (GitHub API + Tauri `latest.json`), semver, channel | `src/main/updates.js` |
| `gm-supervisor` | Spawn, stream, restart, and kill process trees; worker/coordinator launch specs | `src/main/proc.js`, process parts of `agent.js`/`host.js` |
| `gmctl` | Headless CLI over the same crates (probe, ports, discover, join) — for servers, CI and debugging a machine without a display | — |
| `app/src-tauri` | Window, tray, commands, settings, updater plugin | `src/main/main.js` |
| `app/ui` | The interface | `src/renderer/**` |

## Ports: one runtime per device family

The v1 failure mode: a venv built from the machine's own Python (3.9, 3.14, a
Rosetta build, a Store build…) for which PyTorch has no wheel, or a venv copied
or synced from another machine. A **port** removes the machine's Python from
the equation:

1. `gm-probe` reads the hardware and picks a port id (`cuda-cu130`,
   `cuda-cu126`, `cuda-cu128`, `xpu`, `mps`, `cpu`, experimental `rocm`).
2. `gm-ports` loads `ports/<id>.toml`: exact CPython (3.12), exact torch and
   torchvision, the PyTorch index, the rest of the packages, supported
   platforms, minimum driver.
3. uv (single Rust binary, fetched and checksum-verified if absent) installs
   that CPython privately under the app's data folder, creates
   `ports/<id>/`, and installs the pinned set.
4. A verification script launches a real kernel on the accelerator.
5. `port.lock.json` records the resolved plan and the machine fingerprint.
   Anything that doesn't match on the next start is rebuilt, never trusted.

Each port lives in its own directory, so switching (say, after a driver update
moves a Blackwell card from `cuda-cu128` to `cuda-cu130`) builds the new one
beside the old one and never uninstalls torch in place. Details: [PORTS.md](PORTS.md).

## Networking

Finding a mesh used to be a /24 sweep of `GET /health` on port 8000 plus a
`gradmesh.local` lookup. v2 runs, concurrently and cancellably:

- **Beacon** — a GradApp host multicasts a small JSON packet every ~2 s on
  `239.255.71.77:47800` (and broadcast). Listeners see a mesh in under a
  second, on any subnet size.
- **mDNS** — the coordinator's own `gradmesh.local` / service record.
- **Sweep** — the v1 /24 sweep, as the fallback for networks that drop
  multicast (common on campus WiFi with client isolation).

Every hit is confirmed with `/health`, deduplicated by `mesh_id`, timed, and
its invite read from `/gradapp.json` or `/join.sh`. The join surface on :3000
is byte-compatible with v1, so v1 apps, v2 apps and the one-line
`irm …/join.ps1 | iex` join all meet on the same mesh.

## Processes

Training stays in Python because PyTorch is Python. GradApp supervises:

- **worker** — `ports/<id>/python engine/worker.py --server-url … --token …`
- **coordinator** (host only) — `uvicorn coordinator.app:app` in the `cpu` or
  machine port.

`gm-supervisor` starts them without console windows, owns the whole process
tree (Job Object on Windows, process group on Unix), streams their lines to the
UI, restarts with backoff after a crash and never after a requested stop.

## Updates

Release tags `v2.*` build Windows, macOS (universal) and Linux installers with
`tauri-action`, sign the updater artifacts, and attach `latest.json` to the
GitHub release. At start (and every few hours) the app checks; if a newer
version exists the UI shows an **Update available** dialog with the notes,
downloads with a progress bar, installs and restarts. If the signed manifest is
missing, the dialog falls back to a link to the release page.

## Data locations

| | Windows | macOS | Linux |
|---|---|---|---|
| App data, settings, logs | `%LOCALAPPDATA%\GradApp` | `~/Library/Application Support/GradApp` | `~/.local/share/gradapp` |
| Ports (CPython + torch) | `…\GradApp\ports` | `…/GradApp/ports` | `…/gradapp/ports` |
| uv | `…\GradApp\bin` | `…/GradApp/bin` | `…/gradapp/bin` |

## Compatibility contract

- Coordinator protocol 5 (`engine/version.py`); `/health` unchanged.
- Ports 8000 (coordinator) and 3000 (join surface) unchanged.
- The engine directory is shared with v1 and with the GradMesh v5 repo.

## Roadmap

1. **2.0** — Rust shell, ports, fast discovery, in-app updates (this branch).
2. **2.1** — Coordinator control plane in Rust (axum): node registry,
   heartbeats, scheduling, shard planning, event stream. Aggregation stays a
   Python call until a Rust tensor path is justified.
3. **2.2** — Fewer Python dependencies in the worker: drop matplotlib/scipy
   from the training plane (reports rendered by the app), evaluate replacing
   Ultralytics wrappers with a smaller in-house trainer.
4. **2.3** — Compute sharing beyond one LAN (relay/rendezvous), ROCm out of
   experimental after hardware validation.
