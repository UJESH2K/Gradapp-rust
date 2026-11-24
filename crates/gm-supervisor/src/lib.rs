//! Child process supervision for GradApp 2.
//!
//! Replaces v1 `desktop/src/main/proc.js` (spawn quietly, stream lines, kill
//! the tree) and the process-lifecycle parts of `agent.js` / `host.js`
//! (restart with backoff, give up on permanent failures, never restart after
//! a requested stop).
//!
//! ```no_run
//! # async fn demo() {
//! use gm_supervisor::{Supervisor, ProcSpec, Event};
//! use gm_types::ProcRole;
//! let handle = Supervisor::spawn(ProcSpec::new(ProcRole::Worker, "python3").arg("-u").arg("worker.py"));
//! let mut events = handle.subscribe();
//! while let Ok(ev) = events.recv().await {
//!     match ev {
//!         Event::Line(l) => println!("{}", l.line),
//!         Event::Status(s) => println!("{:?}", s.state),
//!     }
//! }
//! handle.stop().await;
//! # }
//! ```

pub mod classify;
pub mod lines;
mod os;
pub mod specs;

use std::collections::VecDeque;
use std::ffi::OsString;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gm_types::{AppEvent, ProcRole, ProcState, ProcStatus};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;
use tokio::sync::{broadcast, watch};
use tokio::time::Instant;

pub use classify::{classify, FailureTracker, Milestone, Step};
pub use lines::strip_ansi;
pub use specs::{
    coordinator_spec, setup_spec, worker_spec, CoordinatorLaunch, SetupLaunch, WorkerLaunch,
};

/// Lines kept for "show me the last output" (crash dialogs, Activity page).
pub const DEFAULT_RING_LINES: usize = 500;
/// Same 6 s v1 `stopTree` gave a worker to leave the mesh before SIGKILL.
pub const DEFAULT_GRACE: Duration = Duration::from_secs(6);

// ---------------------------------------------------------------------------
// Spec
// ---------------------------------------------------------------------------

/// Signal used for the polite part of a stop (Unix only; Windows has no
/// polite stop for a windowless process and kills the job at once).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopSignal {
    #[default]
    Terminate,
    /// SIGINT: what v1 `proc.js::stopTree` sent; Python turns it into
    /// KeyboardInterrupt, which worker.py and setup_env.py handle.
    Interrupt,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RestartPolicy {
    /// Restarts allowed within one "unstable" period before giving up (Failed).
    /// 0 = never restart.
    pub max_restarts: u32,
    /// First delay; doubles each consecutive restart.
    pub backoff_base: Duration,
    pub backoff_cap: Duration,
    /// A run that lasted at least this long resets the consecutive count.
    pub reset_after: Duration,
    /// Exit codes that mean "retrying cannot help" (→ Failed at once).
    pub permanent_exit_codes: Vec<i32>,
    /// Restart on exit code 0 too (a process that should run forever).
    pub restart_on_clean_exit: bool,
}

impl RestartPolicy {
    pub fn never() -> Self {
        RestartPolicy {
            max_restarts: 0,
            backoff_base: Duration::from_secs(1),
            backoff_cap: Duration::from_secs(1),
            reset_after: Duration::from_secs(60),
            permanent_exit_codes: vec![],
            restart_on_clean_exit: false,
        }
    }

    /// Delay before consecutive restart number `n` (1-based):
    /// `min(cap, base * 2^(n-1))`. v1 agent.js: `min(60, 5 * 2 ** min(n-1, 4))` s.
    pub fn delay(&self, n: u32) -> Duration {
        let factor = 1u32 << n.saturating_sub(1).min(20);
        self.backoff_base
            .saturating_mul(factor)
            .min(self.backoff_cap)
    }
}

impl Default for RestartPolicy {
    /// v1 agent.js numbers: 5 s, doubling, capped at 60 s.
    fn default() -> Self {
        RestartPolicy {
            max_restarts: 5,
            backoff_base: Duration::from_secs(5),
            backoff_cap: Duration::from_secs(60),
            reset_after: Duration::from_secs(120),
            permanent_exit_codes: vec![],
            restart_on_clean_exit: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProcSpec {
    pub role: ProcRole,
    pub program: OsString,
    pub args: Vec<OsString>,
    /// Added on top of the inherited environment.
    pub env: Vec<(OsString, OsString)>,
    /// Removed from the inherited environment.
    pub env_remove: Vec<OsString>,
    pub cwd: Option<PathBuf>,
    pub restart: RestartPolicy,
    pub stop_signal: StopSignal,
    /// Polite-stop budget before the tree is killed.
    pub grace: Duration,
    pub ring_lines: usize,
}

impl ProcSpec {
    pub fn new(role: ProcRole, program: impl Into<OsString>) -> Self {
        ProcSpec {
            role,
            program: program.into(),
            args: vec![],
            env: vec![],
            env_remove: vec![],
            cwd: None,
            restart: RestartPolicy::never(),
            stop_signal: StopSignal::Terminate,
            grace: DEFAULT_GRACE,
            ring_lines: DEFAULT_RING_LINES,
        }
    }
    pub fn arg(mut self, a: impl Into<OsString>) -> Self {
        self.args.push(a.into());
        self
    }
    pub fn args<I: IntoIterator<Item = S>, S: Into<OsString>>(mut self, a: I) -> Self {
        self.args.extend(a.into_iter().map(Into::into));
        self
    }
    pub fn env(mut self, k: impl Into<OsString>, v: impl Into<OsString>) -> Self {
        self.env.push((k.into(), v.into()));
        self
    }
    pub fn cwd(mut self, dir: impl Into<PathBuf>) -> Self {
        self.cwd = Some(dir.into());
        self
    }
    pub fn restart(mut self, policy: RestartPolicy) -> Self {
        self.restart = policy;
        self
    }
    pub fn stop_signal(mut self, s: StopSignal) -> Self {
        self.stop_signal = s;
        self
    }
    pub fn grace(mut self, g: Duration) -> Self {
        self.grace = g;
        self
    }
    pub fn ring_lines(mut self, n: usize) -> Self {
        self.ring_lines = n;
        self
    }

    /// Value of an env var this spec sets (last one wins), for tests and logs.
    pub fn env_value(&self, key: &str) -> Option<&OsString> {
        self.env
            .iter()
            .rev()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v)
    }
}

// ---------------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stream {
    Stdout,
    Stderr,
    /// A note from the supervisor itself ("restarting in 5 s").
    Supervisor,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LogLine {
    pub role: ProcRole,
    pub stream: Stream,
    pub line: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Line(LogLine),
    Status(ProcStatus),
}

impl Event {
    /// The UI event this maps to.
    pub fn to_app_event(&self) -> AppEvent {
        match self {
            Event::Line(l) => AppEvent::Log {
                role: l.role,
                line: l.line.clone(),
            },
            Event::Status(s) => AppEvent::Proc { status: s.clone() },
        }
    }
}

// ---------------------------------------------------------------------------
// Shared state
// ---------------------------------------------------------------------------

struct Shared {
    role: ProcRole,
    ring: Mutex<VecDeque<LogLine>>,
    ring_cap: usize,
    events: broadcast::Sender<Event>,
    status: watch::Sender<ProcStatus>,
}

impl Shared {
    fn line(&self, stream: Stream, line: String) {
        let entry = LogLine {
            role: self.role,
            stream,
            line,
        };
        {
            let mut ring = self.ring.lock().unwrap_or_else(|e| e.into_inner());
            if self.ring_cap > 0 {
                if ring.len() >= self.ring_cap {
                    ring.pop_front();
                }
                ring.push_back(entry.clone());
            }
        }
        if stream != Stream::Supervisor {
            // Keep last_line current without waking status watchers per line.
            let text = entry.line.clone();
            self.status.send_if_modified(|s| {
                s.last_line = Some(text);
                false
            });
        }
        let _ = self.events.send(Event::Line(entry));
    }

    fn set(&self, f: impl FnOnce(&mut ProcStatus)) {
        self.status.send_modify(f);
        let snapshot = self.status.borrow().clone();
        tracing::debug!(role = ?snapshot.role, state = ?snapshot.state, pid = ?snapshot.pid, "proc status");
        let _ = self.events.send(Event::Status(snapshot));
    }
}

/// Fires the stop request when the last [`Handle`] is dropped, so a
/// forgotten handle never leaves an orphaned tree behind.
struct StopOnDrop(watch::Sender<bool>);

impl Drop for StopOnDrop {
    fn drop(&mut self) {
        let _ = self.0.send(true);
    }
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// A supervised process. Cheap to clone; dropping the last clone stops it.
#[derive(Clone)]
pub struct Handle {
    shared: Arc<Shared>,
    stop: Arc<StopOnDrop>,
    done: watch::Receiver<bool>,
}

impl std::fmt::Debug for Handle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Handle")
            .field("status", &self.status())
            .finish()
    }
}

pub struct Supervisor;

impl Supervisor {
    /// Start supervising `spec`. Must be called inside a tokio runtime.
    pub fn spawn(spec: ProcSpec) -> Handle {
        let (events, _) = broadcast::channel(1024);
        let (status, _) = watch::channel(ProcStatus {
            role: spec.role,
            state: ProcState::Starting,
            pid: None,
            restarts: 0,
            last_line: None,
            exit_code: None,
        });
        let shared = Arc::new(Shared {
            role: spec.role,
            ring: Mutex::new(VecDeque::with_capacity(spec.ring_lines.min(4096))),
            ring_cap: spec.ring_lines,
            events,
            status,
        });
        let (stop_tx, stop_rx) = watch::channel(false);
        let (done_tx, done_rx) = watch::channel(false);
        let task_shared = shared.clone();
        tokio::spawn(async move {
            run(spec, &task_shared, stop_rx).await;
            let _ = done_tx.send(true);
        });
        Handle {
            shared,
            stop: Arc::new(StopOnDrop(stop_tx)),
            done: done_rx,
        }
    }
}

impl Handle {
    pub fn role(&self) -> ProcRole {
        self.shared.role
    }

    /// Current status snapshot.
    pub fn status(&self) -> ProcStatus {
        self.shared.status.borrow().clone()
    }

    /// Watch status changes (latest value only).
    pub fn status_watch(&self) -> watch::Receiver<ProcStatus> {
        self.shared.status.subscribe()
    }

    /// Every line and status change, in order. Slow receivers lag (see
    /// `broadcast::error::RecvError::Lagged`), they never block the child.
    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.shared.events.subscribe()
    }

    /// The last lines (up to `ring_lines`), oldest first.
    pub fn recent_lines(&self) -> Vec<LogLine> {
        self.shared
            .ring
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .cloned()
            .collect()
    }

    /// Supervision has ended (Stopped or Failed, nothing more will run).
    pub fn is_finished(&self) -> bool {
        *self.done.borrow()
    }

    /// Request a stop without waiting.
    pub fn request_stop(&self) {
        let _ = self.stop.0.send(true);
    }

    /// Graceful stop: polite signal, then kill after the grace period.
    /// Never restarts afterwards. Returns the final status.
    pub async fn stop(&self) -> ProcStatus {
        self.request_stop();
        self.wait().await
    }

    /// Wait until supervision ends on its own (Failed / clean exit) or by stop.
    pub async fn wait(&self) -> ProcStatus {
        let mut done = self.done.clone();
        let _ = done.wait_for(|d| *d).await;
        self.status()
    }
}

// ---------------------------------------------------------------------------
// The supervision loop
// ---------------------------------------------------------------------------

async fn pump<R: AsyncRead + Unpin>(mut r: R, stream: Stream, shared: Arc<Shared>) {
    let mut splitter = lines::LineSplitter::new();
    let mut buf = vec![0u8; 8192];
    let mut out = Vec::new();
    loop {
        match r.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                splitter.feed(&buf[..n], &mut out);
                for line in out.drain(..) {
                    shared.line(stream, line);
                }
            }
        }
    }
    if let Some(line) = splitter.finish() {
        shared.line(stream, line);
    }
}

fn command(spec: &ProcSpec) -> Command {
    let mut cmd = Command::new(&spec.program);
    cmd.args(&spec.args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    for k in &spec.env_remove {
        cmd.env_remove(k);
    }
    for (k, v) in &spec.env {
        cmd.env(k, v);
    }
    if let Some(dir) = &spec.cwd {
        cmd.current_dir(dir);
    }
    os::prepare(&mut cmd);
    cmd
}

async fn stop_requested(rx: &mut watch::Receiver<bool>) {
    // Resolves when true; if every sender is gone (cannot happen while a
    // Handle lives) treat that as a stop as well.
    let _ = rx.wait_for(|s| *s).await;
}

async fn run(spec: ProcSpec, shared: &Arc<Shared>, mut stop_rx: watch::Receiver<bool>) {
    let policy = &spec.restart;
    let mut consecutive: u32 = 0;
    let name = spec.program.to_string_lossy().into_owned();

    loop {
        if *stop_rx.borrow() {
            shared.set(|s| {
                s.state = ProcState::Stopped;
                s.pid = None;
            });
            return;
        }

        let mut child = match command(&spec).spawn() {
            Ok(c) => c,
            Err(e) => {
                // A missing interpreter will not appear by retrying.
                shared.line(Stream::Supervisor, format!("could not start {name}: {e}"));
                shared.set(|s| {
                    s.state = ProcState::Failed;
                    s.pid = None;
                    s.exit_code = None;
                });
                return;
            }
        };
        let tree = os::Tree::attach(&child);
        let pid = child.id();
        shared.set(|s| {
            s.state = ProcState::Running;
            s.pid = pid;
            s.exit_code = None;
        });

        let readers = [
            child
                .stdout
                .take()
                .map(|o| tokio::spawn(pump(o, Stream::Stdout, shared.clone()))),
            child
                .stderr
                .take()
                .map(|e| tokio::spawn(pump(e, Stream::Stderr, shared.clone()))),
        ];
        let started = Instant::now();

        let mut requested = false;
        let exit = tokio::select! {
            r = child.wait() => r,
            _ = stop_requested(&mut stop_rx) => {
                requested = true;
                if tree.signal(spec.stop_signal) {
                    match tokio::time::timeout(spec.grace, child.wait()).await {
                        Ok(r) => r,
                        Err(_) => {
                            shared.line(Stream::Supervisor, format!("{name} did not stop within {:?}; killing it", spec.grace));
                            tree.kill(&mut child);
                            child.wait().await
                        }
                    }
                } else {
                    tree.kill(&mut child);
                    child.wait().await
                }
            }
        };
        // Sweep anything the leader left behind in its group / job.
        tree.kill(&mut child);
        // On Windows this closes the job handle (KILL_ON_JOB_CLOSE).
        #[allow(clippy::drop_non_drop)]
        drop(tree);
        for reader in readers.into_iter().flatten() {
            // Pipes close once every holder is dead; don't hang if one survived.
            let _ = tokio::time::timeout(Duration::from_secs(2), reader).await;
        }

        let code = exit.as_ref().ok().and_then(|s| s.code());
        let ran = started.elapsed();

        if requested || *stop_rx.borrow() {
            shared.set(|s| {
                s.state = ProcState::Stopped;
                s.pid = None;
                s.exit_code = code;
            });
            return;
        }

        let clean = code == Some(0);
        if clean && !policy.restart_on_clean_exit {
            shared.set(|s| {
                s.state = ProcState::Stopped;
                s.pid = None;
                s.exit_code = code;
            });
            return;
        }
        if code.is_some_and(|c| policy.permanent_exit_codes.contains(&c)) {
            shared.line(
                Stream::Supervisor,
                format!("{name} exited with {code:?}; not restarting (permanent failure)"),
            );
            shared.set(|s| {
                s.state = ProcState::Failed;
                s.pid = None;
                s.exit_code = code;
            });
            return;
        }

        if ran >= policy.reset_after {
            consecutive = 0;
        }
        if consecutive >= policy.max_restarts {
            if policy.max_restarts > 0 {
                shared.line(
                    Stream::Supervisor,
                    format!(
                        "{name} exited with {code:?}; gave up after {} restarts",
                        policy.max_restarts
                    ),
                );
            }
            shared.set(|s| {
                s.state = ProcState::Failed;
                s.pid = None;
                s.exit_code = code;
            });
            return;
        }
        consecutive += 1;
        let delay = policy.delay(consecutive);
        shared.line(
            Stream::Supervisor,
            format!("{name} exited with {code:?}; restarting in {delay:?}"),
        );
        shared.set(|s| {
            s.state = ProcState::Restarting;
            s.pid = None;
            s.exit_code = code;
            s.restarts += 1;
        });
        tokio::select! {
            _ = tokio::time::sleep(delay) => {}
            _ = stop_requested(&mut stop_rx) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_and_caps() {
        let p = RestartPolicy::default();
        let secs: Vec<u64> = (1..=7).map(|n| p.delay(n).as_secs()).collect();
        // v1 agent.js: 5, 10, 20, 40, 60 (capped), 60, 60
        assert_eq!(secs, vec![5, 10, 20, 40, 60, 60, 60]);
        assert_eq!(p.delay(1000), Duration::from_secs(60));
    }
}
