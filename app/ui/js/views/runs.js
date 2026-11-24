// Runs: every training run on the mesh, and one run's rounds in detail.

import { store, subscribe, connected, refreshRuns } from "../store.js";
import { h, clear, fmtDuration, fmtAgo, emptyState } from "../ui.js";
import { notConnected } from "./mesh.js";

const STATUS = { running: "badge-accent", planning: "badge-cyan", waiting: "badge-warn", completed: "badge-live", stopped: "", failed: "badge-danger" };

function lineChart(points, { format = (v) => v.toFixed(2), label }) {
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
  const svg = `
    <defs><linearGradient id="g-${label}" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#5b7cfa" stop-opacity="0.32"/><stop offset="1" stop-color="#5b7cfa" stop-opacity="0"/></linearGradient></defs>
    ${ticks.map((t) => `<line class="grid-line" x1="${pad.l}" x2="${W - pad.r}" y1="${y(t)}" y2="${y(t)}"/><text class="axis" x="${pad.l - 6}" y="${y(t) + 3}" text-anchor="end">${format(t)}</text>`).join("")}
    <path d="${area}" fill="url(#g-${label})"/>
    <path d="${path}" fill="none" stroke="#5b7cfa" stroke-width="2" stroke-linejoin="round" stroke-linecap="round"/>
    ${points.map((p, i) => `<circle cx="${x(i)}" cy="${y(p.y)}" r="${i === points.length - 1 ? 4 : 2.2}" fill="${i === points.length - 1 ? "#7b96ff" : "#0e1117"}" stroke="#7b96ff" stroke-width="1.4"><title>Round ${p.x}: ${format(p.y)}</title></circle>`).join("")}
    ${points.map((p, i) => (points.length <= 12 || i % 2 === 0 ? `<text class="axis" x="${x(i)}" y="${H - 6}" text-anchor="middle">${p.x}</text>` : "")).join("")}`;
  const el = h("svg", { svg: true, class: "chart", viewBox: `0 0 ${W} ${H}`, preserveAspectRatio: "none", role: "img", "aria-label": label });
  el.innerHTML = svg;
  return el;
}

function roundBars(metrics) {
  const max = Math.max(...metrics.map((m) => m.wall_clock_seconds), 1);
  return h(
    "div",
    { class: "stack-sm" },
    h(
      "div",
      { class: "spark", style: { height: "110px" } },
      metrics.map((m) => {
        const total = (m.wall_clock_seconds / max) * 100;
        const gap = (m.straggler_gap_seconds / m.wall_clock_seconds) * total;
        return h(
          "div",
          { style: { flex: 1, display: "flex", flexDirection: "column", justifyContent: "flex-end", height: "100%" }, title: `Round ${m.round}: ${m.wall_clock_seconds.toFixed(1)} s, of which ${m.straggler_gap_seconds.toFixed(1)} s waiting for the slowest machine` },
          h("div", { style: { height: `${gap}%`, background: "rgba(245,165,36,0.75)", borderRadius: "3px 3px 0 0", minHeight: "2px" } }),
          h("div", { class: "spark-bar", style: { height: `${total - gap}%`, borderRadius: 0 } })
        );
      })
    ),
    h("div", { class: "row", style: { gap: "16px" } }, h("span", { class: "legend" }, "Training"), h("span", { class: "legend is-fixed" }, "Waiting for the slowest machine"))
  );
}

export function renderRuns(root, { go }) {
  let openId = null;
  const list = h("div", { class: "panel panel-flush" });
  const detail = h("div", { class: "stack" });

  function draw() {
    if (!connected()) return notConnected(root, "Runs", go);
    if (!root.contains(list)) {
      clear(
        root,
        h("header", { class: "page-head" }, h("div", {}, h("h1", { class: "page-title" }, "Runs"), h("p", { class: "lead" }, "Each round, every machine trains a shard sized to finish together, and the host averages the weights."))),
        h("div", { class: "runs-layout" }, list, detail)
      );
    }
    const runs = store.runs;
    if (!runs) return clear(list, h("div", { class: "empty" }, "Loading runs…"));
    if (!runs.length) {
      clear(list, emptyState("No runs yet", "The host starts runs from its GradMesh dashboard. They appear here as they go."));
      return clear(detail);
    }
    if (!openId || !runs.some((r) => r.id === openId)) openId = runs[0].id;
    clear(
      list,
      h("div", { class: "panel-header" }, h("span", { class: "panel-title" }, `${runs.length} runs`), h("span", { class: "small faint" }, `${runs.filter((r) => r.status === "running").length} running`)),
      runs.map((run) =>
        h(
          "button",
          { class: `run-row${run.id === openId ? " is-open" : ""}`, type: "button", onclick: () => ((openId = run.id), draw()) },
          h("div", { class: "stack-sm", style: { gap: "2px" } }, h("span", { class: "truncate", style: { fontWeight: 560 } }, run.name), h("span", { class: "small faint truncate" }, `${run.dataset} · ${run.base_model} · ${fmtAgo(run.started_at)}`)),
          h("span", { class: `badge ${STATUS[run.status] || ""}`, style: { justifySelf: "end" } }, run.status),
          h("div", { class: "row", style: { gridColumn: "1 / -1", gap: "10px" } }, h("div", { class: "meter grow" }, h("div", { class: `meter-fill${run.status === "failed" ? " is-danger" : run.status === "completed" ? " is-cyan" : ""}`, style: { width: `${(run.current_round / run.rounds) * 100}%` } })), h("span", { class: "small mono faint nowrap" }, `${run.current_round}/${run.rounds}`))
        )
      )
    );
    drawDetail(runs.find((r) => r.id === openId));
  }

  function drawDetail(run) {
    const pct = run.current_round / run.rounds;
    const m = run.round_metrics || [];
    const last = m[m.length - 1];
    const ticks = Array.from({ length: run.rounds - 1 }, (_, i) => h("span", { class: "train-bar-tick", style: { left: `${((i + 1) / run.rounds) * 100}%` } }));
    clear(
      detail,
      h(
        "section",
        { class: "train-hero", style: { gridTemplateColumns: "1fr" } },
        h(
          "div",
          { class: "train-progress" },
          h("div", { class: "row-between" }, h("div", { class: "stack-sm", style: { gap: "2px" } }, h("h2", {}, run.name), h("span", { class: "small faint" }, `${run.dataset} · ${run.base_model} · started ${fmtAgo(run.started_at)}`)), h("span", { class: `badge ${STATUS[run.status] || ""}` }, run.status === "running" ? h("span", { class: "dot dot-live" }) : null, run.status)),
          h("div", { class: "row", style: { alignItems: "baseline", gap: "14px" } }, h("div", { class: "train-percent mono" }, `${Math.round(pct * 100)}%`), h("span", { class: "muted" }, `round ${run.current_round} of ${run.rounds}`)),
          h("div", { class: "train-bar" }, h("div", { class: "train-bar-fill", style: { width: `${pct * 100}%` } }), ticks),
          run.error ? h("div", { class: "notice notice-danger", style: { marginTop: "14px" } }, run.error) : null,
          h(
        "dl",
        { class: "ld-telemetry", style: { gridTemplateColumns: "repeat(5, minmax(0,1fr))", margin: "22px 0 0", paddingBottom: 0 } },
        [
          ["mAP50", run.map50 != null ? run.map50.toFixed(3) : "–", "latest round", true],
          ["Speed-up", run.speedup ? `${run.speedup.toFixed(2)}×` : "–", "vs fastest alone"],
          ["Machines", `${run.workers}`, run.status === "running" ? "training now" : "at peak"],
          ["Last round", last ? `${last.wall_clock_seconds.toFixed(0)} s` : "–", last ? `${last.straggler_gap_seconds.toFixed(1)} s straggler gap` : ""],
          ["Wall clock", fmtDuration(run.wall_clock_seconds).replace(" min ", "m ").replace(/ s$/, "s"), run.status === "running" ? "so far" : "total"],
        ].map(([label, value, foot, accent]) =>
          h("div", { title: foot }, h("dt", {}, label), h("dd", { class: accent ? "accent" : "" }, value), h("span", { class: "small faint truncate", style: { display: "block" } }, foot))
        )
      )
        )
      ),
      h(
        "div",
        { class: "grid grid-2" },
        h("section", { class: "panel" }, h("div", { class: "row-between", style: { marginBottom: "10px" } }, h("span", { class: "panel-title" }, "Accuracy by round"), h("span", { class: "small faint" }, "mAP50 on the validation split")), lineChart(m.map((r) => ({ x: r.round, y: r.map50 || 0 })), { label: "map", format: (v) => v.toFixed(2) })),
        h("section", { class: "panel" }, h("div", { class: "row-between", style: { marginBottom: "10px" } }, h("span", { class: "panel-title" }, "Time per round"), h("span", { class: "small faint" }, "shorter is better")), roundBars(m))
      ),
      h(
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
              [...m].reverse().map((r) =>
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
    );
  }

  draw();
  refreshRuns();
  const timer = setInterval(() => !document.hidden && connected() && refreshRuns(), 5000);
  const off = subscribe((r) => {
    if (r.has("runs") || r.has("status")) draw();
  });
  return () => {
    clearInterval(timer);
    off();
  };
}
