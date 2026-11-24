# Ports

A **port** is one pinned, self-contained PyTorch runtime for one backend
family. Each port names the exact CPython it runs on; GradApp downloads that
interpreter privately with [uv](https://github.com/astral-sh/uv) and builds the
port's environment from it. The machine's own Python is never used.

GradApp 1 built its venv from whatever Python the machine had. PyTorch only
publishes wheels for some Python versions on some platforms, so installs broke
on 3.9, 3.14, Rosetta builds, Store Pythons and venvs synced between machines.
With ports the interpreter is part of the pinned set, like torch itself.

Code: `crates/gm-ports`. Manifests: `ports/*.toml`, compiled into the app.

## The ports

| id | backend | torch / torchvision | index | platforms | notes |
|---|---|---|---|---|---|
| `cuda-cu130` | cuda | 2.13.0 / 0.28.0 `+cu130` | `whl/cu130` | windows-x86_64, linux-x86_64, linux-arm64 | Default NVIDIA. Turing → Blackwell (cc 7.5–12.x), driver ≥ R580 |
| `cuda-cu126` | cuda | 2.13.0 / 0.28.0 `+cu126` | `whl/cu126` | windows-x86_64, linux-x86_64 | Maxwell → Hopper (cc 5.0–9.0), older drivers. **No Blackwell.** Driver ≥ R528 Windows / R525 Linux |
| `cuda-cu128` | cuda | **2.11.0 / 0.26.0** `+cu128` | `whl/cu128` | windows-x86_64, linux-x86_64 | Fallback for Blackwell on R570–R579. Off the reference stack |
| `xpu` | xpu | 2.13.0 / 0.28.0 `+xpu` | `whl/xpu` | windows-x86_64, linux-x86_64 | Intel Arc / Core Ultra. Graphics driver only, no oneAPI |
| `mps` | mps | 2.13.0 / 0.28.0 | PyPI | macos-arm64 | Apple Silicon, macOS 14+, native (never Rosetta) |
| `cpu` | cpu | 2.13.0 / 0.28.0 `+cpu` | `whl/cpu` | windows-x86_64, windows-arm64, linux-x86_64, linux-arm64 | Coordinator-only hosts, no usable GPU |
| `rocm` | rocm | 2.13.0 / 0.28.0 `+rocm7.2` | `whl/rocm7.2` | linux-x86_64 | **Experimental**, off the reference stack |

Every port runs CPython **3.12** and installs the same non-torch set: the
union of `desktop/engine/requirements-control.txt` and
`requirements-common.txt`, specifiers kept as written.

## Manifest schema

One file per port, `ports/<id>.toml`; the file name must equal `id`. The
fields are `gm_types::PortSpec`:

```toml
id = "cuda-cu130"              # fixed; gm-probe emits these ids
backend = "cuda"               # cuda | xpu | mps | rocm | cpu; the id starts with it
label = "PyTorch CUDA 13.0 build"
python = "3.12"                # "3.N", N in 10..=13 (PyTorch 2.13 wheel range)
torch = "2.13.0"               # exact X.Y.Z, no suffix
torchvision = "0.28.0"
index_url = "https://download.pytorch.org/whl/cu130"   # omit for PyPI (mps)
local_suffix = "+cu130"        # must be "+" + the index's last path segment; "" without index
platforms = ["windows-x86_64", "linux-x86_64", "linux-arm64"]
reference = true               # on the cross-vendor reference stack
experimental = false           # cannot be both reference and experimental
min_driver = 580               # NVIDIA driver major; cuda ports only (required there)
notes = """Shown to people; what the port covers and what it doesn't."""
packages = ["fastapi>=0.115", "ultralytics==8.4.46", ...]  # never torch/torchvision
```

`Registry::validate()` (run by the tests and worth running on any override)
checks: no duplicate ids, id/backend agreement, python range, exact versions,
`local_suffix` ↔ `index_url` agreement (and `cuda-cuXYZ` ↔ `+cuXYZ`), known and
unique platforms, mps only on macos-arm64, no torch in `packages`, min_driver
only and always on cuda, and that reference ports pin the reference stack
(torch, torchvision and `ultralytics==` from `engine/version.py`).

The pins a port installs come from `gm_ports::requirements(spec)`:
`torch==2.13.0+cu130`, `torchvision==0.28.0+cu130`, then `packages`. The local
suffix is what stops the resolver from swapping in a CPU or other-CUDA wheel.

### Development override

Set `GRADAPP_PORTS_DIR=/path/to/ports` and call
`Registry::from_env_or_builtin()` (or `Registry::load_dir(path)` directly) to
use manifests from disk instead of the embedded copies.

## How a device picks its port

`gm-probe` decides; gm-ports only builds what it is told. In short (see
`crates/gm-probe/src/select.rs`, a port of `engine/hardware.py`):

- **NVIDIA**: compute capability < 5.0 → `cpu`. Driver below R525/R528 →
  blocked. Blackwell (cc ≥ 12.0): driver ≥ R580 → `cuda-cu130`, R570–R579 →
  `cuda-cu128` (flagged off-reference), older → blocked. Turing+ (cc ≥ 7.5)
  with driver ≥ R580 → `cuda-cu130`. Everything else → `cuda-cu126`.
- **Intel Arc / Core Ultra** → `xpu`.
- **Apple Silicon** on macOS 14+ → `mps`.
- **AMD with ROCm on Linux**, only when asked for → `rocm`.
- Otherwise → `cpu`.

## Lifecycle

```
plan(spec, machine) ──▶ status(spec, env_root, machine)
                          │ Ready ─────────────────────────▶ run worker with <env>/bin/python
                          │ Stale / NotInstalled
                          ▼
uv::ensure(data_dir) ──▶ provision(spec, env_root, uv, on_progress)
   1. uv python install 3.12        (UV_PYTHON_INSTALL_DIR=<env_root>/.python, --no-bin)
   2. uv venv --seed --python 3.12  (UV_PYTHON_PREFERENCE=only-managed)
   3. uv pip install --python <env python> --index-strategy unsafe-best-match
         [--extra-index-url <index>] [--find-links <wheelhouse>] <exact pins>
   4. verify: real kernel launch on the backend (VERIFY_SNIPPET)
   5. write port.lock.json (atomically, last)
gc(env_root, keep) ──▶ remove envs of ports no longer used
```

- **Status.** `Ready` only if `port.lock.json` matches `plan()` and the env's
  python starts. `Stale` if a lock is there but differs (other pins, other
  machine, unreadable). `NotInstalled` otherwise, including an interrupted
  install and an env whose interpreter will not start.
- **Fingerprint.** sha256 over port id, python, index and the exact pins plus a
  machine fingerprint (hostname, OS, arch). An env copied or synced from
  another machine is `Stale`, never half-working.
- **Cancel safety.** The lock is deleted before anything changes and written
  last; uv children are killed if the future is dropped. An interrupted or
  failed provision is never `Ready`.
- **Verification.** `VerifyMode::Backend` (default) imports torch and launches
  a kernel on the port's device: `is_available()` alone says True for a
  Blackwell card on a build without its kernels. If the install succeeded but
  the check fails (missing driver), `provision` returns `Ok` with state
  `Failed` and the problem, keeps the env, writes `port.verify.json`, and does
  not write a lock. `VerifyMode::Skip` is for torch-free test ports.
- **Retries.** Network failures (timeouts, resets, DNS, 5xx) are retried 3
  times with exponential backoff (2 s, 4 s, 8 s).
- **Wheelhouse.** `ProvisionOptions::wheelhouse` adds `--find-links` for labs
  with slow internet. Build one with `uv pip download`-style tooling using
  `uv_python_platform(platform)` for the target.
- **Switching ports** (e.g. a driver update moves cu126 → cu130) builds a new
  env beside the old one. There is no in-place torch uninstall as in v1's
  `setup_env.py`; `gc` removes the old env once the new one is Ready.

## Where envs live

| OS | env root (`default_env_root()`) |
|---|---|
| Windows | `%LOCALAPPDATA%\GradApp\ports` |
| macOS | `~/Library/Application Support/GradApp/ports` |
| Linux | `~/.local/share/gradapp/ports` (`$XDG_DATA_HOME/gradapp/ports`) |

```
<data>/bin/uv(.exe)          uv, if GradApp downloaded it
<data>/ports/.python/        private uv-managed CPython builds
<data>/ports/<id>/           one venv per port
    .gradapp-port            ownership marker (gc only removes marked dirs)
    port.lock.json           the plan this env was built from
    port.verify.json         last verification report
```

uv's wheel cache stays in uv's default location (shared across ports, so
rebuilding an env is mostly hard-links); set `ProvisionOptions::cache_dir` to
move it.

## Where uv comes from

Search order: `GRADAPP_UV` → a copy bundled next to the app executable (or in
the macOS bundle's `Resources`) → `<data>/bin/uv(.exe)` → `PATH`. If none is
found, `uv::ensure` downloads `uv::UV_VERSION` from
`https://github.com/astral-sh/uv/releases/download/<ver>/uv-<target>.{tar.gz,zip}`
and verifies it against the release's `.sha256` file. Targets:
`x86_64/aarch64-pc-windows-msvc`, `aarch64/x86_64-apple-darwin`,
`x86_64/aarch64-unknown-linux-musl` (static, any distro).

uv runs with `UV_NO_CONFIG=1` so a user's `uv.toml` cannot change what gets
installed. `UV_INDEX_URL` / `UV_EXTRA_INDEX_URL` from the environment are still
honoured, so a site mirror keeps working.

## Adding a port

Example: PyTorch publishes CUDA 13.1 wheels and you want a `cuda-cu131` port.

1. Copy `ports/cuda-cu130.toml` to `ports/cuda-cu131.toml`; set `id`, `label`,
   `index_url = ".../whl/cu131"`, `local_suffix = "+cu131"`, `min_driver`
   (the CUDA 13.1 driver branch), `platforms`, `notes`.
2. Add it to `BUILTIN_MANIFESTS` and `PORT_IDS` in
   `crates/gm-ports/src/registry.rs`.
3. Teach `gm-probe`'s selection when to pick it (and add its parity tests).
4. `cargo test -p gm-ports` (validation, pins), then
   `cargo test -p gm-ports --test network -- --ignored` on a machine that can
   reach PyPI and download.pytorch.org: `ports_resolve_without_conflicts`
   resolves every port on every listed platform, and
   `resolve_with_pytorch_indexes` resolves the full suffixed set.
5. Run it for real on the hardware (`VerifyMode::Backend`).

A ROCm bump is the same: change `index_url` and `local_suffix` together
(`whl/rocm7.3` ↔ `+rocm7.3`); validation rejects a mismatch.

The `rocm` port's index (`whl/rocm7.2`) is the variant used in the PyTorch 2.13
release announcement; the 2.13 release candidates published `rocm7.1` and
`rocm7.2`. It has not been checked against download.pytorch.org from CI.

## Bumping the reference stack

1. Change `REFERENCE_STACK` in `desktop/engine/version.py` (v1 and the one-line
   join still read it) and `REFERENCE_TORCH` / `REFERENCE_TORCHVISION` /
   `REFERENCE_ULTRALYTICS` in `crates/gm-ports/src/registry.rs`.
2. Change `torch` / `torchvision` (and `ultralytics==` in `packages`) in every
   `reference = true` manifest, and the `requirements-*.txt` files they mirror.
   Tests fail until all of these agree.
3. Check each CUDA/ROCm index still publishes the new version (PyTorch drops
   old CUDA variants: cu128 stopped at 2.11). A port that cannot follow becomes
   `reference = false` with its own pins, like `cuda-cu128`.
4. Run the network tests. Every installed env becomes `Stale` (its pins
   changed) and is rebuilt at next start.
