//! Building a port env with uv: private CPython → venv → pinned install →
//! verify → lock.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use gm_types::{AppEvent, PortSpec, PortState, PortStatus};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;

use crate::error::{Error, Result};
use crate::paths::{env_dir, env_python, python_install_dir, LOCK_FILE, PORT_MARKER, VERIFY_FILE};
use crate::plan::{local_machine_fingerprint, plan};
use crate::status::{python_starts, read_lock};
use crate::uv::Uv;
use crate::verify::{interpret, VerifyReport, VERIFY_SNIPPET};

/// One line of progress. Convert to the UI event with `AppEvent::from`.
#[derive(Debug, Clone, PartialEq)]
pub struct Progress {
    pub port: String,
    /// Coarse step: "prepare", "python", "venv", "install", "verify", "lock".
    pub step: &'static str,
    /// 0..=100 where known.
    pub percent: Option<f32>,
    /// A line uv or python printed, verbatim.
    pub line: Option<String>,
}

impl From<Progress> for AppEvent {
    fn from(p: Progress) -> Self {
        AppEvent::PortProgress {
            port: p.port,
            step: p.step.to_string(),
            percent: p.percent,
            line: p.line,
        }
    }
}

/// How hard to check the env after installing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VerifyMode {
    /// Import torch and launch a real kernel on the port's backend (default).
    #[default]
    Backend,
    /// Install only. For test ports and wheelhouse builds without the hardware.
    Skip,
}

#[derive(Debug, Clone)]
pub struct ProvisionOptions {
    /// Identifies this machine; must match what [`crate::status`] is given.
    pub facts_fingerprint: String,
    /// A local directory of wheels (`--find-links`), for slow or offline labs.
    pub wheelhouse: Option<PathBuf>,
    pub verify: VerifyMode,
    /// Retries after a network failure (so up to `retries + 1` attempts).
    pub retries: u32,
    /// First backoff; doubles each retry.
    pub backoff: Duration,
    /// uv's cache (`UV_CACHE_DIR`); uv's default when None.
    pub cache_dir: Option<PathBuf>,
}

impl Default for ProvisionOptions {
    fn default() -> Self {
        ProvisionOptions {
            facts_fingerprint: local_machine_fingerprint(),
            wheelhouse: None,
            verify: VerifyMode::Backend,
            retries: 3,
            backoff: Duration::from_secs(2),
            cache_dir: None,
        }
    }
}

type Sink<'a> = &'a mut (dyn FnMut(Progress) + Send);

/// Provision `spec` under `env_root` with default options.
pub async fn provision<F>(
    spec: &PortSpec,
    env_root: &Path,
    uv: &Uv,
    mut on_progress: F,
) -> Result<PortStatus>
where
    F: FnMut(Progress) + Send,
{
    provision_with(
        spec,
        env_root,
        uv,
        &ProvisionOptions::default(),
        &mut on_progress,
    )
    .await
}

/// Build (or repair) `<env_root>/<port-id>` and return its status.
///
/// Returns `Err` when the install itself fails (network, resolver, disk).
/// Returns `Ok` with state `Failed` when everything installed but the backend
/// check failed (e.g. no driver); the env is kept, but no lock is written, so
/// it never reports `Ready`. Cancel-safe: the lock is deleted before anything
/// changes and written last, atomically.
pub async fn provision_with<F>(
    spec: &PortSpec,
    env_root: &Path,
    uv: &Uv,
    opts: &ProvisionOptions,
    mut on_progress: F,
) -> Result<PortStatus>
where
    F: FnMut(Progress) + Send,
{
    let sink: Sink<'_> = &mut on_progress;
    let p = Provisioner {
        spec,
        env_root,
        uv,
        opts,
    };
    p.run(sink).await
}

struct Provisioner<'a> {
    spec: &'a PortSpec,
    env_root: &'a Path,
    uv: &'a Uv,
    opts: &'a ProvisionOptions,
}

/// Text in uv's output that means "the network let us down, try again".
/// The disk filled up (Windows error 112, POSIX ENOSPC). Retrying cannot help.
fn looks_like_disk_full(text: &str) -> bool {
    let t = text.to_ascii_lowercase();
    t.contains("not enough space on the disk")
        || t.contains("os error 112")
        || t.contains("no space left on device")
        || t.contains("os error 28")
        || t.contains("disk quota exceeded")
}

/// Windows briefly locks a freshly written .exe (antivirus scanning, the
/// search indexer), so uv's venv launcher step can fail with "Failed to
/// update Windows PE resources" / ERROR_OPEN_FAILED (-2147024786) or a
/// sharing violation. Seen on a real 2.0.0 install; the same step succeeded
/// seconds later. Worth a few quiet retries.
fn looks_like_transient_file_lock(text: &str) -> bool {
    let t = text.to_ascii_lowercase();
    t.contains("failed to update windows pe resources")
        || t.contains("os error -2147024786")
        || t.contains("being used by another process")
        || t.contains("os error 32")
        || t.contains("access is denied. (os error 5)")
}

/// Disk a port needs while installing: the downloaded wheels, their unpacked
/// copy in uv's cache and the environment (hard-linked from the cache where
/// the file system allows, so this is the upper bound). CUDA torch alone is a
/// 1.8 GB wheel that unpacks to ~4.5 GB.
pub fn disk_needed_bytes(spec: &gm_types::PortSpec) -> u64 {
    const GB: u64 = 1024 * 1024 * 1024;
    if spec.torch.trim().is_empty() {
        return GB / 2;
    }
    match spec.backend {
        gm_types::Backend::Cuda => 9 * GB,
        gm_types::Backend::Rocm => 12 * GB,
        gm_types::Backend::Xpu => 7 * GB,
        gm_types::Backend::Mps => 3 * GB,
        gm_types::Backend::Cpu => 3 * GB,
    }
}

fn looks_like_network_failure(text: &str) -> bool {
    let t = text.to_ascii_lowercase();
    [
        "timed out",
        "timeout",
        "connection reset",
        "connection refused",
        "connection closed",
        "error sending request",
        "failed to download",
        "failed to fetch",
        "dns error",
        "temporary failure in name resolution",
        "network is unreachable",
        "broken pipe",
        "unexpected eof",
        "502 bad gateway",
        "503 service unavailable",
        "504 gateway",
        "request failed after",
    ]
    .iter()
    .any(|n| t.contains(n))
}

impl Provisioner<'_> {
    fn emit(
        &self,
        sink: &mut Sink<'_>,
        step: &'static str,
        percent: Option<f32>,
        line: Option<String>,
    ) {
        (sink)(Progress {
            port: self.spec.id.clone(),
            step,
            percent,
            line,
        });
    }

    fn uv_command(&self) -> Command {
        let mut cmd = crate::spawn::hidden(&self.uv.path);
        cmd.env("UV_PYTHON_INSTALL_DIR", python_install_dir(self.env_root))
            // Never the machine's Python: that is what broke v1.
            .env("UV_PYTHON_PREFERENCE", "only-managed")
            .env("UV_PYTHON_DOWNLOADS", "automatic")
            // A user's uv.toml / pyproject must not change what gets installed.
            .env("UV_NO_CONFIG", "1")
            .env("UV_NO_PROGRESS", "1")
            .env("NO_COLOR", "1")
            .env_remove("VIRTUAL_ENV")
            .env_remove("CONDA_PREFIX")
            .env_remove("UV_SYSTEM_PYTHON")
            .env_remove("PYTHONHOME")
            .env_remove("PYTHONPATH")
            .current_dir(self.env_root);
        if let Some(cache) = &self.opts.cache_dir {
            cmd.env("UV_CACHE_DIR", cache);
        }
        cmd
    }

    /// Run a command, streaming each line to the sink. Retries network failures.
    async fn run_step(
        &self,
        sink: &mut Sink<'_>,
        step: &'static str,
        pct: (f32, f32),
        make: impl Fn() -> Command,
    ) -> Result<Vec<String>> {
        let mut attempt = 0;
        loop {
            let (code, tail) = self.run_once(sink, step, pct, make()).await?;
            if code == Some(0) {
                return Ok(tail);
            }
            let text = tail.join("\n");
            if looks_like_disk_full(&text) {
                let free = crate::spawn::free_bytes(self.env_root).unwrap_or(0);
                return Err(Error::DiskFull {
                    path: self.env_root.to_path_buf(),
                    needed_gb: disk_needed_bytes(self.spec) as f64 / 1e9,
                    free_gb: free as f64 / 1e9,
                });
            }
            if attempt < self.opts.retries && looks_like_transient_file_lock(&text) {
                attempt += 1;
                self.emit(
                    sink,
                    step,
                    Some(pct.0),
                    Some(format!(
                        "Windows is still scanning a new file; retrying (attempt {} of {})",
                        attempt + 1,
                        self.opts.retries + 1
                    )),
                );
                tokio::time::sleep(Duration::from_millis(1500 * attempt as u64)).await;
                continue;
            }
            if attempt < self.opts.retries && looks_like_network_failure(&text) {
                let wait = self.opts.backoff * 2u32.pow(attempt);
                attempt += 1;
                self.emit(
                    sink,
                    step,
                    Some(pct.0),
                    Some(format!(
                        "network problem; retrying in {}s (attempt {} of {})",
                        wait.as_secs(),
                        attempt + 1,
                        self.opts.retries + 1
                    )),
                );
                tokio::time::sleep(wait).await;
                continue;
            }
            return Err(Error::Command {
                step: step.to_string(),
                code,
                message: explain(step, &text),
            });
        }
    }

    async fn run_once(
        &self,
        sink: &mut Sink<'_>,
        step: &'static str,
        (lo, hi): (f32, f32),
        mut cmd: Command,
    ) -> Result<(Option<i32>, Vec<String>)> {
        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = cmd.spawn().map_err(|e| Error::io(&self.uv.path, e))?;
        let (tx, mut rx) = mpsc::unbounded_channel::<String>();
        let mut readers = Vec::new();
        if let Some(out) = child.stdout.take() {
            let tx = tx.clone();
            readers.push(tokio::spawn(async move {
                let mut lines = BufReader::new(out).lines();
                while let Ok(Some(l)) = lines.next_line().await {
                    let _ = tx.send(l);
                }
            }));
        }
        if let Some(err) = child.stderr.take() {
            let tx = tx.clone();
            readers.push(tokio::spawn(async move {
                let mut lines = BufReader::new(err).lines();
                while let Ok(Some(l)) = lines.next_line().await {
                    let _ = tx.send(l);
                }
            }));
        }
        drop(tx);

        let mut tail: Vec<String> = Vec::new();
        let mut pct = lo;
        while let Some(line) = rx.recv().await {
            pct = step_percent(step, &line, pct, lo, hi);
            tail.push(line.clone());
            if tail.len() > 80 {
                tail.remove(0);
            }
            self.emit(sink, step, Some(pct), Some(line));
        }
        for r in readers {
            let _ = r.await;
        }
        let status = child
            .wait()
            .await
            .map_err(|e| Error::io(&self.uv.path, e))?;
        Ok((status.code(), tail))
    }

    async fn run(&self, mut sink: Sink<'_>) -> Result<PortStatus> {
        let spec = self.spec;
        // Before anything is deleted or downloaded: a port with no wheel for
        // this OS/CPU would only fail minutes later inside the resolver.
        crate::registry::check_platform(spec, crate::registry::current_platform())?;
        let dir = env_dir(self.env_root, &spec.id);
        let python = env_python(&dir);
        let want = plan(spec, &self.opts.facts_fingerprint);

        self.emit(
            &mut sink,
            "prepare",
            Some(0.0),
            Some(format!("provisioning {} in {}", spec.id, dir.display())),
        );
        std::fs::create_dir_all(self.env_root).map_err(|e| Error::io(self.env_root, e))?;

        // Reuse the venv only if its lock is for exactly this plan and it starts;
        // otherwise start clean (different pins, another machine, half-built).
        let reusable = read_lock(&dir)
            .is_some_and(|l| l.fingerprint == want.fingerprint && l.port == want.port)
            && python_starts(&python).await;
        // From here until the end the env is not Ready.
        remove_if_exists(&dir.join(LOCK_FILE))?;
        if !reusable && dir.exists() {
            self.emit(
                &mut sink,
                "prepare",
                Some(1.0),
                Some("removing the previous environment".into()),
            );
            std::fs::remove_dir_all(&dir).map_err(|e| Error::io(&dir, e))?;
        }

        // 0. Enough disk? Checked before anything is downloaded: a full disk
        // half way through a 1.8 GB torch wheel looked like a network failure
        // and was retried four times on a real install.
        if !reusable && std::env::var_os("GRADAPP_SKIP_DISK_CHECK").is_none() {
            let needed = disk_needed_bytes(spec);
            if let Some(free) = crate::spawn::free_bytes(self.env_root) {
                if free < needed {
                    return Err(Error::DiskFull {
                        path: self.env_root.to_path_buf(),
                        needed_gb: needed as f64 / 1e9,
                        free_gb: free as f64 / 1e9,
                    });
                }
            }
        }

        // 1. Private CPython.
        self.run_step(&mut sink, "python", (2.0, 10.0), || {
            let mut c = self.uv_command();
            c.args(["python", "install", "--no-bin"]);
            if cfg!(windows) {
                c.arg("--no-registry");
            }
            c.arg(&spec.python);
            c
        })
        .await?;

        // 2. The venv.
        if !reusable {
            self.run_step(&mut sink, "venv", (10.0, 15.0), || {
                let mut c = self.uv_command();
                c.args(["venv", "--no-project", "--seed", "--python", &spec.python])
                    .arg(&dir);
                c
            })
            .await?;
        }
        std::fs::write(dir.join(PORT_MARKER), format!("{}\n", spec.id))
            .map_err(|e| Error::io(&dir, e))?;

        // 3. Exact pins.
        let reqs = want.requirements.clone();
        self.run_step(&mut sink, "install", (15.0, 85.0), || {
            let mut c = self.uv_command();
            c.args(["pip", "install", "--python"]).arg(&python);
            c.args(["--index-strategy", "unsafe-best-match"]);
            if let Some(url) = &spec.index_url {
                c.args(["--extra-index-url", url]);
            }
            if let Some(wh) = &self.opts.wheelhouse {
                c.arg("--find-links").arg(wh);
            }
            c.args(&reqs);
            c
        })
        .await?;

        // 4. Verify.
        let mut status = PortStatus {
            port: spec.id.clone(),
            state: PortState::Ready,
            env_dir: dir.to_string_lossy().into_owned(),
            python_path: Some(python.to_string_lossy().into_owned()),
            torch: (!spec.torch.is_empty()).then(|| want.torch.clone()),
            device: None,
            message: None,
        };
        if self.opts.verify == VerifyMode::Backend {
            self.emit(
                &mut sink,
                "verify",
                Some(88.0),
                Some(format!(
                    "checking that {} actually runs",
                    spec.backend.as_str()
                )),
            );
            let report = self.verify(&python, &mut sink).await;
            let json = serde_json::to_string_pretty(&report).unwrap_or_default();
            write_atomic(&dir.join(VERIFY_FILE), json.as_bytes())?;
            status.torch = report.torch.clone().or(status.torch);
            status.device = report.device.clone();
            if !report.ok {
                status.state = PortState::Failed;
                status.message = report.problem.clone();
                self.emit(&mut sink, "verify", Some(100.0), report.problem);
                return Ok(status);
            }
        } else if !python_starts(&python).await {
            return Err(Error::Other(format!(
                "{} was installed but its Python will not start",
                spec.id
            )));
        }

        // 5. Lock, last and atomically.
        let lock = serde_json::to_vec_pretty(&want).map_err(|e| Error::Other(e.to_string()))?;
        write_atomic(&dir.join(LOCK_FILE), &lock)?;
        self.emit(
            &mut sink,
            "lock",
            Some(100.0),
            Some(format!(
                "{} ready: torch {} on {}",
                spec.id,
                status.torch.as_deref().unwrap_or("none"),
                status.device.as_deref().unwrap_or("CPU")
            )),
        );
        Ok(status)
    }

    async fn verify(&self, python: &Path, sink: &mut Sink<'_>) -> VerifyReport {
        let check_ultra = self.spec.packages.iter().any(|p| {
            p.trim_start()
                .to_ascii_lowercase()
                .starts_with("ultralytics")
        });
        let mut cmd = crate::spawn::hidden(python);
        cmd.args(["-c", VERIFY_SNIPPET, if check_ultra { "1" } else { "0" }])
            .env("PYTHONIOENCODING", "utf-8")
            .env_remove("PYTHONHOME")
            .env_remove("PYTHONPATH")
            .stdin(Stdio::null())
            .kill_on_drop(true);
        let out = match tokio::time::timeout(Duration::from_secs(300), cmd.output()).await {
            Ok(Ok(o)) => o,
            Ok(Err(e)) => return interpret("", &e.to_string(), self.spec.backend, check_ultra),
            Err(_) => {
                return interpret(
                    "",
                    "the check did not finish within 5 minutes",
                    self.spec.backend,
                    check_ultra,
                )
            }
        };
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        for line in stderr.lines().filter(|l| !l.trim().is_empty()) {
            self.emit(sink, "verify", Some(92.0), Some(line.to_string()));
        }
        interpret(&stdout, &stderr, self.spec.backend, check_ultra)
    }
}

/// Coarse progress from uv's own messages.
fn step_percent(step: &str, line: &str, current: f32, lo: f32, hi: f32) -> f32 {
    let at = |f: f32| lo + (hi - lo) * f;
    let l = line.trim_start();
    let next = if step == "install" {
        if l.starts_with("Resolved") {
            at(0.2)
        } else if l.starts_with("Downloading") || l.starts_with("Downloaded") {
            // Each wheel moves the bar a little, never past 85% of the step.
            (current + (hi - lo) * 0.03).min(at(0.85))
        } else if l.starts_with("Prepared") {
            at(0.9)
        } else if l.starts_with("Installed") || l.starts_with("Audited") {
            hi
        } else {
            current
        }
    } else {
        (current + (hi - lo) * 0.1).min(hi)
    };
    next.max(current)
}

fn explain(step: &str, tail: &str) -> String {
    let last: Vec<&str> = tail
        .lines()
        .rev()
        .take(25)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    let mut msg = last.join("\n");
    let lower = tail.to_ascii_lowercase();
    if lower.contains("no solution found") || lower.contains("no matching distribution") {
        msg.push_str(
            "\nNo wheel matches this port on this machine. Check the port's platforms and python.",
        );
    } else if looks_like_network_failure(tail) {
        msg.push_str(
            "\nThe download kept failing. Try again on a better connection, or provide a wheelhouse \
             (see docs/PORTS.md).",
        );
    } else if cfg!(windows) && (lower.contains("path too long") || lower.contains("os error 206")) {
        msg.push_str(
            "\nThis looks like Windows' 260 character path limit; enable long paths and retry.",
        );
    }
    if msg.trim().is_empty() {
        msg = format!("{step} printed nothing");
    }
    msg
}

fn remove_if_exists(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(Error::io(path, e)),
    }
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, bytes).map_err(|e| Error::io(&tmp, e))?;
    std::fs::rename(&tmp, path).map_err(|e| Error::io(path, e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn network_detection() {
        assert!(looks_like_network_failure(
            "error: Failed to fetch: `https://pypi.org/simple/six/`\n  Caused by: dns error"
        ));
        assert!(looks_like_network_failure("Request failed after 3 retries"));
        assert!(!looks_like_network_failure(
            "× No solution found when resolving dependencies"
        ));
    }

    #[test]
    fn install_percent_is_monotonic_and_bounded() {
        let mut p = 15.0;
        for line in [
            "Resolved 40 packages in 1s",
            "Downloading torch (800MiB)",
            "Downloaded torch",
            "Prepared 40 packages",
            "Installed 40 packages",
        ] {
            let n = step_percent("install", line, p, 15.0, 85.0);
            assert!(n >= p && n <= 85.0, "{line}: {n}");
            p = n;
        }
        assert_eq!(p, 85.0);
    }
}

#[cfg(test)]
mod field_report_tests {
    //! Lines from a real GradApp 2.0.0 install on Windows (RTX 3050 laptop).
    use super::*;

    #[test]
    fn windows_disk_full_is_not_a_network_problem() {
        let line = "failed to flush file `C:\\Users\\a\\AppData\\Local\\uv\\cache\\.tmp\\torch/include/x.h`: There is not enough space on the disk. (os error 112)";
        assert!(looks_like_disk_full(line));
        let linux = "error: No space left on device (os error 28)";
        assert!(looks_like_disk_full(linux));
        assert!(!looks_like_disk_full(
            "Failed to download `torch`: connection reset"
        ));
    }

    #[test]
    fn windows_venv_launcher_lock_is_retried() {
        let line = "Caused by: Failed to update Windows PE resources: C:\\Users\\a\\AppData\\Local\\Temp\\.tmp8KBQyi\\uv-trampoline-14200.exe\n  Caused by: The system cannot open the device or file specified. (os error -2147024786)";
        assert!(looks_like_transient_file_lock(line));
        assert!(!looks_like_transient_file_lock(
            "No solution found when resolving dependencies"
        ));
    }

    #[test]
    fn disk_budget_by_backend() {
        let reg = crate::Registry::builtin();
        let gb = |id: &str| disk_needed_bytes(reg.require(id).unwrap()) / (1024 * 1024 * 1024);
        assert!(gb("cuda-cu130") >= 8);
        assert!(gb("mps") < gb("cuda-cu130"));
        assert!(gb("cpu") >= 2);
    }
}
