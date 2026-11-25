# Light theme

Settings › Appearance › Theme, and the light palette. Newest first. Same
format as the root `DEVLOG.md`.

---

## 2026-11-25 — Theme: Match system / Dark / Light

### Why
The owner needs screenshots of the app for the Testing chapter of a
university project report, which will be printed, probably in grayscale. The
app was dark-only: a dark page with blue glows wastes toner and prints as a
grey smudge. He asked for a light theme: a white page, black text, close to
black-and-white, and readable in grayscale.

### What
**Setting.** `Settings.theme` (`model.rs`) is `"system" | "dark" | "light"`,
with `"system"` as the default. `#[serde(default)]` means a settings.json
written before this change still loads (there is a unit test for that). The
Settings screen has a new **Appearance** panel with a three-way segmented
control: Match system, Dark, Light. The choice saves through the existing
`set_settings` path.

**No flash.** `js/theme-boot.js` is a classic script in `<head>` (the CSP
allows only `'self'` scripts, so it can't be inline). It sets
`data-theme="dark|light"` on `<html>` before the stylesheets paint. It reads a
copy of the choice from localStorage, because the backend answers later.
`store.js` calls `GradAppTheme.apply(settings.theme)` once settings load and
after each save. "Match system" listens to `prefers-color-scheme` and switches
while the app is open. Each change fires a `gradapp:theme` window event.

**Mock knob.** `?theme=light|dark|system` (browser only; documented in the
`api.js` header). It sets the theme before first paint, and the mock's
settings so that Settings shows the matching option.

**Palette.** Light overrides the tokens under `:root[data-theme="light"]` in
`tokens.css`:
- page `#ffffff`, panels `#fafafa` / `#f4f4f5`
- text `#0a0a0a`, dim text `#3f3f46`, faint text `#71717a`
- black-alpha hairlines
- accent = near-black, so primary buttons, toggles, the selected segment and
  progress fills are black with white text
- status colours are darker so they print: live `#047857`, warn `#b45309`,
  danger `#b91c1c`. They always sit next to a text label ("Ready",
  "admitted", "Not installed"…).

**Hard-coded colours → variables.** 286 colour literals across the six
stylesheets now use 183 `--t-<context>-<family>-<value>` variables. A
throwaway script generated them from each literal and the CSS context it sits
in:

| Context | Light value |
|---|---|
| shadow | faint black, or none for coloured glows |
| radial glow | transparent |
| gradient stop | flat (no gradients on light) |
| border | black-alpha |
| text | dark |
| fill | black-alpha, or white for the dark panels |

The `:root` block declares each variable with its original literal as the
value, so dark renders exactly as before. The light block sits underneath.

A short list of component overrides at the end of `app.css` handles the
rest:
- flat fills for the progress, shard and transfer bars, the radar core and
  the modal orbit
- the hero title gradient becomes flat
- the topbar is opaque white
- backdrop blur is off
- the selected segment is black

**Charts in grayscale.** Vendor shares step through lightness, not hue:
- NVIDIA `#27272a`, with white labels
- Intel `#71717a`, with white labels
- Apple `#d4d4d8`
- CPU `#e4e4e7`

A second machine of the same vendor is **hatched** rather than darkened
(`.is-alt`), so it stays apart in print. The Runs line charts
(`views/runs.js`) read `--chart-1/2`, `--chart-dot` and `--chart-area-*` in
place of fixed hex values. On light that gives a black mAP line, a grey loss
line and a flat 6% area fill.

**Station scene.** It is a night-sky render (additive glows, stars, dark
fog) and has no honest daylight version. On light the hero hides the
backdrop, the orbit annotations and the model credit, and becomes a plain
header panel. `scene.js` also stops rendering frames while the theme is light,
and starts again on switching back.

### Verified
- **Dark is unchanged.** Before/after Playwright captures of dark (old tree
  from `git archive HEAD`, new tree, fixed `Date.now`/`Math.random`,
  animations off):
  - Home, Device, Discover, Mesh, Runs, Datasets, Invite and Activity, plus
    the run detail with line charts: pixel-identical. The only difference is
    a ±1 dither level on two frames.
  - Settings differs only by the new Appearance panel.
- **Switching.** With the OS in dark, "Match system" gives dark. Emulating an
  OS switch to light flips the open app to light. Choosing Dark, then Light,
  applies at once and stores the choice in localStorage. On reload the
  cached choice applies before the backend answers.
- **Screenshots.** Light theme, 1240×800, Chrome with swiftshader, no console
  errors on any screen. They are in `docs/screenshots/light/`, with
  grayscale copies in `docs/screenshots/light/gray/`:
  - 01 home
  - 02 device & ports (port ready plus install log)
  - 03 discover (GradApps on this network)
  - 04 invite received
  - 05 mesh
  - 06 datasets (COCO8 imported, upload at 17%)
  - 07 new run form
  - 08 live run (per-device share bars)
  - 09 activity
  - 10 settings (Theme)
  - 11 update available
- **Rust gates.** `cargo build -p gradapp`, `cargo test --workspace`,
  `cargo fmt --all --check` and `cargo clippy --workspace --all-targets -D
  warnings` all pass.

### Not done
- The brand logo keeps its colour (it is an image).
- The QR code on Invite is dark-on-white in both themes, as before.
