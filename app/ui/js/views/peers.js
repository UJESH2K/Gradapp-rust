// GradApps on this network (app/src-tauri/src/peers.rs): every running
// GradApp 2 announces itself on the LAN. On Discover, a hosting app sees them
// as cards and invites them into its mesh, one by one or all at once. On the
// other side the invite arrives as an OS notification and as the dialog below;
// nothing joins until the person presses Join.

import { api } from "../api.js";
import { store, subscribe, hosting, contributing, refreshPeers, refreshStatus } from "../store.js";
import { h, clear, icon, busy, toast, vendorBadge, initials, copyText } from "../ui.js";
import { qrSvg } from "./invite.js";

const OS = { windows: "Windows", macos: "macOS", linux: "Linux", other: "Other OS" };
const PORT = { ready: "Port ready", installing: "Installing its port", not_installed: "Port not installed yet", stale: "Port needs a rebuild", failed: "Port failed", blocked: "Cannot train" };
const OUTCOME = {
  sent: ["Invite sent · waiting for an answer", "badge-accent"],
  accepted: ["Accepted · joining", "badge-live"],
  declined: ["Not now", "badge-warn"],
  timed_out: ["No answer", "badge-warn"],
  failed: ["Not delivered", "badge-danger"],
};

/** The mesh this computer is in (hosting first). */
function ownMesh() {
  return store.status?.hosting?.mesh_id || store.peerInfo?.me?.mesh_id || null;
}

function statusBadge(p) {
  if (p.status === "hosting") return h("span", { class: "badge badge-cyan", title: "Runs a mesh coordinator" }, h("span", { class: "dot", style: { background: "var(--cyan)" } }), `Hosting ${p.mesh_name || "a mesh"}`);
  if (p.status === "contributing") return h("span", { class: "badge badge-live" }, h("span", { class: "dot dot-live" }), p.mesh_name ? `Contributing to ${p.mesh_name}` : "Contributing");
  return h("span", { class: "badge" }, h("span", { class: "dot", style: { background: "var(--text-faint)" } }), "Idle");
}

/** The "GradApps on this network" panel for the Discover screen. Returns a cleanup. */
export function renderPeersPanel(root, { go }) {
  const selected = new Set();
  const grid = h("div", { class: "peer-grid" });
  const head = h("div", { class: "row-between wrap", style: { gap: "12px" } });
  const notes = h("div", { class: "stack-sm" });
  const aside = h("aside", { class: "peer-aside stack" });

  clear(
    root,
    h(
      "section",
      { class: "panel stack peer-panel" },
      head,
      notes,
      h("div", { class: "peer-layout" }, grid, aside)
    )
  );

  const invitable = (p) => {
    const sent = store.sent.get(p.device_id);
    return hosting() && p.invite_port > 0 && p.mesh_id !== ownMesh() && sent?.outcome !== "sent";
  };

  async function invite(ids, btn) {
    if (!ids.length) return;
    await busy(btn, async () => {
      const results = await api.peerInvite(ids);
      const ok = results.filter((r) => !r.error);
      const bad = results.filter((r) => r.error);
      for (const r of results) selected.delete(r.device_id);
      if (ok.length) toast(ok.length === 1 ? `Invite sent to ${store.peers.get(ok[0].device_id)?.name || "1 GradApp"}.` : `Invites sent to ${ok.length} GradApps.`, "ok");
      for (const r of bad) toast(r.error, "error");
      draw();
    });
  }

  function card(p) {
    const mine = ownMesh() && p.mesh_id === ownMesh();
    const sent = store.sent.get(p.device_id);
    const outcome = sent && !mine ? OUTCOME[sent.outcome] : null;
    const can = invitable(p);
    const check = hosting() && can
      ? h("input", {
          type: "checkbox",
          class: "peer-check",
          checked: selected.has(p.device_id),
          "aria-label": `Select ${p.name}`,
          onchange: (e) => (e.target.checked ? selected.add(p.device_id) : selected.delete(p.device_id), drawHead()),
        })
      : null;
    let action = null;
    if (mine) action = h("span", { class: "peer-member" }, icon("check", 14), "In your mesh");
    else if (hosting()) {
      if (p.invite_port === 0) action = h("span", { class: "small faint", title: "Its invite port is closed (firewall, or the port is in use)." }, "Cannot receive invites · send the link");
      else if (can)
        action = h(
          "button",
          { class: "btn btn-primary btn-sm", type: "button", onclick: (e) => invite([p.device_id], e.currentTarget) },
          icon("invite", 14),
          h("span", {}, sent ? "Invite again" : `Invite to ${store.status.hosting.mesh_name}`)
        );
    }
    return h(
      "article",
      { class: `peer-card${mine ? " is-member" : ""}${selected.has(p.device_id) ? " is-selected" : ""}`, "data-device": p.device_id },
      h(
        "div",
        { class: "peer-head" },
        check,
        h("span", { class: "radar-avatar peer-avatar" }, initials(p.name)),
        h(
          "div",
          { class: "stack-sm grow", style: { gap: "2px", minWidth: 0 } },
          h("span", { class: "node-name truncate", title: p.name }, p.name),
          h("span", { class: "small faint truncate" }, [OS[p.os] || p.os, p.address, `v${p.app_version}`].filter(Boolean).join(" · "))
        )
      ),
      h("div", { class: "row", style: { gap: "8px", minWidth: 0 } }, p.backend ? vendorBadge(p.backend, true) : null, h("span", { class: "small peer-gpu truncate", title: p.gpu || "" }, p.gpu || "No supported GPU · joins on CPU")),
      h("div", { class: "row wrap", style: { gap: "8px" } }, statusBadge(p), p.port_state ? h("span", { class: "small faint" }, PORT[p.port_state] || p.port_state) : null),
      outcome ? h("span", { class: `badge ${outcome[1]} peer-outcome`, title: sent.detail || "" }, outcome[0]) : null,
      outcome && sent.outcome === "failed" && sent.detail ? h("span", { class: "small faint" }, sent.detail) : null,
      action ? h("div", { class: "peer-action" }, action) : null
    );
  }

  function drawHead() {
    const peers = [...store.peers.values()];
    const open = peers.filter(invitable);
    const picked = open.filter((p) => selected.has(p.device_id));
    clear(
      head,
      h(
        "div",
        { class: "stack-sm", style: { gap: "2px" } },
        h("span", { class: "panel-title" }, `GradApps on this network${peers.length ? ` · ${peers.length}` : ""}`),
        h("span", { class: "small faint" }, hosting() ? `Invite them into ${store.status.hosting.mesh_name}. They get a notification and choose.` : "Every GradApp 2 that is open on this Wi-Fi or LAN, idle or not.")
      ),
      hosting()
        ? h(
            "div",
            { class: "row", style: { gap: "8px" } },
            open.length > 1
              ? h("button", { class: "btn btn-ghost btn-sm", type: "button", onclick: () => (picked.length === open.length ? selected.clear() : open.forEach((p) => selected.add(p.device_id)), draw()) }, picked.length === open.length ? "Clear selection" : "Select all")
              : null,
            picked.length
              ? h("button", { class: "btn btn-primary btn-sm", type: "button", onclick: (e) => invite(picked.map((p) => p.device_id), e.currentTarget) }, icon("invite", 14), h("span", {}, `Invite selected (${picked.length})`))
              : h("button", { class: "btn btn-primary btn-sm", type: "button", disabled: !open.length, onclick: (e) => invite(open.map((p) => p.device_id), e.currentTarget) }, icon("invite", 14), h("span", {}, open.length ? `Invite all (${open.length})` : "Invite all"))
          )
        : h("button", { class: "btn btn-sm", type: "button", onclick: (e) => busy(e.currentTarget, async () => (await api.hostStart(), await refreshStatus(), draw())) }, icon("host", 14), h("span", {}, "Host a mesh to invite them"))
    );
  }

  function drawNotes() {
    const info = store.peerInfo;
    const list = [];
    if (info && !info.running) list.push(h("div", { class: "notice small" }, "Looking for other GradApps…"));
    if (info?.running && !info.listening)
      list.push(h("div", { class: "notice notice-warn small" }, "GradApp cannot listen for other GradApps on this computer (UDP port 47800 is in use or blocked by the firewall). Meshes are still found by sweeping; allow GradApp in the firewall to see the others."));
    if (info?.running && !info.visible)
      list.push(h("div", { class: "notice small row-between wrap", style: { gap: "10px" } }, h("span", {}, "This computer is hidden from other GradApps and does not accept invites."), h("button", { class: "btn btn-sm", type: "button", onclick: () => go("settings") }, "Settings")));
    else if (info?.running && info.invite_port === 0)
      list.push(h("div", { class: "notice notice-warn small" }, "This computer cannot receive invites (its invite port could not be opened). Others can still send you their invite link."));
    clear(notes, list);
  }

  function drawAside() {
    const info = store.status?.hosting;
    const me = store.peerInfo?.me;
    clear(
      aside,
      h(
        "div",
        { class: "stack-sm" },
        h("strong", { class: "small" }, "Not seeing a machine?"),
        h("p", { class: "small faint" }, "It needs GradApp 2 open, on this same network, with “Visible to other GradApps” on. Other devices on the Wi-Fi (phones, TVs, machines without GradApp) cannot receive a nudge."),
        h("p", { class: "small faint" }, info ? "For those, send the invite link or show the QR code: it opens GradApp there, or its download page." : "Host a mesh to get an invite link and QR code for them.")
      ),
      info
        ? [
            h("div", { class: "qr-card peer-qr" }, qrSvg(info.invite_link)),
            h(
              "div",
              { class: "row wrap", style: { gap: "8px", justifyContent: "center" } },
              h("button", { class: "btn btn-sm", type: "button", onclick: () => copyText(info.invite_link, "Invite link copied") }, icon("copy", 14), h("span", {}, "Copy invite link")),
              h("button", { class: "btn btn-ghost btn-sm", type: "button", onclick: () => go("invite") }, "More")
            ),
          ]
        : null,
      me ? h("p", { class: "small faint" }, "This computer appears to others as ", h("strong", {}, me.name), store.peerInfo.visible ? "." : " (hidden).") : null
    );
  }

  function drawGrid() {
    // By name only: a card must never move under the pointer because its
    // status changed (found live: a peer started hosting and jumped to the top
    // between aiming and clicking "Invite").
    const peers = [...store.peers.values()].sort((a, b) => a.name.localeCompare(b.name) || a.device_id.localeCompare(b.device_id));
    for (const id of [...selected]) if (!store.peers.has(id)) selected.delete(id);
    clear(
      grid,
      peers.length
        ? peers.map(card)
        : h(
            "div",
            { class: "peer-empty" },
            h("span", { class: "radar-avatar peer-avatar is-empty" }, icon("wifi", 16)),
            h("p", { class: "small faint" }, store.peerInfo?.running === false ? "Starting…" : "No other GradApp is open on this network yet. They appear here within a few seconds of opening.")
          )
    );
  }

  function draw() {
    drawHead();
    drawNotes();
    drawGrid();
    drawAside();
  }

  draw();
  refreshPeers();
  const off = subscribe((r) => {
    if (r.has("peers") || r.has("status") || r.has("invites")) draw();
  });
  return off;
}

// ------------------------------------------------------- incoming invites

let dialog = null;

function openInvite(invite) {
  dialog?.remove();
  const close = () => {
    dialog?.remove();
    dialog = null;
    // Another one waiting?
    setTimeout(showNextInvite, 250);
  };
  const answer = async (action, btn) => {
    await busy(btn, async () => {
      const joined = await api.inviteRespond(invite.nonce, action);
      store.invites.delete(invite.nonce);
      if (action === "join") {
        await refreshStatus();
        toast(`Joining ${joined?.mesh_name || invite.mesh_name}.`, "ok");
        location.hash = "#/home";
      } else if (action === "block") toast(`Invites from ${invite.from_name} are blocked. Settings can undo this.`, "ok");
      close();
    });
  };
  const current = store.status?.joined?.mesh_name || (contributing() ? "another mesh" : null);
  const machines = invite.nodes_active ?? 0;
  const body = h(
    "div",
    { class: "modal-body" },
    h(
      "div",
      { class: "row", style: { gap: "16px", alignItems: "center" } },
      h("div", { class: "modal-orbit" }, icon("invite", 20)),
      h("div", { class: "stack-sm", style: { gap: "3px", minWidth: 0 } }, h("h2", { class: "modal-title", id: "peer-invite-title" }, `${invite.from_name} invites you`), h("span", { class: "small faint" }, "to contribute to the mesh it created, ", h("strong", { style: { color: "var(--text)" } }, invite.mesh_name)))
    ),
    h(
      "div",
      { class: "node-facts" },
      h("div", { class: "node-fact" }, h("span", {}, "From"), h("span", { class: "truncate" }, `${invite.from_name}${invite.from_address ? ` · ${invite.from_address}` : ""}`)),
      h("div", { class: "node-fact" }, h("span", {}, "Mesh"), h("span", { class: "truncate" }, invite.mesh_name)),
      h("div", { class: "node-fact" }, h("span", {}, "Machines"), h("span", {}, `${machines} online now`)),
      h("div", { class: "node-fact" }, h("span", {}, "Coordinator"), h("span", { class: "truncate mono" }, invite.coordinator.replace(/^https?:\/\//, "")))
    ),
    current ? h("div", { class: "notice notice-warn small" }, `This computer is contributing to ${current}. Joining moves it to ${invite.mesh_name}.`) : null,
    h("p", { class: "small faint" }, "Joining lends this computer's GPU to that mesh's training runs. Stop any time from Home or the tray. Only accept invites from people you know on this network.")
  );
  const foot = h(
    "div",
    { class: "modal-foot", style: { justifyContent: "space-between" } },
    h("button", { class: "ld-btn ld-btn-ghost peer-block", type: "button", onclick: (e) => answer("block", e.currentTarget) }, "Block this device"),
    h(
      "div",
      { class: "row", style: { gap: "10px" } },
      h("button", { class: "ld-btn ld-btn-ghost", type: "button", onclick: (e) => answer("decline", e.currentTarget) }, "Not now"),
      h("button", { class: "ld-btn ld-btn-solid", type: "button", onclick: (e) => answer("join", e.currentTarget) }, "Join", h("span", { class: "arrow" }, "→"))
    )
  );
  dialog = h("div", { class: "scrim peer-invite-scrim", "data-nonce": invite.nonce }, h("div", { class: "modal", role: "dialog", "aria-modal": "true", "aria-labelledby": "peer-invite-title" }, body, foot));
  document.body.append(dialog);
  dialog.querySelector(".ld-btn-solid")?.focus();
}

function showNextInvite() {
  if (document.hidden) return;
  if (dialog) {
    // The open invite was answered elsewhere or timed out.
    if (!store.invites.has(dialog.dataset.nonce)) {
      dialog.remove();
      dialog = null;
      toast("The invite expired before it was answered.");
    } else return;
  }
  const next = [...store.invites.values()].sort((a, b) => a.sent_at - b.sent_at)[0];
  if (next) openInvite(next);
}

/** Show each incoming invite as a dialog, whatever screen is open. */
export function watchInvites() {
  subscribe((r) => r.has("invites") && showNextInvite());
  setTimeout(showNextInvite, 0);
  // Opened from the notification or the tray: the invite is waiting on top.
  document.addEventListener("visibilitychange", () => !document.hidden && refreshPeers().then(showNextInvite));
  window.addEventListener("keydown", (e) => {
    if (e.key === "Escape" && dialog) dialog.querySelector(".modal-foot .ld-btn-ghost:not(.peer-block)")?.click();
  });
}
