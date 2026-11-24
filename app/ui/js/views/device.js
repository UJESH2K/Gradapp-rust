// Device & ports: what the probe saw, which port it chose and why, and every
// port with where it can run. Install, repair and switch stream their log here.

import { api } from "../api.js";
import { store, subscribe, refreshDevice, selectedPort, saveSettings } from "../store.js";
import { h, clear, icon, busy, panel, vendorBadge, fmtMemory, toast } from "../ui.js";

const STATE_LABEL = { ready: "Ready", installing: "Installing", not_installed: "Not installed", stale: "Needs rebuild", failed: "Failed", blocked: "Not for this machine" };
// gm-ports provision steps (Progress.step), in order; "lock" ends it.
const STEPS = [
  ["prepare", "uv"],
  ["python", "Python"],
  ["venv", "Environment"],
  ["install", "PyTorch & packages"],
  ["verify", "Test kernel"],
];

function platformKey(facts) {
  return facts ? `${facts.os}-${facts.arch}` : "";
}

export function renderDevice(root) {
  const facts = h("div");
  const chosen = h("div");
  const progress = h("div");
  const list = h("div");
  const probe = h("button", { class: "btn btn-sm", type: "button", onclick: (e) => busy(e.currentTarget, async () => (await refreshDevice(), toast("Hardware probed again.", "ok"))) }, icon("refresh", 14), h("span", {}, "Probe again"));

  clear(
    root,
    h("header", { class: "page-head" }, h("div", {}, h("h1", { class: "page-title" }, "Device & ports"), h("p", { class: "lead" }, "A port is one pinned Python and PyTorch for one kind of GPU, installed privately by GradApp. Your own Python is never touched.")), probe),
    h("div", { class: "home-grid" }, facts, chosen),
    progress,
    list
  );

  function drawFacts() {
    const d = store.device;
    if (!d) return clear(facts, panel({ title: "What GradApp found" }, h("p", { class: "small faint" }, "Probing…")));
    const f = d.facts;
    const row = (k, v) => h("div", { class: "node-fact" }, h("span", {}, k), h("span", { class: "truncate", title: String(v ?? "") }, v ?? "unknown"));
    clear(
      facts,
      panel(
        { title: "What GradApp found", sub: "Read from the vendor tools on this machine" },
        h(
          "div",
          { class: "node-facts" },
          row("Computer", f.hostname),
          row("System", f.os_name_pretty || f.os),
          row("Version", f.os_version),
          row("Architecture", f.arch + (f.translated ? " (Rosetta)" : "")),
          row("Processor", f.cpu_model),
          row("Cores", f.cpu_cores),
          row("Memory", f.ram_mb ? fmtMemory(f.ram_mb) : null),
          row("Platform", platformKey(f))
        ),
        h("div", { class: "divider", style: { margin: "16px 0 4px" } }),
        (f.gpus || []).length
          ? f.gpus.map((g) =>
              h(
                "div",
                { class: "row-between", style: { padding: "10px 0", borderBottom: "1px solid var(--line)" } },
                h("div", { class: "stack-sm", style: { gap: "2px" } }, h("span", { class: "gpu-name" }, `GPU ${g.index} · ${g.name}`), h("span", { class: "small faint mono" }, [g.memory_mb ? fmtMemory(g.memory_mb) : "shared memory", g.compute_capability ? `cc ${g.compute_capability}` : null, g.driver ? `driver ${g.driver}` : null, g.integrated ? "integrated" : "discrete"].filter(Boolean).join("  ·  "))),
                vendorBadge({ nvidia: "cuda", intel: "xpu", apple: "mps", amd: "rocm" }[g.vendor] || "cpu", true)
              )
            )
          : h("p", { class: "small faint", style: { paddingTop: "10px" } }, "No GPU answered. Only the CPU port applies.")
      )
    );
  }

  function drawChosen() {
    const d = store.device;
    const port = selectedPort();
    if (!d || !port) return clear(chosen, panel({ title: "Selected port" }, h("p", { class: "small faint" }, "Choosing…")));
    const p = d.profile;
    const s = port.status.state;
    const installing = s === "installing";
    clear(
      chosen,
      panel(
        { title: "Selected port", sub: d.overridden ? "Chosen by you in Settings" : "Chosen automatically for this hardware", action: h("span", { class: `state-chip is-${s}` }, h("span", { class: "dot" }), STATE_LABEL[s] || s) },
        h(
          "div",
          { class: "port-ticket" },
          h("div", { class: "row", style: { gap: "12px", alignItems: "baseline" } }, h("span", { class: "port-id" }, port.spec.id), h("span", { class: "muted small" }, port.spec.label)),
          h("p", { class: "small muted" }, p.reason),
          p.blocked ? h("div", { class: "notice notice-danger" }, h("strong", {}, p.blocked), p.fix ? h("div", { style: { marginTop: "6px" } }, "Fix: ", p.fix) : null) : null,
          p.warnings?.length ? h("ul", { class: "warn-list" }, p.warnings.map((w) => h("li", {}, w))) : null,
          port.status.message ? h("div", { class: "notice notice-warn" }, port.status.message) : null,
          h(
            "div",
            { class: "node-facts" },
            h("div", { class: "node-fact" }, h("span", {}, "Python"), h("span", {}, port.spec.python)),
            h("div", { class: "node-fact" }, h("span", {}, "PyTorch"), h("span", {}, `${port.spec.torch}${port.spec.local_suffix}`)),
            h("div", { class: "node-fact" }, h("span", {}, "Trains"), h("span", {}, p.can_train ? "yes" : "measured only")),
            h("div", { class: "node-fact" }, h("span", {}, "Device"), h("span", { class: "truncate" }, port.status.device || p.gpu_name || "–"))
          ),
          h("div", { class: "copy-line" }, h("code", { title: port.status.env_dir }, port.status.env_dir)),
          h(
            "div",
            { class: "row wrap" },
            h(
              "button",
              { class: `btn ${s === "ready" ? "" : "btn-primary"} btn-sm`, type: "button", disabled: installing || Boolean(p.blocked), onclick: (e) => busy(e.currentTarget, () => api.installPort(port.spec.id)) },
              icon(s === "ready" ? "wrench" : "download", 14),
              h("span", {}, installing ? "Installing…" : s === "ready" ? "Repair" : s === "stale" || s === "failed" ? "Rebuild" : "Install")
            ),
            d.overridden ? h("button", { class: "btn btn-ghost btn-sm", type: "button", onclick: (e) => busy(e.currentTarget, async () => (await saveSettings({ preferred_backend: null }), await refreshDevice())) }, "Back to automatic") : null
          )
        )
      )
    );
  }

  function drawProgress() {
    const inst = store.install;
    if (!inst) return clear(progress);
    const entry = store.ports?.find((p) => p.spec.id === inst.port);
    const idx = STEPS.findIndex(([id]) => id === inst.step);
    const log = h("pre", { class: "log", style: { maxHeight: "220px" }, tabindex: "0", "aria-label": "Install log" }, inst.lines.map((line) => h("span", { class: `ln${/ready|ok|Installed/.test(line) ? " is-ok" : /error|fail/i.test(line) ? " is-error" : ""}` }, line)));
    clear(
      progress,
      panel(
        {
          title: inst.done ? `${entry?.spec.label || inst.port} is ready` : inst.failed ? `Installing ${inst.port} failed` : `Installing ${entry?.spec.label || inst.port}`,
          sub: inst.done ? "Verified on this machine's GPU." : "uv downloads the exact CPython and wheels the port names; nothing outside GradApp's folder changes.",
          action: h("span", { class: "mono small accent" }, `${Math.round((inst.percent || 0) * 100)}%`),
        },
        h(
          "div",
          { class: "stack" },
          h("div", { class: "train-bar" }, h("div", { class: "train-bar-fill", style: { width: `${(inst.percent || 0.02) * 100}%` } })),
          h(
            "div",
            { class: "steps" },
            STEPS.map(([id, label], i) => h("span", { class: `step ${inst.done || i < idx || (idx === -1 && inst.step === "lock") ? "is-done" : i === idx ? "is-active" : ""}` }, h("i"), label))
          ),
          log
        )
      )
    );
    log.scrollTop = log.scrollHeight;
  }

  function drawList() {
    const ports = store.ports;
    if (!ports) return clear(list);
    const here = platformKey(store.device?.facts);
    const busyInstall = store.install && !store.install.done && !store.install.failed;
    clear(
      list,
      panel(
        { title: "Every port", sub: `Green platforms match this machine (${here}).`, flush: true },
        h(
          "div",
          { class: "port-list" },
          ports.map((entry) => {
            const { spec, status } = entry;
            const s = status.state;
            let action;
            if (!entry.compatible) action = h("span", { class: "small faint", title: entry.incompatible_reason }, "Not for this machine");
            else if (s === "installing") action = h("span", { class: "row small accent", style: { gap: "8px" } }, h("span", { class: "spinner" }), "Installing");
            else if (entry.selected) action = h("button", { class: "btn btn-sm", type: "button", disabled: busyInstall, onclick: (e) => busy(e.currentTarget, () => api.installPort(spec.id)) }, icon(s === "ready" ? "wrench" : "download", 14), h("span", {}, s === "ready" ? "Repair" : "Install"));
            else
              action = h(
                "button",
                {
                  class: "btn btn-ghost btn-sm",
                  type: "button",
                  disabled: busyInstall,
                  title: `Train with ${spec.label} from now on`,
                  onclick: (e) =>
                    busy(e.currentTarget, async () => {
                      await saveSettings({ preferred_backend: spec.backend });
                      await refreshDevice();
                      if (s !== "ready") await api.installPort(spec.id);
                      toast(`Switched to ${spec.id}.`, "ok");
                    }),
                },
                icon("swap", 14),
                h("span", {}, "Switch")
              );
            return h(
              "div",
              { class: `port-row${entry.selected ? " is-selected" : ""}${entry.compatible ? "" : " is-incompatible"}` },
              h(
                "div",
                { class: "stack-sm", style: { gap: "4px" } },
                h("div", { class: "row", style: { gap: "8px" } }, h("span", { class: "mono", style: { fontWeight: 560 } }, spec.id), entry.selected ? h("span", { class: "badge badge-accent" }, "selected") : null, spec.reference ? h("span", { class: "badge badge-live" }, "reference") : null, spec.experimental ? h("span", { class: "badge badge-warn" }, "experimental") : null),
                h("span", { class: "small faint truncate", title: spec.notes || "" }, `${spec.label} · torch ${spec.torch}${spec.local_suffix}${spec.min_driver ? ` · driver ≥ ${spec.min_driver}` : ""}`)
              ),
              h("div", { class: "plat-row port-plat" }, spec.platforms.map((pl) => h("span", { class: `plat${pl === here ? " is-here" : ""}` }, pl))),
              h("span", { class: `state-chip is-${s}`, style: { justifySelf: "start" } }, STATE_LABEL[s] || s),
              h("div", { style: { justifySelf: "end" } }, action)
            );
          })
        )
      )
    );
  }

  drawFacts();
  drawChosen();
  drawProgress();
  drawList();
  return subscribe((r) => {
    if (r.has("device")) (drawFacts(), drawChosen(), drawList());
    if (r.has("install")) (drawProgress(), drawChosen(), drawList());
  });
}
