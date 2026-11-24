// Uploads and downloads (datasets, trained weights): their live state from
// the backend's "gm://transfer" channel, a list widget that draws them, and a
// confirm dialog for destructive actions. Shared by the Datasets and Runs screens.

import { api, onChannel } from "./api.js";
import { h, clear, icon, toast, fmtBytes } from "./ui.js";

const transfers = new Map();
const listeners = new Set();
let loaded = false;

function changed(t) {
  for (const fn of listeners) {
    try {
      fn(t);
    } catch (error) {
      console.error(error);
    }
  }
}

onChannel("transfer", (t) => {
  const before = transfers.get(t.id);
  transfers.set(t.id, t);
  if (before?.state !== t.state) {
    if (t.state === "done") toast(t.kind === "download" ? `Saved to ${t.result}` : `Uploaded. ${t.result || ""}`, "ok");
    else if (t.state === "failed") toast(`${t.label} failed: ${t.error}`, "error");
  }
  changed(t);
});

/** Subscribe to transfer changes; loads the backend's list once. */
export function onTransfer(fn) {
  listeners.add(fn);
  if (!loaded) {
    loaded = true;
    api
      .transfers()
      .then((list) => {
        for (const t of list || []) if (!transfers.has(t.id)) transfers.set(t.id, t);
        changed(null);
      })
      .catch(() => {});
  }
  return () => listeners.delete(fn);
}

export const allTransfers = () => [...transfers.values()].sort((a, b) => b.started_at - a.started_at);
export const activeUpload = () => allTransfers().find((t) => t.kind === "upload" && ["running", "processing"].includes(t.state));

function pct(t) {
  return t.total ? Math.min(1, t.done / t.total) : null;
}

/** One transfer as a card: label, bytes, bar, state, cancel / dismiss. */
export function transferCard(t, onDismiss) {
  const fraction = pct(t);
  const running = t.state === "running" || t.state === "processing";
  const stateText = {
    running: fraction == null ? `${fmtBytes(t.done)} so far` : `${fmtBytes(t.done)} of ${fmtBytes(t.total)}`,
    processing: "Sent. The host is unpacking and checking the archive…",
    done: t.kind === "download" ? `Saved to ${t.result}` : t.result || "Done",
    failed: t.error || "Failed",
    cancelled: "Cancelled",
  }[t.state];
  return h(
    "div",
    { class: `transfer is-${t.state}`, "data-transfer": t.id },
    h("span", { class: "transfer-icon" }, icon(t.kind === "upload" ? "upload" : "download", 16)),
    h(
      "div",
      { class: "stack-sm grow", style: { gap: "6px", minWidth: 0 } },
      h(
        "div",
        { class: "row-between", style: { gap: "12px" } },
        h("strong", { class: "truncate" }, t.state === "done" ? t.label.replace(/^Uploading/, "Uploaded").replace(/^Downloading/, "Downloaded") : t.label),
        h("span", { class: "mono small nowrap" }, t.state === "processing" ? "100%" : fraction != null ? `${(fraction * 100).toFixed(fraction < 0.1 ? 1 : 0)}%` : running ? "…" : "")
      ),
      h("div", { class: "transfer-track" }, h("div", { class: `transfer-fill${t.state === "processing" ? " is-processing" : ""}`, style: { width: `${(t.state === "done" ? 1 : t.state === "processing" ? 1 : fraction ?? 0.04) * 100}%` } })),
      h("span", { class: `small ${t.state === "failed" ? "danger-text" : "faint"} truncate selectable`, title: stateText }, stateText)
    ),
    running && t.state !== "processing"
      ? h("button", { class: "btn btn-sm btn-ghost", type: "button", title: "Cancel", onclick: () => api.transferCancel(t.id).catch((e) => toast(e.message, "error")) }, icon("x", 14), h("span", {}, "Cancel"))
      : !running && onDismiss
        ? h("button", { class: "btn btn-sm btn-ghost", type: "button", title: "Dismiss", onclick: () => (transfers.delete(t.id), onDismiss()) }, icon("x", 14))
        : null
  );
}

/** A live list of transfers matching `filter`; returns [element, cleanup]. */
export function transferList(filter) {
  const el = h("div", { class: "transfer-list" });
  const draw = () => clear(el, allTransfers().filter(filter).slice(0, 4).map((t) => transferCard(t, draw)));
  draw();
  return [el, onTransfer(draw)];
}

/** Modal confirm. Resolves true when confirmed. */
export function confirmDialog({ title, body, confirm = "Confirm", danger = false }) {
  return new Promise((resolve) => {
    let scrim;
    const close = (value) => {
      scrim.remove();
      document.removeEventListener("keydown", onKey);
      resolve(value);
    };
    const onKey = (e) => {
      if (e.key === "Escape") close(false);
    };
    const ok = h("button", { class: `ld-btn ${danger ? "ld-btn-danger" : "ld-btn-solid"}`, type: "button", onclick: () => close(true) }, confirm);
    scrim = h(
      "div",
      { class: "scrim", onclick: (e) => e.target === scrim && close(false) },
      h(
        "div",
        { class: "modal modal-sm", role: "alertdialog", "aria-modal": "true", "aria-label": title },
        h("div", { class: "modal-body" }, h("h2", { class: "modal-title" }, title), body ? h("p", { class: "muted" }, body) : null),
        h("div", { class: "modal-foot" }, h("button", { class: "ld-btn ld-btn-ghost", type: "button", onclick: () => close(false) }, "Cancel"), ok)
      )
    );
    document.body.append(scrim);
    document.addEventListener("keydown", onKey);
    ok.focus();
  });
}

/** Ask for a folder, then start a download there. */
export async function downloadTo(title, start) {
  const dir = await api.pickFolder(title);
  if (!dir) return null;
  return start(dir);
}
