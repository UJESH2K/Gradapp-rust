// Small DOM toolkit and the v5 primitives (panel, stat, meter, vendor badge,
// copy line) as functions. No framework: a handful of screens does not earn
// one, and the window should be interactive the moment it opens.

export function h(tag, attrs = {}, ...children) {
  const el = document.createElementNS(
    attrs && attrs.svg ? "http://www.w3.org/2000/svg" : "http://www.w3.org/1999/xhtml",
    tag
  );
  for (const [key, value] of Object.entries(attrs || {})) {
    if (key === "svg" || value === undefined || value === null || value === false) continue;
    if (key === "class") el.setAttribute("class", value);
    else if (key === "style" && typeof value === "object") Object.assign(el.style, value);
    else if (key.startsWith("on") && typeof value === "function") el.addEventListener(key.slice(2).toLowerCase(), value);
    else if (key === "html") el.innerHTML = value; // only ever our own static markup
    else if (key in el && typeof value !== "string" && !attrs.svg) el[key] = value;
    else el.setAttribute(key, value === true ? "" : value);
  }
  append(el, children);
  return el;
}

export function append(el, children) {
  for (const child of children.flat(Infinity)) {
    if (child === null || child === undefined || child === false) continue;
    el.append(child instanceof Node ? child : document.createTextNode(String(child)));
  }
  return el;
}

export function clear(el, ...children) {
  el.replaceChildren();
  return append(el, children);
}

const PATHS = {
  home: '<path d="M3 10.5 12 3l9 7.5"/><path d="M5 9.5V21h14V9.5"/><path d="M10 21v-6h4v6"/>',
  radar: '<circle cx="12" cy="12" r="9"/><circle cx="12" cy="12" r="5"/><path d="M12 12 18.5 5.5"/><circle cx="12" cy="12" r="1" fill="currentColor"/>',
  mesh: '<circle cx="12" cy="12" r="2.4"/><circle cx="5" cy="6" r="1.8"/><circle cx="19" cy="6" r="1.8"/><circle cx="5" cy="18" r="1.8"/><circle cx="19" cy="18" r="1.8"/><path d="M6.4 7.2 10.3 10.6M17.6 7.2l-3.9 3.4M6.4 16.8l3.9-3.4M17.6 16.8l-3.9-3.4"/>',
  runs: '<path d="M4 19h16"/><path d="M6 15l4-5 3 3 5-7"/>',
  datasets: '<ellipse cx="12" cy="5.5" rx="7" ry="2.5"/><path d="M5 5.5v6c0 1.4 3.1 2.5 7 2.5s7-1.1 7-2.5v-6"/><path d="M5 11.5v6c0 1.4 3.1 2.5 7 2.5s7-1.1 7-2.5v-6"/>',
  invite: '<path d="M15 19v-1.5A3.5 3.5 0 0 0 11.5 14h-5A3.5 3.5 0 0 0 3 17.5V19"/><circle cx="9" cy="7.5" r="3.5"/><path d="M19 8v6M22 11h-6"/>',
  activity: '<path d="M3 12h4l3 8 4-16 3 8h4"/>',
  settings: '<circle cx="12" cy="12" r="3"/><path d="M19.4 15a1.7 1.7 0 0 0 .3 1.8l.1.1a2 2 0 1 1-2.8 2.8l-.1-.1a1.7 1.7 0 0 0-1.8-.3 1.7 1.7 0 0 0-1 1.5V21a2 2 0 1 1-4 0v-.1a1.7 1.7 0 0 0-1.1-1.5 1.7 1.7 0 0 0-1.8.3l-.1.1a2 2 0 1 1-2.8-2.8l.1-.1a1.7 1.7 0 0 0 .3-1.8 1.7 1.7 0 0 0-1.5-1H3a2 2 0 1 1 0-4h.1a1.7 1.7 0 0 0 1.5-1.1 1.7 1.7 0 0 0-.3-1.8l-.1-.1a2 2 0 1 1 2.8-2.8l.1.1a1.7 1.7 0 0 0 1.8.3H9a1.7 1.7 0 0 0 1-1.5V3a2 2 0 1 1 4 0v.1a1.7 1.7 0 0 0 1 1.5 1.7 1.7 0 0 0 1.8-.3l.1-.1a2 2 0 1 1 2.8 2.8l-.1.1a1.7 1.7 0 0 0-.3 1.8V9a1.7 1.7 0 0 0 1.5 1H21a2 2 0 1 1 0 4h-.1a1.7 1.7 0 0 0-1.5 1z"/>',
  chip: '<rect x="6" y="6" width="12" height="12" rx="2"/><rect x="9.5" y="9.5" width="5" height="5" rx="1"/><path d="M9 2v4M15 2v4M9 18v4M15 18v4M2 9h4M2 15h4M18 9h4M18 15h4"/>',
  search: '<circle cx="11" cy="11" r="7"/><path d="m20 20-3.5-3.5"/>',
  copy: '<rect x="9" y="9" width="12" height="12" rx="2"/><path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"/>',
  check: '<path d="M20 6 9 17l-5-5"/>',
  x: '<path d="M18 6 6 18M6 6l12 12"/>',
  refresh: '<path d="M21 12a9 9 0 1 1-2.6-6.4L21 8"/><path d="M21 3v5h-5"/>',
  power: '<path d="M12 3v9"/><path d="M6.3 7.3a8 8 0 1 0 11.4 0"/>',
  download: '<path d="M12 4v12"/><path d="m7 11 5 5 5-5"/><path d="M4 16v3a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2v-3"/>',
  upload: '<path d="M12 16V4"/><path d="m7 9 5-5 5 5"/><path d="M4 16v3a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2v-3"/>',
  stop: '<rect x="6" y="6" width="12" height="12" rx="2"/>',
  play: '<path d="M7 4.5v15l12-7.5z"/>',
  folder: '<path d="M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z"/>',
  wifi: '<path d="M2 8.8a15 15 0 0 1 20 0"/><path d="M5 12.5a10 10 0 0 1 14 0"/><path d="M8.5 16a5 5 0 0 1 7 0"/><circle cx="12" cy="19.5" r=".9"/>',
  link: '<path d="M10 14a4 4 0 0 0 5.7 0l3-3a4 4 0 0 0-5.7-5.7l-1 1"/><path d="M14 10a4 4 0 0 0-5.7 0l-3 3a4 4 0 0 0 5.7 5.7l1-1"/>',
  plus: '<path d="M12 5v14M5 12h14"/>',
  star: '<path d="m12 3 2.8 5.7 6.2.9-4.5 4.4 1 6.2L12 17.3 6.5 20.2l1-6.2L3 9.6l6.2-.9z"/>',
  trash: '<path d="M4 7h16M10 11v6M14 11v6"/><path d="M6 7l1 13h10l1-13M9 7V4h6v3"/>',
  wrench: '<path d="M14.7 6.3a4 4 0 0 0-5.4 5.2L3 17.8V21h3.2l6.3-6.3a4 4 0 0 0 5.2-5.4l-2.6 2.6-2.4-.6-.6-2.4z"/>',
  swap: '<path d="M7 4 3 8l4 4"/><path d="M3 8h13"/><path d="m17 20 4-4-4-4"/><path d="M21 16H8"/>',
  eye: '<path d="M2 12s3.5-7 10-7 10 7 10 7-3.5 7-10 7S2 12 2 12z"/><circle cx="12" cy="12" r="3"/>',
  arrowUp: '<path d="M12 19V5"/><path d="m5 12 7-7 7 7"/>',
  external: '<path d="M14 4h6v6"/><path d="M20 4 10 14"/><path d="M19 14v5a1 1 0 0 1-1 1H5a1 1 0 0 1-1-1V6a1 1 0 0 1 1-1h5"/>',
  host: '<rect x="3" y="4" width="18" height="7" rx="2"/><rect x="3" y="13" width="18" height="7" rx="2"/><path d="M7 7.5h.01M7 16.5h.01"/>',
};

export function icon(name, size = 17) {
  const span = document.createElement("span");
  span.className = "icon";
  span.setAttribute("aria-hidden", "true");
  span.innerHTML = `<svg width="${size}" height="${size}" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round">${PATHS[name] || ""}</svg>`;
  return span;
}

/** The v5 mark: six vendors' worth of nodes around one model. */
export function logo(size = 22) {
  const span = document.createElement("span");
  span.className = "brand-mark";
  span.style.display = "inline-flex";
  span.innerHTML = `<svg width="${size}" height="${size}" viewBox="0 0 24 24" fill="none" aria-hidden="true">
    <path d="M12 3.2 L20.2 7.6 L20.2 16.4 L12 20.8 L3.8 16.4 L3.8 7.6 Z" stroke="currentColor" stroke-opacity="0.35" stroke-width="1.2"/>
    <circle cx="12" cy="12" r="3" fill="#5b7cfa"/>
    <circle cx="12" cy="3.2" r="1.6" fill="currentColor" fill-opacity="0.9"/>
    <circle cx="20.2" cy="7.6" r="1.6" fill="currentColor" fill-opacity="0.75"/>
    <circle cx="20.2" cy="16.4" r="1.6" fill="currentColor" fill-opacity="0.6"/>
    <circle cx="12" cy="20.8" r="1.6" fill="currentColor" fill-opacity="0.75"/>
    <circle cx="3.8" cy="16.4" r="1.6" fill="currentColor" fill-opacity="0.6"/>
    <circle cx="3.8" cy="7.6" r="1.6" fill="currentColor" fill-opacity="0.9"/>
    <g stroke="#5b7cfa" stroke-opacity="0.55" stroke-width="1"><path d="M12 12 L12 3.2"/><path d="M12 12 L20.2 7.6"/><path d="M12 12 L3.8 7.6"/></g>
  </svg>`;
  return span;
}

export function button(label, { kind = "", icon: iconName, onClick, disabled, title, small, large, type = "button" } = {}) {
  const classes = ["btn", kind ? `btn-${kind}` : "", small ? "btn-sm" : "", large ? "btn-lg" : ""].filter(Boolean).join(" ");
  return h(
    "button",
    { class: classes, type, onclick: onClick, disabled, title },
    iconName ? icon(iconName, small ? 14 : 16) : null,
    label ? h("span", {}, label) : null
  );
}

/** Run an async action with the button showing it is busy; errors become toasts. */
export async function busy(btn, fn) {
  const before = btn.disabled;
  btn.disabled = true;
  btn.setAttribute("aria-busy", "true");
  try {
    return await fn();
  } catch (error) {
    toast(String(error?.message || error), "error");
    return null;
  } finally {
    btn.disabled = before;
    btn.removeAttribute("aria-busy");
  }
}

export function toast(message, tone = "") {
  const host = document.getElementById("toasts");
  const el = h("div", { class: `toast${tone ? ` is-${tone}` : ""}`, role: tone === "error" ? "alert" : "status" }, message);
  host.append(el);
  setTimeout(() => el.remove(), tone === "error" ? 6500 : 3200);
}

export async function copyText(text, label = "Copied") {
  try {
    await navigator.clipboard.writeText(text);
    toast(label, "ok");
  } catch {
    toast("Could not reach the clipboard; select the text and copy it.", "error");
  }
}

export function copyLine(value, { mask = false, label = "Copied" } = {}) {
  const shown = mask ? value.replace(/(token=)[^&]+/, "$1••••••••").replace(/^[0-9a-f]{12,}$/i, (t) => `${t.slice(0, 4)}••••••••${t.slice(-4)}`) : value;
  const code = h("code", { title: mask ? "Hidden; copy to use" : value }, shown);
  let revealed = !mask;
  return h(
    "div",
    { class: "copy-line" },
    code,
    mask
      ? h("button", {
          class: "btn btn-ghost btn-sm",
          type: "button",
          title: "Show",
          onclick: (e) => {
            revealed = !revealed;
            code.textContent = revealed ? value : shown;
            e.currentTarget.replaceChildren(icon(revealed ? "x" : "eye", 14));
          },
        }, icon("eye", 14))
      : null,
    h("button", { class: "btn btn-sm", type: "button", onclick: () => copyText(value, label) }, icon("copy", 14), h("span", {}, "Copy"))
  );
}

export function panel({ title, sub, action, flush = false, cls = "" } = {}, ...body) {
  const head =
    title || action
      ? h(
          "div",
          { class: "panel-header" },
          h("div", { class: "stack-sm", style: { gap: "2px" } }, title ? h("span", { class: "panel-title" }, title) : null, sub ? h("span", { class: "small faint" }, sub) : null),
          action || null
        )
      : null;
  return h("section", { class: `panel panel-flush ${cls}` }, head, flush ? body : h("div", { class: "panel-body" }, body));
}

export function stat(label, value, foot, accent = false) {
  return h(
    "div",
    { class: "stat" },
    h("span", { class: "stat-label" }, label),
    h("span", { class: `stat-value${accent ? " is-accent" : ""}` }, value),
    foot ? h("span", { class: "stat-foot" }, foot) : null
  );
}

export function meter(value, tone = "") {
  return h("div", { class: "meter" }, h("div", { class: `meter-fill${tone ? ` is-${tone}` : ""}`, style: { width: `${Math.max(0, Math.min(1, value || 0)) * 100}%` } }));
}

export function emptyState(title, text, ...actions) {
  return h("div", { class: "empty-state" }, h("h3", {}, title), text ? h("p", {}, text) : null, actions.length ? h("div", { class: "row wrap", style: { justifyContent: "center" } }, actions) : null);
}

export function toggle(checked, onChange, label) {
  const el = h("button", { class: "toggle", type: "button", role: "switch", "aria-checked": String(Boolean(checked)), "aria-label": label });
  el.addEventListener("click", () => {
    const next = el.getAttribute("aria-checked") !== "true";
    el.setAttribute("aria-checked", String(next));
    onChange(next);
  });
  return el;
}

export function segmented(options, value, onChange) {
  const wrap = h("div", { class: "seg", role: "radiogroup" });
  const draw = (current) =>
    clear(
      wrap,
      options.map(([id, label]) =>
        h("button", { type: "button", role: "radio", "aria-checked": String(id === current), class: id === current ? "is-on" : "", onclick: () => (draw(id), onChange(id)) }, label)
      )
    );
  draw(value);
  return wrap;
}

export function select(options, value, onChange) {
  const el = h("select", { class: "select", onchange: (e) => onChange(e.target.value) }, options.map(([id, label]) => h("option", { value: id }, label)));
  el.value = value;
  return el;
}

// ------------------------------------------------------------------ vendors

export const VENDORS = {
  cuda: { vendor: "nvidia", name: "NVIDIA", api: "CUDA" },
  xpu: { vendor: "intel", name: "Intel", api: "XPU" },
  mps: { vendor: "apple", name: "Apple", api: "Metal" },
  rocm: { vendor: "cpu", name: "AMD", api: "ROCm" },
  cpu: { vendor: "cpu", name: "CPU", api: "CPU" },
};

export const vendorOf = (backend) => VENDORS[backend] || VENDORS.cpu;

export function vendorBadge(backend, compact = false) {
  const info = vendorOf(backend);
  return h("span", { class: `vendor vendor-${info.vendor}`, title: `${info.name} via ${info.api}` }, h("span", { class: "vendor-dot", "aria-hidden": "true" }), compact ? info.name : `${info.name} · ${info.api}`);
}

export const backendFromVendor = (vendor) => ({ nvidia: "cuda", intel: "xpu", apple: "mps", amd: "rocm" })[vendor] || "cpu";

// ------------------------------------------------------------------ format

export function fmtMemory(mb) {
  if (mb == null) return "unknown";
  if (mb >= 1024) return `${(mb / 1024).toFixed(mb >= 10240 ? 0 : 1)} GB`;
  return `${Math.round(mb)} MB`;
}

export function fmtBytes(bytes) {
  if (bytes == null) return "?";
  if (bytes >= 1048576) return `${(bytes / 1048576).toFixed(1)} MB`;
  if (bytes >= 1024) return `${(bytes / 1024).toFixed(0)} KB`;
  return `${bytes} B`;
}

export function fmtDuration(seconds) {
  if (seconds == null) return "–";
  if (seconds < 60) return `${seconds.toFixed(seconds < 10 ? 1 : 0)} s`;
  const m = Math.floor(seconds / 60);
  if (m < 60) return `${m} min ${Math.round(seconds % 60)} s`;
  return `${Math.floor(m / 60)} h ${m % 60} min`;
}

export function fmtAgo(at) {
  if (!at) return "–";
  const s = Math.max(0, Date.now() / 1000 - at);
  if (s < 45) return "just now";
  if (s < 3600) return `${Math.round(s / 60)} min ago`;
  if (s < 86400) return `${Math.round(s / 3600)} h ago`;
  return `${Math.round(s / 86400)} d ago`;
}

export function fmtClock(at) {
  return new Date(at * 1000).toLocaleTimeString(undefined, { hour12: false, hour: "2-digit", minute: "2-digit", second: "2-digit" });
}

export function initials(label) {
  const words = String(label).replace(/[^A-Za-z0-9 ]/g, " ").trim().split(/\s+/);
  if (!words[0]) return "?";
  if (words.length === 1) return words[0].slice(0, 2).toUpperCase();
  return (words[0][0] + words[1][0]).toUpperCase();
}

/** Stable angle from a string, so radar seats never shuffle (from v5 DeviceRadar). */
export function angleFor(seed) {
  let hash = 2166136261;
  for (let i = 0; i < seed.length; i += 1) {
    hash ^= seed.charCodeAt(i);
    hash = Math.imul(hash, 16777619);
  }
  return ((hash >>> 0) % 3600) / 3600;
}

export const isMac = /Mac|iPhone|iPad/.test(navigator.platform || navigator.userAgent);
export const MOD = isMac ? "⌘" : "Ctrl";
