// Settings: saved by the backend in <config>/GradApp/settings.json.

import { api, isTauri } from "../api.js";
import { store, saveSettings, refreshDevice } from "../store.js";
import { h, clear, icon, busy, toggle, segmented, select, toast } from "../ui.js";
import { CREDIT_URL } from "../scene.js";

export function renderSettings(root, { checkForUpdates }) {
  const s = store.settings || {};
  const save = (patch, note) =>
    saveSettings(patch)
      .then(() => note && toast(note, "ok"))
      .catch((e) => toast(e.message, "error"));

  const line = (label, hint, control) => h("div", { class: "setting" }, h("div", { class: "setting-text" }, h("span", {}, label), hint ? h("span", { class: "hint" }, hint) : null), control);
  const group = (title, sub, ...rows) => h("section", { class: "panel" }, h("div", { class: "stack-sm", style: { gap: "2px", marginBottom: "4px" } }, h("span", { class: "panel-title" }, title), sub ? h("span", { class: "small faint" }, sub) : null), rows);

  const name = h("input", { class: "input", value: s.device_name || "", placeholder: store.device?.facts?.hostname || "This computer", maxlength: 60, style: { maxWidth: "260px" }, onchange: (e) => save({ device_name: e.target.value.trim() }, "Name saved. The mesh sees it the next time this computer joins.") });
  const backend = select(
    [
      ["auto", "Automatic"],
      ["cuda", "NVIDIA · CUDA"],
      ["xpu", "Intel Arc · XPU"],
      ["mps", "Apple Silicon · Metal"],
      ["rocm", "AMD · ROCm (experimental)"],
      ["cpu", "CPU only"],
    ],
    s.preferred_backend || "auto",
    async (value) => {
      await save({ preferred_backend: value === "auto" ? null : value }, value === "auto" ? "Back to automatic." : "Saved. Takes effect the next time this computer joins.");
      refreshDevice();
    }
  );
  backend.style.maxWidth = "260px";

  const version = store.status?.version || "2.0.0";
  clear(
    root,
    h("header", { class: "page-head" }, h("div", {}, h("h1", { class: "page-title" }, "Settings"), h("p", { class: "lead" }, "Changes save as you make them."))),
    h(
      "div",
      { class: "settings-grid" },
      h(
        "div",
        { class: "stack" },
        group(
          "This computer",
          null,
          line("Name in the mesh", "What other machines see in their list.", name),
          line("Preferred backend", "Leave on automatic unless support asks you to change it. A forced backend that cannot run here is shown as blocked on Device & ports.", backend),
          line(
            "Visible to other GradApps on this network",
            "Other GradApps list this computer on Discover and can invite it into their mesh. Nothing joins without your OK. Never shares a mesh token.",
            toggle(s.visible_on_lan !== false, (v) => save({ visible_on_lan: v }, v ? "Visible to other GradApps on this network." : "Hidden. Other GradApps no longer list or invite this computer."), "Visible to other GradApps")
          ),
          s.blocked_peers?.length
            ? line(
                "Blocked devices",
                `${s.blocked_peers.length} device${s.blocked_peers.length === 1 ? "" : "s"} cannot invite this computer.`,
                h("button", { class: "btn btn-sm", type: "button", onclick: (e) => busy(e.currentTarget, async () => (await api.peersUnblockAll(), (store.settings = await api.getSettings()), toast("Unblocked.", "ok"), renderSettings(root, { checkForUpdates }))) }, h("span", {}, "Unblock all"))
              )
            : null
        ),
        group(
          "Running in the background",
          "GradApp lives in the tray; closing the window does not stop it.",
          line("Open at login", null, toggle(s.start_at_login, (v) => save({ start_at_login: v }), "Open at login")),
          line("Start hidden", "Open straight to the tray, without a window.", toggle(s.start_hidden, (v) => save({ start_hidden: v }), "Start hidden")),
          line("Keep running when the window closes", "Contributing carries on from the tray.", toggle(s.close_to_tray, (v) => save({ close_to_tray: v }), "Close to tray")),
          line("Rejoin the last mesh on start", s.last_invite ? "Uses the last invite this computer joined with." : "Join a mesh once to use this.", toggle(s.auto_rejoin, (v) => save({ auto_rejoin: v }), "Auto rejoin")),
          line("Notifications", "Only when something needs you, and never while GradApp is in front.", toggle(s.notifications, (v) => save({ notifications: v }), "Notifications"))
        )
      ),
      h(
        "div",
        { class: "stack" },
        group(
          "Appearance",
          null,
          line(
            "Theme",
            "Match system follows your computer's light or dark setting as it changes.",
            segmented(
              [
                ["system", "Match system"],
                ["dark", "Dark"],
                ["light", "Light"],
              ],
              ["dark", "light"].includes(s.theme) ? s.theme : "system",
              (v) => save({ theme: v })
            )
          )
        ),
        group(
          "Updates",
          `You have GradApp ${version}.`,
          line("Channel", "Beta gets new builds about a week earlier.", segmented([["stable", "Stable"], ["beta", "Beta"]], s.update_channel || "stable", (v) => save({ update_channel: v }, v === "beta" ? "Beta builds from now on." : "Stable builds only."))),
          line("Check now", null, h("button", { class: "btn btn-sm", type: "button", onclick: (e) => busy(e.currentTarget, checkForUpdates) }, icon("refresh", 14), h("span", {}, "Check for updates")))
        ),
        group(
          "Data",
          "Ports, logs and the mesh's datasets when this computer hosts.",
          h("div", { class: "stack-sm", style: { padding: "12px 0" } }, h("div", { class: "copy-line" }, h("code", {}, s.data_dir || "–"), h("button", { class: "btn btn-sm", type: "button", onclick: () => api.openDataDir().catch((e) => toast(e.message, "error")) }, icon("folder", 14), h("span", {}, "Open"))))
        ),
        group(
          "About",
          null,
          h(
            "div",
            { class: "stack-sm small muted", style: { padding: "12px 0" } },
            h("span", {}, `GradApp ${version} · engine 5.1.1 · mesh protocol 5`),
            h("span", {}, "3D scene: “space boi” by silvercrow101, ", h("a", { class: "accent", href: "#", onclick: (e) => (e.preventDefault(), api.openUrl(CREDIT_URL)) }, "CC BY-NC 4.0"), ", used for research."),
            h("span", { class: "faint" }, "three.js (MIT) · qrcode-generator (MIT) · Inter (OFL)")
          ),
          isTauri ? line("Quit GradApp", "Stops contributing and hosting.", h("button", { class: "btn btn-danger btn-sm", type: "button", onclick: () => api.quitApp() }, icon("power", 14), h("span", {}, "Quit"))) : null
        )
      )
    )
  );
}
