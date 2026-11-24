//! Real child processes under the supervisor (Unix shells; Windows CI skips).
#![cfg(unix)]

use std::time::{Duration, Instant};

use gm_supervisor::{Event, ProcSpec, RestartPolicy, StopSignal, Stream, Supervisor};
use gm_types::{ProcRole, ProcState};

fn sh(script: &str) -> ProcSpec {
    ProcSpec::new(ProcRole::Worker, "sh").arg("-c").arg(script)
}

fn quick(max: u32) -> RestartPolicy {
    RestartPolicy {
        max_restarts: max,
        backoff_base: Duration::from_millis(100),
        backoff_cap: Duration::from_millis(400),
        reset_after: Duration::from_secs(60),
        permanent_exit_codes: vec![2],
        restart_on_clean_exit: false,
    }
}

async fn within<T>(secs: u64, f: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(secs), f)
        .await
        .expect("timed out")
}

/// Poll the ring buffer until a line matches (last_line updates do not wake
/// status watchers by design, so tests poll).
async fn line_seen(h: &gm_supervisor::Handle, pred: impl Fn(&str) -> bool) {
    within(10, async {
        while !h.recent_lines().iter().any(|l| pred(&l.line)) {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
}

fn alive(pid: i32) -> bool {
    // SAFETY: signal 0 only checks existence.
    unsafe { libc::kill(pid, 0) == 0 }
}

#[tokio::test]
async fn streams_lines_strips_ansi_and_exits_clean() {
    let h = Supervisor::spawn(sh(
        r#"printf 'one\r\ntwo\n\033[32mgreen\033[0m\n'; printf 'err line\n' >&2; printf ' 10%%\r 99%%\rdone'"#,
    ));
    let mut rx = h.subscribe();
    let st = within(10, h.wait()).await;
    assert_eq!(st.state, ProcState::Stopped);
    assert_eq!(st.exit_code, Some(0));
    let lines: Vec<(Stream, String)> = h
        .recent_lines()
        .into_iter()
        .map(|l| (l.stream, l.line))
        .collect();
    let out: Vec<&str> = lines
        .iter()
        .filter(|l| l.0 == Stream::Stdout)
        .map(|l| l.1.as_str())
        .collect();
    assert_eq!(out, vec!["one", "two", "green", " 10%", " 99%", "done"]);
    assert!(lines.contains(&(Stream::Stderr, "err line".into())));
    assert!(st.last_line.is_some());
    // The broadcast saw lines and a terminal status too.
    let mut saw_line = false;
    let mut saw_stopped = false;
    while let Ok(ev) = rx.try_recv() {
        match ev {
            Event::Line(_) => saw_line = true,
            Event::Status(s) => saw_stopped |= s.state == ProcState::Stopped,
        }
    }
    assert!(saw_line && saw_stopped);
}

#[tokio::test]
async fn env_and_cwd_are_applied() {
    let dir = tempfile::tempdir().unwrap();
    let h = Supervisor::spawn(
        sh("echo \"$GM_TEST_VAR\"; pwd")
            .env("GM_TEST_VAR", "hello-env")
            .cwd(dir.path()),
    );
    within(10, h.wait()).await;
    let lines: Vec<String> = h.recent_lines().into_iter().map(|l| l.line).collect();
    assert_eq!(lines[0], "hello-env");
    let real = std::fs::canonicalize(dir.path()).unwrap();
    assert_eq!(std::fs::canonicalize(&lines[1]).unwrap(), real);
}

#[tokio::test]
async fn ring_buffer_keeps_last_n() {
    let h = Supervisor::spawn(
        sh("i=0; while [ $i -lt 50 ]; do echo line$i; i=$((i+1)); done").ring_lines(10),
    );
    within(10, h.wait()).await;
    let lines: Vec<String> = h.recent_lines().into_iter().map(|l| l.line).collect();
    assert_eq!(lines.len(), 10);
    assert_eq!(lines.first().unwrap(), "line40");
    assert_eq!(lines.last().unwrap(), "line49");
}

#[tokio::test]
async fn crash_restarts_with_backoff_then_fails() {
    let started = Instant::now();
    let h = Supervisor::spawn(sh("echo run; exit 1").restart(quick(3)));
    let mut rx = h.subscribe();
    let st = within(20, h.wait()).await;
    assert_eq!(st.state, ProcState::Failed);
    assert_eq!(st.restarts, 3);
    assert_eq!(st.exit_code, Some(1));
    // 100 + 200 + 400 ms of backoff at least.
    assert!(
        started.elapsed() >= Duration::from_millis(700),
        "{:?}",
        started.elapsed()
    );
    let runs = h.recent_lines().iter().filter(|l| l.line == "run").count();
    assert_eq!(runs, 4, "first run + 3 restarts");
    let mut states = vec![];
    while let Ok(ev) = rx.try_recv() {
        if let Event::Status(s) = ev {
            states.push(s.state);
        }
    }
    assert!(states.contains(&ProcState::Restarting));
}

#[tokio::test]
async fn permanent_exit_code_does_not_restart() {
    let h = Supervisor::spawn(sh("exit 2").restart(quick(5)));
    let st = within(10, h.wait()).await;
    assert_eq!(st.state, ProcState::Failed);
    assert_eq!(st.restarts, 0);
    assert_eq!(st.exit_code, Some(2));
}

#[tokio::test]
async fn recovers_after_transient_crash() {
    let dir = tempfile::tempdir().unwrap();
    let flag = dir.path().join("ran-once");
    let script = format!(
        "if [ -e '{0}' ]; then echo second; exit 0; else touch '{0}'; echo first; exit 7; fi",
        flag.display()
    );
    let h = Supervisor::spawn(sh(&script).restart(quick(3)));
    let st = within(10, h.wait()).await;
    assert_eq!(st.state, ProcState::Stopped);
    assert_eq!(st.restarts, 1);
    assert_eq!(st.exit_code, Some(0));
}

#[tokio::test]
async fn stop_is_graceful_and_never_restarts() {
    // Traps SIGTERM, says goodbye, exits 0.
    let h = Supervisor::spawn(
        sh("trap 'echo bye; exit 0' TERM; echo ready; while true; do sleep 0.05; done")
            .restart(quick(5)),
    );
    line_seen(&h, |l| l == "ready").await;
    let st = within(10, h.stop()).await;
    assert_eq!(st.state, ProcState::Stopped);
    assert_eq!(st.restarts, 0);
    assert!(h.recent_lines().iter().any(|l| l.line == "bye"));
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(h.status().state, ProcState::Stopped);
    assert!(h.is_finished());
}

#[tokio::test]
async fn stop_kills_after_grace_when_ignored() {
    let h = Supervisor::spawn(
        sh("trap '' INT TERM; echo stubborn; while true; do sleep 0.05; done")
            .stop_signal(StopSignal::Interrupt)
            .grace(Duration::from_millis(300))
            .restart(quick(5)),
    );
    line_seen(&h, |l| l == "stubborn").await;
    let t = Instant::now();
    let st = within(10, h.stop()).await;
    assert!(t.elapsed() >= Duration::from_millis(300));
    assert_eq!(st.state, ProcState::Stopped);
    assert!(h
        .recent_lines()
        .iter()
        .any(|l| l.stream == Stream::Supervisor && l.line.contains("killing")));
}

#[tokio::test]
async fn stop_during_backoff() {
    let mut p = quick(5);
    p.backoff_base = Duration::from_secs(30);
    p.backoff_cap = Duration::from_secs(30);
    let h = Supervisor::spawn(sh("exit 1").restart(p));
    let mut w = h.status_watch();
    within(10, w.wait_for(|s| s.state == ProcState::Restarting))
        .await
        .unwrap();
    let st = within(5, h.stop()).await;
    assert_eq!(st.state, ProcState::Stopped);
}

#[tokio::test]
async fn stop_takes_the_grandchild_too() {
    let dir = tempfile::tempdir().unwrap();
    let pidfile = dir.path().join("grandchild.pid");
    // The grandchild ignores nothing but is a separate process the shell
    // does not wait for; only a group kill reaches it.
    let script = format!(
        "sleep 300 & echo $! > '{}'; echo started; wait",
        pidfile.display()
    );
    let h = Supervisor::spawn(sh(&script).grace(Duration::from_millis(500)));
    line_seen(&h, |l| l == "started").await;
    let gpid: i32 = std::fs::read_to_string(&pidfile)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(alive(gpid));
    within(10, h.stop()).await;
    let deadline = Instant::now() + Duration::from_secs(3);
    while alive(gpid) && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    // It may linger as a zombie until init reaps it; check it is not running.
    let stat = std::fs::read_to_string(format!("/proc/{gpid}/stat")).unwrap_or_default();
    assert!(
        !alive(gpid) || stat.contains(") Z "),
        "grandchild {gpid} survived: {stat}"
    );
}

#[tokio::test]
async fn crash_sweeps_orphaned_grandchild() {
    let dir = tempfile::tempdir().unwrap();
    let pidfile = dir.path().join("g.pid");
    let script = format!("sleep 300 & echo $! > '{}'; exit 1", pidfile.display());
    let h = Supervisor::spawn(sh(&script));
    let st = within(10, h.wait()).await;
    assert_eq!(st.state, ProcState::Failed);
    let gpid: i32 = std::fs::read_to_string(&pidfile)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while alive(gpid) && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let stat = std::fs::read_to_string(format!("/proc/{gpid}/stat")).unwrap_or_default();
    assert!(!alive(gpid) || stat.contains(") Z "));
}

#[tokio::test]
async fn missing_program_fails_without_restart() {
    let h = Supervisor::spawn(
        ProcSpec::new(ProcRole::Coordinator, "/definitely/not/here").restart(quick(5)),
    );
    let st = within(5, h.wait()).await;
    assert_eq!(st.state, ProcState::Failed);
    assert!(h.recent_lines()[0].line.starts_with("could not start"));
}

#[tokio::test]
async fn dropping_the_last_handle_stops_the_tree() {
    let dir = tempfile::tempdir().unwrap();
    let pidfile = dir.path().join("p.pid");
    let script = format!(
        "echo $$ > '{}'; echo up; while true; do sleep 0.05; done",
        pidfile.display()
    );
    let h = Supervisor::spawn(sh(&script).grace(Duration::from_millis(300)));
    line_seen(&h, |l| l == "up").await;
    let pid: i32 = std::fs::read_to_string(&pidfile)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    drop(h);
    let deadline = Instant::now() + Duration::from_secs(5);
    while alive(pid) && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(!alive(pid));
}

#[tokio::test]
async fn python_child_lines_unbuffered() {
    if std::process::Command::new("python3")
        .arg("--version")
        .output()
        .is_err()
    {
        return;
    }
    let h = Supervisor::spawn(
        ProcSpec::new(ProcRole::Worker, "python3")
            .args(["-c", "import sys; print('[worker 12:00:00] joined http://h:8000 as 0123456789ab (t: r)'); print('caf\\u00e9', file=sys.stderr)"])
            .env("PYTHONIOENCODING", "utf-8"),
    );
    within(10, h.wait()).await;
    let lines = h.recent_lines();
    let joined = lines.iter().find_map(|l| gm_supervisor::classify(&l.line));
    assert!(matches!(
        joined,
        Some(gm_supervisor::Milestone::Joined { .. })
    ));
    assert!(lines
        .iter()
        .any(|l| l.line == "café" && l.stream == Stream::Stderr));
}
