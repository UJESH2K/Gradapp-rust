// One place for everything the screens show. The backend pushes AppEvents;
// the mesh snapshot is polled only while a screen that shows it is open and
// the window is visible, so a GradApp sitting in the tray costs nothing.

import { api, onEvent } from "./api.js";

const listeners = new Set();

export const store = {
  status: null,
  device: null,
  ports: null,
  settings: null,
  mesh: null,
  runs: null,
  datasets: null,
  logs: [],
  meshes: new Map(),
  scan: { scanned: 0, total: 0, active: false },
  install: null, // { port, step, percent, lines: [], done, failed }
  update: null,
  updateProgress: null, // { downloaded, total, done }
  view: "home",
};

export function subscribe(fn) {
  listeners.add(fn);
  return () => listeners.delete(fn);
}

const pending = new Set();
let scheduled = false;
export function emit(reason) {
  pending.add(reason);
  if (scheduled) return;
  scheduled = true;
  requestAnimationFrame(() => {
    scheduled = false;
    const reasons = new Set(pending);
    pending.clear();
    for (const fn of listeners) {
      try {
        fn(reasons);
      } catch (error) {
        console.error(error);
      }
    }
  });
}

export const contributing = () => ["starting", "running", "restarting"].includes(store.status?.worker?.state);
export const hosting = () => Boolean(store.status?.hosting);
export const connected = () => Boolean(store.status?.joined || store.status?.hosting);
export const selectedPort = () => store.ports?.find((p) => p.selected) || null;

export async function refreshStatus() {
  store.status = await api.appStatus();
  emit("status");
}

export async function refreshDevice() {
  const [device, ports] = await Promise.all([api.getDevice(), api.getPorts()]);
  store.device = device;
  store.ports = ports;
  emit("device");
}

export async function refreshMesh() {
  if (!connected()) {
    if (store.mesh) {
      store.mesh = null;
      emit("mesh");
    }
    return;
  }
  try {
    store.mesh = await api.meshSnapshot();
    emit("mesh");
  } catch (error) {
    console.warn(error);
  }
}

export async function refreshRuns() {
  [store.runs, store.datasets] = await Promise.all([api.runs(), api.datasets()]);
  emit("runs");
}

const LIVE_VIEWS = new Set(["home", "mesh", "runs"]);
let pollTimer = 0;
function schedulePoll() {
  clearTimeout(pollTimer);
  if (document.hidden) return;
  pollTimer = setTimeout(async () => {
    if (LIVE_VIEWS.has(store.view)) await refreshMesh();
    schedulePoll();
  }, 3000);
}

export function setView(view) {
  store.view = view;
  schedulePoll();
}

function pushLog(source, line) {
  store.logs.push({ at: Date.now() / 1000, source, line });
  if (store.logs.length > 2000) store.logs.splice(0, store.logs.length - 2000);
  emit("log");
}

function handle(event) {
  switch (event.kind) {
    case "mesh_found":
      // Upsert: discovery repeats a mesh when a better address or its token arrives.
      // A repeat never blanks what an earlier sighting knew (token, name).
      store.meshes.set(event.mesh.mesh_id, { ...(store.meshes.get(event.mesh.mesh_id) || {}), ...Object.fromEntries(Object.entries(event.mesh).filter(([, v]) => v != null)) });
      emit("meshes");
      break;
    case "scan_progress":
      store.scan = { scanned: event.scanned, total: event.total, active: event.scanned < event.total };
      emit("meshes");
      break;
    case "port_progress": {
      const install = store.install && store.install.port === event.port ? store.install : { port: event.port, lines: [] };
      install.step = event.step;
      install.percent = event.percent;
      if (event.line) install.lines.push(event.line);
      store.install = install;
      emit("install");
      break;
    }
    case "port_status":
      if (store.ports) {
        const entry = store.ports.find((p) => p.spec.id === event.status.port);
        if (entry) entry.status = event.status;
      }
      if (event.status.state === "installing") store.install = { port: event.status.port, lines: [], percent: 0, step: "start" };
      else if (store.install?.port === event.status.port) {
        store.install.done = event.status.state === "ready";
        store.install.failed = event.status.state === "failed";
      }
      emit("install");
      emit("device");
      break;
    case "proc":
      if (store.status) {
        if (event.status.role === "worker") store.status.worker = event.status;
        if (event.status.role === "coordinator") store.status.coordinator = event.status;
      }
      refreshStatus().then(refreshMesh);
      break;
    case "log":
      pushLog(event.role, event.line);
      break;
    case "mesh_event":
      if (store.mesh) {
        const events = store.mesh.events || (store.mesh.events = []);
        if (!events.some((e) => e.at === event.event.at && e.kind === event.event.kind)) {
          events.push(event.event);
          if (events.length > 200) events.splice(0, events.length - 200);
          emit("mesh");
        }
      }
      break;
    case "update":
      if (event.info?.available) {
        store.update = event.info;
        emit("update");
      }
      break;
    case "update_progress":
      store.updateProgress = { downloaded: event.downloaded, total: event.total };
      emit("update-progress");
      break;
    default:
      break;
  }
}

export async function boot() {
  onEvent((event) => {
    handle(event);
    if (event.kind === "notice") noticeHandlers.forEach((fn) => fn(event));
  });
  document.addEventListener("visibilitychange", () => {
    if (!document.hidden) refreshMesh().then(schedulePoll);
    else clearTimeout(pollTimer);
  });
  const [status, settings, logs] = await Promise.all([api.appStatus(), api.getSettings(), api.logs()]);
  store.status = status;
  store.settings = settings;
  store.logs = (logs || []).sort((a, b) => a.at - b.at);
  if (status.update?.available) store.update = status.update;
  emit("status");
  refreshDevice();
  refreshMesh();
  schedulePoll();
}

const noticeHandlers = new Set();
export function onNotice(fn) {
  noticeHandlers.add(fn);
}

export async function saveSettings(patch) {
  const next = await api.setSettings({ ...store.settings, ...patch });
  store.settings = next;
  emit("settings");
  return next;
}
