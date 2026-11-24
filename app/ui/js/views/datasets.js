// Datasets: what the mesh can train on. They live on the host; each machine
// fetches only the images in its shard.

import { api } from "../api.js";
import { store, subscribe, connected, refreshRuns } from "../store.js";
import { h, clear, icon, emptyState } from "../ui.js";
import { notConnected } from "./mesh.js";

const STANDARD = [
  { name: "COCO8", images: 8, classes: 80, mb: 0.4, blurb: "Eight images. Proves the pipeline end to end in under a minute." },
  { name: "COCO128", images: 128, classes: 80, mb: 6.7, blurb: "The usual smoke test: enough to see mAP move." },
  { name: "VOC 2012 (subset)", images: 2500, classes: 20, mb: 412, blurb: "A real workload for timing a mixed-vendor mesh." },
];

export function renderDatasets(root, { go }) {
  const mine = h("div", { class: "dataset-cards" });
  const standard = h("div", { class: "dataset-cards" });

  function draw() {
    if (!connected()) return notConnected(root, "Datasets", go);
    const web = store.status.hosting?.web || store.status.joined?.web;
    if (!root.contains(mine)) {
      clear(
        root,
        h(
          "header",
          { class: "page-head" },
          h("div", {}, h("h1", { class: "page-title" }, "Datasets"), h("p", { class: "lead" }, "YOLO datasets stored on the host. Each machine downloads only the images in its shard, and only for the round it is training.")),
          web ? h("button", { class: "btn btn-sm", type: "button", onclick: () => api.openUrl(`${web}/dashboard/datasets`) }, icon("upload", 14), h("span", {}, "Add on the host")) : null
        ),
        h("section", { class: "panel" }, h("div", { class: "row-between", style: { marginBottom: "14px" } }, h("span", { class: "panel-title" }, "On this mesh")), mine),
        h("section", { class: "panel" }, h("div", { class: "stack-sm", style: { marginBottom: "14px" } }, h("span", { class: "panel-title" }, "Standard datasets"), h("span", { class: "small faint" }, "The host can fetch these from Ultralytics to check a mesh before training on your own data.")), standard)
      );
    }
    const list = store.datasets;
    clear(
      mine,
      !list
        ? h("p", { class: "small faint" }, "Loading…")
        : list.length
          ? list.map((d) =>
              h(
                "article",
                { class: "dataset-card stack-sm" },
                h("div", { class: "row-between" }, h("strong", { class: "truncate" }, d.name), d.is_default ? h("span", { class: "badge badge-accent" }, icon("star", 11), "default") : null),
                h("div", { class: "row", style: { gap: "18px" } }, h("div", { class: "stack-sm", style: { gap: 0 } }, h("span", { class: "mix-count mono", style: { fontSize: "1.3rem" } }, d.images.toLocaleString()), h("span", { class: "small faint" }, "training images")), h("div", { class: "stack-sm", style: { gap: 0 } }, h("span", { class: "mix-count mono", style: { fontSize: "1.3rem" } }, d.classes), h("span", { class: "small faint" }, "classes"))),
                h("span", { class: "small faint mono" }, d.size_mb != null ? `${d.size_mb >= 1000 ? `${(d.size_mb / 1024).toFixed(1)} GB` : `${d.size_mb} MB`} on the host` : "")
              )
            )
          : emptyState("No datasets yet", "Upload a YOLO .zip on the host, or import COCO128 there to try a run in a few minutes.")
    );
    const have = new Set((list || []).map((d) => d.name));
    clear(
      standard,
      STANDARD.map((s) =>
        h(
          "article",
          { class: "dataset-card stack-sm" },
          h("div", { class: "row-between" }, h("strong", {}, s.name), have.has(s.name) ? h("span", { class: "badge badge-live" }, "on this mesh") : h("span", { class: "badge" }, `${s.mb} MB`)),
          h("span", { class: "small muted" }, s.blurb),
          h("span", { class: "small faint mono" }, `${s.images.toLocaleString()} images · ${s.classes} classes`)
        )
      )
    );
  }

  draw();
  refreshRuns();
  return subscribe((r) => {
    if (r.has("runs") || r.has("status")) draw();
  });
}
