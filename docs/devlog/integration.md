# Integration: the app on the real crates

Newest first. Same format as the root `DEVLOG.md`.

---

## 2026-11-24 — every `// INTEGRATE:` stub replaced (2.0.0-dev)

### What
- **`app/src-tauri/src/backend.rs`** rewritten. No placeholders are left (the
  mirrored port list, fake facts, simulated install/join/host, home-made
  invite parser are all deleted).
  - **Device**: `gm_probe::detect` runs once per launch on a blocking thread
    (warmed at start-up) and is cached in a `OnceCell`. `select_profile`
    honours *Settings › preferred backend*; when the forced choice differs
    from the automatic one the profile carries a warning saying so.
  - **Ports**: rows come from `gm_ports::Registry::from_env_or_builtin()`
    (`GRADAPP_PORTS_DIR` still works for development), compatibility from
    `current_platform()`, state from `gm_ports::status` (all rows in
    parallel), plus `Installing` / `Failed` / `Blocked` that only the app
    knows. `install_port` = `uv::ensure` + `provision_with`, each
    `Progress` forwarded as `PortProgress` (percent rescaled from 0..100 to
    0..1, the unit the window draws) and as an installer log line, then a
    final `PortStatus` and a notice. One `install_lock` (tokio mutex):
    the button refuses while another install runs; join/host wait for it.
    Torch-free ports (test ports) skip the kernel check; `GRADAPP_SKIP_VERIFY=1`
    does the same for build machines.
  - **Discover**: `gm_net::discover` with a `CancellationToken`
    (`discover_stop` cancels it); every `MeshFound`/`ScanProgress` is emitted
    as `gm://event`, and a final complete `ScanProgress` always ends the
    sweep so the radar stops spinning even when the sweep was skipped.
  - **Join**: `parse_address` → `/health` (protocol mismatch logged) → token
    from the link, from this app if it hosts that mesh, else `read_invite`
    on the host's join port → profile not blocked → port Ready (installed on
    the spot, with progress) → `worker_spec` under `Supervisor::spawn`.
    The command returns as soon as the mesh answered; installing and starting
    run in the background with the worker in `starting`. Lines go to
    Activity; `classify` milestones: `Joined` records the node id and shows
    a notice, `TokenRejected` an error, (dis)connects are logged; a `Failed`
    worker shows the `FailureTracker`'s best explanation. The saved
    `last_invite` is a canonical `gradapp://join?server=…&token=…` link.
  - **Leave**: stop the worker (SIGINT, so worker.py says goodbye; killed
    after the grace period), *then* `POST /leave {node_id}` — after the stop,
    not before, so the worker cannot re-register in between; it covers
    Windows, which has no polite stop. `/leave` is a worker route outside
    `CoordinatorClient`'s allowlist, so it is sent with `gm_net::lan_client()`
    and the same `X-Mesh-Token` header.
  - **Auto-rejoin**: new setting field `rejoin_on_start` (set on join,
    cleared by an explicit leave). At start the app rejoins only when *auto
    rejoin* is on **and** it was contributing when it last ran; the old code
    rejoined after every start, even after "Stop contributing". The window's
    `set_settings` cannot overwrite `last_invite`/`rejoin_on_start` with a
    stale copy.
  - **Host**: refuses if something already answers `/health` on the port;
    `host_profile` → port Ready → mesh name written into
    `<data>/mesh/coordinator.json` → `coordinator_spec` (state dir
    `<data>/mesh`) → wait up to 90 s for `/health` (fails fast if the process
    dies, with its last lines) → token/id read from `coordinator.json` →
    `JoinSurface` on `0.0.0.0:3000` (engine dir, `logo.png`, `/health`
    proxied; token read per request so a rotation is served at once; a busy
    :3000 is a warning, not a failure) → `Beacon` with `beacon_packet`, its
    node count refreshed every 5 s. `host_stop` stops beacon, join page and
    coordinator (and our own worker if it trained on this mesh).
    `GRADAPP_COORDINATOR_PORT` / `GRADAPP_WEB_PORT` move a hosted mesh for
    development; the beacon carries the port, so it is still found.
  - **Mesh data**: `mesh_snapshot`, `runs`, `datasets` use
    `CoordinatorClient` (loopback for a mesh hosted here). The new
    **`src/mesh.rs`** turns `/mesh`, `/runs` + `/runs/{id}`
    (`round_history`, `accuracy_history`), `/datasets` and `/events`
    payloads into the window's types: vendor/backend, GFLOP/s from
    `capability`, shard progress from the heartbeat, port and torch build
    from `software`, "This computer" by node id, status `done` → `completed`,
    0-based rounds → 1-based, and each event kind in words. The live feed
    follows `/events` (`follow_events`, reconnecting); events are kept (200,
    deduplicated across reconnect replays) for the snapshot and pushed as
    the new `AppEvent::MeshEvent`.
  - **Quit**: `RunEvent::Exit` runs `shutdown` (discovery, worker,
    coordinator, join page, beacon), so no Python process outlives the app.
- **Engine bundling**: `tauri.conf.json` `bundle.resources` maps
  `../../desktop/engine/*.py`, `*.txt` and `coordinator/*.py` into `engine/`
  (globs, so `__pycache__` is never bundled; Tauri's resource walker has no
  exclude), and `../ui/logo.png` → `assets/logo.png` for the join page.
  `engine_dir()` = `GRADAPP_ENGINE_DIR` → `<resources>/engine` →
  `<repo>/desktop/engine`.
- **Updates** (`commands.rs`): the check is gm-update's `Checker` (ETag
  cache in `<data>/update-check.json`, highest semver, stable/beta), with
  `reason` and `manifest_url` copied into `UpdateInfo`. When the release is
  installable the Tauri updater is pointed at that release's own
  `latest.json` (this is what makes beta work); if the signed manifest is
  missing for this platform or does not verify, the update becomes link-only
  with a reason. Background check 10 s after start, then every 6 h; "Check
  now" forces past the throttle. The window toasts the reason when a check
  could not complete instead of claiming "latest version".
- **Logging**: `tracing` to stderr and to `<data>/logs/gradapp.<date>.log`
  (daily, 7 kept, non-blocking writer); filter via `GRADAPP_LOG`.
- **Tray**: Start/Stop contributing now runs the async join/leave; a failed
  rejoin shows a notice and opens Discover.
- **gm-types**: `UpdateInfo.reason`, `UpdateInfo.manifest_url`
  (`#[serde(default)]`), `MeshEvent`, `AppEvent::MeshEvent` (committed
  earlier today; the two `UpdateInfo` initialisers in gm-update got `None`s).
- **UI**: `mesh_found` is an explicit upsert that never blanks a known token
  or name; `mesh_event` appends to the open mesh's feed; Discover builds v1
  `?server=` links (gm-net's `parse_address` only reads `server=`); the app
  also still accepts the older `?host=&port=` links (`normalize_invite`), so
  links and QR codes from 2.0-dev builds keep working; the install stepper
  uses gm-ports' real step names (prepare, python, venv, install, verify).
  Mock updated to the same shapes.

### Why
- The window was a simulation; now every button does the real thing through
  the crates that were already tested on their own.
- Two bugs found while wiring: (1) the Discover "Join" button produced a
  `host=` link that the real parser rejects, so joining a discovered mesh
  would have failed; (2) auto-rejoin ignored an explicit leave.

### Verified
- `cargo build -p gradapp`, `cargo test --workspace` (gradapp: 8 unit tests,
  incl. `mesh.rs` mappings against coordinator-shaped JSON, legacy links,
  mesh-name file), `cargo clippy --workspace --all-targets -D warnings`,
  `cargo fmt --all --check`: green.
- **Live, debug build under Xvfb with the real WebKitGTK window**, driven
  with xdotool; isolated home (`XDG_CONFIG_HOME`/`XDG_DATA_HOME` under
  `/tmp/gahome`), real Python coordinator on :8000, hosted mesh on
  8100/3100 (`GRADAPP_*_PORT`). Screenshots `docs/screenshots/real-*.png`:
  - Probe: real facts of this VM (Xeon, 7.8 GB, no GPU → cpu port), 2 ms.
  - Discover found the real coordinator (sweep, `local`), and later both it
    and the app's own hosted mesh on a non-default port (beacon).
  - **Port install for real**: uv 0.11.32 downloaded CPython 3.12.13 and
    installed a *test* `cpu` port (control-plane packages, no torch, via
    `GRADAPP_PORTS_DIR`) in ~40 s, streaming into the stepper → Ready.
  - **Hosting for real**: coordinator from that port under the supervisor,
    `/health` on 8100, `/gradapp.json` on 3100 with the token, beacon,
    Invite screen with link/QR, Mesh screen from `/mesh`, live feed from
    `/events` (`coordinator.ready`, two `node.joined`). The two nodes on the
    Mesh screen are **synthetic**: registered with curl through the real
    coordinator API (`/register_node` + heartbeats), labelled "(synthetic)".
  - **Join**: "Train on this machine too" and an auto-rejoin of the real
    :8000 coordinator both went parse → health → token → port Ready →
    worker spawned with the right argv/env from the bundled engine folder.
    worker.py then exits on `import torch` (the test port has no torch) and
    the supervisor restarts it with backoff (10 s, 20 s, 40 s), all visible in
    Activity. Leave stopped it; `rejoin_on_start` cleared.
  - `host_stop` freed 8100 and 3100; Quit (palette) left no coordinator,
    join page or worker running.
  - Update check: GitHub has no v2 release reachable from here, so only the
    "could not check" path ran (reason recorded, nothing offered).

### Not verified (needs torch / real hardware / a release)
- A worker that registers and trains (needs a real torch port): the
  `Joined` milestone → node id → "This computer" card and `POST /leave` ran
  only in unit form.
- A real GPU port install and kernel check; Windows/macOS builds of the app.
- An installable update end to end (needs a signed v2 release).
- Two machines on a real LAN (multicast beacon across hosts).

### Next
- Run the same script on an NVIDIA, an Arc and an Apple machine with the
  built-in ports; tag a beta to exercise the updater.
- `CoordinatorClient` could gain a `leave(node_id)` method (gm-net), then
  `post_leave` in the app goes away.
