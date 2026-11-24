// Datasets: what the mesh can train on. They live on the host; each machine
// fetches only the images in its shard. The host imports standard sets,
// uploads its own YOLO .zip (streamed from disk by the app, with progress),
// picks the default, deletes, and downloads; a contributor sees the same list
// read-only (and may download).

import { api, onEvent } from "../api.js";
import { store, subscribe, connected, refreshRuns } from "../store.js";
import { h, clear, icon, emptyState, toast, busy, fmtBytes } from "../ui.js";
import { notConnected } from "./mesh.js";
import { transferList, onTransfer, activeUpload, confirmDialog, downloadTo } from "../transfers.js";

const SOURCE = { standard: "standard", upload: "uploaded", subset: "subset" };

export function renderDatasets(root, { go }) {
  let overview = null;
  let error = null;
  let picked = null; // { path, name, bytes } waiting for "Upload"
  let pollTimer = 0;
  const mine = h("div", { class: "dataset-cards" });
  const standard = h("div", { class: "dataset-cards" });
  const uploadBox = h("div", { class: "stack" });
  const head = h("div");
  const [transfers, offTransfers] = transferList(() => true);
  let built = false;

  async function load() {
    if (!connected()) return;
    try {
      overview = await api.datasetsOverview();
      error = null;
    } catch (e) {
      error = e.message;
    }
    draw();
    clearTimeout(pollTimer);
    // While an import runs on the host, follow its message every second.
    if (overview?.import?.running) pollTimer = setTimeout(load, 1000);
  }

  const canManage = () => Boolean(overview?.can_manage);

  async function chooseZip(btn) {
    await busy(btn, async () => {
      const file = await api.pickDatasetZip();
      if (!file) return;
      picked = { ...file, makeDefault: !(overview?.datasets || []).length };
      drawUpload();
    });
  }

  function drawUpload() {
    uploadBox.style.display = canManage() && picked ? "" : "none";
    if (!canManage()) return clear(uploadBox);
    const running = activeUpload();
    if (!picked) return clear(uploadBox);
    const nameInput = h("input", { class: "input", value: picked.name, maxlength: 80, "aria-label": "Dataset name", oninput: (e) => (picked.name = e.target.value) });
    const send = h("button", { class: "btn btn-primary", type: "button", disabled: Boolean(running) }, icon("upload", 15), h("span", {}, "Upload to the host"));
    send.addEventListener("click", () =>
      busy(send, async () => {
        await api.datasetUpload(picked.path, nameInput.value.trim() || picked.name, picked.makeDefault);
        picked = null;
        drawUpload();
      })
    );
    clear(
      uploadBox,
      h(
        "section",
        { class: "panel upload-panel" },
        h("div", { class: "row-between", style: { marginBottom: "12px" } }, h("span", { class: "panel-title" }, "Upload a dataset"), h("button", { class: "btn btn-sm btn-ghost", type: "button", onclick: () => ((picked = null), drawUpload()) }, icon("x", 14), h("span", {}, "Cancel"))),
        h(
          "div",
          { class: "upload-form" },
          h("div", { class: "field" }, h("span", { class: "label" }, "File"), h("span", { class: "mono small truncate selectable", title: picked.path }, picked.path), h("span", { class: "hint" }, `${fmtBytes(picked.bytes)}. Sent straight from disk; the window never loads it.`)),
          h("label", { class: "field" }, h("span", { class: "label" }, "Name on the mesh"), nameInput),
          h("label", { class: "row small", style: { gap: "8px" } }, h("input", { type: "checkbox", checked: picked.makeDefault, onchange: (e) => (picked.makeDefault = e.target.checked) }), "Make it the default for new runs")
        ),
        h("div", { class: "row", style: { marginTop: "14px", gap: "10px" } }, send, running ? h("span", { class: "small faint" }, "Another upload is still running.") : null)
      )
    );
  }

  function card(d) {
    const manage = canManage();
    const actions = [];
    if (manage && !d.is_default)
      actions.push(h("button", { class: "btn btn-sm", type: "button", title: "New runs use this dataset unless the form says otherwise", onclick: (e) => busy(e.currentTarget, async () => (await api.datasetSetDefault(d.id), toast(`${d.name} is the default`, "ok"), load())) }, icon("star", 13), h("span", {}, "Make default")));
    if (d.available)
      actions.push(
        h(
          "button",
          { class: "btn btn-sm", type: "button", title: "Save this dataset as a .zip", onclick: (e) => busy(e.currentTarget, () => downloadTo(`Save ${d.name} to…`, (dir) => api.datasetDownload(d.id, d.name, dir))) },
          icon("download", 13),
          h("span", {}, "Download")
        )
      );
    if (manage)
      actions.push(
        h(
          "button",
          {
            class: "btn btn-sm btn-ghost danger-text",
            type: "button",
            title: "Delete from the host",
            onclick: (e) =>
              busy(e.currentTarget, async () => {
                const ok = await confirmDialog({
                  title: `Delete ${d.name}?`,
                  body: d.source === "upload" ? "The uploaded archive and its unpacked images are removed from the host. Finished runs keep their results." : "It is removed from this mesh's list. Finished runs keep their results.",
                  confirm: "Delete",
                  danger: true,
                });
                if (!ok) return;
                await api.datasetDelete(d.id);
                toast(`${d.name} deleted`, "ok");
                load();
                refreshRuns();
              }),
          },
          icon("trash", 13),
          h("span", {}, "Delete")
        )
      );
    return h(
      "article",
      { class: `dataset-card stack-sm${d.is_default ? " is-default" : ""}`, "data-dataset": d.id },
      h(
        "div",
        { class: "row-between", style: { gap: "8px" } },
        h("strong", { class: "truncate", title: d.name }, d.name),
        h("span", { class: "row", style: { gap: "6px" } }, h("span", { class: "badge" }, SOURCE[d.source] || d.source), d.is_default ? h("span", { class: "badge badge-accent" }, icon("star", 11), "default") : null)
      ),
      h(
        "div",
        { class: "row", style: { gap: "18px" } },
        h("div", { class: "stack-sm", style: { gap: 0 } }, h("span", { class: "mix-count mono", style: { fontSize: "1.3rem" } }, d.images.toLocaleString()), h("span", { class: "small faint" }, "training images")),
        h("div", { class: "stack-sm", style: { gap: 0 } }, h("span", { class: "mix-count mono", style: { fontSize: "1.3rem" } }, d.val_images.toLocaleString()), h("span", { class: "small faint" }, "validation")),
        h("div", { class: "stack-sm", style: { gap: 0 } }, h("span", { class: "mix-count mono", style: { fontSize: "1.3rem" } }, d.classes), h("span", { class: "small faint" }, "classes"))
      ),
      d.class_names.length ? h("span", { class: "small faint truncate", title: d.class_names.join(", ") }, d.class_names.join(", ") + (d.classes > d.class_names.length ? ` +${d.classes - d.class_names.length}` : "")) : null,
      !d.available ? h("div", { class: "notice notice-warn small" }, "Its files are no longer on the host. Upload or import it again.") : null,
      h("span", { class: "small faint mono" }, d.size_mb != null ? `${d.size_mb >= 1000 ? `${(d.size_mb / 1024).toFixed(1)} GB` : `${d.size_mb} MB`} archive on the host` : "stored where it was imported, on the host"),
      actions.length ? h("div", { class: "row wrap", style: { gap: "6px", marginTop: "4px" } }, actions) : null
    );
  }

  function catalogueCard(entry) {
    const imp = overview.import || {};
    const importing = imp.running && imp.key === entry.key;
    const manage = canManage();
    const blocked = overview.torch_ready === false;
    let action = null;
    if (entry.on_mesh) action = h("span", { class: "badge badge-live" }, "on this mesh");
    else if (manage)
      action = h(
        "button",
        {
          class: `btn btn-sm${importing ? "" : " btn-primary"}`,
          type: "button",
          disabled: Boolean(imp.running) || blocked,
          title: blocked ? "The host needs PyTorch before it can import" : `Download ${entry.name} on the host (${entry.download_mb} MB)`,
          onclick: (e) =>
            busy(e.currentTarget, async () => {
              await api.datasetImport(entry.key, !(overview.datasets || []).length);
              load();
            }),
        },
        importing ? h("span", { class: "spinner" }) : icon("download", 13),
        h("span", {}, importing ? "Importing" : "Import")
      );
    else action = h("span", { class: "badge" }, `${entry.download_mb} MB`);
    return h(
      "article",
      { class: `dataset-card stack-sm${importing ? " is-importing" : ""}`, "data-standard": entry.key },
      h("div", { class: "row-between" }, h("strong", {}, entry.name), action),
      h("span", { class: "small muted" }, entry.blurb),
      h("span", { class: "small faint mono" }, `${entry.images.toLocaleString()} images · ${entry.classes} classes · ${entry.download_mb >= 1000 ? `${(entry.download_mb / 1000).toFixed(1)} GB` : `${entry.download_mb} MB`}${entry.good_for ? ` · ${entry.good_for}` : ""}`),
      importing ? h("div", { class: "stack-sm", style: { gap: "6px" } }, h("div", { class: "transfer-track" }, h("div", { class: "transfer-fill is-processing", style: { width: "100%" } })), h("span", { class: "small accent" }, imp.message || "Starting")) : null
    );
  }

  function draw() {
    if (!connected()) return notConnected(root, "Datasets", go);
    if (!built || !root.contains(mine)) {
      built = true;
      clear(
        root,
        head,
        transfers,
        uploadBox,
        h("section", { class: "panel" }, h("div", { class: "row-between", style: { marginBottom: "14px" } }, h("span", { class: "panel-title" }, "On this mesh")), mine),
        h("section", { class: "panel" }, h("div", { class: "stack-sm", style: { marginBottom: "14px" } }, h("span", { class: "panel-title" }, "Standard datasets"), h("span", { class: "small faint", id: "std-sub" }, "")), standard)
      );
    }
    const manage = canManage();
    clear(
      head,
      h(
        "header",
        { class: "page-head" },
        h("div", {}, h("h1", { class: "page-title" }, "Datasets"), h("p", { class: "lead" }, "YOLO datasets stored on the host. Each machine downloads only the images in its shard, and only for the round it is training.")),
        manage
          ? h("button", { class: "btn btn-primary", type: "button", onclick: (e) => chooseZip(e.currentTarget) }, icon("upload", 15), h("span", {}, "Upload a .zip"))
          : overview
            ? h("span", { class: "badge" }, icon("eye", 12), "Read-only: the host manages datasets")
            : null
      ),
      error ? h("div", { class: "notice notice-danger" }, error) : null,
      overview?.import?.error && !overview.import.running ? h("div", { class: "notice notice-danger" }, `The last import failed: ${overview.import.error}`) : null
    );
    const list = overview?.datasets;
    clear(
      mine,
      !overview
        ? h("p", { class: "small faint" }, "Loading…")
        : list.length
          ? list.map(card)
          : emptyState(
              "No datasets yet",
              manage ? "Upload a YOLO .zip (images/ and labels/ folders plus a data.yaml), or import COCO8 below to check the mesh in a minute." : "The host has not added a dataset yet. It appears here as soon as it does."
            )
    );
    const sub = root.querySelector("#std-sub");
    if (sub)
      sub.textContent = !overview
        ? ""
        : overview.torch_ready === false
          ? "The host fetches these with Ultralytics, which needs its PyTorch install to finish first."
          : manage
            ? "The host downloads these from Ultralytics. Start with COCO8 to check the mesh end to end."
            : "The host can fetch these from Ultralytics.";
    clear(standard, overview ? (overview.catalogue.length ? overview.catalogue.map(catalogueCard) : h("p", { class: "small faint" }, "This host's GradMesh has no catalogue.")) : null);
    drawUpload();
  }

  draw();
  load();
  const offStore = subscribe((r) => {
    if (r.has("status")) {
      if (connected() && !overview) load();
      else draw();
    }
  });
  const offEvents = onEvent((e) => {
    if (e.kind === "mesh_event" && e.event.kind.startsWith("dataset.")) load();
  });
  const offT = onTransfer((t) => {
    if (t && t.kind === "upload" && ["done", "failed", "cancelled"].includes(t.state)) load();
    drawUpload();
  });
  return () => {
    clearTimeout(pollTimer);
    offStore();
    offEvents();
    offT();
    offTransfers();
  };
}
