# app/ — GradApp 2 desktop shell and interface

## 2026-11-24 — Tauri 2 shell and the GradMesh v5 interface (2.0.0-dev)

### What
- **`app/src-tauri`** (crate `gradapp`, lib `gradapp_lib`): Tauri 2 with the
  updater, notification, single-instance, autostart, process and opener plugins.
  - `tauri.conf.json`: productName GradApp, identifier `com.ujesh.gradapp`,
    `frontendDist: ../ui`, window 1240×800 (min 980×640), dark, starts hidden
    and is shown by `setup` unless `--hidden` / "start hidden";
    `createUpdaterArtifacts: true`; updater endpoint
    `…/releases/latest/download/latest.json`, pubkey `REPLACE_WITH_TAURI_PUBKEY`.
  - `capabilities/default.json`: core events/window, notification, opener,
    process restart, updater, autostart.
  - `icons/`: generated from `desktop/assets/icon.png` with Pillow (PNG sizes,
    ICO, ICNS, Windows Square*Logo), plus the tray PNGs.
  - `src/commands.rs`: the window's whole API (see below). `src/backend.rs`:
    shared state and every call into the other crates, each marked
    `// INTEGRATE:` with a placeholder. `src/model.rs`: UI types not yet in
    gm-types. `src/tray.rs`: tray with Open / Start-Stop contributing / Quit,
    icon swaps to the active variant while contributing or hosting.
  - Close-to-tray (setting, default on), single instance forwards `gradapp://`
    links as `gm://invite`, auto-rejoin of the last invite, a quiet update check
    6 s after start (notification if the window is hidden).
  - Real, not stubbed: settings persistence (`<config>/GradApp/settings.json`),
    autostart toggle, open data folder, signed update check/download/install via
    `tauri-plugin-updater` (stable or beta feed), restart.
- **`app/ui`**: no build step. `index.html`, `css/tokens.css` +
  `css/dashboard.css` + `css/radar.css` (copied from Gradmesh-v5 verbatim; one
  marked breakpoint edit), `css/app.css` (shell, hero lifted from v5
  landing.css, ports, modal, palette, toasts). ES modules in `js/`:
  `api.js` (the only backend boundary), `mock.js` (browser-only backend),
  `store.js`, `app.js` (shell, routing, Ctrl/⌘K palette, update dialog),
  `scene.js` (v5 station.ts + SpaceBackdrop as a module), `ui.js`, and nine
  views: home, device (Device & ports), discover, mesh, runs, datasets, invite,
  activity, settings.
- Vendored: three.js 0.171.0 (`three.module.min.js`, `three.core.min.js`,
  `GLTFLoader.js`, `BufferGeometryUtils.js`, imports rewritten to relative
  paths), qrcode-generator (MIT), Inter variable font (OFL). Model
  `models/space-station.glb` + `CREDITS.md` (CC BY-NC 4.0); credit is shown in
  the hero corner and in Settings › About.

### Why
- The owner wants v5's look; reusing v5's own stylesheets keeps the tokens
  identical instead of approximated.
- One backend boundary (`api.js`) and one Rust integration file
  (`backend.rs`) let the other crates land without touching the interface.
- The scene is the costly part, so it is a singleton that renders only while
  the Home hero is attached, intersecting and the document visible; DPR ≤ 1.5;
  reduced motion holds it still; no WebGL ⇒ a CSS still and three.js is never
  loaded.
- `.mjs` is not served as JavaScript by every webview asset protocol (it broke
  the module graph in WebKitGTK), so the QR module ships as `.js`.

### Verified
- `cargo build -p gradapp` and `cargo test -p gradapp` (invite parsing) pass.
- Ran the debug binary under Xvfb (WebKitGTK 2.52): window, tray, real
  commands and the 3D scene render (`screenshots/15-…real-backend.png`).
- Browser preview via `python3 -m http.server` + Playwright/Chrome
  (swiftshader): every screen at 1240×800 plus the update flow (available,
  downloading, restart) and the palette, no console errors
  (`docs/screenshots/`). No-WebGL fallback and reduced motion checked;
  render loop measured at 0 frames while another screen is open.

### Next
- Wire the `// INTEGRATE:` calls in `backend.rs` (probe, ports, net,
  supervisor) and move `model.rs` types into gm-types.
- Replace the updater pubkey; move `docs/screenshots/` out of `frontendDist`
  before release (they would be bundled, ~7 MB).
