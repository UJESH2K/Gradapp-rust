// Activity: the output of every process GradApp runs, one tab per process.

import { api } from "../api.js";
import { store, subscribe } from "../store.js";
import { h, clear, icon, copyText, fmtClock } from "../ui.js";

const TABS = [
  ["all", "Everything"],
  ["worker", "Worker"],
  ["coordinator", "Coordinator"],
  ["installer", "Installer"],
  ["app", "App"],
];

const tone = (line) => (/error|failed|traceback/i.test(line) ? " is-error" : /warn|capped|slow/i.test(line) ? " is-warn" : /ready|connected|installed|ok\b/i.test(line) ? " is-ok" : "");

export function renderActivity(root) {
  let tab = "all";
  let filter = "";
  let follow = true;
  const tabs = h("div", { class: "tabs", role: "tablist" });
  const pre = h("pre", { class: "log", tabindex: "0", style: { height: "calc(100vh - 250px)", minHeight: "320px" }, "aria-label": "Process output" });
  const search = h("input", { class: "input", placeholder: "Filter lines", style: { maxWidth: "220px", padding: "6px 12px" }, oninput: (e) => ((filter = e.target.value.toLowerCase()), draw()) });
  const state = h("div", { class: "row", style: { gap: "8px" } });
  pre.addEventListener("scroll", () => (follow = pre.scrollTop + pre.clientHeight >= pre.scrollHeight - 24));

  const visible = () => store.logs.filter((l) => (tab === "all" || l.source === tab) && (!filter || l.line.toLowerCase().includes(filter)));

  clear(
    root,
    h("header", { class: "page-head" }, h("div", {}, h("h1", { class: "page-title" }, "Activity"), h("p", { class: "lead" }, "What GradApp's processes printed, newest at the bottom. Attach this when something goes wrong.")), state),
    h(
      "div",
      { class: "row-between" },
      tabs,
      h(
        "div",
        { class: "row" },
        search,
        h("button", { class: "btn btn-sm", type: "button", onclick: () => copyText(visible().map((l) => `${new Date(l.at * 1000).toISOString()} [${l.source}] ${l.line}`).join("\n"), "Log copied") }, icon("copy", 14), h("span", {}, "Copy")),
        h("button", { class: "btn btn-ghost btn-sm", type: "button", onclick: () => api.openDataDir() }, icon("folder", 14), h("span", {}, "Log folder"))
      )
    ),
    pre
  );

  function drawTabs() {
    const counts = Object.fromEntries(TABS.map(([id]) => [id, id === "all" ? store.logs.length : store.logs.filter((l) => l.source === id).length]));
    clear(
      tabs,
      TABS.map(([id, label]) => h("button", { class: `tab${tab === id ? " is-on" : ""}`, role: "tab", "aria-selected": String(tab === id), type: "button", onclick: () => ((tab = id), (follow = true), drawTabs(), draw()) }, label, h("span", { class: "count" }, counts[id])))
    );
    const s = store.status;
    const chip = (label, proc) => {
      const st = proc?.state || "stopped";
      const cls = st === "running" ? "badge-live" : st === "failed" ? "badge-danger" : st === "starting" || st === "restarting" ? "badge-accent" : "";
      return h("span", { class: `badge ${cls}` }, h("span", { class: `dot${st === "running" ? " dot-live" : ""}` }), `${label} ${st}${proc?.pid ? ` · pid ${proc.pid}` : ""}`);
    };
    clear(state, chip("Worker", s?.worker), chip("Coordinator", s?.coordinator));
  }

  function draw() {
    const lines = visible().slice(-800);
    clear(
      pre,
      lines.length
        ? lines.map((l) => h("span", { class: `ln${tone(l.line)}` }, h("span", { class: "ln-time" }, fmtClock(l.at)), tab === "all" ? h("span", { class: "ln-src" }, l.source) : null, l.line))
        : h("span", { class: "ln faint" }, filter ? "No line matches the filter." : "Nothing yet. Output appears here as soon as a process prints it.")
    );
    if (follow) pre.scrollTop = pre.scrollHeight;
  }

  drawTabs();
  draw();
  let last = 0;
  return subscribe((r) => {
    if (r.has("status")) drawTabs();
    if (r.has("log") && Date.now() - last > 200) {
      last = Date.now();
      drawTabs();
      draw();
    }
  });
}
