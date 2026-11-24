# GradApp 2 — what it does

The functional spec: every screen, action and background behaviour, with the
code that implements it. Keep this in step with the code; when behaviour
changes, change this file and add a DEVLOG entry saying why.

Status legend: ✅ done · 🧪 implemented, needs hardware validation · 🚧 in progress · 📋 planned

> **State on 2026-11-24:** every crate below is built and tested, and `gmctl` runs
> the real path. The desktop window still uses placeholder data for install /
> join / host until `app/src-tauri/src/backend.rs` is wired (🚧, next session).

## 1. First launch

| Step | Behaviour | Code | Status |
|---|---|---|---|
| Probe | Reads OS, CPU, RAM, every GPU, driver and (NVIDIA) compute capability. No admin rights, no installs. ~1 s. | `gm-probe::detect` | ✅ |
| Choose port | Picks the port for this device and explains why (e.g. "RTX 5080 is compute 12.0 (Blackwell), driver 590 → CUDA 13.0"). Shows warnings (8 GB Mac, mixed GPUs) and hard blocks with the fix (old driver, Intel Mac, macOS < 14). | `gm-probe::select_profile` | ✅ |
| Install port | On first Join/Host: downloads uv if absent, installs private CPython 3.12, the pinned torch build and engine packages, then runs a real GPU kernel to prove it works. Progress streams into the Device & Ports screen. | `gm-ports::provision_with` (app: one install at a time, progress in Device & ports) | 🧪 (live-tested with a torch-free test port; GPU ports need hardware) |

## 2. Contribute (Join)

| Behaviour | Code | Status |
|---|---|---|
| Discover meshes on the WiFi/LAN: beacon (<1 s), mDNS, /24 sweep fallback; radar shows each mesh with node count, version and latency | `gm-net::discover` | ✅ |
| Join by click, by pasting an IP / hostname / URL, or by a `gradapp://join?...` invite link | `gm-net::parse_address`, `read_invite` | ✅ |
| Start the worker inside the port, no console window; restart after a crash with backoff | `gm-supervisor` + `worker_spec` (gmctl ✅, window 🚧) | 🧪 |
| Progress stepper from worker output (installing → verifying → registered → training) | `gm-supervisor::classify` | ✅ |
| Stop contributing (UI or tray); closing the window keeps contributing in the tray | app | 🚧 |

## 3. Host a mesh

| Behaviour | Code | Status |
|---|---|---|
| Start the coordinator (`uvicorn coordinator.app:app`) in the host's port, wait for `/health` | `gm-supervisor::coordinator_spec` | ✅ (live-tested) |
| Announce it: LAN beacon + the coordinator's own mDNS | `gm-net::Beacon` | ✅ |
| Join surface on :3000 (`/gradapp.json`, `/join.sh`, `/join.ps1`, agent files) so v1 apps and the one-line join still work | `gm-net::JoinSurface` | ✅ |
| Invite screen: link, QR code, rotate token | app + `CoordinatorClient::rotate_token` | ✅ |

## 4. Watching the mesh

Mesh (node cards, vendor badges, hardware mix, live events), Runs (list, detail,
per-round metrics), Datasets, Activity (per-process logs) — all read from the
coordinator API through `gm-net::CoordinatorClient` (`/mesh`, `/runs`,
`/runs/{id}`, `/datasets`) and the `/events` stream, converted in
`app/src-tauri/src/mesh.rs`. ✅ (live-tested against the real coordinator)

## 5. Updates

| Behaviour | Code | Status |
|---|---|---|
| Check GitHub Releases at start and every few hours (ETag-cached, rate-limit safe) | `gm-update::check` | ✅ |
| "GradApp X is available" dialog with notes → Update now / Later; pill in the top bar while pending | app/ui | ✅ |
| Download with progress, verify signature, install, restart | Tauri updater plugin | 🧪 (needs first signed release) |
| Stable / beta channel | settings | ✅ |

## 6. Settings

Start at login, start hidden, auto-rejoin last mesh, update channel, preferred
backend override (for a laptop with both Intel Arc and NVIDIA), data folder.
Logs: `<data>/logs/gradapp.<date>.log` (daily, a week kept; `GRADAPP_LOG` filter).

## 7. Headless: `gmctl`

```
gmctl probe [--json]           # facts + chosen port
gmctl ports list|plan|install|status|gc
gmctl discover [--seconds N]
gmctl join <address|invite>    # install the port and run the worker in the foreground
gmctl version
```
