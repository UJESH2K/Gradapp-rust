// Mesh: who is in it (v5 NodeCard), the hardware mix by vendor, and the live
// feed of what the scheduler decided.

import { api } from "../api.js";
import { store, subscribe, connected, hosting, contributing, refreshMesh, refreshStatus } from "../store.js";
import { h, clear, icon, busy, emptyState, stat, meter, vendorBadge, fmtMemory, fmtClock } from "../ui.js";

const PHASE = { downloading: "fetching data", loading: "loading model", training: "training", uploading: "sending weights" };
const TONE = { "node.joined": "var(--live)", "round.completed": "var(--accent)", "round.evaluated": "var(--cyan)", "shard.speculated": "var(--warn)", "node.offline": "var(--warn)", "run.failed": "var(--danger)", "shard.failed": "var(--danger)" };
const MIX = [
  { backend: "cuda", vendor: "nvidia", name: "NVIDIA", api: "CUDA", needs: "GeForce GTX 900 or newer on Windows or Linux, with the NVIDIA driver." },
  { backend: "xpu", vendor: "intel", name: "Intel", api: "XPU", needs: "Arc A or B series, or Core Ultra with Arc Graphics, on Windows or Linux." },
  { backend: "mps", vendor: "apple", name: "Apple", api: "Metal", needs: "Any Apple Silicon Mac (M1 or later) on macOS 14 or newer." },
];

export function notConnected(root, title, go) {
  clear(
    root,
    h("header", { class: "page-head" }, h("h1", { class: "page-title" }, title)),
    h(
      "section",
      { class: "panel" },
      emptyState(
        "Not in a mesh yet",
        "Join a mesh on this network, or host one here and invite the rest. Machines, runs and datasets appear once this computer is connected.",
        h("button", { class: "btn btn-primary", type: "button", onclick: () => go("discover") }, icon("radar", 15), h("span", {}, "Find a mesh")),
        h("button", { class: "btn", type: "button", onclick: (e) => busy(e.currentTarget, async () => (await api.hostStart(), await refreshStatus(), go("invite"))) }, icon("host", 15), h("span", {}, "Host a mesh"))
      )
    )
  );
}

function nodeCard(n) {
  const suspect = false;
  return h(
    "article",
    { class: `node-card${n.training ? " is-training" : ""}${n.active ? "" : " is-offline"}` },
    h(
      "div",
      { class: "node-head" },
      h(
        "div",
        { class: "stack-sm", style: { gap: "4px", minWidth: 0 } },
        h(
          "div",
          { class: "row", style: { gap: "8px" } },
          h("span", { class: `dot${n.training ? " dot-live" : ""}`, style: n.training ? undefined : { background: suspect ? "var(--warn)" : n.active ? "var(--cyan)" : "var(--text-faint)" } }),
          h("span", { class: "node-name truncate" }, n.display_name)
        ),
        h("span", { class: "small faint truncate", title: n.gpu }, n.is_self ? h("span", { class: "accent" }, "This computer · ") : null, n.gpu)
      ),
      h("div", { class: "stack-sm", style: { gap: "6px", alignItems: "flex-end" } }, vendorBadge(n.backend, true), h("span", { class: `badge${n.tier === "full" ? " badge-live" : n.tier === "probation" ? " badge-warn" : " badge-danger"}` }, n.tier === "full" ? "admitted" : n.tier))
    ),
    n.training
      ? h("div", { class: "stack-sm", style: { gap: "6px" } }, h("div", { class: "row-between small" }, h("span", { class: "phase" }, PHASE[n.phase] || n.phase || "training"), n.progress != null ? h("span", { class: "mono faint" }, `${Math.round(n.progress * 100)}%`) : null), meter(n.progress ?? 0.05, "cyan"))
      : null,
    h(
      "div",
      { class: "node-facts" },
      h("div", { class: "node-fact" }, h("span", {}, "Memory"), h("span", {}, n.gpu_memory_mb ? `${fmtMemory(n.gpu_memory_mb)}${n.backend === "mps" ? " shared" : ""}` : "–")),
      h("div", { class: "node-fact" }, h("span", {}, "Measured"), h("span", {}, n.gflops ? `${(n.gflops / 1000).toFixed(1)} TFLOP/s` : "unmeasured")),
      h("div", { class: "node-fact" }, h("span", {}, "Training rate"), h("span", {}, n.throughput_sps ? `${n.throughput_sps.toFixed(1)} img/s` : "unmeasured")),
      h("div", { class: "node-fact" }, h("span", {}, "Rounds"), h("span", {}, n.completed_rounds)),
      h("div", { class: "node-fact" }, h("span", {}, "Port"), h("span", { class: "truncate" }, n.port || "–")),
      h("div", { class: "node-fact" }, h("span", {}, "System"), h("span", { class: "truncate" }, n.os || "–"))
    ),
    h("div", { class: "stack-sm", style: { gap: "6px" } }, h("div", { class: "row-between small faint" }, h("span", {}, "Fitness"), h("span", { class: "mono" }, n.fitness.toFixed(3))), meter(n.fitness, n.tier === "probation" ? "warn" : n.tier === "rejected" ? "danger" : "")),
    n.warnings?.length ? h("ul", { class: "warn-list" }, n.warnings.map((w) => h("li", {}, w))) : null
  );
}

export function renderMesh(root, { go }) {
  const head = h("header", { class: "page-head" });
  const metrics = h("div", { class: "metrics-row" });
  const mix = h("div", { class: "mix-grid" });
  const grid = h("div", { class: "node-grid" });
  const feed = h("div", { class: "feed" });

  function draw() {
    if (!connected()) return notConnected(root, "Mesh", go);
    if (!root.contains(grid)) {
      clear(
        root,
        head,
        metrics,
        mix,
        grid,
        h("section", { class: "panel panel-flush" }, h("div", { class: "panel-header" }, h("span", { class: "panel-title" }, "Live feed"), h("span", { class: "small faint" }, "What the scheduler decided, with the numbers behind it")), feed)
      );
    }
    const mesh = store.mesh;
    const status = store.status;
    clear(
      head,
      h("div", {}, h("h1", { class: "page-title" }, mesh?.mesh_name || status.joined?.mesh_name || "Mesh"), h("p", { class: "lead mono small" }, mesh ? `GradMesh ${mesh.version} at ${mesh.coordinator}${mesh.torch_ready ? "" : " · host still installing PyTorch"}` : "Connecting…")),
      h(
        "div",
        { class: "row" },
        hosting() ? h("button", { class: "btn btn-sm", type: "button", onclick: () => go("invite") }, icon("invite", 14), h("span", {}, "Invite")) : null,
        contributing() ? h("button", { class: "btn btn-danger btn-sm", type: "button", onclick: (e) => busy(e.currentTarget, async () => (await api.leave(), await refreshStatus())) }, icon("stop", 14), h("span", {}, "Leave")) : null,
        hosting() ? h("button", { class: "btn btn-danger btn-sm", type: "button", onclick: (e) => busy(e.currentTarget, async () => (await api.hostStop(), await refreshStatus())) }, icon("power", 14), h("span", {}, "Stop hosting")) : null
      )
    );
    if (!mesh) return;
    const nodes = [...mesh.nodes].sort((a, b) => Number(b.is_self) - Number(a.is_self) || Number(b.active) - Number(a.active) || (b.gflops || 0) - (a.gflops || 0));
    const online = nodes.filter((n) => n.active);
    clear(
      metrics,
      stat("Machines online", `${online.length}`, `of ${nodes.length} known`),
      stat("Training now", `${online.filter((n) => n.training).length}`, "shards in flight"),
      stat("Compute pooled", `${(online.reduce((s, n) => s + (n.gflops || 0), 0) / 1000).toFixed(1)}`, "TFLOP/s measured", true),
      stat("GPU memory", fmtMemory(online.reduce((s, n) => s + (n.gpu_memory_mb || 0), 0)), "across the mesh")
    );
    clear(
      mix,
      MIX.map((v) => {
        const of = online.filter((n) => n.backend === v.backend);
        return h(
          "div",
          { class: `mix-card vendor-${v.vendor}${of.length ? "" : " is-empty"}` },
          h("div", { class: "row-between" }, h("span", { class: `vendor vendor-${v.vendor}` }, h("span", { class: "vendor-dot" }), `${v.name} · ${v.api}`), h("span", { class: "small faint" }, `${of.filter((n) => n.tier === "full").length} admitted`)),
          h("div", { class: "row", style: { gap: "8px", alignItems: "baseline" } }, h("span", { class: "mix-count mono" }, of.length), h("span", { class: "small faint" }, "online")),
          of.length
            ? h("div", { class: "small faint" }, `${(of.reduce((s, n) => s + (n.gflops || 0), 0) / 1000).toFixed(1)} TFLOP/s · ${fmtMemory(of.reduce((s, n) => s + (n.gpu_memory_mb || 0), 0))} · ${of.reduce((s, n) => s + (n.throughput_sps || 0), 0).toFixed(1)} img/s`)
            : h("div", { class: "small faint" }, v.needs)
        );
      })
    );
    clear(grid, nodes.map(nodeCard));
    const events = [...(mesh.events || [])].sort((a, b) => b.at - a.at);
    clear(
      feed,
      events.length
        ? events.map((e) => h("div", { class: "feed-item" }, h("span", { class: "feed-time" }, fmtClock(e.at)), h("span", { style: { color: TONE[e.kind] || "var(--text-dim)" } }, e.text)))
        : h("div", { class: "empty" }, "Nothing has happened yet. Events appear here the moment they do.")
    );
  }

  draw();
  refreshMesh();
  return subscribe((r) => {
    if (r.has("mesh") || r.has("status")) draw();
  });
}
