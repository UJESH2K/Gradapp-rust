// The only module that talks to the backend.
//
// Inside the Tauri window: `window.__TAURI__.core.invoke` for commands and
// `event.listen("gm://event")` for the AppEvent stream (gm-types::AppEvent,
// tagged by `kind`). In a plain browser: a mock backend with the same names and
// shapes, so the interface can be developed and screenshotted without Rust.
//
// Mock knobs (query string, browser only):
//   ?mock=idle|joined|hosting   starting state (default idle)
//   ?update=modal|later|none    the "2.0.1 available" event after ~3 s (default modal)
//   ?install=1                  start a port install on load (to screenshot progress)
//   ?invite=1                   another GradApp on the LAN invites this one after ~3 s
//   ?theme=light|dark|system    Settings › Theme for this load (applied before first paint)

const TAURI = typeof window !== "undefined" ? window.__TAURI__ : undefined;
export const isTauri = Boolean(TAURI?.core?.invoke);

const listeners = new Set();
const channelListeners = new Map();

/** Subscribe to AppEvents. Returns an unsubscribe function. */
export function onEvent(fn) {
  listeners.add(fn);
  return () => listeners.delete(fn);
}

/** Subscribe to a side channel: "invite" (gradapp:// link) or "navigate" (screen id). */
export function onChannel(name, fn) {
  if (!channelListeners.has(name)) channelListeners.set(name, new Set());
  channelListeners.get(name).add(fn);
  return () => channelListeners.get(name).delete(fn);
}

function dispatch(event) {
  for (const fn of listeners) {
    try {
      fn(event);
    } catch (error) {
      console.error(error);
    }
  }
}

function dispatchChannel(name, payload) {
  for (const fn of channelListeners.get(name) || []) fn(payload);
}

let call;

if (isTauri) {
  const { invoke } = TAURI.core;
  TAURI.event.listen("gm://event", (e) => dispatch(e.payload));
  TAURI.event.listen("gm://invite", (e) => dispatchChannel("invite", e.payload));
  TAURI.event.listen("gm://navigate", (e) => dispatchChannel("navigate", e.payload));
  TAURI.event.listen("gm://transfer", (e) => dispatchChannel("transfer", e.payload));
  call = (cmd, args) =>
    invoke(cmd, args).catch((error) => {
      throw new Error(typeof error === "string" ? error : error?.message || String(error));
    });
} else {
  call = (await import("./mock.js")).createMock(dispatch, dispatchChannel);
}

export const api = {
  appStatus: () => call("app_status"),
  logs: () => call("logs"),
  getDevice: () => call("get_device"),
  getPorts: () => call("get_ports"),
  installPort: (portId) => call("install_port", { portId }),
  discoverStart: () => call("discover_start"),
  discoverStop: () => call("discover_stop"),
  join: (invite) => call("join", { invite }),
  leave: () => call("leave"),
  hostStart: (meshName) => call("host_start", { meshName: meshName || null }),
  hostStop: () => call("host_stop"),
  meshSnapshot: () => call("mesh_snapshot"),
  runs: () => call("runs"),
  datasets: () => call("datasets"),
  checkUpdate: () => call("check_update"),
  installUpdate: () => call("install_update"),
  restartApp: () => call("restart_app"),
  getSettings: () => call("get_settings"),
  setSettings: (settings) => call("set_settings", { settings }),
  openDataDir: () => call("open_data_dir"),
  openUrl: (url) => (isTauri ? call("open_url", { url }) : Promise.resolve(window.open(url, "_blank", "noopener"))),
  hideWindow: () => call("hide_window"),
  quitApp: () => call("quit_app"),
  // Datasets and runs (app/src-tauri/src/datasets.rs, runs.rs).
  datasetsOverview: () => call("datasets_overview"),
  datasetImport: (key, makeDefault) => call("dataset_import", { key, makeDefault }),
  datasetUpload: (path, name, makeDefault) => call("dataset_upload", { path, name, makeDefault }),
  datasetSetDefault: (id) => call("dataset_set_default", { id }),
  datasetDelete: (id) => call("dataset_delete", { id }),
  datasetDownload: (id, name, dir) => call("dataset_download", { id, name, dir }),
  pickDatasetZip: () => call("pick_dataset_zip"),
  pickFolder: (title) => call("pick_folder", { title: title || null }),
  transfers: () => call("transfers"),
  transferCancel: (id) => call("transfer_cancel", { id }),
  runOptions: () => call("run_options"),
  runCreate: (request) => call("run_create", { request }),
  runStop: (id) => call("run_stop", { id }),
  runDelete: (id) => call("run_delete", { id }),
  runLive: (id) => call("run_live", { id }),
  runDownloadWeights: (id, name, dir) => call("run_download_weights", { id, name, dir }),
  // GradApps on this network (peers.rs).
  peersState: () => call("peers_state"),
  peerInvite: (deviceIds) => call("peer_invite", { deviceIds }),
  inviteRespond: (nonce, action) => call("invite_respond", { nonce, action }),
  peersUnblockAll: () => call("peers_unblock_all"),
};
