// Mock backend for running the interface in a normal browser. Same command
// names and JSON shapes as app/src-tauri/src/commands.rs; loaded by api.js only
// when there is no Tauri. Everything here is invented but plausible: an RTX
// 4070 laptop on Windows, a lab mesh of four mixed-vendor machines, runs with
// per-round metrics, and an update to 2.0.1 arriving a few seconds in.

const params = new URLSearchParams(location.search);
const now = () => Date.now() / 1000;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

const FACTS = {
  os: "windows",
  os_version: "10.0.26100",
  arch: "x86_64",
  translated: false,
  hostname: "ujesh-zephyrus",
  cpu_model: "Intel Core i9-13900H",
  cpu_cores: 14,
  ram_mb: 32768,
  os_name_pretty: "Windows 11 Home 24H2",
  gpus: [
    { vendor: "nvidia", name: "NVIDIA GeForce RTX 4070 Laptop GPU", index: 0, compute_capability: 8.9, driver: "581.57", memory_mb: 8188, cores: null, integrated: false },
    { vendor: "intel", name: "Intel Iris Xe Graphics", index: 1, compute_capability: null, driver: "32.0.101.6078", memory_mb: null, cores: null, integrated: true },
  ],
};

const PROFILE = {
  port: "cuda-cu130",
  backend: "cuda",
  label: "NVIDIA · CUDA 13.0",
  reason: "RTX 4070 Laptop GPU (Ada, compute 8.9) with driver 581.57, which meets CUDA 13.0's minimum of 580. This is the reference stack, so results compare directly with Intel and Apple machines.",
  torch: "2.13.0",
  reference: true,
  can_train: true,
  gpu_index: 0,
  gpu_name: "NVIDIA GeForce RTX 4070 Laptop GPU",
  blocked: null,
  fix: null,
  warnings: [
    "Laptop GPU: plug in the charger, or Windows caps it at roughly 40% of its clocks on battery.",
    "The integrated Intel Iris Xe is ignored; it is too slow to be worth a shard.",
  ],
};

const COMMON = ["ultralytics==8.3.228", "numpy<3", "httpx==0.28.1", "psutil"];
const SPECS = [
  { id: "cuda-cu130", backend: "cuda", label: "NVIDIA · CUDA 13.0", python: "3.12", torch: "2.13.0", torchvision: "0.24.1", index_url: "https://download.pytorch.org/whl/cu130", local_suffix: "+cu130", packages: COMMON, platforms: ["windows-x86_64", "linux-x86_64", "linux-arm64"], reference: true, experimental: false, min_driver: 580, notes: "Turing (RTX 20) and newer, including Blackwell." },
  { id: "cuda-cu126", backend: "cuda", label: "NVIDIA · CUDA 12.6", python: "3.12", torch: "2.13.0", torchvision: "0.24.1", index_url: "https://download.pytorch.org/whl/cu126", local_suffix: "+cu126", packages: COMMON, platforms: ["windows-x86_64", "linux-x86_64"], reference: false, experimental: false, min_driver: 560, notes: "For drivers older than 580 and Pascal/Volta cards." },
  { id: "xpu", backend: "xpu", label: "Intel Arc · XPU", python: "3.12", torch: "2.13.0", torchvision: "0.24.1", index_url: "https://download.pytorch.org/whl/xpu", local_suffix: "+xpu", packages: COMMON, platforms: ["windows-x86_64", "linux-x86_64"], reference: true, experimental: false, min_driver: null, notes: "Arc A/B series and Core Ultra with Arc Graphics." },
  { id: "mps", backend: "mps", label: "Apple Silicon · Metal", python: "3.12", torch: "2.13.0", torchvision: "0.24.1", index_url: null, local_suffix: "", packages: COMMON, platforms: ["macos-arm64"], reference: true, experimental: false, min_driver: null, notes: "M1 or later on macOS 14 Sonoma or newer." },
  { id: "rocm", backend: "rocm", label: "AMD · ROCm 6.4", python: "3.12", torch: "2.13.0", torchvision: "0.24.1", index_url: "https://download.pytorch.org/whl/rocm6.4", local_suffix: "+rocm6.4", packages: COMMON, platforms: ["linux-x86_64"], reference: false, experimental: true, min_driver: null, notes: "Experimental. Radeon RX 7000 and Instinct on Linux." },
  { id: "cpu", backend: "cpu", label: "CPU only", python: "3.12", torch: "2.13.0", torchvision: "0.24.1", index_url: "https://download.pytorch.org/whl/cpu", local_suffix: "+cpu", packages: COMMON, platforms: ["windows-x86_64", "linux-x86_64", "linux-arm64", "macos-arm64", "macos-x86_64"], reference: false, experimental: false, min_driver: null, notes: "Joins and is measured; receives no training work." },
];

const MESHES = [
  { mesh_id: "m-7f3a91c2", hostname: "lab-rtx3090", coordinator: "http://192.168.1.42:8000", web: "http://192.168.1.42:3000", version: "5.1.1", protocol: 5, nodes_active: 4, torch_ready: true, local: false, source: "beacon", token: "c41d8e0f9b2a7731", mesh_name: "Vision lab", latency_ms: 3 },
  { mesh_id: "m-21be0d77", hostname: "studio-m3max.local", coordinator: "http://192.168.1.57:8000", web: "http://192.168.1.57:3000", version: "5.1.1", protocol: 5, nodes_active: 2, torch_ready: true, local: false, source: "mdns", token: null, mesh_name: "Studio", latency_ms: 14 },
  { mesh_id: "m-90aa1f04", hostname: "192.168.1.88", coordinator: "http://192.168.1.88:8000", web: "http://192.168.1.88:3000", version: "5.0.4", protocol: 5, nodes_active: 1, torch_ready: false, local: false, source: "sweep", token: null, mesh_name: null, latency_ms: 41 },
];

function nodes(selfActive) {
  return [
    { node_id: "n-self", display_name: "ujesh-zephyrus", gpu: "NVIDIA GeForce RTX 4070 Laptop GPU", backend: "cuda", gpu_memory_mb: 8188, gflops: 15800, throughput_sps: 41.7, active: selfActive, training: selfActive, phase: "training", progress: 0.62, completed_rounds: 7, tier: "full", fitness: 0.81, port: "cuda-cu130", torch: "2.13.0+cu130", os: "Windows 11", is_self: true, warnings: ["On battery: clocks capped"] },
    { node_id: "n-3090", display_name: "lab-rtx3090", gpu: "NVIDIA GeForce RTX 3090", backend: "cuda", gpu_memory_mb: 24576, gflops: 29400, throughput_sps: 78.2, active: true, training: true, phase: "training", progress: 0.71, completed_rounds: 12, tier: "full", fitness: 0.94, port: "cuda-cu130", torch: "2.13.0+cu130", os: "Ubuntu 24.04", is_self: false, warnings: [] },
    { node_id: "n-m3max", display_name: "studio-m3max", gpu: "Apple M3 Max (40-core GPU)", backend: "mps", gpu_memory_mb: 49152, gflops: 13600, throughput_sps: 36.4, active: true, training: true, phase: "uploading", progress: 1, completed_rounds: 12, tier: "full", fitness: 0.88, port: "mps", torch: "2.13.0", os: "macOS 15.1", is_self: false, warnings: [] },
    { node_id: "n-a770", display_name: "arc-a770-tower", gpu: "Intel Arc A770 16GB", backend: "xpu", gpu_memory_mb: 16384, gflops: 11900, throughput_sps: 27.9, active: true, training: false, phase: null, progress: null, completed_rounds: 9, tier: "probation", fitness: 0.58, port: "xpu", torch: "2.13.0+xpu", os: "Windows 11", is_self: false, warnings: ["Resizable BAR is off; transfers run at half speed."] },
    { node_id: "n-nuc", display_name: "hallway-nuc", gpu: "No supported GPU", backend: "cpu", gpu_memory_mb: null, gflops: 410, throughput_sps: null, active: false, training: false, phase: null, progress: null, completed_rounds: 0, tier: "rejected", fitness: 0.08, port: "cpu", torch: "2.13.0+cpu", os: "Ubuntu 22.04", is_self: false, warnings: [] },
  ];
}

function rounds(n, base) {
  const out = [];
  for (let i = 0; i < n; i += 1) {
    out.push({
      round: i + 1,
      wall_clock_seconds: base - i * 1.6 + Math.sin(i * 1.7) * 2.1,
      straggler_gap_seconds: Math.max(0.4, 6.2 - i * 0.55 + Math.cos(i) * 0.4),
      aggregation_seconds: 0.62 + (i % 3) * 0.08,
      speedup: Math.min(3.4, 2.1 + i * 0.13 + Math.sin(i) * 0.06),
      workers: i < 2 ? 3 : 4,
      map50: Math.min(0.79, 0.18 + Math.log1p(i) * 0.23),
      loss: Math.max(0.9, 3.4 - Math.log1p(i) * 0.98),
    });
  }
  return out;
}

const RUNS = [
  { id: "r-1043", name: "parts-detector-v3", status: "running", dataset: "workshop-parts", base_model: "yolo11n.pt", rounds: 20, current_round: 12, workers: 4, speedup: 3.21, map50: 0.712, wall_clock_seconds: 1534, started_at: now() - 1560, error: null, round_metrics: rounds(12, 138) },
  { id: "r-1038", name: "coco128-smoke", status: "completed", dataset: "COCO128", base_model: "yolov8n.pt", rounds: 6, current_round: 6, workers: 3, speedup: 2.64, map50: 0.581, wall_clock_seconds: 412, started_at: now() - 86400, error: null, round_metrics: rounds(6, 74) },
  { id: "r-1031", name: "parts-detector-v2", status: "completed", dataset: "workshop-parts", base_model: "yolo11n.pt", rounds: 16, current_round: 16, workers: 3, speedup: 2.48, map50: 0.664, wall_clock_seconds: 2210, started_at: now() - 3 * 86400, error: null, round_metrics: rounds(16, 146) },
  { id: "r-1029", name: "voc-subset-xpu-check", status: "failed", dataset: "VOC 2012 (subset)", base_model: "yolov8s.pt", rounds: 10, current_round: 3, workers: 2, speedup: 1.71, map50: 0.302, wall_clock_seconds: 381, started_at: now() - 4 * 86400, error: "arc-a770-tower ran out of GPU memory at batch 16; its batch is now capped at 8.", round_metrics: rounds(3, 120) },
];

const DATASETS = [
  { id: "d-parts", name: "workshop-parts", images: 4812, classes: 14, is_default: true, size_mb: 1830 },
  { id: "d-coco128", name: "COCO128", images: 128, classes: 80, is_default: false, size_mb: 6.7 },
  { id: "d-voc", name: "VOC 2012 (subset)", images: 2500, classes: 20, is_default: false, size_mb: 412 },
];

const FEED = [
  [-420, "node.joined", "arc-a770-tower joined on Intel (XPU) at 11,900 GFLOP/s. Admitted on probation until it finishes two rounds."],
  [-372, "round.started", "Round 11 planned across 4 machines, 4,812 images, predicted 121 s."],
  [-251, "round.completed", "Round 11 done in 119.4 s. Straggler gap 1.1 s, aggregation 0.64 s, speedup 3.18x."],
  [-250, "round.evaluated", "Round 11 scored mAP50 0.7046 in 6.2 s."],
  [-244, "round.started", "Round 12 planned across 4 machines, 4,812 images, predicted 118 s."],
  [-201, "shard.speculated", "arc-a770-tower passed its soft deadline at 96 s, so a copy was started on lab-rtx3090."],
  [-122, "round.completed", "Round 12 done in 116.8 s. Straggler gap 0.9 s, aggregation 0.61 s, speedup 3.21x."],
  [-118, "shard.assigned", "studio-m3max took 1,190 images, predicted 32.7 s."],
  [-64, "shard.completed", "studio-m3max finished 1,190 images in 33.1 s (30.4 s of it training)."],
];

const NOTES = `### What's new
- Ports can now be repaired in place: a broken PyTorch install is rebuilt without touching your datasets.
- Discover finds meshes on networks that block multicast, by asking every address on the /24 in parallel.
- The radar keeps each mesh in the same seat between sweeps.

### Fixes
- Intel Arc machines with Resizable BAR off are measured correctly instead of being rejected.
- Closing the window while a port installs no longer stops the install.`;

export function createMock(dispatch, dispatchChannel) {
  const startState = params.get("mock") || "idle";
  const state = {
    settings: {
      device_name: "",
      start_at_login: true,
      start_hidden: false,
      close_to_tray: true,
      auto_rejoin: true,
      notifications: true,
      update_channel: "stable",
      data_dir: "C:\\Users\\ujesh\\AppData\\Local\\GradApp",
      preferred_backend: null,
      last_invite: "gradapp://join?host=192.168.1.42&port=8000&token=c41d8e0f9b2a7731&name=Vision%20lab",
    },
    joined: null,
    hosting: null,
    worker: { role: "worker", state: "stopped", pid: null, restarts: 0, last_line: null, exit_code: null },
    coordinator: { role: "coordinator", state: "stopped", pid: null, restarts: 0, last_line: null, exit_code: null },
    portStates: { "cuda-cu130": "ready", "cuda-cu126": "not_installed", xpu: "not_installed", cpu: "stale" },
    logs: [],
    update: null,
    discovering: false,
  };

  const emit = (event) => setTimeout(() => dispatch(event), 0);
  const log = (role, line) => {
    state.logs.push({ at: now(), source: role, line });
    emit({ kind: "log", role, line });
  };
  const setProc = (role, patch) => {
    state[role] = { ...state[role], ...patch };
    emit({ kind: "proc", status: state[role] });
  };

  // History so Activity is not empty.
  const seed = [
    ["installer", -3600, "uv 0.9.11 (bundled)"],
    ["installer", -3598, "Using CPython 3.12.12 (managed)"],
    ["installer", -3570, "Resolved 41 packages in 1.2s"],
    ["installer", -3402, "Installed torch==2.13.0+cu130 torchvision==0.24.1+cu130"],
    ["installer", -3390, "verify: torch.cuda.is_available() = True · RTX 4070 Laptop GPU · 8.0 GiB"],
    ["installer", -3389, "port cuda-cu130 ready (lock 3f9c21ab)"],
    ["worker", -1500, "agent 5.1.1 · protocol 5 · port cuda-cu130 · torch 2.13.0+cu130"],
    ["worker", -1498, "connected to http://192.168.1.42:8000 as ujesh-zephyrus"],
    ["worker", -1490, "benchmark: 15.8 TFLOP/s fp16, 41.7 img/s at imgsz 640"],
    ["worker", -420, "round 11: shard 1,104 images, predicted 27.4 s"],
    ["worker", -392, "round 11: trained 1,104 images in 26.9 s, uploading 5.4 MB"],
    ["worker", -244, "round 12: shard 1,098 images, predicted 27.1 s"],
    ["worker", -215, "WARNING on battery power; GPU clocks capped at 1,140 MHz"],
    ["worker", -190, "round 12: trained 1,098 images in 31.2 s, uploading 5.4 MB"],
    ["coordinator", -7200, "GradMesh coordinator 5.1.1 listening on 0.0.0.0:8000"],
    ["coordinator", -7199, "beacon: multicasting on 239.255.71.77:47800"],
  ];
  for (const [source, dt, line] of seed) state.logs.push({ at: now() + dt, source, line });

  if (startState === "joined") {
    state.joined = { hostname: "192.168.1.42", coordinator: "http://192.168.1.42:8000", web: "http://192.168.1.42:3000", token: "c41d8e0f9b2a7731", mesh_name: "Vision lab" };
    state.worker = { ...state.worker, state: "running", pid: 18244, last_line: "round 12: training" };
  }
  if (startState === "hosting") hostStart("Vision lab");

  function hostStart(name) {
    state.hosting = {
      mesh_id: "m-7f3a91c2",
      mesh_name: name || "ujesh-zephyrus's mesh",
      coordinator: "http://192.168.1.31:8000",
      web: "http://192.168.1.31:3000",
      invite_link: `gradapp://join?host=192.168.1.31&port=8000&token=c41d8e0f9b2a7731&name=${encodeURIComponent(name || "ujesh-zephyrus's mesh")}`,
      token: "c41d8e0f9b2a7731",
    };
    state.coordinator = { ...state.coordinator, state: "running", pid: 20112 };
    return state.hosting;
  }

  const ports = () =>
    SPECS.map((spec) => {
      const compatible = spec.platforms.includes("windows-x86_64");
      const s = state.portStates[spec.id] || (compatible ? "not_installed" : "blocked");
      return {
        spec,
        compatible,
        selected: spec.id === PROFILE.port,
        incompatible_reason: compatible ? null : `Built for ${spec.platforms.join(", ")}; this machine is windows-x86_64.`,
        status: {
          port: spec.id,
          state: s,
          env_dir: `C:\\Users\\ujesh\\AppData\\Local\\GradApp\\ports\\${spec.id}`,
          python_path: s === "ready" ? `C:\\Users\\ujesh\\AppData\\Local\\GradApp\\ports\\${spec.id}\\.venv\\Scripts\\python.exe` : null,
          torch: s === "ready" ? `${spec.torch}${spec.local_suffix}` : null,
          device: s === "ready" && spec.backend === "cuda" ? "NVIDIA GeForce RTX 4070 Laptop GPU" : null,
          message: s === "stale" ? "Lock fingerprint changed (driver updated); rebuild to refresh." : null,
        },
      };
    });

  async function installPort(portId) {
    const entry = ports().find((p) => p.spec.id === portId);
    if (!entry) throw new Error(`Unknown port ${portId}`);
    if (!entry.compatible) throw new Error(entry.incompatible_reason);
    state.portStates[portId] = "installing";
    const status = { ...entry.status, state: "installing" };
    emit({ kind: "port_status", status });
    (async () => {
      const spec = entry.spec;
      const steps = [
        ["prepare", 0.04, "uv 0.9.11 (bundled)"],
        ["python", 0.1, `Downloading cpython-${spec.python}.12-windows-x86_64 (21.4 MiB)`],
        ["python", 0.16, `Using CPython ${spec.python}.12 (managed, not the system Python)`],
        ["venv", 0.2, `Creating virtual environment at ${entry.status.env_dir}\\.venv`],
        ["install", 0.26, `Resolved 41 packages from ${spec.index_url || "PyPI"} in 1.4s`],
        ["install", 0.34, `Downloading torch-${spec.torch}${spec.local_suffix}-cp312-win_amd64.whl (812.6 MiB)`],
        ["install", 0.52, "  ████████░░░░░░░░  48%  31.2 MiB/s"],
        ["install", 0.66, "  ██████████████░░  88%  33.0 MiB/s"],
        ["install", 0.72, `Installed torch==${spec.torch}${spec.local_suffix} torchvision==${spec.torchvision}${spec.local_suffix}`],
        ["install", 0.84, `Installed ${spec.packages.length + 37} packages in 6.1s`],
        ["verify", 0.94, `verify: torch ${spec.torch} · ${spec.backend} available · 1×1024² matmul ok in 3.1 ms`],
        ["lock", 1, `port ${spec.id} ready (lock 3f9c21ab)`],
      ];
      for (const [step, percent, line] of steps) {
        await sleep(520 + Math.random() * 380);
        log("installer", line);
        emit({ kind: "port_progress", port: spec.id, step, percent, line });
      }
      state.portStates[portId] = "ready";
      emit({ kind: "port_status", status: { ...status, state: "ready", torch: `${spec.torch}${spec.local_suffix}` } });
      emit({ kind: "notice", level: "info", title: `${spec.label} is ready`, body: "This machine can join a mesh with it." });
    })();
    return status;
  }

  async function discover() {
    if (state.discovering) return;
    state.discovering = true;
    const total = 254;
    let shown = 0;
    for (let scanned = 0; scanned <= total && state.discovering; scanned += 18) {
      emit({ kind: "scan_progress", scanned: Math.min(scanned, total), total });
      if (shown < MESHES.length && scanned >= [18, 90, 180][shown]) {
        emit({ kind: "mesh_found", mesh: MESHES[shown] });
        shown += 1;
      }
      await sleep(140);
    }
    while (shown < MESHES.length) emit({ kind: "mesh_found", mesh: MESHES[shown++] });
    emit({ kind: "scan_progress", scanned: total, total });
    state.discovering = false;
  }

  function parseInvite(text) {
    const raw = String(text || "").trim();
    if (!raw) throw new Error("Paste an address or an invite link.");
    let host;
    let port = 8000;
    let token = null;
    let name = null;
    if (raw.startsWith("gradapp://join?")) {
      const q = new URLSearchParams(raw.slice("gradapp://join?".length));
      const server = q.get("server") ? new URL(q.get("server")) : null;
      host = server ? server.hostname : q.get("host");
      port = Number((server ? server.port : q.get("port")) || 8000);
      token = q.get("token");
      name = q.get("name");
    } else {
      const authority = raw.replace(/^https?:\/\//, "").split("/")[0];
      const m = authority.match(/^(.*?)(?::(\d+))?$/);
      host = m[1];
      if (m[2]) port = Number(m[2]);
    }
    if (!host || /\s/.test(host)) throw new Error(`“${raw}” is not an address GradApp understands.`);
    const known = MESHES.find((mesh) => mesh.coordinator.includes(host) || mesh.hostname === host);
    return { hostname: host, coordinator: `http://${host}:${port}`, web: `http://${host}:3000`, token: token || known?.token || null, mesh_name: name || known?.mesh_name || null };
  }

  async function installUpdate() {
    if (!state.update) throw new Error("GradApp is already up to date.");
    const total = 9_874_311;
    let downloaded = 0;
    while (downloaded < total) {
      await sleep(120);
      downloaded = Math.min(total, downloaded + 380_000 + Math.random() * 260_000);
      emit({ kind: "update_progress", downloaded: Math.round(downloaded), total });
    }
    await sleep(500);
    state.update = null;
  }

  const update = {
    available: true,
    current: "2.0.0",
    latest: "2.0.1",
    notes: NOTES,
    url: "https://github.com/UJESH2K/Gradapp-rust/releases/latest",
    published_at: new Date(Date.now() - 3 * 3600 * 1000).toISOString(),
    installable: true,
  };
  const updateMode = params.get("update") || "modal";
  if (updateMode !== "none") {
    setTimeout(() => {
      state.update = update;
      emit({ kind: "update", info: { ...update, _quiet: updateMode === "later" } });
    }, updateMode === "later" ? 200 : 3000);
  }

  if (params.get("install")) setTimeout(() => installPort(params.get("install") === "1" ? "cuda-cu126" : params.get("install")), 300);

  // Live feeling while joined: progress ticks and the odd line.
  setInterval(() => {
    if (state.worker.state === "running" && Math.random() < 0.3) log("worker", `round 13: batch ${Math.floor(Math.random() * 60) + 1}/68 · loss ${(1.1 + Math.random() * 0.2).toFixed(3)}`);
  }, 2500);

  const commands = {
    app_status: () => ({ version: "2.0.0", worker: state.worker, coordinator: state.coordinator, joined: state.joined, hosting: state.hosting, update: state.update }),
    logs: () => state.logs.slice(),
    get_device: () => {
      const pref = state.settings.preferred_backend;
      const profile = { ...PROFILE, warnings: [...PROFILE.warnings] };
      if (pref && pref !== PROFILE.backend) {
        const spec = SPECS.find((s) => s.backend === pref);
        Object.assign(profile, { port: spec.id, backend: pref, label: spec.label, can_train: pref !== "cpu", reference: spec.reference });
        profile.warnings.unshift(`Forced to ${pref} in Settings; the automatic choice was cuda.`);
        if (pref === "mps") Object.assign(profile, { blocked: "Metal needs an Apple Silicon Mac; this is a Windows PC.", fix: "Set the preferred backend back to Automatic in Settings." });
      }
      return { facts: FACTS, profile, overridden: Boolean(pref) };
    },
    get_ports: ports,
    install_port: ({ portId }) => installPort(portId),
    discover_start: () => {
      discover();
    },
    discover_stop: () => {
      state.discovering = false;
    },
    join: async ({ invite }) => {
      const parsed = parseInvite(invite);
      await sleep(500);
      state.joined = parsed;
      state.settings.last_invite = invite;
      setProc("worker", { state: "starting", pid: null });
      log("worker", `Joining ${parsed.coordinator} on port ${PROFILE.port}`);
      setTimeout(() => {
        if (!state.joined) return;
        setProc("worker", { state: "running", pid: 18244, last_line: "Connected. Waiting for work." });
        log("worker", `connected to ${parsed.coordinator} as ujesh-zephyrus`);
      }, 1400);
      return parsed;
    },
    leave: async () => {
      await sleep(250);
      state.joined = null;
      setProc("worker", { state: "stopped", pid: null });
      log("worker", "Left the mesh.");
    },
    host_start: async ({ meshName }) => {
      await sleep(600);
      const info = hostStart(meshName);
      emit({ kind: "proc", status: state.coordinator });
      log("coordinator", `GradMesh coordinator 5.1.1 listening on ${info.coordinator}`);
      return info;
    },
    host_stop: async () => {
      state.hosting = null;
      setProc("coordinator", { state: "stopped", pid: null });
      log("coordinator", "Coordinator stopped.");
    },
    mesh_snapshot: () => {
      const target = state.joined || state.hosting;
      if (!target) return null;
      const selfActive = state.worker.state === "running";
      return {
        mesh_id: "m-7f3a91c2",
        mesh_name: target.mesh_name || "Vision lab",
        coordinator: target.coordinator,
        version: "5.1.1",
        torch_ready: true,
        nodes: nodes(selfActive),
        events: FEED.map(([dt, kind, text]) => ({ at: now() + dt, kind, text })),
      };
    },
    runs: () => (state.joined || state.hosting ? RUNS : []),
    datasets: () => (state.joined || state.hosting ? DATASETS : []),
    check_update: async () => {
      await sleep(700);
      state.update = update;
      emit({ kind: "update", info: update });
      return update;
    },
    install_update: installUpdate,
    restart_app: () => location.reload(),
    get_settings: () => ({ ...state.settings }),
    set_settings: ({ settings }) => (state.settings = { ...settings }),
    open_data_dir: () => emit({ kind: "notice", level: "info", title: "Data folder", body: state.settings.data_dir }),
    hide_window: () => {},
    quit_app: () => {},
  };

  return async (cmd, args = {}) => {
    const fn = commands[cmd];
    if (!fn) throw new Error(`mock: no command ${cmd}`);
    await sleep(40);
    const result = await fn(args);
    return result === undefined ? null : JSON.parse(JSON.stringify(result));
  };
}
