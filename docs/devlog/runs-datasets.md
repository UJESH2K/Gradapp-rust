# Runs and datasets: the host can run its mesh

Newest first. Same format as the root `DEVLOG.md`.

---

## 2026-11-25 — host controls and the live per-device run view (2.0.1)

### Why
First hardware report on 2.0.0 (RTX 3050 laptop hosting "MSI's mesh", its own
worker joined): the host could not do anything with its mesh. Runs said "The
host starts runs from its GradMesh dashboard", Datasets said "Upload a YOLO
.zip on the host, or import COCO128 there", and no button did either. GradApp 1
and the GradMesh v5 dashboard had all of it; v2 had only read-only views. The
owner asked for: start training, download and upload datasets, and "show these
are the devices and this is the percentage of compute we are using on all
these devices for this run, so you can compare in parallel".

### What
- **gm-net `CoordinatorClient`** (`crates/gm-net/src/client.rs`): allowlist adds
  `/runs/{id}/artifact`, `/runs/{id}/weights.bin`, `/datasets/{id}/download`.
  New calls: `standard_datasets`, `import_standard`, `set_default_dataset`,
  `delete_dataset`, `upload_dataset` (multipart streamed from disk in 256 KB
  chunks with a per-chunk progress callback; 6 h transfer limit; FastAPI
  `detail` on errors) and `download_file` (authenticated GET into a folder,
  `.part` then rename, never overwrites: `name (1).zip`; file name from the
  caller, else the server's `Content-Disposition`, sanitised). `download.rs`
  shares its stream-to-file code (`save_response`). reqwest `multipart` feature
  enabled for gm-net only. Tests: `crates/gm-net/tests/transfer.rs` (axum
  mock: multipart body byte-checked, progress monotone, 401/404/409/410/204
  details, allowlist refusal, no `.part` left behind).
- **App, new files only for logic**: `app/src-tauri/src/datasets.rs`
  (overview = `/datasets` + `/datasets/standard` + `/health`; import, upload,
  default, delete, download; native pickers via `tauri-plugin-dialog`, called
  from Rust so no capability change; transfers registry with progress on the
  `gm://transfer` window event, throttled to ~8/s, cancellable by aborting the
  task) and `runs.rs` (`run_options` = `/models`; `run_create` with the
  coordinator's limits checked first; stop; delete; weights download;
  `run_live`). Shared files touched additively: `lib.rs` (modules, dialog
  plugin, 16 commands), `backend.rs` (`mesh_access()` = client + "this app
  hosts that mesh"; `self_info` made `pub(crate)`), `Cargo.toml`s.
- **Who may change what**: every mutating command calls `host_access`, which
  refuses unless this app hosts the mesh it is talking to. A contributor (or a
  host that is also contributing to someone else's mesh) gets the read-only
  view. Downloads are allowed to both.
- **Live run view** (`runs::live_run`, pure, unit-tested on JSON captured from
  the real coordinator: `app/src-tauri/tests/fixtures/`): one row per machine.
  *Share* = that machine's images ÷ the round's images, from the coordinator's
  shard plan (`live_shards[].samples`; backup copies listed with no share).
  Fallbacks are labelled in the UI: last round's shards after a run ends; the
  stored plan; the mesh's `plan_preview`; and, only for a run waiting for
  machines, measured throughput ÷ total ("throughput" basis). *img/s* = last
  round's images ÷ its training seconds, else the coordinator's measured
  estimate, else the scheduler's planned rate; the source is shown. Status
  from the shard and the node's heartbeat phase; progress from the heartbeat's
  `{batch, batches}`; late = past the soft deadline; offline nodes flagged.
  Machines not in the round are listed greyed. Round progress is
  share-weighted. Timeline from `round_history` (bar length = wall clock,
  segments = each machine's share). Refresh: every `/events` round./shard./run.
  event (already followed for the feed) nudges a reload; poll every 2 s while
  live, 15 s otherwise. A newly created run is opened automatically.
- **UI**: `views/runs.js` (run strip on top, full-width live view, New run
  panel mirroring v1 and v5 fields incl. the v5 advanced recipe),
  `views/datasets.js` (cards with actions, catalogue from the host, upload
  panel, import progress), `transfers.js` (transfer cards, confirm dialog),
  `css/runs.css`. Color follows the machine: vendor hue, a darker shade for a
  second machine of the same vendor, names on segments/legend/rows (the vendor
  palette fails the dataviz lightness/chroma checks by design but passes CVD
  separation; identity never relies on color alone). Removed the "host starts
  runs from its GradMesh dashboard" / "upload … there" copy.
- **Mock**: `js/mock-runs.js` (same command names and shapes, simulated rounds,
  import, 1.8 GB upload, downloads), hooked into `mock.js` with one line.

### Verified
- `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -D
  warnings`, `cargo test --workspace` (new: 3 gm-net integration + 2 unit,
  5 app unit incl. the real-fixture test), `cargo build -p gradapp`: green.
- **Live, real window under Xvfb (:93, own home `/tmp/garuns`), real Python
  coordinator hosted by the app** (cpu test port, ports 8120/3120). PyTorch
  cannot be installed here, and the coordinator refuses imports and runs
  without it, so `PYTHONPATH` carried a **test shim** (`/tmp/torchshim`,
  outside the repo): a fake `torch` whose `save`/`load` pickle plain dicts
  (enough for the coordinator's FedAvg path, which passes non-tensors
  through), and a stand-in for `ultralytics.data.utils.check_det_dataset` that
  downloads the real `coco8.zip`/`coco128.zip` from github.com/ultralytics/assets.
  Everything else is the real coordinator: import thread and events, upload
  parsing and zip validation, shard planning, barrier, aggregation, archive,
  artifact. Workers were **synthetic** (`/tmp/gatest/sim_workers.py`: four
  machines speaking the real worker protocol — register, heartbeats with phase
  and progress, `get_batch`, binary result upload — training = sleep of
  samples ÷ a per-machine speed), labelled "(synthetic)" on screen.
  - Import COCO8 and COCO128 from the catalogue (progress message shown).
  - Native GTK file dialog → upload of a 1.8 GB YOLO zip: progress 6 %→86 %
    in ~4 s on loopback, then "host is unpacking", then the card; window
    never held the file. Delete with confirm removed it from the host.
  - Download COCO8 (packaged by the host, 17 files) and a run's weights via
    the native folder dialog.
  - New run form → `POST /runs` → live view: round 1 split 47/22/19/13 % from
    probed GFLOP/s; from round 2 on 49/23/17/11 % from measured speed; rows
    showed training progress, img/s from the last round, time vs plan;
    rounds closed with speed-up 3.8×; timeline and loss chart filled in.
  - Contributor: the same app joined a second real coordinator (:8300) by
    invite link; with a synthetic node registered under this machine's device
    name, its row was highlighted "This computer" and no controls showed.
  - Screenshots: `docs/screenshots/v201-*.png` (datasets import progress /
    COCO8 imported / upload progress / delete confirm, new-run form, live run
    host top / devices / timeline, contributor, weights downloaded).
- Browser mock (`?mock=hosting`, `?mock=joined`): every screen renders with no
  console errors (Chrome via Playwright).

### Found on the way
- A host that also contributes to another mesh showed "New run" for the
  joined mesh; the button now waits for `can_manage` from the coordinator.
- Coordinator facts that shaped this: `POST /datasets/standard` and `POST
  /runs` both return 503 without torch on the host; standard import is
  asynchronous (poll `GET /datasets/standard`.import or follow
  `dataset.importing`); the catalogue has no VOC (COCO8/128, African
  Wildlife, Global Wheat, VisDrone, COCO, DOTA); the `.pt` artifact is
  written after every round (weights are downloadable mid-run); `/runs/{id}`
  carries everything for the live view (`plan`, `live_shards` with heartbeat
  phase/progress, `round_history[].shards` with compute seconds) — there is no
  per-node GPU-utilisation figure, so "compute share" is the share of the
  round's images the scheduler gave each machine.

### Not verified
- Real GPUs and real torch: an actual training round, mAP from evaluation,
  the multipart upload into a Windows host over WiFi.
- Windows/macOS native dialogs (Linux GTK portal only).

### Next
- Ship as 2.0.1. Leftover from testing: a mistyped folder path created
  `/mp/garuns/dl/COCO8.zip` on this VM (outside the repo); the sandbox refused
  to remove `/mp`, so it is still there.
