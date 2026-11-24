// Discover & join: the v5 radar, showing meshes instead of devices. Radius is
// the measured /health round trip; angle is a hash of the mesh id, so a mesh
// keeps its seat between sweeps. Below it, joining by address or invite link.

import { api } from "../api.js";
import { store, subscribe, refreshStatus, contributing } from "../store.js";
import { h, clear, busy, icon, initials, angleFor, meter, toast } from "../ui.js";

const RING = { close: 0.3, nearby: 0.55, far: 0.78, unknown: 0.93 };
const RING_LABEL = { close: "Same link", nearby: "Nearby", far: "Distant", unknown: "Not measurable" };
const SOURCE = { beacon: "Announced itself", mdns: "Found by name (mDNS)", sweep: "Found by sweeping the network", manual: "Entered by hand" };

function proximity(ms) {
  if (ms == null) return "unknown";
  if (ms <= 5) return "close";
  if (ms <= 20) return "nearby";
  return "far";
}

function seatInitials(label) {
  const ip = /^(\d+\.){3}(\d+)$/.exec(label);
  return ip ? `.${ip[2]}` : initials(label);
}

/** Hash seats, then push apart any two closer than ~40 degrees so labels never collide. */
function seats(meshes) {
  const list = meshes.map((m) => ({ id: m.mesh_id, turn: angleFor(`${m.mesh_id}:${m.hostname}`) })).sort((a, b) => a.turn - b.turn);
  const gap = list.length > 1 ? Math.min(0.11, 1 / list.length) : 0;
  for (let pass = 0; pass < 4; pass += 1) {
    for (let i = 1; i < list.length; i += 1) if (list[i].turn - list[i - 1].turn < gap) list[i].turn = list[i - 1].turn + gap;
  }
  return new Map(list.map((s) => [s.id, s.turn % 1]));
}

function meshLabel(m) {
  return m.mesh_name || (m.local ? "Mesh on this computer" : m.hostname);
}

export function renderDiscover(root, { go }) {
  let selected = null;
  const radar = h("div", { class: "radar" });
  const side = h("div", { class: "radar-side" });
  const address = h("input", { class: "input mono", placeholder: "192.168.1.42, lab-pc.local or gradapp://join?…", spellcheck: false, autocomplete: "off", "aria-label": "Address or invite link" });
  const joinForm = h(
    "form",
    {
      class: "row",
      style: { gap: "10px" },
      onsubmit: (e) => {
        e.preventDefault();
        joinWith(address.value, e.submitter || joinForm.querySelector("button"));
      },
    },
    address,
    h("button", { class: "btn btn-primary", type: "submit" }, "Join")
  );

  if (store.pendingInvite) {
    address.value = store.pendingInvite;
    store.pendingInvite = null;
  }

  clear(
    root,
    h(
      "header",
      { class: "page-head" },
      h("div", {}, h("h1", { class: "page-title" }, "Discover & join"), h("p", { class: "lead" }, "Meshes on this Wi-Fi announce themselves. GradApp also asks by name and sweeps the network for hosts that cannot announce."))
    ),
    h("div", { class: "radar-wrap" }, radar, side),
    h(
      "section",
      { class: "panel" },
      h("div", { class: "stack-sm" }, h("span", { class: "panel-title" }, "Join with an address or invite link"), h("p", { class: "small faint" }, "What the host's GradApp shows on its Invite screen. A gradapp:// link carries the mesh token; a bare address asks the host for it."), joinForm)
    )
  );

  async function joinWith(invite, btn) {
    if (!invite.trim()) {
      address.focus();
      return;
    }
    await busy(btn, async () => {
      if (contributing()) await api.leave();
      const joined = await api.join(invite.trim());
      await refreshStatus();
      toast(`Joining ${joined.mesh_name || joined.hostname}.`, "ok");
      go("home");
    });
  }

  function drawRadar() {
    const meshes = [...store.meshes.values()];
    const scanning = store.scan.active;
    radar.classList.toggle("is-scanning", scanning || meshes.length === 0);
    const seat = seats(meshes);
    clear(
      radar,
      Object.keys(RING).map((ring) => h("div", { class: `radar-ring radar-ring-${ring}`, style: { inset: `${(1 - RING[ring]) * 50}%` } }, h("span", { class: "radar-ring-label" }, RING_LABEL[ring]))),
      h("div", { class: "radar-sweep" }),
      h("button", { class: "radar-core", type: "button", title: "This computer", onclick: () => ((selected = null), drawSide(), drawRadar()) }, h("span", { class: "radar-core-dot" }), h("span", { class: "radar-core-label" }, "This computer")),
      meshes.map((m) => {
        // Keep clear of the ring labels at the top and the core's label below centre.
        let turn = seat.get(m.mesh_id);
        const ring = proximity(m.latency_ms);
        if (Math.abs(turn - 0.75) < 0.07) turn = turn < 0.75 ? 0.68 : 0.82;
        if (ring === "close" && turn < 0.5) turn = turn < 0.25 ? 0.93 : 0.57; // upper half: the core label sits below
        const r = RING[ring] * 50;
        return h(
          "button",
          {
            class: `radar-node${m.local ? " is-local" : ""}${selected === m.mesh_id ? " is-selected" : ""}`,
            type: "button",
            style: { left: `${50 + Math.cos(turn * Math.PI * 2) * r}%`, top: `${50 + Math.sin(turn * Math.PI * 2) * r}%` },
            title: `${meshLabel(m)} · ${m.latency_ms != null ? `${m.latency_ms} ms` : "distance unknown"}`,
            onclick: () => ((selected = selected === m.mesh_id ? null : m.mesh_id), drawRadar(), drawSide()),
          },
          h("span", { class: "radar-avatar is-mesh" }, seatInitials(meshLabel(m))),
          h("span", { class: "radar-node-label" }, `${meshLabel(m)} · ${m.nodes_active}`)
        );
      }),
      meshes.length === 0 && !scanning ? h("div", { class: "radar-empty" }, h("p", { class: "small faint" }, "No mesh answered on this network.")) : null
    );
  }

  function drawSide() {
    const meshes = [...store.meshes.values()];
    const scan = store.scan;
    const active = selected ? store.meshes.get(selected) : null;
    const rescan = h("button", { class: "btn btn-sm", type: "button", disabled: scan.active, onclick: () => start(true) }, scan.active ? "Sweeping…" : "Sweep again");
    clear(
      side,
      h("div", { class: "row-between" }, h("span", { class: "panel-title" }, `${meshes.length} mesh${meshes.length === 1 ? "" : "es"} nearby`), rescan),
      scan.active ? h("div", { class: "stack-sm", style: { gap: "6px" } }, meter(scan.total ? scan.scanned / scan.total : 0.05), h("span", { class: "small faint mono" }, `${scan.scanned} of ${scan.total} addresses`)) : null,
      h(
        "div",
        { class: "radar-legend" },
        h("span", { class: "radar-key" }, h("i", { class: "radar-swatch", style: { background: "var(--accent)", borderColor: "var(--accent)" } }), "Mesh"),
        h("span", { class: "radar-key" }, h("i", { class: "radar-swatch is-member" }), "On this computer")
      ),
      active
        ? h(
            "div",
            { class: "radar-detail stack" },
            h("div", { class: "row-between" }, h("strong", { class: "truncate" }, meshLabel(active)), h("button", { class: "btn btn-ghost btn-sm", type: "button", onclick: () => ((selected = null), drawSide(), drawRadar()) }, "Close")),
            h(
              "div",
              { class: "node-facts" },
              h("div", { class: "node-fact" }, h("span", {}, "Host"), h("span", { class: "truncate" }, active.hostname)),
              h("div", { class: "node-fact" }, h("span", {}, "Distance"), h("span", {}, active.latency_ms != null ? `${active.latency_ms} ms` : "unknown")),
              h("div", { class: "node-fact" }, h("span", {}, "Machines"), h("span", {}, `${active.nodes_active} online`)),
              h("div", { class: "node-fact" }, h("span", {}, "GradMesh"), h("span", {}, `${active.version} · p${active.protocol}`))
            ),
            h("p", { class: "small faint" }, SOURCE[active.source] || active.source, active.torch_ready ? "." : ". The host is still installing PyTorch; runs start once it finishes."),
            active.token ? null : h("div", { class: "notice notice-warn small" }, "This host did not share its token. Ask for the invite link from its Invite screen and paste it below."),
            h(
              "button",
              {
                class: "ld-btn ld-btn-solid ld-btn-lg",
                type: "button",
                style: { justifyContent: "center" },
                disabled: !active.token,
                onclick: (e) => joinWith(`gradapp://join?host=${new URL(active.coordinator).hostname}&port=${new URL(active.coordinator).port || 8000}&token=${active.token}${active.mesh_name ? `&name=${encodeURIComponent(active.mesh_name)}` : ""}`, e.currentTarget),
              },
              `Join ${meshLabel(active)}`,
              h("span", { class: "arrow" }, "→")
            )
          )
        : h(
            "div",
            { class: "stack-sm" },
            meshes.map((m) =>
              h(
                "button",
                { class: "mesh-row", type: "button", style: { border: 0, background: "none", width: "100%", textAlign: "left", color: "inherit", padding: "9px 4px", gridTemplateColumns: "34px minmax(0,1fr) auto" }, onclick: () => ((selected = m.mesh_id), drawRadar(), drawSide()) },
                h("span", { class: "radar-avatar is-mesh", style: { width: "32px", height: "32px", fontSize: "0.68rem" } }, seatInitials(meshLabel(m))),
                h("span", { class: "stack-sm", style: { gap: "1px" } }, h("span", { class: "small truncate", style: { fontWeight: 560 } }, meshLabel(m)), h("span", { class: "small faint mono truncate" }, `${m.hostname} · ${m.nodes_active} online`)),
                h("span", { class: "small faint mono" }, m.latency_ms != null ? `${m.latency_ms} ms` : "–")
              )
            ),
            meshes.length ? null : h("p", { class: "small faint" }, "Rings are measured round trip to each host, which tracks link quality rather than metres. Both machines need to be on the same Wi-Fi or LAN.")
          )
    );
  }

  function start(fresh) {
    if (fresh) store.meshes.clear();
    store.scan = { scanned: 0, total: 254, active: true };
    drawRadar();
    drawSide();
    api.discoverStart().catch((e) => toast(e.message, "error"));
  }

  start(false);
  const off = subscribe((r) => {
    if (r.has("meshes")) (drawRadar(), drawSide());
  });
  return () => {
    off();
    api.discoverStop().catch(() => {});
  };
}
