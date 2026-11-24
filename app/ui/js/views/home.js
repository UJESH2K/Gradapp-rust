// Home: the station scene, what this machine is doing, and the one thing to do next.

import { api } from "../api.js";
import { store, subscribe, contributing, hosting, connected, selectedPort, refreshStatus } from "../store.js";
import { h, clear, icon, busy, vendorBadge, vendorOf, backendFromVendor, fmtMemory, panel } from "../ui.js";
import { attachScene, CREDIT_URL } from "../scene.js";

const STATE_LABEL = { ready: "Ready", installing: "Installing", not_installed: "Not installed", stale: "Needs rebuild", failed: "Failed", blocked: "Blocked" };

function shortGpu(name) {
  return String(name || "")
    .replace(/^NVIDIA GeForce /, "")
    .replace(/^NVIDIA /, "")
    .replace(/^Intel\(R\) /, "")
    .replace(/ Laptop GPU$/, "");
}

function inviteName(link) {
  if (!link) return null;
  const m = /[?&]name=([^&]+)/.exec(link);
  if (m) return decodeURIComponent(m[1]);
  return link.replace(/^gradapp:\/\/join\?host=/, "").replace(/^https?:\/\//, "").split(/[&:/]/)[0];
}

export function renderHome(root, { go }) {
  const hud = h("div", { class: "ld-hud" });
  const title = h("h1", { class: "ld-title" });
  const lede = h("p", { class: "ld-lede" });
  const actions = h("div", { class: "ld-actions" });
  const telemetry = h("dl", { class: "ld-telemetry" });
  const credit = h("a", { class: "rig-note hidden", title: "Model licence: CC BY-NC 4.0", onclick: () => api.openUrl(CREDIT_URL) }, "“space boi” by silvercrow101 · CC BY-NC 4.0");

  const hero = h(
    "section",
    { class: "hero" },
    h("div", { class: "hero-backdrop", "aria-hidden": "true" }, h("div", { class: "ld-nebula" }), h("div", { class: "ld-canvas" }), h("div", { class: "ld-vignette" }), h("div", { class: "ld-grain" })),
    h("div", { class: "hero-copy" }, hud, title, lede, actions),
    h(
      "div",
      { class: "hero-stage", "aria-hidden": "true" },
      h("div", { class: "ld-annotation", style: { top: "8%", left: "6%" } }, h("span", { class: "vendor-dot vendor-nvidia" }), h("span", {}, h("b", {}, "orbit 1"), "nvidia · cuda")),
      h("div", { class: "ld-annotation", style: { top: "40%", right: "0%" } }, h("span", { class: "vendor-dot vendor-intel" }), h("span", {}, h("b", {}, "orbit 2"), "intel · xpu")),
      h("div", { class: "ld-annotation", style: { bottom: "6%", left: "2%" } }, h("span", { class: "vendor-dot vendor-apple" }), h("span", {}, h("b", {}, "orbit 3"), "apple · metal")),
      h("span", { class: "ld-corner ld-corner-tl" }),
      h("span", { class: "ld-corner ld-corner-br" })
    ),
    telemetry,
    credit
  );

  const deviceCard = h("div");
  const portCard = h("div");
  clear(root, hero, h("div", { class: "home-grid" }, deviceCard, portCard));

  const scene = attachScene(hero);
  hero.addEventListener("scene-source", (e) => credit.classList.toggle("hidden", e.detail !== "model"));

  function drawHero() {
    const status = store.status;
    const device = store.device;
    const port = selectedPort();
    const gpu = device?.profile?.gpu_name || device?.facts?.gpus?.[0]?.name;
    const meshName = status?.joined?.mesh_name || status?.hosting?.mesh_name || store.mesh?.mesh_name || "the mesh";
    const worker = status?.worker?.state;
    const nodes = (store.mesh?.nodes || []).filter((n) => n.active);
    const lastName = inviteName(store.settings?.last_invite);

    const mood = worker === "running" ? "contributing" : hosting() ? "hosting" : worker === "failed" ? "failed" : "idle";
    scene.setMood(mood, nodes.filter((n) => n.training).length / Math.max(1, nodes.length));

    const pulse = worker === "running" || hosting() ? "is-on" : worker === "starting" ? "is-busy" : "";
    clear(
      hud,
      h("span", { class: `ld-pulse ${pulse}` }),
      h("span", {}, connected() ? (worker === "starting" ? "joining" : "mesh online") : "no mesh"),
      h("span", { class: "ld-hud-sep" }, "/"),
      h("span", {}, device?.facts?.hostname || "this computer"),
      h("span", { class: "ld-hud-sep" }, "/"),
      h("span", {}, port ? `torch ${port.spec.torch}${port.spec.local_suffix}` : "probing")
    );

    let line1;
    let line2;
    let text;
    if (worker === "starting") {
      [line1, line2] = [`Joining ${meshName}.`, "Hold on a moment."];
      text = "Starting the worker on this machine's port and introducing it to the coordinator.";
    } else if (worker === "running") {
      [line1, line2] = [`Training with ${meshName}.`, `${nodes.length || 1} GPUs, one model.`];
      text = `This machine trains its share of every round and sends its weights back to be averaged. ${nodes.length > 1 ? `${nodes.length - 1} other machine${nodes.length === 2 ? " is" : "s are"} online.` : ""}`;
    } else if (hosting()) {
      [line1, line2] = [`${status.hosting.mesh_name} is live.`, "Invite the room."];
      text = "Machines on this network find this mesh on their own. Send the invite link to anyone whose GradApp does not.";
    } else {
      [line1, line2] = [gpu ? `${shortGpu(gpu)}, ready.` : "Every GPU in the room.", gpu ? "Lend it to the mesh." : "One model."];
      text = "Join a mesh on this network and this machine trains its share of each round on its own pinned runtime. Or host one and invite the rest.";
    }
    clear(title, line1, h("span", { class: "ld-title-glow" }, line2));
    lede.textContent = text;

    const blocked = device?.profile?.blocked;
    const items = [];
    if (worker === "running" || worker === "starting" || worker === "restarting") {
      items.push(h("button", { class: "ld-btn ld-btn-solid ld-btn-xl", type: "button", onclick: () => go("mesh") }, "Open the mesh", h("span", { class: "arrow", "aria-hidden": "true" }, "→")));
      items.push(h("button", { class: "ld-btn ld-btn-danger ld-btn-lg", type: "button", onclick: (e) => busy(e.currentTarget, async () => (await api.leave(), refreshStatus())) }, icon("stop", 15), "Stop contributing"));
    } else if (hosting()) {
      items.push(h("button", { class: "ld-btn ld-btn-solid ld-btn-xl", type: "button", onclick: () => go("invite") }, "Invite machines", h("span", { class: "arrow", "aria-hidden": "true" }, "→")));
      items.push(
        h("button", { class: "ld-btn ld-btn-ghost ld-btn-lg", type: "button", disabled: Boolean(blocked), onclick: (e) => busy(e.currentTarget, async () => (await api.join(status.hosting.coordinator), refreshStatus())) }, "Train on this machine too")
      );
    } else {
      const join = h(
        "button",
        {
          class: "ld-btn ld-btn-solid ld-btn-xl",
          type: "button",
          disabled: Boolean(blocked),
          title: blocked || undefined,
          onclick: (e) =>
            lastName
              ? busy(e.currentTarget, async () => (await api.join(store.settings.last_invite), refreshStatus()))
              : go("discover"),
        },
        lastName ? `Join ${lastName}` : "Join a mesh",
        h("span", { class: "arrow", "aria-hidden": "true" }, "→")
      );
      items.push(join);
      items.push(h("button", { class: "ld-btn ld-btn-ghost ld-btn-lg", type: "button", onclick: (e) => busy(e.currentTarget, async () => (await api.hostStart(), await refreshStatus(), go("invite"))) }, icon("host", 16), "Host a mesh"));
      if (lastName) items.push(h("button", { class: "btn btn-ghost btn-sm", type: "button", onclick: () => go("discover") }, "Find another"));
    }
    clear(actions, items);

    const backendInfo = port ? vendorOf(port.spec.backend) : null;
    const memory = device?.facts?.gpus?.find((g) => !g.integrated)?.memory_mb;
    clear(
      telemetry,
      h("div", {}, h("dt", {}, "gpu memory"), h("dd", {}, memory ? fmtMemory(memory) : device ? "shared" : "…")),
      h("div", {}, h("dt", {}, "backend"), h("dd", {}, backendInfo ? `${backendInfo.api}` : "…", port?.spec?.local_suffix ? h("small", {}, port.spec.local_suffix.replace("+", "")) : null)),
      h("div", {}, h("dt", {}, "port"), h("dd", { class: "mono" }, port ? port.spec.id : "…", port ? h("small", {}, STATE_LABEL[port.status.state] || port.status.state) : null)),
      h("div", {}, h("dt", {}, "mesh"), h("dd", {}, connected() ? `${nodes.length || "…"} online` : "none"))
    );
  }

  function drawDevice() {
    const device = store.device;
    if (!device) {
      clear(deviceCard, panel({ title: "This computer" }, h("p", { class: "small faint" }, "Looking at the hardware…")));
      return;
    }
    const { facts, profile } = device;
    const gpus = facts.gpus || [];
    const rows = gpus.length
      ? gpus.map((gpu) => {
          const backend = backendFromVendor(gpu.vendor);
          const used = profile.gpu_index === gpu.index && profile.backend !== "cpu";
          return h(
            "div",
            { class: "gpu-row" },
            h("span", { class: `gpu-glyph vendor-${vendorOf(backend).vendor}` }, icon("chip", 20)),
            h(
              "div",
              { class: "stack-sm", style: { gap: "2px" } },
              h("span", { class: "gpu-name truncate" }, gpu.name),
              h(
                "span",
                { class: "small faint mono truncate" },
                [gpu.memory_mb ? fmtMemory(gpu.memory_mb) : gpu.integrated ? "shared memory" : null, gpu.compute_capability ? `compute ${gpu.compute_capability}` : null, gpu.driver ? `driver ${gpu.driver}` : null, gpu.cores ? `${gpu.cores} cores` : null]
                  .filter(Boolean)
                  .join("  ·  ")
              )
            ),
            h("div", { class: "row", style: { gap: "6px" } }, used ? h("span", { class: "badge badge-live" }, "trains") : h("span", { class: "badge" }, "not used"), vendorBadge(backend, true))
          );
        })
      : [h("div", { class: "gpu-row" }, h("span", { class: "gpu-glyph" }, icon("chip", 20)), h("div", {}, h("span", { class: "gpu-name" }, "No supported GPU found"), h("p", { class: "small faint" }, "This machine can host a mesh and be measured, but it will not receive training work.")), vendorBadge("cpu", true))];
    clear(
      deviceCard,
      panel(
        { title: "This computer", sub: [facts.hostname, facts.os_name_pretty || facts.os, facts.arch].filter(Boolean).join(" · "), action: h("button", { class: "btn btn-ghost btn-sm", type: "button", onclick: () => go("device") }, "Details") },
        rows,
        h(
          "div",
          { class: "node-facts", style: { marginTop: "14px" } },
          h("div", { class: "node-fact" }, h("span", {}, "Processor"), h("span", { class: "truncate" }, facts.cpu_model || "unknown")),
          h("div", { class: "node-fact" }, h("span", {}, "Cores"), h("span", {}, facts.cpu_cores ?? "?")),
          h("div", { class: "node-fact" }, h("span", {}, "Memory"), h("span", {}, facts.ram_mb ? fmtMemory(facts.ram_mb) : "unknown")),
          h("div", { class: "node-fact" }, h("span", {}, "System"), h("span", { class: "truncate" }, `${facts.os} ${facts.os_version}`.trim()))
        )
      )
    );
  }

  function drawPort() {
    const port = selectedPort();
    const profile = store.device?.profile;
    if (!port || !profile) {
      clear(portCard, panel({ title: "Runtime" }, h("p", { class: "small faint" }, "Choosing a port…")));
      return;
    }
    const s = port.status.state;
    const installing = s === "installing";
    const pct = store.install?.port === port.spec.id ? store.install.percent : null;
    clear(
      portCard,
      panel(
        { title: "Runtime for this machine", sub: "The port GradApp picked: one pinned Python and PyTorch, private to GradApp" },
        h(
          "div",
          { class: "port-ticket" },
          h("div", { class: "row-between" }, h("span", { class: "port-id" }, port.spec.id), h("span", { class: `state-chip is-${s}` }, installing ? h("span", { class: "spinner", style: { width: "10px", height: "10px" } }) : h("span", { class: "dot" }), STATE_LABEL[s] || s)),
          h("div", { class: "row wrap", style: { gap: "6px" } }, vendorBadge(port.spec.backend), port.spec.reference ? h("span", { class: "badge badge-live" }, "reference stack") : h("span", { class: "badge badge-warn" }, "off reference"), h("span", { class: "badge mono" }, `python ${port.spec.python}`)),
          installing ? h("div", { class: "meter" }, h("div", { class: "meter-fill", style: { width: `${(pct ?? 0.04) * 100}%` } })) : null,
          h("p", { class: "small muted" }, profile.reason),
          profile.warnings?.length ? h("ul", { class: "warn-list" }, profile.warnings.map((w) => h("li", {}, w))) : null,
          profile.blocked ? h("div", { class: "notice notice-danger" }, h("strong", {}, profile.blocked), profile.fix ? h("div", { style: { marginTop: "4px" } }, profile.fix) : null) : null,
          h(
            "div",
            { class: "row" },
            s !== "ready" && !installing
              ? h("button", { class: "btn btn-primary btn-sm", type: "button", disabled: Boolean(profile.blocked), onclick: (e) => busy(e.currentTarget, () => api.installPort(port.spec.id)) }, icon("download", 14), h("span", {}, s === "stale" || s === "failed" ? "Rebuild" : "Install"))
              : null,
            h("button", { class: "btn btn-sm", type: "button", onclick: () => go("device") }, installing ? "Watch the install" : "All ports")
          )
        )
      )
    );
  }

  drawHero();
  drawDevice();
  drawPort();
  const off = subscribe((r) => {
    if (r.has("status") || r.has("mesh") || r.has("device") || r.has("settings") || r.has("install")) drawHero();
    if (r.has("device")) drawDevice();
    if (r.has("device") || r.has("install")) drawPort();
  });
  return () => {
    off();
    scene.detach();
  };
}
