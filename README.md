<p align="center"><img src="desktop/assets/logo.png" alt="GradApp" width="112" /></p>

# GradApp 2 (Rust)

**Every GPU on the WiFi, one training mesh.** GradApp joins NVIDIA, Intel Arc
and Apple Silicon machines (AMD experimental) into a GradMesh federated-training
mesh. Version 2 is a rewrite of the Electron app in Rust with Tauri 2: small
installers, native async networking, and **ports** — a pinned, private PyTorch
runtime per device family, so the machine's own Python can never conflict.

| | |
|---|---|
| What it does, feature by feature | [docs/FUNCTIONALITY.md](docs/FUNCTIONALITY.md) |
| How it's built | [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) |
| Ports (per-device runtimes) | [docs/PORTS.md](docs/PORTS.md) |
| Every change and why | [DEVLOG.md](DEVLOG.md) |
| GradApp 1 (Electron) docs | [docs/README-v1.md](docs/README-v1.md) |

```
Cargo.toml            workspace
crates/
  gm-types            shared contract (device facts, ports, mesh, events)
  gm-probe            hardware detection → which port this machine runs
  gm-ports            ports: uv + private CPython 3.12 + pinned torch, verify, lock
  gm-net              WiFi beacon, mDNS, /24 sweep, invites, coordinator client, join page
  gm-update           update check (GitHub Releases + Tauri latest.json)
  gm-supervisor       runs the Python worker / coordinator, owns the process tree
  gmctl               headless CLI over the same crates
app/
  src-tauri/          the desktop app (window, tray, commands, updater)
  ui/                 the interface (GradMesh v5 design, three.js, no build step)
ports/*.toml          one manifest per backend: cuda-cu130, cuda-cu126, cuda-cu128, xpu, mps, cpu, rocm
desktop/engine/       the GradMesh Python engine (worker, trainers, coordinator)
desktop/, website/    GradApp 1 (Electron) and its download page, kept for reference
```

## Build and run

```bash
cargo test                      # all crates (the app is excluded by default)
cargo run -p gmctl -- probe     # what this machine is and which port it gets
cargo run -p gmctl -- discover  # meshes on this network
cargo run -p gradapp            # the desktop app (needs the platform webview SDK)
```

Linux needs `libwebkit2gtk-4.1-dev libgtk-3-dev librsvg2-dev libayatana-appindicator3-dev`.
The UI also runs in a plain browser against a mock backend:
`python3 -m http.server -d app/ui` → http://localhost:8000.

## Repositories

- **github.com/UJESH2K/gradapp** — the home: releases, the update feed the
  installed apps check, and the website at gradapp.ujesh.in (Vercel, `website/`).
- **github.com/UJESH2K/Gradapp-rust** — the same code, mirrored. CI runs there;
  releases do not.

## Release

Push a tag `v2.x.y` (or `v2.x.y-beta.n`). `release-v2.yml` builds Windows,
macOS (universal) and Linux installers, signs the updater artifacts and
publishes `latest.json`; running apps then show **Update available**.
Requires the repository secrets `TAURI_SIGNING_PRIVATE_KEY` and
`TAURI_SIGNING_PRIVATE_KEY_PASSWORD`.

## Credits

3D station model: see `app/ui/models/CREDITS.md` (CC BY-NC, research use).
