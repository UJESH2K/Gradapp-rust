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

## 3a. GradApps on this network (peers and invites)

Details, trust model and firewall notes: [devlog/peers-invites.md](devlog/peers-invites.md).

| Behaviour | Code | Status |
|---|---|---|
| Every running GradApp 2 announces itself (name, OS, GPU/backend, port state, idle/contributing/hosting, mesh, invite port; never the token) every 3 s on UDP 47800 (multicast + broadcast) and answers a peer query at once; old 2.0.0 listeners ignore it | `gm-net::peers` (`PeerNode`, `encode_peer`/`parse_peer`) | ✅ (live: two instances) |
| Discover › *GradApps on this network*: one card per peer, deduped by device id, gone after 10 s silent or at once on quit/hide ("bye") | `app/src-tauri/src/peers.rs`, `app/ui/js/views/peers.js` | ✅ |
| While hosting: *Invite to <mesh>* per card, multi-select, *Invite all*; each card shows sent / accepted (+ *In your mesh*) / not now / no answer / not delivered | `PeerNode::invite`, `peer_invite` | ✅ (live) |
| Invite endpoint (TCP 47801 or any free port): only from private/link-local/loopback or this machine's own address, 1 per sender per 30 s, ≤ 5 waiting, blocked devices refused, coordinator must answer `/health` with protocol 5 and the named mesh; never joins by itself | `gm-net::peers` (`/v1/invite`, `/v1/invite-reply`) | ✅ |
| Incoming invite: OS notification when the window is not in front, and a dialog on any screen (inviter, mesh, machines online, coordinator) with **Join** (normal join flow, then "accepted" to the inviter), **Not now**, **Block this device** | `peers.rs`, `views/peers.js` | ✅ (live); 🧪 notification click on Windows/macOS |
| Honest guidance: machines without GradApp 2 open cannot be nudged; invite link + QR beside the list | `views/peers.js` | ✅ |
| Settings › *Visible to other GradApps on this network* (default on; off = bye, no presence, invites refused, others still listed); *Blocked devices · Unblock all* | `Settings.visible_on_lan`, `blocked_peers` | ✅ |
| Degrades when blocked: no UDP → notice, meshes still found by sweep/mDNS; no TCP → `invite_port` 0, others show "send the link"; Windows firewall prompt now on first launch (allow on Private networks) | `peers.rs`, docs | ✅ / 🧪 Windows prompt |

## 4. Watching the mesh

Mesh (node cards, vendor badges, hardware mix, live events), Runs (list, detail,
per-round metrics), Datasets, Activity (per-process logs) — all read from the
coordinator API through `gm-net::CoordinatorClient` (`/mesh`, `/runs`,
`/runs/{id}`, `/datasets`) and the `/events` stream, converted in
`app/src-tauri/src/mesh.rs`. ✅ (live-tested against the real coordinator)

### Runs and datasets (the host manages; contributors watch)

The host's app is a full mesh console; a joined contributor sees the same
screens read-only (the Rust commands refuse changes when this app is not the
host of the mesh it talks to). Code: `app/src-tauri/src/runs.rs`,
`datasets.rs`, `app/ui/js/views/runs.js`, `datasets.js`, `transfers.js`.

| Behaviour | Coordinator route | Status |
|---|---|---|
| Datasets list: images, validation images, classes, source (uploaded / standard / subset), default, files missing on the host | `GET /datasets` | ✅ live |
| Import a standard dataset from the host's catalogue (COCO8, COCO128, African Wildlife, Global Wheat, VisDrone, COCO, DOTA), progress message polled while it runs | `GET`/`POST /datasets/standard` | ✅ live (Ultralytics fetcher shimmed in the sandbox) |
| Upload a YOLO `.zip` chosen in a native file dialog; streamed from disk by Rust as multipart in 256 KB chunks (never through the window), progress bar, "host is unpacking" state, cancel | `POST /datasets` (multipart `file`, `name`, `make_default`) | ✅ live (1.8 GB) |
| Make default; delete (confirm dialog) | `POST /datasets/{id}/default`, `DELETE /datasets/{id}` | ✅ live |
| Download a dataset `.zip` into a folder chosen in a native dialog (`.part` then rename, never overwrites) — host and contributors | `GET /datasets/{id}/download` | ✅ live |
| New run form: name, dataset, base checkpoint (from the host), mode (mesh / single-GPU baseline), rounds, image size, batch ceiling, GPU vendors; advanced: shard sizing, warmup, optimiser, learning rate, seed, dataloader processes, evaluate, deterministic, worker validation | `GET /models`, `POST /runs` | ✅ live |
| Stop (confirm) and delete (confirm) a run | `POST /runs/{id}/stop`, `DELETE /runs/{id}` | ✅ live (stop: mock + unit) |
| Download trained weights (`.pt`, the averaged global model after the latest round), during or after the run | `GET /runs/{id}/artifact` | ✅ live |
| **Live run view**: one row per machine with vendor, GPU, port, its share of the round's images (from the coordinator's shard plan), images/s (last round, else measured, else planned — labelled), status (fetching images, training, uploading, finished, late, dropped, not answering), shard progress and time vs plan; a stacked split bar; round timeline (bar = wall clock, segments = shares); loss / mAP50 charts; rounds table. Refreshes on `/events` round/shard/run events and polls every 2 s while live | `GET /runs/{id}` (`plan`, `live_shards`, `round_history`), `GET /mesh` | ✅ live (synthetic workers) |
| Contributor: same view, no controls, own machine highlighted | same | ✅ live |

## 5. Updates

| Behaviour | Code | Status |
|---|---|---|
| Check GitHub Releases at start and every few hours (ETag-cached, rate-limit safe) | `gm-update::check` | ✅ |
| "GradApp X is available" dialog with notes → Update now / Later; pill in the top bar while pending | app/ui | ✅ |
| Download with progress, verify signature, install, restart | Tauri updater plugin | 🧪 (needs first signed release) |
| Stable / beta channel | settings | ✅ |

## 6. Settings

Start at login, start hidden, auto-rejoin last mesh, update channel, preferred
backend override (for a laptop with both Intel Arc and NVIDIA), data folder,
visibility to other GradApps on the network and blocked devices.
Logs: `<data>/logs/gradapp.<date>.log` (daily, a week kept; `GRADAPP_LOG` filter).

## 7. Headless: `gmctl`

```
gmctl probe [--json]           # facts + chosen port
gmctl ports list|plan|install|status|gc
gmctl discover [--seconds N]
gmctl join <address|invite>    # install the port and run the worker in the foreground
gmctl version
```
