# gm-ports development log

Newest first. Same format as the root `DEVLOG.md`.

---

## 2026-11-24 — gm-ports: port manifests, uv provisioning, locks (2.0.0-dev)

### What
- `ports/*.toml`: `cuda-cu130`, `cuda-cu126`, `cuda-cu128` (torch 2.11.0 /
  torchvision 0.26.0, off-reference), `xpu`, `mps`, `cpu`, `rocm`
  (experimental, linux-x86_64, `whl/rocm7.2`). All CPython 3.12; packages are
  the union of `requirements-control.txt` + `requirements-common.txt`; facts
  from the v1 requirement-file comments are in `notes`; `min_driver` from
  hardware.py (cu126 uses 528, the stricter Windows value, since PortSpec has
  one number; gm-probe applies 525 on Linux).
- `crates/gm-ports`:
  - `Registry`: manifests embedded with `include_str!`; `load_dir` /
    `GRADAPP_PORTS_DIR` override; `get`, `require`, `all`, `for_platform`,
    `validate` (reference pins = version.py, suffix ↔ index, platforms,
    duplicates, cuda min_driver, ...).
  - `requirements(spec)` exact pins (`torch==2.13.0+cu130`), `plan(spec,
    machine) -> PortLock` with a sha256 fingerprint over port, python, index,
    pins and machine; `machine_fingerprint(&DeviceFacts)` /
    `local_machine_fingerprint()`.
  - `uv`: find (GRADAPP_UV → bundled → `<data>/bin` → PATH), download the
    pinned release (`UV_VERSION = 0.11.32`) from GitHub with sha256 check,
    tar.gz/zip extraction.
  - `provision` / `provision_with`: private CPython (`uv python install
    --no-bin`, `UV_PYTHON_INSTALL_DIR=<root>/.python`,
    `UV_PYTHON_PREFERENCE=only-managed`, `UV_NO_CONFIG=1`), venv per port,
    `uv pip install --index-strategy unsafe-best-match`, verification by a Rust
    copy of `VERIFY_SNIPPET` (adds a CPU kernel and HIP build info), lock
    written last and atomically. Every output line goes to `on_progress` with a
    step and a monotonic percent; network failures retried 3× with backoff;
    wheelhouse via `--find-links`; children killed on drop.
  - `status` (Ready / Stale / NotInstalled) and `gc`.
- `docs/PORTS.md`.

### Why
- v1's recurring install failure was the machine's Python. A port pins the
  interpreter like it pins torch, and uv provisions it privately.
- One env per port removes v1's "uninstall torch before switching builds"
  dance: switching ports builds a new env, and `gc` removes the old one.
- Lock written last + deleted first means a crash, a cancel or a failed
  verification can never look Ready.

### Verified
- `cargo test -p gm-ports`: 33 tests green (manifests cross-checked against
  `desktop/engine/requirements-*.txt`, `version.py` and `hardware.py` at test
  time; fingerprints; status with fake envs; gc; verify interpretation; uv
  archive extraction). `cargo clippy -p gm-ports --all-targets -- -D warnings`
  clean.
- `--ignored` network tests on Linux x86_64 with uv 0.11.32:
  - `provision_end_to_end`: python 3.12.13 downloaded under the temp env root,
    `six` installed, lock → Ready, base prefix inside `<root>/.python`, Stale on
    other machine / changed pins, reuse, cancel mid-install → not Ready, bad pin
    → `Err` + NotInstalled, gc. Pass.
  - `download_pinned_uv`: GitHub download + sha256 + extract → `uv 0.11.32`. Pass.
  - `ports_resolve_without_conflicts`: `uv pip compile --only-binary :all:`
    for every port × platform (17 combinations; torch excluded except mps,
    which resolves torch 2.13.0 from PyPI for macOS 14 arm64), plus torch
    2.13.0/0.28.0 and 2.11.0/0.26.0 with the full set from PyPI. Pass.
  - `resolve_with_pytorch_indexes`: fails here only because
    download.pytorch.org is blocked by this sandbox's proxy (mps, PyPI-only,
    passes). Run it on a normal network.

### Next
- Run `resolve_with_pytorch_indexes` and a real `VerifyMode::Backend` install
  on NVIDIA/Intel/Apple/AMD hardware; confirm the `+cpu` wheels on
  windows-arm64 / linux-arm64 and the `rocm7.2` index.
- Wire into gmctl (`gmctl ports status|install|gc`) and the app.
