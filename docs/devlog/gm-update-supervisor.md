# gm-update, gm-supervisor and the v2 release pipeline

## 2026-11-24: first implementation (2.0.0-dev)

### What
- **`crates/gm-update`**: update checks against GitHub Releases.
  - `check(current, repo, channel) -> UpdateInfo` (no cache) and
    `Checker` (`with_cache`, `with_api_base`, `with_min_interval`,
    `with_timeout`; `check_detailed(.., force) -> Checked { info, reason, from_cache, manifest_url }`).
  - Lists `/repos/<repo>/releases?per_page=30`, skips drafts, keeps
    pre-releases only on `Channel::Beta`, takes the **highest semver**
    (not the newest release), and compares it with `semver`.
  - `installable` = an update exists and the release has `latest.json`.
  - ETag cache file (write-then-rename), at most one request per
    `min_interval` (default 30 min) unless forced, 8 s timeout. Failures
    (offline, 403/429 rate limit, 404, malformed JSON, bad current version)
    return `available: false` plus a reason; a rate limit reuses the last
    known release list.
  - `parse_latest_json`, `UpdaterManifest::asset_for(key)` (bare key, then
    `-nsis`/`-msi`/`-app`/`-appimage`/`-deb`/`-rpm`), `platform_key()`.
- **`crates/gm-supervisor`**: child process supervision.
  - `Supervisor::spawn(ProcSpec) -> Handle`; `Handle::{status, status_watch,
    subscribe, recent_lines, stop, request_stop, wait, is_finished}`;
    `Event::{Line, Status}` with `to_app_event()`.
  - Unix: own process group, stop signal (SIGTERM, or SIGINT for worker
    and setup, as in v1) to the group, then SIGKILL after the grace period;
    the group is swept after every exit, so orphaned grandchildren die too.
  - Windows: `CREATE_NO_WINDOW` plus a Job Object with
    `KILL_ON_JOB_CLOSE`, terminated on stop. There is no graceful stop on
    Windows (same as v1), so the caller sends `/leave` for the worker.
  - Output is split on `\n`, `\r\n` and `\r`, decoded as lossy UTF-8 and
    stripped of ANSI codes. Lines go to a ring buffer (default 500) and a
    broadcast channel.
  - Restarts use exponential backoff (`min(cap, base*2^(n-1))`); a long run
    resets the count; `max_restarts` leads to `Failed`; permanent exit codes
    fail at once; a clean exit or a requested stop never restarts. Dropping
    the last `Handle` stops the process tree.
  - `worker_spec`, `coordinator_spec`, `setup_spec` mirror v1's launches;
    each argument cites its v1 source.
  - `classify(line) -> Option<Milestone>`, `level()`, `FailureTracker`:
    a regex-for-regex port of agent.js `onLine`.
- **Workflows**: `release.yml` is limited to `v1.*` tags. New
  `release-v2.yml` (tauri-action, 3-OS matrix, draft, upload, check
  `latest.json`, publish) and `ci-v2.yml` (fmt, clippy -D warnings, test
  on 3 OSes, gm-probe parity drift check).
- **website/app.js**: when the latest release has no v1 file for a button,
  the page finds the v2 file by pattern (`ASSETS_V2`) and links to it
  directly. v1 behaviour is unchanged.

### Why
- v1's update check compared three numbers and asked `/releases/latest`.
  A beta looked equal to the final release, and a v1 hotfix would have
  hidden 2.0. v1 also could only link to the download page.
- v1 process code was spread across proc.js, agent.js and host.js, each with
  its own restart and kill logic. One supervisor gives one set of rules,
  and those rules are tested.
- The worker token is passed in `GRADMESH_TOKEN`, not on argv, so it does
  not appear in `ps` or Task Manager. worker.py already reads that variable.

### Verified
- `cargo test -p gm-update -p gm-supervisor`: 6+12 update tests on a mock
  axum server (newer, older, equal, prerelease, draft, link-only,
  malformed, 404, 403 rate limit, timeout, throttle + ETag 304, stale
  fallback, latest.json per platform). 14 unit + 14 process tests:
  streaming, ANSI, ring buffer, crash → backoff → Failed, permanent code,
  recover, graceful stop, kill after grace, stop during backoff, grandchild
  killed on stop and on crash, drop-stops, python3 child. Three runs were
  green.
- `cargo clippy -p gm-update -p gm-supervisor --all-targets -D warnings`
  and `cargo fmt --check` pass.
- Windows code was not compiled here, because the windows-gnu target could
  not be downloaded through the proxy. The FFI signatures were checked by
  hand against windows-sys 0.59 sources. ci-v2 on windows-latest compiles it.
- All workflow YAML parses.

### Next
- The app needs to wire `Checker` into the Tauri updater plugin. On the
  Beta channel it should point the plugin at `Checked::manifest_url`
  (the fixed `/releases/latest/download/latest.json` endpoint never serves
  pre-releases).
- gm-types: add `UpdateInfo.reason` and `UpdateInfo.manifest_url` so the UI
  gets them without a second type.
