// Mock of the dataset and run commands (app/src-tauri/src/datasets.rs and
// runs.rs) for the browser preview: same names, same shapes. A run trains in
// simulated rounds; shares follow each machine's speed, like the coordinator's
// shard plan. Invented but plausible.

const now = () => Date.now() / 1000;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

const MACHINES = [
  { node_id: "n-3090", name: "lab-rtx3090", gpu: "NVIDIA GeForce RTX 3090", backend: "cuda", port: "cuda-cu130", ips: 78.2 },
  { node_id: "n-m3max", name: "studio-m3max", gpu: "Apple M3 Max (40-core GPU)", backend: "mps", port: "mps", ips: 36.4 },
  { node_id: "n-self", name: "ujesh-zephyrus", gpu: "NVIDIA GeForce RTX 4070 Laptop GPU", backend: "cuda", port: "cuda-cu130", ips: 41.7, self: true },
  { node_id: "n-a770", name: "arc-a770-tower", gpu: "Intel Arc A770 16GB", backend: "xpu", port: "xpu", ips: 27.9 },
];
const IDLE = [{ node_id: "n-nuc", name: "hallway-nuc", gpu: "No supported GPU", backend: "cpu", port: "cpu", online: false }];

const CATALOGUE = [
  { key: "coco8", name: "COCO8", images: 8, classes: 80, download_mb: 1, blurb: "Eight images. Proves the pipeline runs. Useless as a result.", good_for: "smoke test" },
  { key: "coco128", name: "COCO128", images: 128, classes: 80, download_mb: 7, blurb: "A 128-image slice of COCO. Fast enough to iterate on the harness.", good_for: "smoke test" },
  { key: "african-wildlife", name: "African Wildlife", images: 1052, classes: 4, download_mb: 100, blurb: "Four animal classes, clean labels, small download. A sensible first real sweep.", good_for: "100 and 1000 image sweeps" },
  { key: "VisDrone", name: "VisDrone2019-DET", images: 6471, classes: 10, download_mb: 2300, blurb: "Drone imagery, ten classes, many small objects.", good_for: "the full scaling ladder" },
  { key: "coco", name: "COCO 2017", images: 118287, classes: 80, download_mb: 20000, blurb: "The citable standard, with published YOLO baselines. A 20 GB download.", good_for: "a headline result" },
];

const COCO = ["person", "bicycle", "car", "motorcycle", "airplane", "bus", "train", "truck", "boat", "traffic light", "fire hydrant", "stop sign"];

export function runsMock({ state, emit, channel }) {
  const datasets = [
    { id: "d-parts", name: "workshop-parts", images: 4812, val_images: 602, classes: 14, class_names: ["bolt", "nut", "washer", "bracket", "hinge", "spring", "gear", "bearing", "clip", "rivet", "screw", "pin"], is_default: true, size_mb: 1830, available: true, source: "upload", standard_key: null, created_at: now() - 5 * 86400 },
    { id: "std-coco128", name: "COCO128", images: 128, val_images: 128, classes: 80, class_names: COCO, is_default: false, size_mb: null, available: true, source: "standard", standard_key: "coco128", created_at: now() - 86400 },
  ];
  const imp = { running: false, key: null, message: null, error: null };
  const transfers = new Map();

  // One live run (round 13 of 20 in progress) plus history.
  const roundSeconds = 42;
  const mkHistory = (n, total) =>
    Array.from({ length: n }, (_, i) => {
      const speedSum = MACHINES.reduce((s, m) => s + m.ips, 0);
      const shards = MACHINES.map((m) => ({ node_id: m.node_id, name: m.name, backend: m.backend, samples: Math.round((total * m.ips) / speedSum), share: m.ips / speedSum, seconds: 38 + (i % 3), compute_seconds: 33 }));
      return { round: i + 1, wall_clock_seconds: 46 - Math.min(6, i * 0.6) + Math.sin(i) * 1.5, makespan_seconds: 40, straggler_gap_seconds: Math.max(0.6, 5.5 - i * 0.45), aggregation_seconds: 0.62, samples: total, workers: 4, speedup: Math.min(3.4, 2.2 + i * 0.1), map50: Math.min(0.79, 0.18 + Math.log1p(i) * 0.23), loss: Math.max(0.9, 3.4 - Math.log1p(i) * 0.98), shards };
    });
  const runs = [
    { id: "r-1043", name: "parts-detector-v3", status: "running", dataset: "workshop-parts", dataset_id: "d-parts", base_model: "yolo11n.pt", rounds: 20, current_round: 12, imgsz: 640, batch_size: 16, total: 4812, startedRound: now() - 17, started_at: now() - 1560, has_artifact: true, history: mkHistory(12, 4812), error: null, mode: "mesh" },
    { id: "r-1038", name: "coco128-smoke", status: "completed", dataset: "COCO128", dataset_id: "std-coco128", base_model: "yolov8n.pt", rounds: 6, current_round: 6, imgsz: 640, batch_size: 8, total: 128, started_at: now() - 86400, has_artifact: true, history: mkHistory(6, 128), error: null, mode: "mesh" },
    { id: "r-1029", name: "voc-subset-xpu-check", status: "failed", dataset: "VOC 2012 (subset)", dataset_id: "d-voc", base_model: "yolov8s.pt", rounds: 10, current_round: 3, imgsz: 640, batch_size: 16, total: 2500, started_at: now() - 4 * 86400, has_artifact: true, history: mkHistory(3, 2500), error: "arc-a770-tower ran out of GPU memory at batch 16; its batch is now capped at 8.", mode: "mesh" },
  ];

  const connected = () => Boolean(state.joined || state.hosting);
  const canManage = () => Boolean(state.hosting) && !state.joined;
  const meshEvent = (kind, text) => emit({ kind: "mesh_event", event: { at: now(), kind, text } });

  function summary(r) {
    const h = r.history;
    const wall = h.reduce((s, x) => s + x.wall_clock_seconds, 0);
    return {
      id: r.id, name: r.name, status: r.status, dataset: r.dataset, base_model: r.base_model, rounds: r.rounds, current_round: r.current_round,
      workers: r.status === "running" ? MACHINES.length : h.length ? 4 : 0, speedup: h.length ? h[h.length - 1].speedup : null,
      map50: h.length ? h[h.length - 1].map50 : null, wall_clock_seconds: wall || null, started_at: r.started_at, error: r.error,
      round_metrics: h.map((x) => ({ round: x.round, wall_clock_seconds: x.wall_clock_seconds, straggler_gap_seconds: x.straggler_gap_seconds, aggregation_seconds: x.aggregation_seconds, speedup: x.speedup, workers: x.workers, map50: x.map50, loss: x.loss })),
    };
  }

  // Advance live runs: a round lasts ~roundSeconds; each machine's shard is
  // sized by speed, so they finish within a few seconds of each other.
  function tick(r) {
    if (r.status !== "running") return;
    while (now() - r.startedRound > roundSeconds + 3) {
      r.history.push(mkHistory(r.history.length + 1, r.total)[r.history.length]);
      r.current_round += 1;
      r.startedRound += roundSeconds + 3;
      meshEvent("round.completed", `Round ${r.current_round} done in ${roundSeconds.toFixed(1)} s.`);
      if (r.current_round >= r.rounds) {
        r.status = "completed";
        meshEvent("run.completed", `Run ${r.name} completed.`);
        return;
      }
    }
  }

  function live(r) {
    tick(r);
    const speedSum = MACHINES.reduce((s, m) => s + m.ips, 0);
    const elapsed = now() - r.startedRound;
    const last = r.history[r.history.length - 1];
    const running = r.status === "running";
    const devices = MACHINES.map((m, i) => {
      const share = m.ips / speedSum;
      const samples = Math.round(r.total * share);
      const planned = roundSeconds - 3 + i * 0.8;
      const t = elapsed - 2.5;
      const progress = running ? Math.max(0, Math.min(1, t / planned)) : 1;
      const status = !running ? "done" : elapsed < 2.5 ? "downloading" : progress >= 1 ? (elapsed < planned + 4.5 ? "uploading" : "done") : "training";
      return { node_id: m.node_id, name: m.name, gpu: m.gpu, backend: m.backend, port: m.port, is_self: Boolean(m.self) && Boolean(state.joined || state.worker?.state === "running"), online: true, share, samples, status, progress: status === "downloading" ? null : progress, images_per_sec: m.ips * (0.97 + 0.06 * Math.sin(now() / 7 + i)), rate_source: last ? "last round" : "planned", predicted_seconds: planned, elapsed_seconds: running ? elapsed : last?.shards?.[i]?.seconds ?? null, late: false, batch_size: r.batch_size, tier: m.backend === "xpu" ? "probation" : "full", speculative: false, error: null };
    });
    for (const m of IDLE) devices.push({ node_id: m.node_id, name: m.name, gpu: m.gpu, backend: m.backend, port: m.port, is_self: false, online: false, share: 0, samples: 0, status: "offline", progress: null, images_per_sec: null, rate_source: null, predicted_seconds: null, elapsed_seconds: null, late: false, batch_size: null, tier: "rejected", speculative: false, error: null });
    devices.sort((a, b) => b.share - a.share);
    const inRound = devices.filter((d) => d.share > 0);
    const progress = inRound.reduce((s, d) => s + d.share * (d.progress || 0), 0);
    return {
      run: summary(r), can_manage: canManage(), mode: r.mode, partition_strategy: "proportional", imgsz: r.imgsz, batch_size: r.batch_size, total_samples: r.total,
      has_artifact: r.has_artifact, efficiency: last ? last.speedup / 4 : null, best_map50: last ? Math.max(...r.history.map((x) => x.map50)) : null, map50_95: null,
      round: running ? r.current_round + 1 : r.current_round, round_progress: running ? progress : 1, round_elapsed_seconds: running ? elapsed : null, predicted_round_seconds: roundSeconds,
      share_basis: running ? "round" : r.history.length ? "last_round" : "throughput", devices, timeline: r.history, waiting_reasons: [],
    };
  }

  function transfer(kind, label, total, durationMs, result, after) {
    const t = { id: `t${Math.random().toString(16).slice(2, 10)}`, kind, label, done: 0, total, state: "running", error: null, result: null, started_at: now() };
    transfers.set(t.id, t);
    channel("transfer", { ...t });
    const started = Date.now();
    (async () => {
      while (t.state === "running") {
        await sleep(150);
        if (t.state !== "running") return;
        t.done = Math.min(total, Math.round((total * (Date.now() - started)) / durationMs));
        if (t.done >= total) t.state = kind === "upload" ? "processing" : "running";
        channel("transfer", { ...t });
        if (t.done >= total) break;
      }
      if (t.state === "cancelled") return;
      if (kind === "upload") await sleep(1500);
      t.state = "done";
      t.result = result;
      channel("transfer", { ...t });
      after?.();
    })();
    return t.id;
  }

  const need = () => {
    if (!connected()) throw new Error("This machine is not in a mesh.");
  };
  const needHost = () => {
    need();
    if (!canManage()) throw new Error("Only the machine hosting the mesh can change this.");
  };

  return {
    runs: () => (connected() ? runs.map((r) => (tick(r), summary(r))) : []),
    datasets: () => (connected() ? datasets.map((d) => ({ id: d.id, name: d.name, images: d.images, classes: d.classes, is_default: d.is_default, size_mb: d.size_mb })) : []),
    datasets_overview: () => {
      need();
      return { datasets, catalogue: CATALOGUE.map((c) => ({ ...c, on_mesh: datasets.some((d) => d.standard_key === c.key) })), import: { ...imp }, can_manage: canManage(), torch_ready: true };
    },
    dataset_import: async ({ key, makeDefault }) => {
      needHost();
      if (imp.running) throw new Error("Another import is already running.");
      const entry = CATALOGUE.find((c) => c.key === key);
      Object.assign(imp, { running: true, key, message: `Resolving ${entry.name}`, error: null });
      (async () => {
        for (const message of [`Downloading ${entry.key}.zip from github.com/ultralytics/assets`, `Unzipping ${entry.images} images`, `Checking labels`]) {
          await sleep(1400);
          imp.message = message;
          meshEvent("dataset.importing", `Dataset ${entry.key}: importing.`);
        }
        await sleep(1000);
        if (makeDefault) datasets.forEach((d) => (d.is_default = false));
        datasets.unshift({ id: `std-${key}`, name: entry.name, images: entry.images, val_images: entry.images, classes: entry.classes, class_names: COCO, is_default: Boolean(makeDefault), size_mb: null, available: true, source: "standard", standard_key: key, created_at: now() });
        Object.assign(imp, { running: false, key: null, message: `Imported ${entry.name}` });
        meshEvent("dataset.added", `Dataset ${entry.name}: added.`);
      })();
    },
    dataset_upload: async ({ path, name, makeDefault }) => {
      needHost();
      const bytes = 1_873_412_096;
      return transfer("upload", `Uploading ${name}`, bytes, 14000, `${name}: 3,140 training images`, () => {
        if (makeDefault) datasets.forEach((d) => (d.is_default = false));
        datasets.unshift({ id: `d-${Date.now().toString(16)}`, name, images: 3140, val_images: 392, classes: 6, class_names: ["crack", "dent", "rust", "scratch", "chip", "stain"], is_default: Boolean(makeDefault), size_mb: Math.round(bytes / 1048576), available: true, source: "upload", standard_key: null, created_at: now() });
        meshEvent("dataset.added", `Dataset ${name}: added.`);
        void path;
      });
    },
    dataset_set_default: ({ id }) => {
      needHost();
      datasets.forEach((d) => (d.is_default = d.id === id));
    },
    dataset_delete: ({ id }) => {
      needHost();
      if (runs.some((r) => r.dataset_id === id && r.status === "running")) throw new Error("This dataset is in use by a running job");
      datasets.splice(datasets.findIndex((d) => d.id === id), 1);
    },
    dataset_download: ({ name, dir }) => {
      need();
      return transfer("download", `Downloading ${name}`, 412_000_000, 5000, `${dir}\\${name}.zip`);
    },
    pick_dataset_zip: () => ({ path: "C:\\Users\\ujesh\\Downloads\\surface-defects-yolo.zip", name: "surface-defects-yolo", bytes: 1_873_412_096 }),
    pick_folder: () => "C:\\Users\\ujesh\\Downloads",
    transfers: () => [...transfers.values()],
    transfer_cancel: ({ id }) => {
      const t = transfers.get(id);
      if (!t) throw new Error("That transfer has already finished.");
      t.state = "cancelled";
      channel("transfer", { ...t });
    },
    run_options: () => {
      need();
      return { models: [{ name: "yolov8n.pt", bytes: 6549796 }, { name: "yolo11n.pt", bytes: 5613764 }, { name: "yolov8s.pt", bytes: 22588772 }], can_manage: canManage() };
    },
    run_create: async ({ request }) => {
      needHost();
      await sleep(400);
      const ds = datasets.find((d) => d.id === request.dataset_id) || datasets.find((d) => d.is_default) || datasets[0];
      if (!ds) throw new Error("No dataset is available. Upload one from the Datasets page first.");
      const r = { id: `r-${Math.floor(Math.random() * 9000 + 1000)}`, name: request.name, status: "running", dataset: ds.name, dataset_id: ds.id, base_model: request.base_model, rounds: request.rounds, current_round: 0, imgsz: request.imgsz, batch_size: request.batch_size, total: ds.images, startedRound: now(), started_at: now(), has_artifact: false, history: [], error: null, mode: request.mode };
      runs.unshift(r);
      meshEvent("run.created", `Run ${r.name} created (${r.rounds} rounds).`);
      return summary(r);
    },
    run_stop: ({ id }) => {
      needHost();
      const r = runs.find((x) => x.id === id);
      r.status = "stopped";
      meshEvent("run.stopped", "Run stopped.");
    },
    run_delete: ({ id }) => {
      needHost();
      const i = runs.findIndex((x) => x.id === id);
      if (runs[i].status === "running") throw new Error("Stop the run before deleting it");
      runs.splice(i, 1);
    },
    run_live: ({ id }) => {
      need();
      const r = runs.find((x) => x.id === id);
      if (!r) throw new Error("Unknown run");
      return live(r);
    },
    run_download_weights: ({ id, name, dir }) => {
      need();
      return transfer("download", `Downloading weights of ${name}`, 6_310_221, 2500, `${dir}\\${name}-${id}.pt`);
    },
  };
}
