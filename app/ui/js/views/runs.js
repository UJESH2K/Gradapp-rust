// Runs: every training run on the mesh; the host starts, stops and deletes
// them; and the live view of one run: which machines are training it, what
// share of the round each one carries, how fast it goes and how far it is,
// round by round. Contributors see the same view read-only, their own
// machine highlighted.

import { api, onEvent } from "../api.js";
import { store, subscribe, connected, refreshRuns } from "../store.js";
import { h, clear, icon, fmtDuration, fmtAgo, emptyState, toast, busy, vendorOf, VENDORS } from "../ui.js";
import { notConnected } from "./mesh.js";
import { transferList, confirmDialog, downloadTo } from "../transfers.js";

const STATUS = { running: "badge-accent", planning: "badge-cyan", waiting: "badge-warn", completed: "badge-live", stopped: "", failed: "badge-danger" };
const LIVE = new Set(["running", "planning", "waiting"]);
const DEVICE_STATUS = {
  training: ["training", "badge-accent"],
  downloading: ["fetching images", "badge-cyan"],
  loading: ["loading model", "badge-cyan"],
  starting: ["starting", "badge-cyan"],
  uploading: ["uploading weights", "badge-cyan"],
  queued: ["waiting to start", ""],
  done: ["finished shard", "badge-live"],
  dropped: ["dropped", "badge-danger"],
  offline: ["not answering", "badge-danger"],
  superseded: ["overtaken by backup", ""],
  idle: ["not in this round", ""],
};
const RATE_NOTE = {
  "last round": "its images ÷ its training seconds in the last round",
  measured: "the coordinator's running estimate for this workload",
  planned: "the scheduler's estimate (from the GPU benchmark until it has finished a round)",
};

const pct = (x, digits = 0) => `${(x * 100).toFixed(digits)}%`;
const secs = (s) => (s == null ? "–" : s < 60 ? `${s.toFixed(s < 10 ? 1 : 0)} s` : fmtDuration(s));

function lineChart(points, { format = (v) => v.toFixed(2), label, color = "#5b7cfa" }) {
  const W = 560;
  const H = 170;
  const pad = { l: 38, r: 12, t: 12, b: 22 };
  if (!points.length) return h("div", { class: "empty" }, "No rounds yet.");
  const ys = points.map((p) => p.y);
  const min = Math.min(...ys) * 0.92;
  const max = Math.max(...ys) * 1.04 || 1;
  const x = (i) => pad.l + (points.length === 1 ? 0.5 : i / (points.length - 1)) * (W - pad.l - pad.r);
  const y = (v) => pad.t + (1 - (v - min) / (max - min || 1)) * (H - pad.t - pad.b);
  const path = points.map((p, i) => `${i ? "L" : "M"}${x(i).toFixed(1)},${y(p.y).toFixed(1)}`).join(" ");
  const area = `${path} L${x(points.length - 1).toFixed(1)},${H - pad.b} L${x(0).toFixed(1)},${H - pad.b} Z`;
  const ticks = [min, (min + max) / 2, max];
  const el = h("svg", { svg: true, class: "chart", viewBox: `0 0 ${W} ${H}`, preserveAspectRatio: "none", role: "img", "aria-label": label });
  el.innerHTML = `
    <defs><linearGradient id="g-${label}" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="${color}" stop-opacity="0.32"/><stop offset="1" stop-color="${color}" stop-opacity="0"/></linearGradient></defs>
    ${ticks.map((t) => `<line class="grid-line" x1="${pad.l}" x2="${W - pad.r}" y1="${y(t)}" y2="${y(t)}"/><text class="axis" x="${pad.l - 6}" y="${y(t) + 3}" text-anchor="end">${format(t)}</text>`).join("")}
    <path d="${area}" fill="url(#g-${label})"/>
    <path d="${path}" fill="none" stroke="${color}" stroke-width="2" stroke-linejoin="round" stroke-linecap="round"/>
    ${points.map((p, i) => `<circle cx="${x(i)}" cy="${y(p.y)}" r="${i === points.length - 1 ? 4 : 2.6}" fill="${i === points.length - 1 ? color : "#0e1117"}" stroke="${color}" stroke-width="1.4"><title>Round ${p.x}: ${format(p.y)}</title></circle>`).join("")}
    ${points.map((p, i) => (points.length <= 12 || i % 2 === 0 ? `<text class="axis" x="${x(i)}" y="${H - 6}" text-anchor="middle">${p.x}</text>` : "")).join("")}`;
  return el;
}

/** One stacked bar of machines' shares (2px gaps, vendor hue, names inside when they fit). */
function splitBar(parts, { height = 30, labels = true, alt = new Set() } = {}) {
  return h(
    "div",
    { class: "share-split", style: { height: `${height}px` } },
    parts
      .filter((p) => p.share > 0)
      .map((p) =>
        h(
          "span",
          {
            class: `share-seg vendor-${vendorOf(p.backend).vendor}${p.is_self ? " is-self" : ""}${alt.has(p.node_id) ? " is-alt" : ""}`,
            style: { flexGrow: p.share, flexBasis: 0 },
            title: `${p.name}: ${pct(p.share, 1)}${p.samples != null ? ` · ${p.samples} images` : ""}`,
          },
          labels && p.share >= 0.09 ? h("span", { class: "share-seg-label" }, h("span", { class: "truncate" }, p.name), h("span", { class: "mono" }, pct(p.share))) : null
        )
      )
  );
}

/** The second, third... machine of one vendor is drawn a shade darker, the
 * same machine the same way in every bar (color follows the machine). */
function altShades(devices) {
  const seen = new Set();
  const alt = new Set();
  for (const d of devices) {
    const v = vendorOf(d.backend).vendor;
    if (seen.has(v)) alt.add(d.node_id);
    seen.add(v);
  }
  return alt;
}

function basisNote(live) {
  const n = live.devices.reduce((sum, d) => sum + (d.speculative ? 0 : d.samples), 0);
  switch (live.share_basis) {
    case "round":
      return `Share of round ${live.round}'s ${n.toLocaleString()} images. The coordinator sizes each machine's shard from its measured speed so that every machine finishes at the same time.`;
    case "last_round":
      return `Share of the last round's ${n.toLocaleString()} images (the run is ${live.run.status}).`;
    case "plan":
      return "Share of the planned round, before any machine has picked up its shard.";
    case "preview":
      return "No round is planned yet. This is the mesh's plan preview for its default dataset.";
    default:
      return "No round is planned yet, so this compares each machine's measured (or benchmarked) speed. The real split is made when the round starts.";
  }
}

function deviceRow(d, live, alt) {
  const shade = alt.has(d.node_id) ? " is-alt" : "";
  const v = vendorOf(d.backend);
  const [statusText, statusClass] = DEVICE_STATUS[d.status] || [d.status, ""];
  const inRound = d.share > 0 || d.speculative;
  const progress = d.progress ?? 0;
  return h(
    "div",
    { class: `share-row${d.is_self ? " is-self" : ""}${inRound ? "" : " is-out"}`, "data-node": d.node_id },
    h(
      "div",
      { class: "share-who" },
      h("span", { class: `vendor-dot vendor-${v.vendor}${shade}`, "aria-hidden": "true" }),
      h(
        "div",
        { class: "stack-sm", style: { gap: "3px", minWidth: 0 } },
        h("div", { class: "row", style: { gap: "8px", minWidth: 0 } }, h("strong", { class: "truncate", title: d.name }, d.name), d.is_self ? h("span", { class: "badge badge-accent self-badge" }, "This computer") : null, d.speculative ? h("span", { class: "badge badge-cyan" }, "backup copy") : null),
        h("span", { class: "small faint truncate", title: d.gpu }, d.gpu || "GPU not reported"),
        h("div", { class: "row", style: { gap: "6px" } }, h("span", { class: `vendor vendor-${v.vendor}` }, `${v.name} · ${v.api}`), d.port ? h("span", { class: "port-chip mono" }, d.port) : null)
      )
    ),
    h(
      "div",
      { class: "share-amount" },
      h("div", { class: "row-between", style: { alignItems: "baseline" } }, h("span", { class: "share-pct mono" }, inRound && !d.speculative ? pct(d.share, d.share < 0.1 ? 1 : 0) : "–"), h("span", { class: "small faint mono" }, d.samples ? `${d.samples.toLocaleString()} images` : "")),
      h("div", { class: "share-track" }, h("div", { class: `share-fill vendor-${v.vendor}${shade}`, style: { width: `${Math.max(0, Math.min(1, d.share)) * 100}%` } }))
    ),
    h(
      "div",
      { class: "share-speed", title: d.rate_source ? `img/s: ${RATE_NOTE[d.rate_source] || d.rate_source}` : "No speed measured yet" },
      h("span", { class: "mono share-ips" }, d.images_per_sec != null ? d.images_per_sec.toFixed(d.images_per_sec < 10 ? 1 : 0) : "–", h("span", { class: "small faint" }, " img/s")),
      h("span", { class: "small faint" }, d.rate_source || "not measured")
    ),
    h(
      "div",
      { class: "share-progress" },
      h("div", { class: "row-between", style: { gap: "8px" } }, h("span", { class: `badge ${d.late && !["done", "idle"].includes(d.status) ? "badge-warn" : statusClass}` }, ["training", "downloading", "loading", "uploading", "starting"].includes(d.status) ? h("span", { class: "dot dot-live" }) : null, d.late && !["done", "idle"].includes(d.status) ? `${statusText} · late` : statusText), h("span", { class: "small mono faint" }, inRound && d.progress != null ? pct(progress) : "")),
      inRound ? h("div", { class: "shard-track mini" }, h("div", { class: `shard-fill ${d.status === "done" ? "is-done" : d.status === "dropped" || d.status === "offline" ? "is-lost" : d.late ? "is-late" : d.status === "queued" ? "is-queued" : "is-running"}`, style: { width: `${(d.status === "queued" ? 0.02 : Math.max(progress, 0.02)) * 100}%` } })) : null,
      inRound && live.share_basis === "round" ? h("span", { class: "small faint mono" }, `${secs(d.elapsed_seconds)}${d.predicted_seconds ? ` of ~${secs(d.predicted_seconds)} planned` : ""}`) : null
    )
  );
}

function timeline(live) {
  const alt = altShades(live.devices);
  const rows = [...live.timeline];
  const max = Math.max(1, ...rows.map((r) => r.wall_clock_seconds), live.round_elapsed_seconds || 0, live.predicted_round_seconds || 0);
  const current = live.share_basis === "round" && LIVE.has(live.run.status);
  if (!rows.length && !current) return h("div", { class: "empty" }, "The first round has not finished yet.");
  return h(
    "div",
    { class: "timeline" },
    current
      ? h(
          "div",
          { class: "tl-row is-live" },
          h("span", { class: "tl-label mono" }, `R${live.round}`),
          h("div", { class: "tl-bar", style: { width: `${(Math.max(live.round_elapsed_seconds || 0, (live.predicted_round_seconds || 0) * live.round_progress) / max) * 100}%` } }, splitBar(live.devices.filter((d) => !d.speculative), { height: 16, labels: false, alt })),
          h("span", { class: "tl-meta small mono" }, `${pct(live.round_progress)} · ${secs(live.round_elapsed_seconds)}`)
        )
      : null,
    rows
      .slice()
      .reverse()
      .map((r) =>
        h(
          "div",
          { class: "tl-row", title: `Round ${r.round}: ${r.workers} machines, ${r.samples} images, ${r.wall_clock_seconds.toFixed(1)} s (straggler gap ${r.straggler_gap_seconds.toFixed(1)} s, aggregation ${r.aggregation_seconds.toFixed(2)} s), ${r.speedup.toFixed(2)}× speed-up` },
          h("span", { class: "tl-label mono" }, `R${r.round}`),
          h("div", { class: "tl-bar", style: { width: `${(r.wall_clock_seconds / max) * 100}%` } }, splitBar(r.shards, { height: 16, labels: false, alt })),
          h("span", { class: "tl-meta small mono" }, `${r.wall_clock_seconds.toFixed(1)} s · ${r.speedup.toFixed(2)}×`)
        )
      )
  );
}

function legend(devices, alt) {
  return h(
    "div",
    { class: "row wrap", style: { gap: "12px" } },
    devices
      .filter((d) => d.share > 0 && !d.speculative)
      .map((d) => h("span", { class: "share-legend small" }, h("span", { class: `vendor-dot vendor-${vendorOf(d.backend).vendor}${alt.has(d.node_id) ? " is-alt" : ""}` }), d.name, d.is_self ? " (you)" : ""))
  );
}

// ------------------------------------------------------------------ the form

function runForm({ onCreated, onClose }) {
  const datasets = (store.datasets || []).filter((d) => d.available !== false);
  const nodes = (store.mesh?.nodes || []).filter((n) => n.active && n.tier !== "rejected");
  const present = [...new Set(nodes.map((n) => n.backend))].filter((b) => b !== "cpu");
  const stamp = new Date().toISOString().slice(5, 16).replace("T", "-").replace(":", "");
  const form = {
    name: `run-${stamp}`,
    dataset_id: (datasets.find((d) => d.is_default) || datasets[0])?.id || "",
    base_model: "yolov8n.pt",
    mode: "mesh",
    rounds: 4,
    imgsz: 640,
    batch_size: 8,
    backends: [],
    notes: "",
    partition_strategy: "proportional",
    warmup_mode: "first-round",
    warmup_epochs: 1,
    optimizer: "auto",
    lr0: "",
    seed: 0,
    dataloader_workers: "",
    evaluate: true,
    deterministic: true,
    worker_validation: false,
  };
  const el = h("section", { class: "panel run-form" });
  const models = h("select", { class: "select", id: "f-model", onchange: (e) => (form.base_model = e.target.value) }, h("option", { value: "yolov8n.pt" }, "yolov8n.pt"));
  api
    .runOptions()
    .then((o) => {
      if (!o.models.length) return;
      if (!o.models.some((m) => m.name === form.base_model)) form.base_model = o.models[0].name;
      clear(models, o.models.map((m) => h("option", { value: m.name }, `${m.name}${m.bytes ? ` · ${(m.bytes / 1048576).toFixed(1)} MB` : ""}`)));
      models.value = form.base_model;
    })
    .catch(() => {});

  const input = (key, attrs = {}) =>
    h("input", {
      class: "input",
      id: `f-${key}`,
      value: form[key],
      ...attrs,
      oninput: (e) => (form[key] = attrs.type === "number" ? e.target.value : e.target.value),
    });
  const check = (key, label) => h("label", { class: "row small", style: { gap: "8px" } }, h("input", { type: "checkbox", checked: form[key], onchange: (e) => (form[key] = e.target.checked) }), label);
  const field = (label, control, hint) => h("div", { class: "field" }, h("label", { class: "label", for: control.id }, label), control, hint ? h("span", { class: "hint" }, hint) : null);
  const choose = (key, options) => {
    const s = h("select", { class: "select", id: `f-${key}`, onchange: (e) => (form[key] = e.target.value) }, options.map(([v, l]) => h("option", { value: v }, l)));
    s.value = String(form[key]);
    return s;
  };
  const vendorRow = h("div", { class: "chip-row" });
  const drawVendors = () =>
    clear(
      vendorRow,
      h("button", { type: "button", class: `chip${form.backends.length ? "" : " is-on"}`, onclick: () => ((form.backends = []), drawVendors()) }, "Every eligible machine"),
      ["cuda", "xpu", "mps"].map((b) => {
        const on = form.backends.includes(b);
        return h(
          "button",
          { type: "button", class: `chip${on ? " is-on" : ""}`, disabled: !present.includes(b) && !on, title: present.includes(b) ? `${VENDORS[b].name} machines only` : `No eligible ${VENDORS[b].name} machine is online`, onclick: () => ((form.backends = on ? form.backends.filter((x) => x !== b) : [...form.backends, b]), drawVendors()) },
          VENDORS[b].name
        );
      })
    );
  drawVendors();

  const submit = h("button", { class: "btn btn-primary", type: "submit", disabled: !datasets.length }, icon("play", 15), h("span", {}, "Start run"));
  const torch = store.mesh?.torch_ready;
  const num = (v) => (v === "" || v == null ? null : Number(v));
  clear(
    el,
    h("div", { class: "row-between", style: { marginBottom: "14px" } }, h("span", { class: "panel-title" }, "Start a run"), h("button", { class: "btn btn-sm btn-ghost", type: "button", onclick: onClose }, icon("x", 14), h("span", {}, "Close"))),
    !datasets.length ? h("div", { class: "notice notice-warn", style: { marginBottom: "14px" } }, "Add a dataset first: import COCO8 or upload a .zip on the Datasets screen.") : null,
    torch === false ? h("div", { class: "notice notice-warn", style: { marginBottom: "14px" } }, "PyTorch is still installing on this host; the coordinator needs it to average the weights. Runs can start once it finishes.") : null,
    h(
      "div",
      { class: `notice ${nodes.length ? "notice-accent" : "notice-warn"}`, style: { marginBottom: "16px" } },
      nodes.length
        ? `${nodes.length} machine${nodes.length === 1 ? "" : "s"} can train (${present.map((b) => VENDORS[b].name).join(", ") || "CPU only"}). Each round is split between them by measured speed.`
        : "No machine is eligible to train yet. Start one (Home › Train on this machine too) or invite a GPU; the run waits for it."
    ),
    h(
      "form",
      {
        class: "stack",
        onsubmit: (e) => {
          e.preventDefault();
          busy(submit, async () => {
            const request = {
              ...form,
              rounds: num(form.rounds),
              imgsz: num(form.imgsz),
              batch_size: num(form.batch_size),
              seed: num(form.seed) ?? 0,
              warmup_epochs: num(form.warmup_epochs),
              lr0: num(form.lr0),
              dataloader_workers: num(form.dataloader_workers),
              dataset_id: form.dataset_id || null,
              notes: form.notes || null,
              backends: form.backends.length ? form.backends : null,
            };
            const run = await api.runCreate(request);
            toast(`${run.name} started`, "ok");
            onCreated(run);
          });
        },
      },
      h(
        "div",
        { class: "grid grid-3" },
        field("Run name", input("name", { maxlength: 80, required: true })),
        field("Dataset", choose("dataset_id", datasets.length ? datasets.map((d) => [d.id, `${d.name} · ${d.images.toLocaleString()} images`]) : [["", "No datasets"]])),
        field("Base checkpoint", models),
        field("Mode", choose("mode", [["mesh", "Mesh: every eligible machine"], ["solo", "Single GPU baseline"]]), "Run both to measure the speed-up."),
        field("Rounds", input("rounds", { type: "number", min: 1, max: 200 }), "One local epoch per machine per round, then averaging."),
        field("Image size", choose("imgsz", [320, 416, 512, 640, 800].map((v) => [String(v), `${v} px`]))),
        field("Batch size per machine", input("batch_size", { type: "number", min: 1, max: 128 }), "A ceiling: smaller GPUs get what fits."),
        h("div", { class: "field", style: { gridColumn: "span 2" } }, h("span", { class: "label" }, "GPU vendors"), vendorRow, h("span", { class: "hint" }, "Restrict a run to one vendor or a mix, for cross-vendor comparisons."))
      ),
      h(
        "details",
        { class: "advanced" },
        h("summary", {}, "Advanced: scheduling and training recipe"),
        h(
          "div",
          { class: "grid grid-3", style: { paddingBottom: "14px" } },
          field("Shard sizing", choose("partition_strategy", [["proportional", "Affine: overhead + rate, finish together"], ["proportional-linear", "Linear: proportional to rate"], ["equal", "Equal split (baseline)"]])),
          field("Learning-rate warmup", choose("warmup_mode", [["first-round", "Round 1 only (recommended)"], ["none", "Never"], ["every-round", "Every round (v4)"]])),
          field("Warmup epochs (round 1)", input("warmup_epochs", { type: "number", min: 0, max: 10, step: 0.5 })),
          field("Optimiser", choose("optimizer", [["auto", "Ultralytics auto"], ["AdamW", "AdamW"], ["Adam", "Adam"], ["SGD", "SGD"]]), "Identical on every vendor."),
          field("Learning rate", input("lr0", { type: "number", min: 0, max: 1, step: "any", placeholder: "automatic" })),
          field("Seed", input("seed", { type: "number", min: 0 })),
          field("Dataloader processes", input("dataloader_workers", { type: "number", min: 0, max: 16, placeholder: "each machine decides" })),
          field("Notes", input("notes", { placeholder: "What is this run testing?" })),
          h("div", { class: "stack-sm", style: { paddingTop: "22px" } }, check("evaluate", "Score the global model every round (mAP50)"), check("deterministic", "Deterministic kernels"), check("worker_validation", "Also validate on every worker (slower)"))
        )
      ),
      h("div", { class: "row", style: { gap: "10px" } }, submit, h("span", { class: "small faint" }, "The run starts at once; machines pick up their shards within a few seconds."))
    )
  );
  return el;
}

// ------------------------------------------------------------------ screen

export function renderRuns(root, { go }) {
  let openId = null;
  let live = null;
  let liveError = null;
  let showForm = false;
  let formEl = null;
  let canManage = null;
  const head = h("div");
  const formSlot = h("div");
  const list = h("div", { class: "panel panel-flush" });
  const detail = h("div", { class: "stack" });
  const [transfers, offTransfers] = transferList((t) => t.label.startsWith("Downloading weights"));
  let liveTimer = 0;
  let pending = 0;

  async function loadLive() {
    clearTimeout(liveTimer);
    const id = openId;
    if (!id || !connected()) return;
    try {
      const next = await api.runLive(id);
      if (id !== openId) return;
      live = next;
      liveError = null;
      if (canManage !== live.can_manage) {
        canManage = live.can_manage;
        if (!canManage) showForm = false;
        drawHead();
        drawForm();
      }
    } catch (e) {
      liveError = e.message;
    }
    drawDetail();
    // Fast while the run is live (the event stream also nudges), slow after.
    if (!document.hidden) liveTimer = setTimeout(loadLive, live && LIVE.has(live.run.status) ? 2000 : 15000);
  }

  function nudge() {
    clearTimeout(pending);
    pending = setTimeout(() => {
      loadLive();
    }, 250);
  }

  function drawHead() {
    // Known once the coordinator answered (run_options / run_live): this app
    // may host one mesh and contribute to another.
    const manage = live ? live.can_manage : canManage;
    clear(
      head,
      h(
        "header",
        { class: "page-head" },
        h("div", {}, h("h1", { class: "page-title" }, "Runs"), h("p", { class: "lead" }, "Each round, every machine trains a shard sized to finish together, and the host averages the weights.")),
        manage == null
          ? null
          : manage
          ? h("button", { class: `btn${showForm ? "" : " btn-primary"}`, type: "button", onclick: () => ((showForm = !showForm), drawForm(), drawHead()) }, icon(showForm ? "x" : "plus", 15), h("span", {}, showForm ? "Close" : "New run"))
          : h("span", { class: "badge" }, icon("eye", 12), "Watching: the host starts and stops runs")
      )
    );
  }

  function drawForm() {
    if (!showForm) {
      formEl = null;
      return clear(formSlot);
    }
    if (!formEl)
      formEl = runForm({
        onClose: () => ((showForm = false), drawForm(), drawHead()),
        onCreated: async (run) => {
          showForm = false;
          openId = run.id;
          live = null;
          drawForm();
          drawHead();
          await refreshRuns();
          loadLive();
        },
      });
    clear(formSlot, formEl);
  }

  function draw() {
    if (!connected()) return notConnected(root, "Runs", go);
    if (!root.contains(list)) {
      clear(root, head, formSlot, transfers, h("div", { class: "runs-layout" }, list, detail));
      drawHead();
      drawForm();
    }
    const runs = store.runs;
    if (!runs) return clear(list, h("div", { class: "empty" }, "Loading runs…"));
    if (!runs.length) {
      clear(
        list,
        emptyState(
          "No runs yet",
          canManage ? "Press New run to train on this mesh. Add a dataset first if there is none." : "When the host starts a run, it appears here and you can watch every machine's share live."
        )
      );
      live = null;
      return clear(detail);
    }
    if (!openId || !runs.some((r) => r.id === openId)) {
      openId = runs[0].id;
      live = null;
      loadLive();
    }
    clear(
      list,
      h("div", { class: "panel-header" }, h("span", { class: "panel-title" }, `${runs.length} run${runs.length === 1 ? "" : "s"}`), h("span", { class: "small faint" }, `${runs.filter((r) => LIVE.has(r.status)).length} live`)),
      runs.map((run) =>
        h(
          "button",
          { class: `run-row${run.id === openId ? " is-open" : ""}`, type: "button", "data-run": run.id, onclick: () => (openId !== run.id && ((openId = run.id), (live = null), loadLive()), draw()) },
          h("div", { class: "stack-sm", style: { gap: "2px", minWidth: 0 } }, h("span", { class: "truncate", style: { fontWeight: 560 } }, run.name), h("span", { class: "small faint truncate" }, `${run.dataset} · ${run.base_model} · ${fmtAgo(run.started_at)}`)),
          h("span", { class: `badge ${STATUS[run.status] || ""}`, style: { justifySelf: "end" } }, run.status),
          h("div", { class: "row", style: { gridColumn: "1 / -1", gap: "10px" } }, h("div", { class: "meter grow" }, h("div", { class: `meter-fill${run.status === "failed" ? " is-danger" : run.status === "completed" ? " is-cyan" : ""}`, style: { width: `${(run.current_round / Math.max(1, run.rounds)) * 100}%` } })), h("span", { class: "small mono faint nowrap" }, `${run.current_round}/${run.rounds}`))
        )
      )
    );
    drawDetail();
  }

  function actions(l) {
    const run = l.run;
    const out = [];
    if (l.has_artifact)
      out.push(
        h(
          "button",
          { class: "btn btn-sm", type: "button", title: "The averaged global model after the latest round, as a .pt file", onclick: (e) => busy(e.currentTarget, () => downloadTo(`Save the weights of ${run.name} to…`, (dir) => api.runDownloadWeights(run.id, run.name, dir))) },
          icon("download", 14),
          h("span", {}, LIVE.has(run.status) ? "Weights so far" : "Download weights")
        )
      );
    if (l.can_manage && LIVE.has(run.status))
      out.push(
        h(
          "button",
          {
            class: "btn btn-sm btn-danger",
            type: "button",
            onclick: (e) =>
              busy(e.currentTarget, async () => {
                if (!(await confirmDialog({ title: `Stop ${run.name}?`, body: "Machines drop the shard they are training. Rounds already averaged are kept, and so are their weights.", confirm: "Stop run", danger: true }))) return;
                await api.runStop(run.id);
                toast(`${run.name} stopped`, "ok");
                await refreshRuns();
                loadLive();
              }),
          },
          icon("stop", 14),
          h("span", {}, "Stop")
        )
      );
    if (l.can_manage && !LIVE.has(run.status))
      out.push(
        h(
          "button",
          {
            class: "btn btn-sm btn-ghost danger-text",
            type: "button",
            onclick: (e) =>
              busy(e.currentTarget, async () => {
                if (!(await confirmDialog({ title: `Delete ${run.name}?`, body: "Its history and its trained weights are removed from the host. Download the weights first if you want to keep them.", confirm: "Delete run", danger: true }))) return;
                await api.runDelete(run.id);
                toast(`${run.name} deleted`, "ok");
                openId = null;
                live = null;
                await refreshRuns();
              }),
          },
          icon("trash", 14),
          h("span", {}, "Delete")
        )
      );
    return out;
  }

  function drawDetail() {
    if (!openId) return clear(detail);
    if (!live) return clear(detail, liveError ? h("div", { class: "notice notice-danger" }, liveError) : h("section", { class: "panel" }, h("div", { class: "empty" }, "Loading the run…")));
    const l = live;
    const run = l.run;
    const isLive = LIVE.has(run.status);
    const overall = Math.min(1, (run.current_round + (isLive && l.share_basis === "round" ? l.round_progress : 0)) / Math.max(1, run.rounds));
    const ticks = Array.from({ length: Math.max(0, run.rounds - 1) }, (_, i) => h("span", { class: "train-bar-tick", style: { left: `${((i + 1) / run.rounds) * 100}%` } }));
    const inRound = l.devices.filter((d) => d.share > 0 && !d.speculative);
    const metrics = run.round_metrics || [];
    const maps = metrics.filter((m) => m.map50 != null).map((m) => ({ x: m.round, y: m.map50 }));
    const losses = metrics.filter((m) => m.loss != null).map((m) => ({ x: m.round, y: m.loss }));
    const state =
      run.status === "waiting"
        ? "Waiting for an eligible machine"
        : run.status === "planning"
          ? "Planning the next round"
          : run.status === "running"
            ? l.share_basis === "round"
              ? `${inRound.filter((d) => ["training", "downloading", "loading", "starting", "uploading"].includes(d.status)).length} of ${inRound.length} machines working on round ${l.round}`
              : "Averaging and planning the next round"
            : run.status === "completed"
              ? `Finished in ${fmtDuration(run.wall_clock_seconds)}`
              : run.status === "failed"
                ? "Failed"
                : "Stopped";
    clear(
      detail,
      !l.can_manage ? h("div", { class: "notice notice-accent small" }, "You are watching this run as a contributor. The host starts and stops runs; your machine is highlighted below.") : null,
      h(
        "section",
        { class: "train-hero", style: { gridTemplateColumns: "1fr" } },
        h(
          "div",
          { class: "train-progress" },
          h(
            "div",
            { class: "row-between", style: { alignItems: "flex-start", gap: "16px" } },
            h(
              "div",
              { class: "stack-sm", style: { gap: "3px", minWidth: 0 } },
              h("div", { class: "row", style: { gap: "10px" } }, h("h2", { class: "truncate" }, run.name), h("span", { class: `badge ${STATUS[run.status] || ""}` }, isLive ? h("span", { class: "dot dot-live" }) : null, run.status)),
              h("span", { class: "small faint" }, `${run.dataset} · ${run.base_model} · ${l.total_samples.toLocaleString()} images · ${l.imgsz} px · batch ≤${l.batch_size} · ${l.mode === "solo" ? "single GPU" : "mesh"} · started ${fmtAgo(run.started_at)}`)
            ),
            h("div", { class: "row wrap", style: { gap: "8px", justifyContent: "flex-end" } }, actions(l))
          ),
          h("div", { class: "row", style: { alignItems: "baseline", gap: "14px", marginTop: "10px" } }, h("div", { class: "train-percent mono" }, pct(overall)), h("span", { class: "muted" }, `round ${Math.min(l.round, run.rounds)} of ${run.rounds} · ${state}`)),
          h("div", { class: "train-bar" }, h("div", { class: "train-bar-fill", style: { width: `${overall * 100}%` } }), ticks),
          run.error ? h("div", { class: "notice notice-danger", style: { marginTop: "14px" } }, run.error) : null,
          l.waiting_reasons.length ? h("div", { class: "notice notice-warn small", style: { marginTop: "14px" } }, h("strong", {}, "Not eligible: "), l.waiting_reasons.join(" · ")) : null,
          h(
            "dl",
            { class: "ld-telemetry", style: { gridTemplateColumns: "repeat(5, minmax(0,1fr))", margin: "22px 0 0", paddingBottom: 0 } },
            [
              ["mAP50", run.map50 != null ? run.map50.toFixed(3) : "–", l.best_map50 != null ? `best ${l.best_map50.toFixed(3)}` : "scored after each round", true],
              ["Speed-up", run.speedup ? `${run.speedup.toFixed(2)}×` : "–", "vs one machine doing it all"],
              ["Efficiency", l.efficiency ? pct(l.efficiency) : "–", "speed-up per machine"],
              ["Machines", `${inRound.length || run.workers}`, isLive ? "in this round" : "at peak"],
              ["Wall clock", run.wall_clock_seconds ? fmtDuration(run.wall_clock_seconds).replace(" min ", "m ").replace(/ s$/, "s") : "–", isLive ? "rounds finished so far" : "total"],
            ].map(([label, value, foot, accent]) => h("div", { title: foot }, h("dt", {}, label), h("dd", { class: accent ? "accent" : "" }, value), h("span", { class: "small faint truncate", style: { display: "block" } }, foot)))
          )
        )
      ),
      h(
        "section",
        { class: "panel share-panel", "data-basis": l.share_basis },
        h(
          "div",
          { class: "row-between", style: { gap: "16px", alignItems: "flex-start", marginBottom: "12px" } },
          h("div", { class: "stack-sm", style: { gap: "3px", flex: "1 1 0", minWidth: 0 } }, h("span", { class: "panel-title" }, l.share_basis === "round" ? `Who is training round ${l.round}` : l.share_basis === "last_round" ? "How the last round was split" : "How the mesh would split it"), h("span", { class: "small faint" }, basisNote(l))),
          l.share_basis === "round" ? h("div", { class: "stack-sm", style: { gap: 0, alignItems: "flex-end", flex: "none" } }, h("span", { class: "mono", style: { fontSize: "1.25rem" } }, pct(l.round_progress)), h("span", { class: "small faint nowrap" }, `of this round · ${secs(l.round_elapsed_seconds)}${l.predicted_round_seconds ? ` / ~${secs(l.predicted_round_seconds)}` : ""}`)) : null
        ),
        inRound.length ? splitBar(inRound, { alt: altShades(l.devices) }) : null,
        inRound.length ? h("div", { style: { margin: "10px 0 6px" } }, legend(l.devices, altShades(l.devices))) : null,
        h(
          "div",
          { class: "share-table" },
          h("div", { class: "share-row share-head small faint" }, h("span", {}, "Machine"), h("span", {}, "Share of the round"), h("span", {}, "Speed"), h("span", {}, "Its shard")),
          l.devices.length ? l.devices.map((d) => deviceRow(d, l, altShades(l.devices))) : h("div", { class: "empty" }, "No machine is in this mesh right now.")
        )
      ),
      h(
        "div",
        { class: "grid grid-2" },
        h("section", { class: "panel" }, h("div", { class: "row-between", style: { marginBottom: "12px" } }, h("span", { class: "panel-title" }, "Round timeline"), h("span", { class: "small faint" }, "bar length = wall clock, segments = each machine's share")), timeline(l)),
        maps.length
          ? h("section", { class: "panel" }, h("div", { class: "row-between", style: { marginBottom: "10px" } }, h("span", { class: "panel-title" }, "Accuracy by round"), h("span", { class: "small faint" }, "mAP50 on the validation split")), lineChart(maps, { label: "map", format: (v) => v.toFixed(2) }))
          : h(
              "section",
              { class: "panel" },
              h("div", { class: "row-between", style: { marginBottom: "10px" } }, h("span", { class: "panel-title" }, "Training loss by round"), h("span", { class: "small faint" }, losses.length ? "mean of the machines' reports" : "no scores yet")),
              losses.length ? lineChart(losses, { label: "loss", format: (v) => v.toFixed(2), color: "#22d3ee" }) : h("div", { class: "empty" }, "mAP and loss appear after the first round is averaged.")
            )
      ),
      metrics.length
        ? h(
            "section",
            { class: "panel panel-flush" },
            h("div", { class: "panel-header" }, h("span", { class: "panel-title" }, "Rounds")),
            h(
              "div",
              { class: "table-scroll", style: { maxHeight: "300px" } },
              h(
                "table",
                { class: "table" },
                h("thead", {}, h("tr", {}, ["Round", "Machines", "Wall clock", "Straggler gap", "Aggregation", "Speed-up", "Loss", "mAP50"].map((t, i) => h("th", { class: i ? "num" : "" }, t)))),
                h(
                  "tbody",
                  {},
                  [...metrics].reverse().map((r) =>
                    h(
                      "tr",
                      {},
                      h("td", { class: "mono" }, r.round),
                      h("td", { class: "num" }, r.workers),
                      h("td", { class: "num" }, `${r.wall_clock_seconds.toFixed(1)} s`),
                      h("td", { class: "num" }, `${r.straggler_gap_seconds.toFixed(1)} s`),
                      h("td", { class: "num" }, `${r.aggregation_seconds.toFixed(2)} s`),
                      h("td", { class: "num" }, `${r.speedup.toFixed(2)}×`),
                      h("td", { class: "num" }, r.loss != null ? r.loss.toFixed(3) : "–"),
                      h("td", { class: "num accent" }, r.map50 != null ? r.map50.toFixed(3) : "–")
                    )
                  )
                )
              )
            )
          )
        : null
    );
  }

  draw();
  refreshRuns();
  api
    .runOptions()
    .then((o) => {
      canManage = o.can_manage;
      if (root.contains(head)) drawHead();
    })
    .catch(() => {});
  const timer = setInterval(() => !document.hidden && connected() && refreshRuns(), 5000);
  const off = subscribe((r) => {
    if (r.has("runs") || r.has("status")) draw();
  });
  const offEvents = onEvent((e) => {
    if (e.kind !== "mesh_event") return;
    const k = e.event.kind;
    if (k.startsWith("round.") || k.startsWith("shard.") || k.startsWith("run.")) nudge();
    if (k === "run.created") {
      // A run just started: show it, unless a live run is already open.
      refreshRuns().then(() => {
        const current = (store.runs || []).find((r) => r.id === openId);
        const fresh = (store.runs || []).find((r) => LIVE.has(r.status));
        if (fresh && (!current || !LIVE.has(current.status)) && fresh.id !== openId) {
          openId = fresh.id;
          live = null;
          draw();
          loadLive();
        }
      });
    } else if (k.startsWith("run.")) refreshRuns();
  });
  return () => {
    clearInterval(timer);
    clearTimeout(liveTimer);
    clearTimeout(pending);
    off();
    offEvents();
    offTransfers();
  };
}
