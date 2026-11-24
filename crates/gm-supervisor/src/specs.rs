//! Launch specs for the engine's Python processes, mirroring GradApp 1.
//!
//! Every argument and variable below names the v1 line it comes from, so a
//! change to the engine's CLI can be traced to both apps.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

use gm_types::{Backend, ProcRole};

use crate::{ProcSpec, RestartPolicy, StopSignal};

/// Variables every v1 child got: `proc.js::start` sets
/// `PYTHONUNBUFFERED=1` (lines arrive as they are printed, not in 4 KB
/// blocks) and `PYTHONIOENCODING=utf-8` (a GPU name with a ™ must not crash
/// a Windows cp1252 console encoder).
fn python_spec(role: ProcRole, python: &Path) -> ProcSpec {
    ProcSpec::new(role, python.as_os_str())
        .env("PYTHONUNBUFFERED", "1")
        .env("PYTHONIOENCODING", "utf-8")
}

/// The worker's `--backend` value. worker.py accepts
/// `auto|cuda|xpu|mps|cpu` (worker.py `parse_args`); ROCm PyTorch exposes
/// its GPUs through `torch.cuda`, so ROCm runs as `cuda`.
pub fn worker_backend_arg(backend: Option<Backend>) -> &'static str {
    match backend {
        None => "auto",
        Some(Backend::Cuda) | Some(Backend::Rocm) => "cuda",
        Some(Backend::Xpu) => "xpu",
        Some(Backend::Mps) => "mps",
        Some(Backend::Cpu) => "cpu",
    }
}

/// How to start `engine/worker.py` (contributing this machine's GPU).
/// `Debug` shows everything but the token.
#[derive(Clone, PartialEq)]
pub struct WorkerLaunch {
    /// The port's interpreter (gm-ports), replacing v1's venv python from
    /// `setup_env.py run_agent` (`venv_python(venv)`).
    pub python: PathBuf,
    /// Directory holding worker.py and its sibling modules (v1: the agent
    /// home the files were copied to, `agent.js::prepareFiles`).
    pub engine_dir: PathBuf,
    /// Coordinator URL (v1 `target.coordinator`).
    pub server_url: String,
    /// Mesh join token (v1 `target.token`).
    pub token: String,
    /// Dashboard display name (v1 `options.name`).
    pub name: Option<String>,
    /// None = let the worker decide (v1 `options.backend || "auto"`).
    pub backend: Option<Backend>,
    /// v1 `profile.gpu_index`.
    pub gpu_index: Option<u32>,
    /// v1 `options.owner` → `GRADMESH_OWNER`.
    pub owner: Option<String>,
    /// v1 `GRADMESH_AGENT_HOME` (shared with the one-line join, ~/.gradmesh/agent).
    pub agent_home: Option<PathBuf>,
    /// worker.py `GRADMESH_WORKER_HOME` (locks, caches; default ~/.gradmesh).
    pub worker_home: Option<PathBuf>,
    /// Re-find the coordinator if its address changes. setup_env.py
    /// `run_agent` always passes `--discover`; keep it true unless testing.
    pub discover: bool,
    /// Anything else, appended verbatim (e.g. `--max-batch-size 32`).
    pub extra_args: Vec<String>,
}

impl std::fmt::Debug for WorkerLaunch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkerLaunch")
            .field("python", &self.python)
            .field("engine_dir", &self.engine_dir)
            .field("server_url", &self.server_url)
            .field("token", &"<redacted>")
            .field("name", &self.name)
            .field("backend", &self.backend)
            .field("gpu_index", &self.gpu_index)
            .field("owner", &self.owner)
            .field("agent_home", &self.agent_home)
            .field("worker_home", &self.worker_home)
            .field("discover", &self.discover)
            .field("extra_args", &self.extra_args)
            .finish()
    }
}

impl WorkerLaunch {
    pub fn new(
        python: impl Into<PathBuf>,
        engine_dir: impl Into<PathBuf>,
        server_url: &str,
        token: &str,
    ) -> Self {
        WorkerLaunch {
            python: python.into(),
            engine_dir: engine_dir.into(),
            server_url: server_url.to_string(),
            token: token.to_string(),
            name: None,
            backend: None,
            gpu_index: None,
            owner: None,
            agent_home: None,
            worker_home: None,
            discover: true,
            extra_args: vec![],
        }
    }
}

/// `python worker.py --server-url URL --backend B [--gpu-index N] --discover [--name NAME] …`
///
/// v1 ran `setup_env.py agent …` (agent.js `start`), which installed PyTorch
/// and then ran exactly this worker command (setup_env.py `run_agent`,
/// `command = [...]`). In v2 gm-ports provisions the environment, so we start
/// the worker directly with the same arguments.
///
/// One deliberate change: the token travels as `GRADMESH_TOKEN` (worker.py
/// `--token` defaults to it) instead of argv, so it does not show up in
/// `ps` / Task Manager command lines.
pub fn worker_spec(w: &WorkerLaunch) -> ProcSpec {
    let mut spec = python_spec(ProcRole::Worker, &w.python)
        // setup_env.py run_agent: str(home / "worker.py")
        .arg(w.engine_dir.join("worker.py"))
        // run_agent: "--server-url", args.server (agent.js passes target.coordinator as --server)
        .args(["--server-url", w.server_url.as_str()])
        // run_agent: "--backend", profile.backend (agent.js: options.backend || "auto")
        .args(["--backend", worker_backend_arg(w.backend)]);
    if let Some(i) = w.gpu_index {
        // run_agent: "--gpu-index", str(profile.gpu_index)
        spec = spec.args([OsString::from("--gpu-index"), OsString::from(i.to_string())]);
    }
    if w.discover {
        // run_agent: "--discover"
        spec = spec.arg("--discover");
    }
    if let Some(name) = w.name.as_deref().filter(|n| !n.is_empty()) {
        // run_agent: if args.name: command += ["--name", args.name]
        spec = spec.args(["--name", name]);
    }
    spec = spec.args(w.extra_args.iter().map(String::as_str));

    // worker.py parse_args: --token default=env("GRADMESH_TOKEN")
    spec = spec.env("GRADMESH_TOKEN", &w.token);
    // agent.js start: env GRADMESH_OWNER: options.owner || ""
    spec = spec.env("GRADMESH_OWNER", w.owner.as_deref().unwrap_or(""));
    if let Some(home) = &w.agent_home {
        // agent.js start: env GRADMESH_AGENT_HOME: home
        spec = spec.env("GRADMESH_AGENT_HOME", home.as_os_str());
    }
    if let Some(home) = &w.worker_home {
        // worker.py WORKER_HOME = GRADMESH_WORKER_HOME
        spec = spec.env("GRADMESH_WORKER_HOME", home.as_os_str());
    }
    // host.js sets this for its children; the bundled engine dir may be
    // read-only (Program Files, a signed .app), so never write .pyc there.
    spec = spec.env("PYTHONDONTWRITEBYTECODE", "1");

    spec.cwd(&w.engine_dir)
        // proc.js stopTree: SIGINT, so worker.py's KeyboardInterrupt path
        // runs agent.leave() and the host reassigns the shard at once.
        .stop_signal(StopSignal::Interrupt)
        // proc.js stopTree default graceMs = 6000
        .grace(Duration::from_secs(6))
        .restart(RestartPolicy {
            // agent.js onExit: delay = min(60, 5 * 2 ** min(restarts - 1, 4)) seconds
            backoff_base: Duration::from_secs(5),
            backoff_cap: Duration::from_secs(60),
            // v1 retried forever while contributing; v2 gives up after 10
            // fast failures in a row (a run of 2 min resets the count, like
            // v1's `this.restarts = 0` on "joined").
            max_restarts: 10,
            reset_after: Duration::from_secs(120),
            // agent.js onExit: code 2 = "this machine cannot contribute as
            // configured"; worker.py main: 3 = another worker holds this GPU.
            permanent_exit_codes: vec![2, 3],
            restart_on_clean_exit: false,
        })
}

/// How to start the FastAPI coordinator (hosting a mesh).
#[derive(Debug, Clone, PartialEq)]
pub struct CoordinatorLaunch {
    /// Host venv interpreter (v1 `venvPython(PATHS.hostVenv)`).
    pub python: PathBuf,
    /// Contains the `coordinator` package (v1 `engineDir()`).
    pub engine_dir: PathBuf,
    /// v1 `this.port`, default 8000.
    pub port: u16,
    /// v1 `this.webPort`, default 3000.
    pub web_port: u16,
    /// v1 `this.stateDir` (PATHS.defaultMeshState). The coordinator creates
    /// and keeps the mesh token in `<state_dir>/coordinator.json`; there is
    /// no token argument (host.js `readToken`).
    pub state_dir: PathBuf,
    /// v1 always bound 0.0.0.0 so other machines can join.
    pub bind: String,
}

impl CoordinatorLaunch {
    pub fn new(
        python: impl Into<PathBuf>,
        engine_dir: impl Into<PathBuf>,
        state_dir: impl Into<PathBuf>,
    ) -> Self {
        CoordinatorLaunch {
            python: python.into(),
            engine_dir: engine_dir.into(),
            port: gm_types::COORDINATOR_PORT,
            web_port: gm_types::WEB_PORT,
            state_dir: state_dir.into(),
            bind: "0.0.0.0".into(),
        }
    }
}

/// `python -m uvicorn coordinator.app:app --host 0.0.0.0 --port 8000
/// --log-level warning --timeout-keep-alive 75` (host.js `startCoordinator`).
pub fn coordinator_spec(c: &CoordinatorLaunch) -> ProcSpec {
    python_spec(ProcRole::Coordinator, &c.python)
        // host.js startCoordinator args, in order
        .args(["-m", "uvicorn", "coordinator.app:app"])
        .args(["--host", c.bind.as_str()])
        .args([OsString::from("--port"), OsString::from(c.port.to_string())])
        .args(["--log-level", "warning"])
        .args(["--timeout-keep-alive", "75"])
        // host.js startCoordinator env
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .env("GRADMESH_STATE_DIR", c.state_dir.as_os_str())
        .env("GRADMESH_WEB_PORT", c.web_port.to_string())
        .env("GRADMESH_COORDINATOR_PORT", c.port.to_string())
        // host.js: cwd: engineDir() (uvicorn imports coordinator.app from cwd)
        .cwd(&c.engine_dir)
        // uvicorn shuts down gracefully on SIGTERM
        .stop_signal(StopSignal::Terminate)
        // host.js stop: stopTree(this.coordinatorProcess, 4000)
        .grace(Duration::from_secs(4))
        // v1 never restarted the coordinator (it showed an error). v2 tries
        // three times, quickly: connected workers reconnect on their own.
        .restart(RestartPolicy {
            max_restarts: 3,
            backoff_base: Duration::from_secs(2),
            backoff_cap: Duration::from_secs(30),
            reset_after: Duration::from_secs(300),
            permanent_exit_codes: vec![],
            restart_on_clean_exit: false,
        })
}

/// How to run `setup_env.py install` (host control/training plane).
#[derive(Debug, Clone, PartialEq)]
pub struct SetupLaunch {
    pub python: PathBuf,
    pub engine_dir: PathBuf,
    /// v1 PATHS.hostVenv
    pub venv: PathBuf,
    /// "control" or "training" (host.js installPlane)
    pub plane: String,
    /// v1 PATHS.hostSetup
    pub state_file: PathBuf,
}

/// `python setup_env.py install --venv V --plane P --host --state S` (host.js `installPlane`).
/// Runs to completion; never restarted.
pub fn setup_spec(s: &SetupLaunch) -> ProcSpec {
    python_spec(ProcRole::Installer, &s.python)
        .arg(s.engine_dir.join("setup_env.py"))
        .arg("install")
        .arg("--venv")
        .arg(s.venv.as_os_str())
        .args(["--plane", s.plane.as_str()])
        .arg("--host")
        .arg("--state")
        .arg(s.state_file.as_os_str())
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .cwd(&s.engine_dir)
        // setup_env.py catches KeyboardInterrupt
        .stop_signal(StopSignal::Interrupt)
        // host.js stop: stopTree(child, 2000) for installs
        .grace(Duration::from_secs(2))
        .restart(RestartPolicy::never())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(spec: &ProcSpec) -> Vec<String> {
        spec.args
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn worker_matches_setup_env_run_agent() {
        let mut w = WorkerLaunch::new("/py/bin/python", "/eng", "http://10.0.0.2:8000", "tok123");
        w.name = Some("Ujesh's 4090".into());
        w.backend = Some(Backend::Cuda);
        w.gpu_index = Some(1);
        w.owner = Some("ujesh".into());
        w.agent_home = Some("/home/u/.gradmesh/agent".into());
        let s = worker_spec(&w);
        assert_eq!(s.role, ProcRole::Worker);
        assert_eq!(s.program, OsString::from("/py/bin/python"));
        let script = PathBuf::from("/eng")
            .join("worker.py")
            .to_string_lossy()
            .into_owned();
        assert_eq!(
            args(&s),
            vec![
                script.as_str(),
                "--server-url",
                "http://10.0.0.2:8000",
                "--backend",
                "cuda",
                "--gpu-index",
                "1",
                "--discover",
                "--name",
                "Ujesh's 4090"
            ]
        );
        assert!(
            !args(&s).iter().any(|a| a.contains("tok123")),
            "token must not be on argv"
        );
        assert_eq!(s.env_value("GRADMESH_TOKEN").unwrap(), "tok123");
        assert_eq!(s.env_value("GRADMESH_OWNER").unwrap(), "ujesh");
        assert_eq!(
            s.env_value("GRADMESH_AGENT_HOME").unwrap(),
            "/home/u/.gradmesh/agent"
        );
        assert_eq!(s.env_value("PYTHONUNBUFFERED").unwrap(), "1");
        assert_eq!(s.env_value("PYTHONIOENCODING").unwrap(), "utf-8");
        assert_eq!(s.cwd.as_deref(), Some(std::path::Path::new("/eng")));
        assert_eq!(s.stop_signal, StopSignal::Interrupt);
        assert_eq!(s.restart.permanent_exit_codes, vec![2, 3]);
    }

    #[test]
    fn worker_defaults() {
        let s = worker_spec(&WorkerLaunch::new("py", "e", "http://h:8000", "t"));
        let a = args(&s);
        assert!(a.windows(2).any(|p| p == ["--backend", "auto"]));
        assert!(!a.contains(&"--gpu-index".to_string()));
        assert!(!a.contains(&"--name".to_string()));
        assert_eq!(worker_backend_arg(Some(Backend::Rocm)), "cuda");
    }

    #[test]
    fn coordinator_matches_host_js() {
        let c = CoordinatorLaunch::new("/venv/bin/python", "/eng", "/state");
        let s = coordinator_spec(&c);
        assert_eq!(s.role, ProcRole::Coordinator);
        assert_eq!(
            args(&s),
            vec![
                "-m",
                "uvicorn",
                "coordinator.app:app",
                "--host",
                "0.0.0.0",
                "--port",
                "8000",
                "--log-level",
                "warning",
                "--timeout-keep-alive",
                "75"
            ]
        );
        assert_eq!(s.env_value("GRADMESH_STATE_DIR").unwrap(), "/state");
        assert_eq!(s.env_value("GRADMESH_WEB_PORT").unwrap(), "3000");
        assert_eq!(s.env_value("GRADMESH_COORDINATOR_PORT").unwrap(), "8000");
        assert_eq!(s.env_value("PYTHONDONTWRITEBYTECODE").unwrap(), "1");
        assert_eq!(s.cwd.as_deref(), Some(std::path::Path::new("/eng")));
        assert_eq!(s.grace, Duration::from_secs(4));
    }

    #[test]
    fn setup_matches_install_plane() {
        let s = setup_spec(&SetupLaunch {
            python: "py".into(),
            engine_dir: "/eng".into(),
            venv: "/v".into(),
            plane: "control".into(),
            state_file: "/s.json".into(),
        });
        let script = PathBuf::from("/eng")
            .join("setup_env.py")
            .to_string_lossy()
            .into_owned();
        assert_eq!(
            args(&s),
            vec![
                script.as_str(),
                "install",
                "--venv",
                "/v",
                "--plane",
                "control",
                "--host",
                "--state",
                "/s.json"
            ]
        );
        assert_eq!(s.role, ProcRole::Installer);
        assert_eq!(s.restart.max_restarts, 0);
    }
}
