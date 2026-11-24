//! Worker / installer output → progress milestones.
//!
//! Port of the stepper in v1 `desktop/src/main/agent.js` (`Agent.onLine`),
//! regex for regex, so the v2 UI shows the same steps from the same engine
//! output. The lines come from `engine/setup_env.py` (`say(...)`, indented
//! by two spaces, coloured on a TTY) and `engine/worker.py`
//! (`log(...)` = `[worker HH:MM:SS] message`).
//!
//! * [`classify`] — one line → at most one [`Milestone`] (the `if/else if`
//!   chain in `onLine`).
//! * [`level`] — the info/warn/error level v1 gave each line.
//! * [`FailureTracker`] — v1's `lastError` / `errorRank` / `lastFix`: keeps
//!   the most telling failure line so a crash can say *why*.

use std::sync::LazyLock;

use regex::Regex;
use serde::{Deserialize, Serialize};

/// v1 `STEPS` in agent.js, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Step {
    Python,
    Prepare,
    Detect,
    Install,
    Verify,
    Join,
    Contribute,
}

pub const STEPS: [Step; 7] = [
    Step::Python,
    Step::Prepare,
    Step::Detect,
    Step::Install,
    Step::Verify,
    Step::Join,
    Step::Contribute,
];

/// How bad a failure line is. v1's `errorRank` 1..=3; higher replaces lower.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    /// Any line that looks like an error (rank 1).
    Generic = 1,
    /// pip could not resolve a version (rank 2).
    Resolve = 2,
    /// GradMesh's own diagnosis (rank 3); wins over everything.
    Diagnosed = 3,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Milestone {
    /// `detected <gpu>` → step install.
    Detected { gpu: String },
    /// `creating a Python environment` / `rebuilding the Python environment`.
    CreatingEnv,
    /// `installing requirements-xpu.txt` → "xpu".
    Installing { what: String },
    /// `checking that <backend> actually runs` → step verify.
    Verifying { backend: String },
    /// `Contributing this GPU` → step join.
    Joining,
    /// `joined <server> as <node id>` → online.
    Joined { server: String, node_id: String },
    /// `round N shard S: K images` → training.
    Training {
        round: u32,
        shard: String,
        images: u64,
    },
    /// `finished shard S`.
    ShardDone,
    /// The coordinator stopped answering.
    Disconnected,
    /// The coordinator answers again.
    Reconnected,
    /// The mesh token was rejected; restarting will not help.
    TokenRejected,
    /// A failure explanation, ranked.
    Failure { message: String, severity: Severity },
}

impl Milestone {
    /// The stepper position this milestone moves to, if it moves it.
    pub fn step(&self) -> Option<Step> {
        match self {
            Milestone::Detected { .. } | Milestone::CreatingEnv | Milestone::Installing { .. } => {
                Some(Step::Install)
            }
            Milestone::Verifying { .. } => Some(Step::Verify),
            Milestone::Joining => Some(Step::Join),
            Milestone::Joined { .. } | Milestone::Training { .. } | Milestone::ShardDone => {
                Some(Step::Contribute)
            }
            _ => None,
        }
    }

    /// The human sentence v1 put under the stepper (`detail`).
    pub fn detail(&self) -> Option<String> {
        Some(match self {
            Milestone::Detected { .. } => "Installing the PyTorch build for this GPU".into(),
            Milestone::CreatingEnv => "Creating a private Python environment".into(),
            Milestone::Installing { what } => format!("Installing {what} packages"),
            Milestone::Verifying { backend } => format!("Running a test kernel on {backend}"),
            Milestone::Joining => "Measuring the device and joining".into(),
            Milestone::Joined { .. } => "Contributing. Waiting for work.".into(),
            Milestone::Training { round, images, .. } => {
                format!("Training round {round}: {images} images")
            }
            Milestone::ShardDone => "Shard delivered. Waiting for the next round.".into(),
            Milestone::Disconnected => "The host stopped answering. Reconnecting.".into(),
            Milestone::Reconnected => "Reconnected.".into(),
            Milestone::TokenRejected => {
                "The mesh token was rejected. Ask the host for a fresh invite.".into()
            }
            Milestone::Failure { message, .. } => message.clone(),
        })
    }
}

macro_rules! re {
    ($name:ident, $pat:expr) => {
        static $name: LazyLock<Regex> = LazyLock::new(|| Regex::new($pat).expect("valid regex"));
    };
}

re!(DETECTED, r"^detected (.+)$");
re!(
    CREATING,
    r"^(creating a Python environment|rebuilding the Python environment)"
);
re!(INSTALLING, r"^installing (\S+)");
re!(CHECKING, r"^checking that (\S+) actually runs");
re!(CONTRIBUTING, r"^Contributing this GPU");
re!(JOINED, r"joined (\S+) as ([0-9a-f]{8,})");
re!(ROUND, r"round (\d+) shard (\S+): (\d+) images");
re!(FINISHED, r"finished shard");
re!(
    DISCONNECTED,
    r"stopped answering|is unreachable|looking for the coordinator"
);
re!(RECONNECTED, r"reachable again|found the coordinator at");
re!(TOKEN_REJECTED, r"(?i)token was rejected");
re!(PIP_FAILED, r"pip could not install (\S+)");
re!(
    DIAGNOSED,
    r"^(The GPU could not run PyTorch|GradMesh needs|No GradMesh coordinator was found)"
);
re!(
    RESOLVE,
    r"No matching distribution|Could not find a version"
);
re!(
    PIP_NOISE,
    r"^\s*(WARNING:|Requirement already satisfied|Looking in indexes|Collecting |Downloading |Using cached )"
);
re!(PIP_WARNING, r"^\s*WARNING:");
re!(ERRORISH, r"(?i)error|failed|could not|rejected|traceback");
re!(IGNORED_ERROR, r"^\s*(WARNING|ERROR: Ignored)");
re!(FIX, r"^(Install|Fix|Update|Run) ");
re!(FIX_CMD, r"winget install|brew install|apt install");
re!(REQ_AFFIX, r"^requirements-|\.txt\.?$");

/// `requirements-train-cu130.txt` → `train-cu130` (v1's `.replace(/^requirements-|\.txt$/g, "")`).
fn package_set(name: &str) -> String {
    REQ_AFFIX.replace_all(name, "").into_owned()
}

/// One line → the milestone it marks, if any. Mirrors `Agent.onLine`.
pub fn classify(line: &str) -> Option<Milestone> {
    let text = line.trim_start();
    if let Some(m) = DETECTED.captures(text) {
        return Some(Milestone::Detected {
            gpu: m[1].to_string(),
        });
    }
    if CREATING.is_match(text) {
        return Some(Milestone::CreatingEnv);
    }
    if let Some(m) = INSTALLING.captures(text) {
        return Some(Milestone::Installing {
            what: package_set(&m[1]),
        });
    }
    if let Some(m) = CHECKING.captures(text) {
        return Some(Milestone::Verifying {
            backend: m[1].to_string(),
        });
    }
    if CONTRIBUTING.is_match(text) {
        return Some(Milestone::Joining);
    }
    if let Some(m) = JOINED.captures(text) {
        return Some(Milestone::Joined {
            server: m[1].to_string(),
            node_id: m[2].to_string(),
        });
    }
    if let Some(m) = ROUND.captures(text) {
        return Some(Milestone::Training {
            round: m[1].parse().unwrap_or(0),
            shard: m[2].to_string(),
            images: m[3].parse().unwrap_or(0),
        });
    }
    if FINISHED.is_match(text) {
        return Some(Milestone::ShardDone);
    }
    if DISCONNECTED.is_match(text) {
        return Some(Milestone::Disconnected);
    }
    if RECONNECTED.is_match(text) {
        return Some(Milestone::Reconnected);
    }
    if TOKEN_REJECTED.is_match(text) {
        return Some(Milestone::TokenRejected);
    }
    if let Some(m) = PIP_FAILED.captures(text) {
        return Some(Milestone::Failure {
            message: format!(
                "Installing {} packages failed. Check this computer's internet connection \
                 (download.pytorch.org must be reachable), then try again.",
                package_set(&m[1])
            ),
            severity: Severity::Diagnosed,
        });
    }
    if DIAGNOSED.is_match(text) {
        return Some(Milestone::Failure {
            message: text.to_string(),
            severity: Severity::Diagnosed,
        });
    }
    if RESOLVE.is_match(text) {
        return Some(Milestone::Failure {
            message: text
                .strip_prefix("ERROR:")
                .map(str::trim_start)
                .unwrap_or(text)
                .to_string(),
            severity: Severity::Resolve,
        });
    }
    None
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    Info,
    Warn,
    Error,
}

/// v1's per-line level: pip chatter is info (its WARNINGs warn), error-ish
/// words are errors, other stderr is a warning.
pub fn level(line: &str, stderr: bool) -> Level {
    if PIP_NOISE.is_match(line) {
        if PIP_WARNING.is_match(line) {
            Level::Warn
        } else {
            Level::Info
        }
    } else if ERRORISH.is_match(line) {
        Level::Error
    } else if stderr {
        Level::Warn
    } else {
        Level::Info
    }
}

/// Remembers the most telling failure and fix lines of a run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FailureTracker {
    error: Option<(Severity, String)>,
    fix: Option<String>,
    token_rejected: bool,
}

impl FailureTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed every line (after [`classify`] or not; this classifies itself).
    pub fn observe(&mut self, line: &str, stderr: bool) {
        let text = line.trim_start();
        let lvl = level(line, stderr);
        match classify(line) {
            Some(Milestone::TokenRejected) => {
                self.token_rejected = true;
                self.error = Some((
                    Severity::Diagnosed,
                    "The mesh token was rejected. Ask the host for a fresh invite.".into(),
                ));
            }
            Some(Milestone::Failure { message, severity }) => {
                // Diagnosed always replaces (v1 assigns rank 3 unconditionally);
                // Resolve only replaces Generic.
                let replace = match &self.error {
                    None => true,
                    Some((s, _)) => severity == Severity::Diagnosed || *s < severity,
                };
                if replace {
                    self.error = Some((severity, message));
                }
            }
            Some(_) => {}
            None => {
                if lvl == Level::Error && !IGNORED_ERROR.is_match(text) && self.error.is_none() {
                    self.error = Some((Severity::Generic, text.to_string()));
                }
            }
        }
        if FIX.is_match(text) || FIX_CMD.is_match(text) {
            self.fix = Some(text.to_string());
        }
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_ref().map(|(_, m)| m.as_str())
    }

    pub fn severity(&self) -> Option<Severity> {
        self.error.as_ref().map(|(s, _)| *s)
    }

    pub fn fix(&self) -> Option<&str> {
        self.fix.as_deref()
    }

    /// v1 `onExit`: exit code 2 (GradMesh decided this machine cannot
    /// contribute as configured) or a rejected token mean retrying is futile.
    pub fn is_permanent(&self, exit_code: Option<i32>) -> bool {
        exit_code == Some(2) || self.token_rejected
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lines::strip_ansi;

    #[test]
    fn setup_env_lines() {
        // Real say() output, coloured as on a TTY.
        let raw = "\u{1b}[32m  detected NVIDIA GeForce RTX 4090\u{1b}[0m";
        assert_eq!(
            classify(&strip_ansi(raw)),
            Some(Milestone::Detected {
                gpu: "NVIDIA GeForce RTX 4090".into()
            })
        );
        assert_eq!(
            classify("  creating a Python environment at /home/u/.gradmesh/agent/.venv"),
            Some(Milestone::CreatingEnv)
        );
        assert_eq!(
            classify("  rebuilding the Python environment: torch missing"),
            Some(Milestone::CreatingEnv)
        );
        assert_eq!(
            classify("  installing requirements-train-cu130.txt"),
            Some(Milestone::Installing {
                what: "train-cu130".into()
            })
        );
        assert_eq!(
            classify("  checking that cuda actually runs"),
            Some(Milestone::Verifying {
                backend: "cuda".into()
            })
        );
        assert_eq!(
            classify("  Contributing this GPU. Press Ctrl+C to leave the mesh."),
            Some(Milestone::Joining)
        );
        assert_eq!(Milestone::Joining.step(), Some(Step::Join));
    }

    #[test]
    fn worker_lines() {
        assert_eq!(
            classify("[worker 12:01:02] joined http://192.168.1.20:8000 as 3f9a1c22b0d4 (trusted: same LAN)"),
            Some(Milestone::Joined { server: "http://192.168.1.20:8000".into(), node_id: "3f9a1c22b0d4".into() })
        );
        assert_eq!(
            classify("[worker 12:05:00] round 3 shard 7: 1200 images, batch 32"),
            Some(Milestone::Training {
                round: 3,
                shard: "7".into(),
                images: 1200
            })
        );
        assert_eq!(
            classify("[worker 12:09:00] finished shard 7: epoch 41.2s, setup 3.0s, fetched 120 MB"),
            Some(Milestone::ShardDone)
        );
        assert_eq!(
            classify("[worker 12:10:00] the coordinator stopped answering; will keep trying"),
            Some(Milestone::Disconnected)
        );
        assert_eq!(
            classify("[worker 12:10:00] http://10.0.0.2:8000 is unreachable, looking for the coordinator on this network"),
            Some(Milestone::Disconnected)
        );
        assert_eq!(
            classify("[worker 12:11:00] the coordinator is reachable again"),
            Some(Milestone::Reconnected)
        );
        assert_eq!(
            classify("[worker 12:11:00] found the coordinator at http://10.0.0.3:8000"),
            Some(Milestone::Reconnected)
        );
        assert_eq!(
            classify(
                "[worker] The mesh token was rejected. Copy a fresh join command from the host."
            ),
            Some(Milestone::TokenRejected)
        );
        assert_eq!(
            classify("[worker 12:00:00] keeping this machine awake"),
            None
        );
        assert_eq!(classify("Collecting torch==2.9.0"), None);
    }

    #[test]
    fn failures_rank_like_v1() {
        let mut t = FailureTracker::new();
        t.observe("Traceback (most recent call last):", true);
        assert_eq!(t.error(), Some("Traceback (most recent call last):"));
        t.observe(
            "ERROR: Could not find a version that satisfies the requirement torch==9",
            true,
        );
        assert_eq!(t.severity(), Some(Severity::Resolve));
        assert_eq!(
            t.error(),
            Some("Could not find a version that satisfies the requirement torch==9")
        );
        t.observe(
            "  pip could not install requirements-train-cu130.txt.",
            false,
        );
        assert!(t
            .error()
            .unwrap()
            .starts_with("Installing train-cu130 packages failed."));
        // A later resolve error does not displace a diagnosis.
        t.observe("ERROR: No matching distribution found for torch", true);
        assert_eq!(t.severity(), Some(Severity::Diagnosed));
        t.observe(
            "  Install the NVIDIA driver 570 or newer: https://www.nvidia.com/drivers",
            false,
        );
        assert!(t.fix().unwrap().starts_with("Install the NVIDIA driver"));
        assert!(t.is_permanent(Some(2)));
        assert!(!t.is_permanent(Some(1)));
        t.observe("token was rejected", false);
        assert!(t.is_permanent(Some(1)));
        t.reset();
        assert_eq!(t.error(), None);
    }

    #[test]
    fn generic_error_does_not_replace() {
        let mut t = FailureTracker::new();
        t.observe(
            "  GradMesh needs Python 3.10 to 3.13; this is 3.14.0. Install it with:",
            false,
        );
        t.observe("RuntimeError: boom", true);
        assert!(t.error().unwrap().starts_with("GradMesh needs"));
        t.observe("    winget install Python.Python.3.12", false);
        assert_eq!(t.fix(), Some("winget install Python.Python.3.12"));
        // pip warnings are never the error.
        let mut t = FailureTracker::new();
        t.observe(
            "WARNING: Retrying (Retry(total=4)) after connection broken: failed",
            true,
        );
        assert_eq!(t.error(), None);
    }

    #[test]
    fn levels() {
        assert_eq!(level("WARNING: pip is old", true), Level::Warn);
        assert_eq!(
            level("Downloading torch-2.9.0.whl (failed?)", false),
            Level::Info
        );
        assert_eq!(level("RuntimeError: CUDA failed", false), Level::Error);
        assert_eq!(level("INFO: started", true), Level::Warn);
        assert_eq!(level("hello", false), Level::Info);
    }
}
