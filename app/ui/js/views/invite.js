// Invite: the link, address and token for other machines, plus a QR code for
// phones and the one-line join for machines without GradApp.

import { api } from "../api.js";
import { store, subscribe, hosting, refreshStatus } from "../store.js";
import { h, clear, icon, busy, copyLine, emptyState, copyText } from "../ui.js";
import qrcode from "../../vendor/qrcode.js";

/** QR as an SVG: one path, crisp at any size. qrcode-generator (MIT, Kazuhiko Arase). */
export function qrSvg(text) {
  const qr = qrcode(0, "M");
  qr.addData(text);
  qr.make();
  const n = qr.getModuleCount();
  const quiet = 2;
  let d = "";
  for (let r = 0; r < n; r += 1) for (let c = 0; c < n; c += 1) if (qr.isDark(r, c)) d += `M${c + quiet},${r + quiet}h1v1h-1z`;
  const size = n + quiet * 2;
  const el = h("svg", { svg: true, viewBox: `0 0 ${size} ${size}`, role: "img", "aria-label": "QR code of the invite link", "shape-rendering": "crispEdges" });
  el.innerHTML = `<path d="${d}" fill="#07080b"/>`;
  return el;
}

export function renderInvite(root, { go }) {
  function draw() {
    if (!hosting()) {
      clear(
        root,
        h("header", { class: "page-head" }, h("div", {}, h("h1", { class: "page-title" }, "Invite"), h("p", { class: "lead" }, "Machines join the mesh this computer hosts."))),
        h(
          "section",
          { class: "panel" },
          emptyState(
            "Host a mesh to invite machines",
            "This computer runs the coordinator; the others join it. Their GradApp usually finds it on its own.",
            h("button", { class: "btn btn-primary", type: "button", onclick: (e) => busy(e.currentTarget, async () => (await api.hostStart(), await refreshStatus())) }, icon("host", 15), h("span", {}, "Host a mesh")),
            h("button", { class: "btn", type: "button", onclick: () => go("discover") }, "Join one instead")
          )
        )
      );
      return;
    }
    const info = store.status.hosting;
    const host = info.coordinator.replace(/^https?:\/\//, "").replace(/:\d+$/, "");
    clear(
      root,
      h(
        "header",
        { class: "page-head" },
        h("div", {}, h("h1", { class: "page-title" }, `Invite to ${info.mesh_name}`), h("p", { class: "lead" }, "Anyone on this network can join. Their GradApp finds this mesh by itself; these are for when it does not.")),
        h("button", { class: "btn btn-danger btn-sm", type: "button", onclick: (e) => busy(e.currentTarget, async () => (await api.hostStop(), await refreshStatus())) }, icon("power", 14), h("span", {}, "Stop hosting"))
      ),
      h(
        "div",
        { class: "invite-layout" },
        h(
          "div",
          { class: "stack" },
          h(
            "section",
            { class: "panel stack" },
            h("div", { class: "stack-sm" }, h("span", { class: "panel-title" }, "With GradApp"), h("p", { class: "small faint" }, "Send the invite link. Opening it starts GradApp on the other machine, ready to join. Or type the address into its Discover & join screen.")),
            h("div", { class: "kv" }, h("span", {}, "Invite link"), copyLine(info.invite_link, { mask: true, label: "Invite link copied" })),
            h("div", { class: "kv" }, h("span", {}, "Address"), copyLine(host, { label: "Address copied" })),
            h("div", { class: "kv" }, h("span", {}, "Mesh token"), copyLine(info.token, { mask: true, label: "Token copied" }))
          ),
          h(
            "section",
            { class: "panel stack" },
            h("div", { class: "stack-sm" }, h("span", { class: "panel-title" }, "Without GradApp"), h("p", { class: "small faint" }, "One line in a terminal on the machine with the GPU. It installs into ~/.gradmesh/agent and nothing else.")),
            h("div", { class: "kv" }, h("span", {}, "Windows"), copyLine(`irm ${info.web}/join.ps1 | iex`, { label: "Command copied" })),
            h("div", { class: "kv" }, h("span", {}, "macOS, Linux"), copyLine(`curl -fsSL ${info.web}/join.sh | sh`, { label: "Command copied" })),
            h("div", { class: "kv" }, h("span", {}, "Join page"), copyLine(`${info.web}/join`, { label: "Link copied" }))
          ),
          h("div", { class: "notice" }, "The token lets a machine receive dataset shards. Anyone who can reach this computer's port 3000 can read it, so host only on networks you trust.")
        ),
        h(
          "section",
          { class: "panel stack", style: { alignItems: "center", textAlign: "center" } },
          h("span", { class: "panel-title" }, "Scan to join"),
          h("div", { class: "qr-card" }, qrSvg(info.invite_link)),
          h("p", { class: "small faint" }, "Opens the invite on a phone or a laptop with a camera. It carries the token, so treat it like a password."),
          h("button", { class: "btn btn-sm", type: "button", onclick: () => copyText(info.invite_link, "Invite link copied") }, icon("link", 14), h("span", {}, "Copy link"))
        )
      )
    );
  }
  draw();
  return subscribe((r) => r.has("status") && draw());
}
