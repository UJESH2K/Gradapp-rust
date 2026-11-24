// The shell: v5 sidebar and topbar, hash routing between screens, the command
// palette (Ctrl/⌘K), and the update dialog.

import { api, onChannel, isTauri } from "./api.js";
import { store, subscribe, boot, setView, contributing, hosting, connected, selectedPort, onNotice, refreshStatus } from "./store.js";
import { h, clear, icon, logo, toast, MOD, isMac, fmtBytes, copyText } from "./ui.js";
import { renderHome } from "./views/home.js";
import { renderDiscover } from "./views/discover.js";
import { renderMesh } from "./views/mesh.js";
import { renderRuns } from "./views/runs.js";
import { renderDatasets } from "./views/datasets.js";
import { renderDevice } from "./views/device.js";
import { renderActivity } from "./views/activity.js";
import { renderInvite } from "./views/invite.js";
import { renderSettings } from "./views/settings.js";
import { watchInvites } from "./views/peers.js";

export const VIEWS = [
  { id: "home", label: "Home", icon: "home", render: renderHome, key: "1", group: "This machine" },
  { id: "device", label: "Device & ports", icon: "chip", render: renderDevice, key: "2", group: "This machine" },
  { id: "discover", label: "Discover & join", icon: "radar", render: renderDiscover, key: "3", group: "Mesh" },
  { id: "mesh", label: "Mesh", icon: "mesh", render: renderMesh, key: "4", group: "Mesh" },
  { id: "runs", label: "Runs", icon: "runs", render: renderRuns, key: "5", group: "Mesh" },
  { id: "datasets", label: "Datasets", icon: "datasets", render: renderDatasets, key: "6", group: "Mesh" },
  { id: "invite", label: "Invite", icon: "invite", render: renderInvite, key: "7", group: "Mesh" },
  { id: "activity", label: "Activity", icon: "activity", render: renderActivity, key: "8", group: "App" },
  { id: "settings", label: "Settings", icon: "settings", render: renderSettings, key: ",", group: "App" },
];

export function go(view) {
  if (location.hash !== `#/${view}`) location.hash = `#/${view}`;
  else show(view);
}

const app = document.getElementById("app");
const nav = h("nav", { class: "dash-nav", "aria-label": "Screens" });
const sideFoot = h("div", { class: "dash-side-foot" });
const topLeft = h("div", { class: "row wrap", style: { gap: "18px" } });
const topRight = h("div", { class: "row", style: { gap: "8px" } });
const content = h("div", { class: "dash-content", id: "content" });
const main = h("main", { class: "dash-main", tabindex: "-1" }, h("header", { class: "dash-topbar" }, topLeft, topRight), content);
let cleanup = null;
let current = null;

function shell() {
  const groups = [...new Set(VIEWS.map((v) => v.group))];
  clear(
    nav,
    groups.map((group) => [
      h("span", { class: "dash-nav-label" }, group),
      VIEWS.filter((v) => v.group === group).map((view) =>
        h(
          "button",
          { class: "dash-link", type: "button", "data-view": view.id, title: `${view.label} (${MOD}+${view.key})`, onclick: () => go(view.id) },
          icon(view.icon, 16),
          h("span", { class: "dash-link-label" }, view.label),
          ["home", "mesh", "device"].includes(view.id) ? h("span", { class: "nav-dot", "data-dot": view.id }) : null,
          h("kbd", {}, `${isMac ? "⌘" : "^"}${view.key}`)
        )
      ),
    ])
  );
  clear(
    app,
    h("aside", { class: "dash-side" }, h("div", { class: "dash-brand" }, logo(22), h("span", {}, "GradApp"), h("span", { class: "brand-version", id: "brand-version" }, "2.0")), nav, sideFoot),
    main
  );
}

function show(viewId) {
  const view = VIEWS.find((v) => v.id === viewId) || VIEWS[0];
  if (current === view.id && content.childElementCount) return;
  cleanup?.();
  cleanup = null;
  current = view.id;
  setView(view.id);
  for (const item of nav.querySelectorAll(".dash-link")) {
    const active = item.dataset.view === view.id;
    item.classList.toggle("is-active", active);
    if (active) item.setAttribute("aria-current", "page");
    else item.removeAttribute("aria-current");
  }
  content.replaceChildren();
  main.scrollTop = 0;
  const result = view.render(content, { go, checkForUpdates, openUpdate });
  cleanup = typeof result === "function" ? result : null;
  try {
    sessionStorage.setItem("gradapp:view", view.id);
  } catch {
    // convenience only
  }
}

// ------------------------------------------------------------------ chrome

function workerLabel() {
  const w = store.status?.worker?.state;
  if (w === "starting") return ["Joining", "busy"];
  if (w === "restarting") return ["Reconnecting", "warn"];
  if (w === "failed") return ["Worker stopped", "danger"];
  if (w === "running") return ["Contributing", "live"];
  return null;
}

function renderChrome() {
  const status = store.status;
  if (!status) return;
  document.getElementById("brand-version").textContent = status.version;
  const meshName = status.joined?.mesh_name || status.hosting?.mesh_name || store.mesh?.mesh_name;
  const worker = workerLabel();
  const port = selectedPort();

  // Sidebar dots.
  nav.querySelector('[data-dot="home"]')?.setAttribute("data-tone", worker?.[1] || (hosting() ? "live" : ""));
  nav.querySelector('[data-dot="mesh"]')?.setAttribute("data-tone", connected() ? "live" : "");
  nav.querySelector('[data-dot="device"]')?.setAttribute(
    "data-tone",
    store.install && !store.install.done && !store.install.failed ? "busy" : port && ["failed", "blocked"].includes(port.status.state) ? "danger" : port && port.status.state === "stale" ? "warn" : ""
  );

  clear(
    sideFoot,
    h(
      "div",
      { class: "row small", style: { gap: "8px", padding: "0 8px" } },
      h("span", { class: `dot${worker?.[1] === "live" || hosting() ? " dot-live" : ""}`, style: worker?.[1] === "live" || hosting() ? undefined : { background: worker ? `var(--${worker[1] === "busy" ? "accent" : worker[1]})` : "var(--text-faint)" } }),
      h("span", { class: "faint truncate" }, hosting() ? `Hosting ${status.hosting.mesh_name}` : worker ? `${worker[0]}${meshName ? ` · ${meshName}` : ""}` : "Not in a mesh")
    ),
    port
      ? h(
          "button",
          { class: "dash-stack dash-stack-btn", type: "button", onclick: () => go("device"), title: "The runtime this machine trains with" },
          h("span", {}, `Port ${port.spec.id}`),
          h("span", { class: "mono" }, `python ${port.spec.python} · torch ${port.spec.torch}${port.spec.local_suffix}`)
        )
      : null,
    h("div", { class: "dash-stack" }, h("span", {}, `GradApp ${status.version}`), h("span", { class: "mono" }, isTauri ? "engine 5.1.1 · protocol 5" : "browser preview · mock backend"))
  );

  // Topbar.
  const nodes = store.mesh?.nodes || [];
  const online = nodes.filter((n) => n.active);
  const tflops = online.reduce((sum, n) => sum + (n.gflops || 0), 0) / 1000;
  const memory = online.reduce((sum, n) => sum + (n.gpu_memory_mb || 0), 0);
  clear(
    topLeft,
    connected()
      ? [
          h("span", { class: "badge badge-live" }, h("span", { class: "dot dot-live" }), store.mesh ? `${online.length} online` : "connecting"),
          meshName ? h("span", { class: "small", style: { fontWeight: 560 } }, meshName) : null,
          store.mesh
            ? [
                h("span", { class: "small faint" }, "Mesh compute ", h("strong", { class: "mono accent" }, `${tflops.toFixed(1)} TFLOP/s`)),
                h("span", { class: "small faint" }, "Device memory ", h("strong", { class: "mono", style: { color: "var(--text)" } }, `${Math.round(memory / 1024)} GB`)),
              ]
            : null,
        ]
      : [
          h("span", { class: "badge" }, h("span", { class: "dot", style: { background: "var(--text-faint)" } }), "Not in a mesh"),
          port ? h("span", { class: "small faint" }, "Ready to train on ", h("strong", { class: "mono accent" }, port.spec.id)) : null,
        ]
  );

  clear(
    topRight,
    store.update && !store.updateInstalled
      ? h("button", { class: "update-pill", type: "button", onclick: () => openUpdate(store.update), title: "An update is ready to install" }, h("span", { class: "dot" }), store.updateProgress?.done ? "Restart to update" : `Update ${store.update.latest}`)
      : null,
    h("button", { class: "search-pill", type: "button", onclick: openPalette, "aria-label": "Search screens and actions" }, icon("search", 14), h("span", {}, "Search or jump to…"), h("kbd", {}, `${MOD} K`))
  );
}

// ---------------------------------------------------------------- palette

let paletteEl = null;

function actions() {
  const list = [];
  if (contributing()) list.push({ label: "Stop contributing", icon: "stop", run: () => api.leave() });
  else {
    list.push({ label: "Join a mesh", icon: "radar", run: () => go("discover") });
    if (store.settings?.last_invite) list.push({ label: "Rejoin the last mesh", icon: "refresh", run: () => api.join(store.settings.last_invite) });
  }
  if (hosting()) {
    list.push({ label: "Stop hosting", icon: "power", run: () => api.hostStop() });
    list.push({ label: "Copy invite link", icon: "link", run: () => copyText(store.status.hosting.invite_link, "Invite link copied") });
  } else list.push({ label: "Host a mesh", icon: "host", run: async () => (await api.hostStart(), go("invite")) });
  list.push({ label: "Repair this machine's port", icon: "wrench", run: () => (go("device"), selectedPort() && api.installPort(selectedPort().spec.id)) });
  list.push({ label: "Check for updates", icon: "arrowUp", run: () => checkForUpdates() });
  list.push({ label: "Open data folder", icon: "folder", run: () => api.openDataDir() });
  if (isTauri) {
    list.push({ label: "Hide to tray", icon: "x", run: () => api.hideWindow() });
    list.push({ label: "Quit GradApp", icon: "power", run: () => api.quitApp() });
  }
  return list;
}

function openPalette() {
  if (paletteEl) return;
  const previous = document.activeElement;
  const input = h("input", { class: "palette-input", placeholder: "Type a screen or an action", "aria-label": "Search", autocomplete: "off", spellcheck: false });
  const list = h("ul", { class: "palette-list", role: "listbox" });
  let items = [];
  let index = 0;
  const all = () => [...VIEWS.map((v) => ({ label: v.label, hint: `Go to · ${MOD} ${v.key}`, icon: v.icon, run: () => go(v.id) })), ...actions().map((a) => ({ ...a, hint: "Action" }))];
  const score = (label, q) => {
    const text = label.toLowerCase();
    if (!q) return 1;
    if (text.startsWith(q)) return 3;
    if (text.split(/\s+/).some((w) => w.startsWith(q))) return 2;
    let at = 0;
    for (const ch of q) {
      at = text.indexOf(ch, at);
      if (at === -1) return 0;
      at += 1;
    }
    return 1;
  };
  const draw = () => {
    const q = input.value.trim().toLowerCase();
    items = all()
      .map((item) => ({ item, s: score(item.label, q) }))
      .filter((e) => e.s > 0)
      .sort((a, b) => b.s - a.s)
      .map((e) => e.item);
    index = Math.min(index, Math.max(items.length - 1, 0));
    clear(
      list,
      items.length
        ? items.map((item, i) =>
            h(
              "li",
              { class: `palette-item${i === index ? " is-active" : ""}`, role: "option", "aria-selected": String(i === index), onmousemove: () => index !== i && ((index = i), draw()), onclick: () => choose(i) },
              icon(item.icon, 16),
              h("span", { class: "palette-label" }, item.label),
              h("span", { class: "palette-hint" }, item.hint)
            )
          )
        : h("li", { class: "palette-item" }, "Nothing matches.")
    );
  };
  const close = () => {
    paletteEl?.remove();
    paletteEl = null;
    previous?.focus?.();
  };
  const choose = async (i) => {
    const item = items[i];
    close();
    if (!item) return;
    try {
      await item.run();
    } catch (error) {
      toast(error.message, "error");
    }
  };
  input.addEventListener("input", () => ((index = 0), draw()));
  input.addEventListener("keydown", (e) => {
    if (e.key === "ArrowDown") (e.preventDefault(), (index = (index + 1) % Math.max(items.length, 1)), draw());
    else if (e.key === "ArrowUp") (e.preventDefault(), (index = (index - 1 + items.length) % Math.max(items.length, 1)), draw());
    else if (e.key === "Enter") (e.preventDefault(), choose(index));
    else if (e.key === "Escape") (e.preventDefault(), close());
  });
  paletteEl = h("div", { class: "scrim is-top", onclick: (e) => e.target === paletteEl && close() }, h("div", { class: "palette", role: "dialog", "aria-label": "Command palette" }, input, list));
  document.body.append(paletteEl);
  draw();
  input.focus();
}

// ----------------------------------------------------------------- update

let updateEl = null;

/** Minimal markdown for release notes: ### headings, - bullets, plain lines. */
function notesToNodes(text) {
  const out = [];
  let list = null;
  for (const raw of String(text || "").split("\n")) {
    const line = raw.trim();
    if (!line) {
      list = null;
      continue;
    }
    if (line.startsWith("#")) {
      list = null;
      out.push(h("h4", {}, line.replace(/^#+\s*/, "")));
    } else if (/^[-*]\s+/.test(line)) {
      if (!list) out.push((list = h("ul")));
      list.append(h("li", {}, line.replace(/^[-*]\s+/, "")));
    } else {
      list = null;
      out.push(h("p", {}, line));
    }
  }
  return out.length ? out : [h("p", {}, "This release has no notes.")];
}

export function openUpdate(info) {
  if (!info) return;
  updateEl?.remove();
  const body = h("div", { class: "modal-body" });
  const foot = h("div", { class: "modal-foot" });
  const modal = h("div", { class: "modal", role: "dialog", "aria-modal": "true", "aria-labelledby": "update-title" }, body, foot);
  updateEl = h("div", { class: "scrim", onclick: (e) => e.target === updateEl && phase !== "downloading" && close() }, modal);
  let phase = store.updateProgress?.done ? "ready" : "offer";
  let error = null;

  const close = () => {
    updateEl?.remove();
    updateEl = null;
    offProgress();
  };

  const header = (title, sub) =>
    h(
      "div",
      { class: "row", style: { gap: "16px", alignItems: "center" } },
      h("div", { class: "modal-orbit" }, icon("arrowUp", 20)),
      h("div", { class: "stack-sm", style: { gap: "3px" } }, h("h2", { class: "modal-title", id: "update-title" }, title), h("span", { class: "small faint" }, sub))
    );

  const draw = () => {
    const published = info.published_at ? new Date(info.published_at).toLocaleDateString(undefined, { day: "numeric", month: "short", year: "numeric" }) : null;
    if (phase === "offer") {
      clear(
        body,
        header(`GradApp ${info.latest} is available`, `You have ${info.current}${published ? `. Released ${published}.` : "."}`),
        h("div", { class: "notes selectable" }, notesToNodes(info.notes)),
        error ? h("div", { class: "notice notice-danger" }, error) : null,
        h("p", { class: "small faint" }, info.installable ? "The update downloads in the background and is checked against GradApp's signing key before it installs. Contributing pauses only for the restart." : "This copy of GradApp cannot update itself; the download page has the new installer.")
      );
      clear(
        foot,
        h("button", { class: "ld-btn ld-btn-ghost", type: "button", onclick: close }, "Later"),
        info.installable
          ? h("button", { class: "ld-btn ld-btn-solid", type: "button", onclick: startInstall }, error ? "Try again" : "Update now")
          : h("button", { class: "ld-btn ld-btn-solid", type: "button", onclick: () => api.openUrl(info.url) }, "Open download page")
      );
    } else if (phase === "downloading") {
      const p = store.updateProgress;
      const fraction = p?.total ? Math.min(1, p.downloaded / p.total) : null;
      clear(
        body,
        header(`Downloading GradApp ${info.latest}`, "You can keep using GradApp while it downloads."),
        h(
          "div",
          { class: "row", style: { gap: "18px" } },
          h("div", { class: "progress-ring", style: { "--p": `${(fraction ?? 0) * 360}deg` } }, h("span", {}, fraction == null ? "…" : `${Math.round(fraction * 100)}%`)),
          h(
            "div",
            { class: "stack-sm grow" },
            h("div", { class: "train-bar" }, h("div", { class: "train-bar-fill", style: { width: `${(fraction ?? 0.05) * 100}%` } })),
            h("span", { class: "small faint mono" }, p ? `${fmtBytes(p.downloaded)} of ${p.total ? fmtBytes(p.total) : "unknown size"}` : "Connecting to GitHub Releases")
          )
        )
      );
      clear(foot, h("button", { class: "ld-btn ld-btn-ghost", type: "button", onclick: close }, "Hide"));
    } else {
      clear(
        body,
        header("Restart to finish", `GradApp ${info.latest} is installed. It takes over when GradApp restarts.`),
        h("p", { class: "small muted" }, contributing() ? "This machine rejoins its mesh on its own after the restart; the round in progress is handed to another machine." : "Nothing is running that a restart would interrupt.")
      );
      clear(
        foot,
        h("button", { class: "ld-btn ld-btn-ghost", type: "button", onclick: close }, "Later"),
        h("button", { class: "ld-btn ld-btn-solid", type: "button", onclick: () => api.restartApp() }, "Restart now")
      );
    }
  };

  async function startInstall() {
    phase = "downloading";
    error = null;
    store.updateProgress = null;
    draw();
    try {
      await api.installUpdate();
      store.updateProgress = { ...(store.updateProgress || {}), done: true };
      phase = "ready";
    } catch (e) {
      error = e.message;
      phase = "offer";
    }
    renderChrome();
    if (updateEl) draw();
  }

  const offProgress = subscribe((reasons) => {
    if (reasons.has("update-progress") && phase === "downloading") draw();
  });

  draw();
  document.body.append(updateEl);
  modal.querySelector(".ld-btn-solid")?.focus();
}

async function checkForUpdates() {
  const info = await api.checkUpdate();
  if (info.available) {
    store.update = info;
    openUpdate(info);
  } else if (info.reason) toast(`Could not check for updates: ${info.reason}`, "error");
  else toast(`GradApp ${info.current} is the latest version.`, "ok");
  renderChrome();
}
export { checkForUpdates };

// ------------------------------------------------------------------ boot

function keyboard() {
  window.addEventListener("keydown", (e) => {
    const meta = isMac ? e.metaKey : e.ctrlKey;
    if (meta && e.key.toLowerCase() === "k") {
      e.preventDefault();
      paletteEl ? paletteEl.remove() || (paletteEl = null) : openPalette();
      return;
    }
    if (meta && !e.shiftKey && !e.altKey) {
      const view = VIEWS.find((v) => v.key === e.key);
      if (view) {
        e.preventDefault();
        go(view.id);
      }
    }
    if (e.key === "Escape" && updateEl && !paletteEl) {
      updateEl.querySelector(".ld-btn-ghost")?.click();
    }
  });
}

async function start() {
  shell();
  keyboard();
  window.addEventListener("hashchange", () => show(location.hash.replace(/^#\/?/, "")));
  onChannel("navigate", (view) => go(view));
  onChannel("invite", (link) => {
    store.pendingInvite = link;
    go("discover");
    toast("Invite received. Check the address and press Join.");
  });
  onNotice((n) => toast(n.body ? `${n.title}. ${n.body}` : n.title, n.level === "error" ? "error" : "ok"));
  subscribe((reasons) => {
    if (["status", "mesh", "device", "install", "update", "update-progress"].some((r) => reasons.has(r))) renderChrome();
    if (reasons.has("update") && store.update && !store.update._quiet) {
      const seen = sessionStorage.getItem("gradapp:update-offered");
      if (seen !== store.update.latest) {
        sessionStorage.setItem("gradapp:update-offered", store.update.latest);
        openUpdate(store.update);
      }
    }
  });
  await boot();
  watchInvites(); // another GradApp's invite, on any screen
  let initial = location.hash.replace(/^#\/?/, "");
  if (!initial) {
    try {
      initial = sessionStorage.getItem("gradapp:view") || "home";
    } catch {
      initial = "home";
    }
  }
  show(initial);
  renderChrome();
  app.dataset.ready = "yes";
  refreshStatus();
}

start().catch((error) => {
  console.error(error);
  app.textContent = `GradApp could not start its window: ${error.message}`;
});
