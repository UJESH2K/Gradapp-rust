//! `gmctl` — GradApp without a window.
//!
//! The same crates the desktop app uses, behind a CLI, for three jobs:
//! a server or lab machine with no display, CI smoke tests, and debugging a
//! machine that "doesn't work" (`gmctl probe` + `gmctl ports status` answer
//! most support questions).

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use clap::{Parser, Subcommand};
use gm_ports::{Registry, VerifyMode};
use gm_probe::Prefer;
use gm_types::{AppEvent, ProcState};

#[derive(Parser)]
#[command(name = "gmctl", version, about = "GradApp headless CLI")]
struct Cli {
    /// Backend preference: auto, cuda, xpu, mps, cpu, rocm.
    #[arg(long, global = true, default_value = "auto")]
    prefer: String,
    /// Where ports live (default: the app's data folder).
    #[arg(long, global = true)]
    ports_dir: Option<PathBuf>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Versions of the app, engine and protocol.
    Version,
    /// Detect hardware and show the port this machine would run.
    Probe {
        #[arg(long)]
        json: bool,
        /// Show the profile a coordinator host would use.
        #[arg(long)]
        host: bool,
    },
    /// Inspect and manage ports.
    Ports {
        #[command(subcommand)]
        cmd: PortsCmd,
    },
    /// Find meshes on this network.
    Discover {
        /// Stop after this many seconds.
        #[arg(long, default_value_t = 6)]
        seconds: u64,
        #[arg(long)]
        json: bool,
        /// Extra addresses to try (IP, hostname, URL).
        #[arg(long)]
        manual: Vec<String>,
    },
    /// Install this machine's port if needed and contribute to a mesh (foreground).
    Join {
        /// IP, hostname, URL or gradapp:// invite link.
        address: String,
        #[arg(long)]
        token: Option<String>,
        #[arg(long)]
        name: Option<String>,
        /// Folder with worker.py (default: GRADAPP_ENGINE_DIR, next to the binary, or ./desktop/engine).
        #[arg(long)]
        engine: Option<PathBuf>,
    },
    /// Check GitHub for a newer GradApp.
    UpdateCheck {
        #[arg(long, default_value = "UJESH2K/Gradapp-rust")]
        repo: String,
        #[arg(long)]
        beta: bool,
    },
}

#[derive(Subcommand)]
enum PortsCmd {
    /// Every known port and whether it runs on this platform.
    List,
    /// The exact install plan (pins, index, fingerprint) for a port.
    Plan { port: Option<String> },
    /// Installed state of a port.
    Status { port: Option<String> },
    /// Provision a port (uv + private CPython + pinned packages + verify).
    Install {
        port: Option<String>,
        /// Skip the GPU kernel check (build machines without the hardware).
        #[arg(long)]
        skip_verify: bool,
        /// A folder of wheels to install from (offline labs).
        #[arg(long)]
        wheelhouse: Option<PathBuf>,
    },
    /// Delete every port environment except the given ones (default: this machine's).
    Gc { keep: Vec<String> },
}

fn prefer(cli: &Cli) -> Result<Prefer> {
    cli.prefer
        .parse()
        .map_err(|_| anyhow!("unknown --prefer {}", cli.prefer))
}

fn env_root(cli: &Cli) -> PathBuf {
    cli.ports_dir
        .clone()
        .unwrap_or_else(gm_ports::default_env_root)
}

/// The port id for this machine, or the one given.
fn pick_port(cli: &Cli, given: Option<String>) -> Result<String> {
    if let Some(id) = given {
        return Ok(id);
    }
    let facts = gm_probe::detect();
    Ok(gm_probe::select_profile(&facts, prefer(cli)?).port)
}

/// Like [`pick_port`], but an auto-picked port must be installable: a
/// blocked profile (driver too old, Intel Mac, no build for this CPU) stops
/// here with its fix instead of failing minutes later inside uv. Naming a
/// port explicitly still installs it.
fn pick_installable_port(cli: &Cli, given: Option<String>) -> Result<String> {
    if let Some(id) = given {
        return Ok(id);
    }
    let facts = gm_probe::detect();
    let profile = gm_probe::select_profile(&facts, prefer(cli)?);
    if let Some(blocked) = &profile.blocked {
        bail!(
            "this machine's port ({}) is blocked: {blocked}\n{}",
            profile.port,
            profile.fix.clone().unwrap_or_default()
        );
    }
    Ok(profile.port)
}

fn engine_dir(given: Option<PathBuf>) -> Result<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(g) = given {
        candidates.push(g);
    }
    if let Some(e) = std::env::var_os("GRADAPP_ENGINE_DIR") {
        candidates.push(e.into());
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join("engine"));
            candidates.push(dir.join("resources").join("engine"));
        }
    }
    candidates.push(PathBuf::from("desktop/engine"));
    candidates
        .into_iter()
        .find(|d| d.join("worker.py").is_file())
        .ok_or_else(|| anyhow!("engine folder with worker.py not found; pass --engine"))
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    match &cli.cmd {
        Cmd::Version => {
            println!("gmctl {}", gm_types::APP_VERSION);
            println!(
                "engine {}  protocol {}",
                gm_types::ENGINE_VERSION,
                gm_types::MESH_PROTOCOL
            );
            println!(
                "platform {}",
                gm_ports::current_platform().unwrap_or("unsupported")
            );
        }
        Cmd::Probe { json, host } => {
            let facts = gm_probe::detect();
            let p = prefer(&cli)?;
            let profile = if *host {
                gm_probe::host_profile(&facts, p)
            } else {
                gm_probe::select_profile(&facts, p)
            };
            if *json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(
                        &serde_json::json!({"facts": facts, "profile": profile})
                    )?
                );
            } else {
                println!("{}", gm_probe::summary(&facts));
                for g in &facts.gpus {
                    println!(
                        "  gpu{} {:?} {}  cc={:?} driver={} mem={:?}MB",
                        g.index,
                        g.vendor,
                        g.name,
                        g.compute_capability,
                        g.driver.as_deref().unwrap_or("-"),
                        g.memory_mb
                    );
                }
                println!(
                    "port   {}  ({}, torch {})",
                    profile.port, profile.label, profile.torch
                );
                println!("why    {}", profile.reason);
                for w in &profile.warnings {
                    println!("warn   {w}");
                }
                if let Some(b) = &profile.blocked {
                    println!("BLOCKED {b}");
                }
                if let Some(f) = &profile.fix {
                    println!("fix    {f}");
                }
            }
        }
        Cmd::Ports { cmd } => ports(&cli, cmd).await?,
        Cmd::Discover {
            seconds,
            json,
            manual,
        } => {
            let opts = gm_net::DiscoverOptions {
                manual: manual.clone(),
                ..Default::default()
            };
            let cancel = opts.cancel.clone();
            let secs = *seconds;
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_secs(secs)).await;
                cancel.cancel();
            });
            let quiet = *json;
            let meshes = gm_net::discover(opts, |e| {
                if let (false, AppEvent::MeshFound { mesh }) = (quiet, &e) {
                    eprintln!(
                        "  found {} via {:?} ({} nodes)",
                        mesh.coordinator, mesh.source, mesh.nodes_active
                    );
                }
            })
            .await;
            if *json {
                println!("{}", serde_json::to_string_pretty(&meshes)?);
            } else if meshes.is_empty() {
                println!("no mesh found");
            } else {
                for m in meshes {
                    println!(
                        "{}  mesh {}  v{}  {} nodes  {}ms  token:{}",
                        m.coordinator,
                        m.mesh_id,
                        m.version,
                        m.nodes_active,
                        m.latency_ms.unwrap_or(0),
                        if m.token.is_some() { "yes" } else { "no" }
                    );
                }
            }
        }
        Cmd::Join {
            address,
            token,
            name,
            engine,
        } => join(&cli, address, token.clone(), name.clone(), engine.clone()).await?,
        Cmd::UpdateCheck { repo, beta } => {
            let ch = if *beta {
                gm_update::Channel::Beta
            } else {
                gm_update::Channel::Stable
            };
            let info = gm_update::check(gm_types::APP_VERSION, repo, ch).await;
            println!("{}", serde_json::to_string_pretty(&info)?);
        }
    }
    Ok(())
}

async fn ports(cli: &Cli, cmd: &PortsCmd) -> Result<()> {
    let reg = Registry::from_env_or_builtin().context("loading port manifests")?;
    let root = env_root(cli);
    let fp = gm_ports::local_machine_fingerprint();
    match cmd {
        PortsCmd::List => {
            let here = gm_ports::current_platform();
            for p in reg.all() {
                let ok = here
                    .map(|h| p.platforms.iter().any(|x| x == h))
                    .unwrap_or(false);
                println!(
                    "{:<11} {:<5} py{} torch {:<7} {}{}{}",
                    p.id,
                    p.backend.as_str(),
                    p.python,
                    p.torch,
                    if ok { "runs here" } else { "-" },
                    if p.reference { "  reference" } else { "" },
                    if p.experimental { "  experimental" } else { "" }
                );
            }
        }
        PortsCmd::Plan { port } => {
            let id = pick_port(cli, port.clone())?;
            let spec = reg.require(&id)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&gm_ports::plan(spec, &fp))?
            );
        }
        PortsCmd::Status { port } => {
            let id = pick_port(cli, port.clone())?;
            let spec = reg.require(&id)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&gm_ports::status(spec, &root, &fp).await)?
            );
        }
        PortsCmd::Install {
            port,
            skip_verify,
            wheelhouse,
        } => {
            let id = pick_installable_port(cli, port.clone())?;
            let spec = reg.require(&id)?;
            install(spec, &root, *skip_verify, wheelhouse.clone()).await?;
        }
        PortsCmd::Gc { keep } => {
            let keep: Vec<String> = if keep.is_empty() {
                // This machine's worker port and the port it would host on
                // (they differ when the GPU is blocked: the host falls back
                // to cpu). Deleting the host port would break hosting.
                let facts = gm_probe::detect();
                let p = prefer(cli)?;
                let mut keep = vec![gm_probe::select_profile(&facts, p).port];
                let host = gm_probe::host_profile(&facts, p).port;
                if !keep.contains(&host) {
                    keep.push(host);
                }
                keep
            } else {
                keep.clone()
            };
            let refs: Vec<&str> = keep.iter().map(String::as_str).collect();
            for removed in gm_ports::gc(&root, &refs)? {
                println!("removed {removed}");
            }
        }
    }
    Ok(())
}

async fn install(
    spec: &gm_types::PortSpec,
    root: &std::path::Path,
    skip_verify: bool,
    wheelhouse: Option<PathBuf>,
) -> Result<gm_types::PortStatus> {
    let uv = gm_ports::uv::ensure(&gm_ports::data_dir()).await?;
    let opts = gm_ports::ProvisionOptions {
        verify: if skip_verify {
            VerifyMode::Skip
        } else {
            VerifyMode::Backend
        },
        wheelhouse,
        ..Default::default()
    };
    let mut last = String::new();
    let status = gm_ports::provision_with(spec, root, &uv, &opts, |p| {
        if p.step != last {
            eprintln!("== {}", p.step);
            last = p.step.to_string();
        }
        if let Some(l) = p.line {
            eprintln!("   {l}");
        }
    })
    .await?;
    println!("{}", serde_json::to_string_pretty(&status)?);
    if status.state != gm_types::PortState::Ready {
        bail!(
            "port {} is {:?}: {}",
            spec.id,
            status.state,
            status.message.clone().unwrap_or_default()
        );
    }
    Ok(status)
}

async fn join(
    cli: &Cli,
    address: &str,
    token: Option<String>,
    name: Option<String>,
    engine: Option<PathBuf>,
) -> Result<()> {
    let invite =
        gm_net::parse_address(address).ok_or_else(|| anyhow!("can't read address {address}"))?;
    let client = gm_net::CoordinatorClient::new(invite.coordinator.clone(), None);
    let health = client
        .health()
        .await
        .with_context(|| format!("no GradMesh coordinator at {}", invite.coordinator))?;
    if health.protocol != gm_types::MESH_PROTOCOL {
        eprintln!(
            "warning: mesh speaks protocol {}, this app {}",
            health.protocol,
            gm_types::MESH_PROTOCOL
        );
    }
    let token = match token.or(invite.token.clone()) {
        Some(t) => t,
        None => {
            let host = invite.hostname.clone();
            gm_net::read_invite(&host, gm_types::WEB_PORT)
                .await
                .token
                .ok_or_else(|| anyhow!("the mesh did not publish its token; pass --token"))?
        }
    };

    let facts = gm_probe::detect();
    let profile = gm_probe::select_profile(&facts, prefer(cli)?);
    if let Some(b) = &profile.blocked {
        bail!("{b}\n{}", profile.fix.clone().unwrap_or_default());
    }
    eprintln!("port {}: {}", profile.port, profile.reason);
    let reg = Registry::from_env_or_builtin()?;
    let spec = reg.require(&profile.port)?;
    let root = env_root(cli);
    let fp = gm_ports::local_machine_fingerprint();
    let mut st = gm_ports::status(spec, &root, &fp).await;
    if st.state != gm_types::PortState::Ready {
        st = install(spec, &root, false, None).await?;
    }
    let python = st
        .python_path
        .clone()
        .ok_or_else(|| anyhow!("port has no python"))?;

    let mut launch =
        gm_supervisor::WorkerLaunch::new(python, engine_dir(engine)?, &invite.coordinator, &token);
    launch.name = name;
    launch.backend = Some(profile.backend);
    launch.gpu_index = profile.gpu_index;
    let handle = gm_supervisor::Supervisor::spawn(gm_supervisor::worker_spec(&launch));
    let mut events = handle.subscribe();
    eprintln!("contributing to {} (Ctrl+C to stop)", invite.coordinator);
    // Wait on supervision itself, not on a status event: a clean exit ends in
    // Stopped (which the old loop ignored, so gmctl hung forever), and a
    // lagging receiver can miss the Failed event altogether.
    let finished = handle.wait();
    tokio::pin!(finished);
    let print = |ev: gm_supervisor::Event| {
        if let gm_supervisor::Event::Line(l) = ev {
            println!("{}", l.line);
        }
    };
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                eprintln!("stopping…");
                let _ = handle.stop().await;
                return Ok(());
            }
            st = &mut finished => {
                while let Ok(ev) = events.try_recv() {
                    print(ev);
                }
                if st.state == ProcState::Failed {
                    bail!("worker failed (exit {:?}): {}", st.exit_code, st.last_line.unwrap_or_default());
                }
                eprintln!("worker exited (code {:?})", st.exit_code);
                return Ok(());
            }
            ev = events.recv() => match ev {
                Ok(ev) => print(ev),
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => eprintln!("({n} lines skipped)"),
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {}
            }
        }
    }
}
