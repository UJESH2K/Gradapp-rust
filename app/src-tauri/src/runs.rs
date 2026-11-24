//! Runs screen: start, stop and delete runs (host), download trained weights,
//! and the live view of one run: every machine's share of the round's work,
//! its speed and progress, the round timeline and the metrics.
//!
//! Where the numbers come from (`desktop/engine/coordinator/app.py`):
//! * `GET /runs/{id}` (`_run_view`): `plan` (the round's `RoundPlan`: per-node
//!   `samples`, `throughput_sps`, `predicted_seconds`), `live_shards` (one per
//!   shard handed out this round: `samples`, `status`, `phase`, `progress`,
//!   `elapsed_seconds`, deadlines, `speculative`), `round_history` (per round:
//!   wall clock, straggler gap, aggregation, speedup, per-node `shards` with
//!   `samples`, `seconds`, `compute_seconds`) and `accuracy_history`.
//! * `GET /mesh`: every machine (GPU, backend, measured `throughput_sps`,
//!   online or not), turned into `MeshNode`s by `mesh.rs`.
//!
//! A machine's share is the fraction of the round's images the coordinator's
//! shard plan gave it (shards are sized from measured speed so that every
//! machine finishes together). Only when no plan exists yet (a run waiting
//! for machines) is the share each machine's measured or probed throughput
//! over the total, and the view says so (`share_basis`).

use std::collections::HashMap;

use gm_types::Backend;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::{AppHandle, State};

use crate::backend::AppBackend;
use crate::datasets::{access, host_access, start_transfer};
use crate::mesh::{self, SelfInfo};
use crate::model::{MeshNode, RunSummary};

type Res<T> = Result<T, String>;

fn s(v: &Value, key: &str) -> Option<String> {
    match v.get(key)? {
        Value::String(t) if !t.is_empty() => Some(t.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

fn f(v: &Value, key: &str) -> Option<f64> {
    v.get(key).and_then(Value::as_f64)
}

fn u(v: &Value, key: &str) -> Option<u64> {
    v.get(key).and_then(|x| {
        x.as_u64()
            .or_else(|| x.as_f64().filter(|f| *f >= 0.0).map(|f| f as u64))
    })
}

fn arr<'a>(v: &'a Value, key: &str) -> &'a [Value] {
    v.get(key)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

// ------------------------------------------------------------------ types

/// One machine in the live view.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DeviceShare {
    pub node_id: String,
    pub name: String,
    pub gpu: String,
    pub backend: Backend,
    pub port: Option<String>,
    pub is_self: bool,
    pub online: bool,
    /// Fraction (0..=1) of this round's work (see `LiveRun::share_basis`).
    pub share: f64,
    /// Images in its shard (0 when not in the round).
    pub samples: u64,
    /// "starting" | "downloading" | "loading" | "training" | "uploading" |
    /// "queued" | "done" | "dropped" | "superseded" | "idle" | "offline"
    pub status: String,
    /// How far through its shard (0..=1), when known.
    pub progress: Option<f32>,
    pub images_per_sec: Option<f64>,
    /// "last round" (its shard's images ÷ training seconds), "measured" (the
    /// coordinator's running estimate) or "planned" (the scheduler's estimate,
    /// from the GPU benchmark until the machine has finished a round).
    pub rate_source: Option<String>,
    pub predicted_seconds: Option<f64>,
    pub elapsed_seconds: Option<f64>,
    /// Past its soft deadline: the coordinator may start a backup copy.
    pub late: bool,
    pub batch_size: Option<u64>,
    pub tier: Option<String>,
    /// A backup copy of a slow machine's shard (duplicate work, no share).
    pub speculative: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ShardBar {
    pub node_id: String,
    pub name: String,
    pub backend: Backend,
    pub samples: u64,
    pub share: f64,
    pub seconds: f64,
    pub compute_seconds: f64,
}

/// One finished round.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RoundBar {
    /// 1-based.
    pub round: u32,
    pub wall_clock_seconds: f64,
    pub makespan_seconds: f64,
    pub straggler_gap_seconds: f64,
    pub aggregation_seconds: f64,
    pub samples: u64,
    pub workers: u32,
    pub speedup: f64,
    pub map50: Option<f64>,
    pub loss: Option<f64>,
    pub shards: Vec<ShardBar>,
}

#[derive(Debug, Clone, Serialize)]
pub struct LiveRun {
    pub run: RunSummary,
    pub can_manage: bool,
    pub mode: String,
    pub partition_strategy: String,
    pub imgsz: u64,
    pub batch_size: u64,
    pub total_samples: u64,
    pub has_artifact: bool,
    pub efficiency: Option<f64>,
    pub best_map50: Option<f64>,
    pub map50_95: Option<f64>,
    /// The round on screen, 1-based (the one training, or the last one).
    pub round: u32,
    /// Images done ÷ images in the round, from each machine's progress.
    pub round_progress: f32,
    pub round_elapsed_seconds: Option<f64>,
    pub predicted_round_seconds: Option<f64>,
    /// "round" (this round's shard plan), "last_round" (the last finished
    /// round's shards), "plan" (the stored plan), "preview" (the mesh's plan
    /// preview for its default dataset) or "throughput" (measured speed only).
    pub share_basis: String,
    pub devices: Vec<DeviceShare>,
    pub timeline: Vec<RoundBar>,
    /// Why a waiting run is waiting (machines the planner rejected).
    pub waiting_reasons: Vec<String>,
}

// ------------------------------------------------------------------ mapping

fn shard_status(shard: &Value, online: bool) -> String {
    let status = s(shard, "status").unwrap_or_default();
    match status.as_str() {
        "assigned" => {
            if !online {
                return "offline".into();
            }
            match s(shard, "phase").as_deref() {
                Some(p @ ("downloading" | "loading" | "training" | "uploading")) => p.into(),
                _ => "starting".into(),
            }
        }
        "queued" => "queued".into(),
        "done" => "done".into(),
        "superseded" => "superseded".into(),
        "failed" | "dropped" => "dropped".into(),
        other if !other.is_empty() => other.into(),
        _ => "queued".into(),
    }
}

fn shard_progress(shard: &Value, status: &str) -> Option<f32> {
    match status {
        "done" | "uploading" => return Some(1.0),
        "queued" | "superseded" => return Some(0.0),
        _ => {}
    }
    let p = shard.get("progress").filter(|p| p.is_object())?;
    let (done, total) = (f(p, "batch")?, f(p, "batches")?);
    (total > 0.0).then(|| (done / total).clamp(0.0, 1.0) as f32)
}

fn blank_row(node_id: &str, node: Option<&MeshNode>, fallback_name: Option<String>) -> DeviceShare {
    DeviceShare {
        node_id: node_id.to_string(),
        name: node
            .map(|n| n.display_name.clone())
            .or(fallback_name)
            .unwrap_or_else(|| node_id.to_string()),
        gpu: node.map(|n| n.gpu.clone()).unwrap_or_default(),
        backend: node.map(|n| n.backend).unwrap_or(Backend::Cpu),
        port: node.and_then(|n| n.port.clone()),
        is_self: node.is_some_and(|n| n.is_self),
        online: node.is_some_and(|n| n.active),
        share: 0.0,
        samples: 0,
        status: "idle".into(),
        progress: None,
        images_per_sec: None,
        rate_source: None,
        predicted_seconds: None,
        elapsed_seconds: None,
        late: false,
        batch_size: None,
        tier: node.map(|n| n.tier.clone()),
        speculative: false,
        error: None,
    }
}

/// The live view from `GET /runs/{id}` and `GET /mesh`. Pure; unit tested.
pub fn live_run(
    view: &Value,
    mesh_raw: Option<&Value>,
    me: &SelfInfo,
    can_manage: bool,
) -> LiveRun {
    let run = mesh::run(view);
    let nodes: Vec<MeshNode> = mesh_raw
        .map(|m| arr(m, "nodes").iter().map(|n| mesh::node(n, me)).collect())
        .unwrap_or_default();
    let by_id: HashMap<&str, &MeshNode> = nodes.iter().map(|n| (n.node_id.as_str(), n)).collect();
    let plan = view.get("plan").cloned().unwrap_or(Value::Null);
    let planned: HashMap<String, &Value> = arr(&plan, "assignments")
        .iter()
        .filter_map(|a| Some((s(a, "node_id")?, a)))
        .collect();
    let history = arr(view, "round_history");
    let accuracy = arr(view, "accuracy_history");

    // Images per second each machine actually trained at last round.
    let last_rates: HashMap<String, f64> = history
        .last()
        .map(|r| {
            arr(r, "shards")
                .iter()
                .filter_map(|sh| {
                    let samples = f(sh, "samples")?;
                    let secs = f(sh, "compute_seconds")
                        .filter(|x| *x > 0.0)
                        .or_else(|| f(sh, "seconds").filter(|x| *x > 0.0))?;
                    Some((s(sh, "node_id")?, samples / secs))
                })
                .collect()
        })
        .unwrap_or_default();
    let rate_of = |node_id: &str| -> (Option<f64>, Option<String>) {
        if let Some(r) = last_rates.get(node_id) {
            return (Some(*r), Some("last round".into()));
        }
        if let Some(t) = by_id.get(node_id).and_then(|n| n.throughput_sps) {
            return (Some(t), Some("measured".into()));
        }
        if let Some(t) = planned
            .get(node_id)
            .and_then(|a| f(a, "throughput_sps"))
            .filter(|t| *t > 0.0)
        {
            return (Some(t), Some("planned".into()));
        }
        (None, None)
    };

    let live = arr(view, "live_shards");
    let finished = matches!(run.status.as_str(), "completed" | "stopped" | "failed");
    let mut devices: Vec<DeviceShare> = Vec::new();
    let basis;
    if !live.is_empty() && !finished {
        basis = "round";
        let total: u64 = live
            .iter()
            .filter(|sh| sh.get("speculative").and_then(Value::as_bool) != Some(true))
            .filter_map(|sh| u(sh, "samples"))
            .sum();
        for sh in live {
            let node_id = s(sh, "node_id").unwrap_or_default();
            let node = by_id.get(node_id.as_str()).copied();
            let speculative = sh.get("speculative").and_then(Value::as_bool) == Some(true);
            let online = node.map(|n| n.active).unwrap_or(true);
            let status = shard_status(sh, online);
            let samples = u(sh, "samples").unwrap_or(0);
            let elapsed = f(sh, "elapsed_seconds");
            let (rate, source) = rate_of(&node_id);
            let mut row = blank_row(&node_id, node, s(sh, "node_name"));
            if node.is_none() {
                row.backend = mesh::backend_of(&s(sh, "backend").unwrap_or_default());
            }
            row.online = online;
            row.share = if speculative || total == 0 {
                0.0
            } else {
                samples as f64 / total as f64
            };
            row.samples = samples;
            row.progress = shard_progress(sh, &status);
            row.late = status != "done"
                && matches!(
                    (elapsed, f(sh, "soft_deadline_seconds")),
                    (Some(e), Some(d)) if d > 0.0 && e > d
                );
            row.status = status;
            row.images_per_sec = rate;
            row.rate_source = source;
            row.predicted_seconds = f(sh, "predicted_seconds");
            row.elapsed_seconds = elapsed;
            row.batch_size = u(sh, "batch_size");
            row.tier = s(sh, "tier").or(row.tier);
            row.speculative = speculative;
            row.error = s(sh, "error");
            devices.push(row);
        }
    } else if let Some(last) = history.last().filter(|r| !arr(r, "shards").is_empty()) {
        basis = "last_round";
        let shards = arr(last, "shards");
        let total: u64 = shards.iter().filter_map(|sh| u(sh, "samples")).sum();
        for sh in shards {
            let node_id = s(sh, "node_id").unwrap_or_default();
            let node = by_id.get(node_id.as_str()).copied();
            let mut row = blank_row(&node_id, node, s(sh, "node_name"));
            if node.is_none() {
                row.backend = mesh::backend_of(&s(sh, "backend").unwrap_or_default());
            }
            let samples = u(sh, "samples").unwrap_or(0);
            row.share = if total > 0 {
                samples as f64 / total as f64
            } else {
                0.0
            };
            row.samples = samples;
            row.status = "done".into();
            row.progress = Some(1.0);
            (row.images_per_sec, row.rate_source) = rate_of(&node_id);
            row.predicted_seconds = f(sh, "predicted_seconds");
            row.elapsed_seconds = f(sh, "seconds");
            row.tier = s(sh, "tier").or(row.tier);
            devices.push(row);
        }
    } else {
        let preview = mesh_raw
            .and_then(|m| m.get("plan_preview"))
            .cloned()
            .unwrap_or(Value::Null);
        let (source, assignments) = if !planned.is_empty() {
            ("plan", arr(&plan, "assignments"))
        } else {
            ("preview", arr(&preview, "assignments"))
        };
        if !assignments.is_empty() {
            basis = source;
            let total: u64 = assignments.iter().filter_map(|a| u(a, "samples")).sum();
            for a in assignments {
                let node_id = s(a, "node_id").unwrap_or_default();
                let node = by_id.get(node_id.as_str()).copied();
                let mut row = blank_row(&node_id, node, None);
                let samples = u(a, "samples").unwrap_or(0);
                row.share = if total > 0 {
                    samples as f64 / total as f64
                } else {
                    0.0
                };
                row.samples = samples;
                row.status = if row.online { "queued" } else { "offline" }.into();
                row.images_per_sec = f(a, "throughput_sps").filter(|t| *t > 0.0);
                row.rate_source = row.images_per_sec.map(|_| "planned".into());
                row.predicted_seconds = f(a, "predicted_seconds");
                row.tier = s(a, "tier").or(row.tier);
                devices.push(row);
            }
        } else {
            // Nothing planned: compare machines by measured (or probed) speed.
            basis = "throughput";
            let speed = |n: &MeshNode| n.throughput_sps.or(n.gflops.map(|g| g / 1000.0));
            let eligible: Vec<&MeshNode> = nodes
                .iter()
                .filter(|n| n.active && n.tier != "rejected" && speed(n).is_some())
                .collect();
            let total: f64 = eligible.iter().filter_map(|n| speed(n)).sum();
            for n in eligible {
                let mut row = blank_row(&n.node_id, Some(n), None);
                row.share = if total > 0.0 {
                    speed(n).unwrap_or(0.0) / total
                } else {
                    0.0
                };
                (row.images_per_sec, row.rate_source) = rate_of(&n.node_id);
                row.status = "idle".into();
                devices.push(row);
            }
        }
    }

    // Machines in the mesh that have no part in this round, so nobody wonders
    // where they went.
    for n in &nodes {
        if devices.iter().any(|d| d.node_id == n.node_id) {
            continue;
        }
        let mut row = blank_row(&n.node_id, Some(n), None);
        row.status = if n.active { "idle" } else { "offline" }.into();
        (row.images_per_sec, row.rate_source) = rate_of(&n.node_id);
        devices.push(row);
    }
    devices.sort_by(|a, b| {
        b.share
            .total_cmp(&a.share)
            .then(b.online.cmp(&a.online))
            .then(a.name.cmp(&b.name))
    });

    let in_round: f64 = devices
        .iter()
        .filter(|d| !d.speculative)
        .map(|d| d.share)
        .sum();
    let round_progress = if in_round > 0.0 {
        devices
            .iter()
            .filter(|d| !d.speculative)
            .map(|d| d.share * f64::from(d.progress.unwrap_or(0.0)))
            .sum::<f64>()
            / in_round
    } else {
        0.0
    };

    let timeline: Vec<RoundBar> = history
        .iter()
        .map(|r| {
            let index = u(r, "round").unwrap_or(0);
            let shards = arr(r, "shards");
            let total: u64 = shards.iter().filter_map(|sh| u(sh, "samples")).sum();
            let metric = run
                .round_metrics
                .iter()
                .find(|m| u64::from(m.round) == index + 1);
            RoundBar {
                round: index as u32 + 1,
                wall_clock_seconds: f(r, "wall_clock_seconds").unwrap_or(0.0),
                makespan_seconds: f(r, "makespan_seconds").unwrap_or(0.0),
                straggler_gap_seconds: f(r, "straggler_gap_seconds").unwrap_or(0.0),
                aggregation_seconds: f(r, "aggregation_seconds").unwrap_or(0.0),
                samples: u(r, "samples").unwrap_or(total),
                workers: u(r, "workers").unwrap_or(0) as u32,
                speedup: f(r, "speedup").unwrap_or(0.0),
                map50: metric.and_then(|m| m.map50).or_else(|| {
                    accuracy
                        .iter()
                        .find(|a| u(a, "round") == Some(index))
                        .and_then(|a| f(a, "map50"))
                }),
                loss: metric.and_then(|m| m.loss),
                shards: shards
                    .iter()
                    .map(|sh| {
                        let samples = u(sh, "samples").unwrap_or(0);
                        let node_id = s(sh, "node_id").unwrap_or_default();
                        let node = by_id.get(node_id.as_str()).copied();
                        ShardBar {
                            name: s(sh, "node_name")
                                .or_else(|| node.map(|n| n.display_name.clone()))
                                .unwrap_or_else(|| node_id.clone()),
                            backend: mesh::backend_of(&s(sh, "backend").unwrap_or_default()),
                            node_id,
                            samples,
                            share: if total > 0 {
                                samples as f64 / total as f64
                            } else {
                                0.0
                            },
                            seconds: f(sh, "seconds").unwrap_or(0.0),
                            compute_seconds: f(sh, "compute_seconds").unwrap_or(0.0),
                        }
                    })
                    .collect(),
            }
        })
        .collect();

    let live_round = !finished && run.status != "waiting" && basis == "round";
    let round = if live_round || run.status == "waiting" {
        (run.current_round + 1).min(run.rounds.max(1))
    } else {
        run.current_round.max(1).min(run.rounds.max(1))
    };
    let names: HashMap<&str, &str> = nodes
        .iter()
        .map(|n| (n.node_id.as_str(), n.display_name.as_str()))
        .collect();
    let waiting_reasons = if run.status == "waiting" {
        arr(&plan, "rejected")
            .iter()
            .filter_map(|r| {
                let id = s(r, "node_id")?;
                let who = names.get(id.as_str()).map(|n| n.to_string()).unwrap_or(id);
                Some(format!("{who}: {}", s(r, "reason").unwrap_or_default()))
            })
            .collect()
    } else {
        Vec::new()
    };

    LiveRun {
        can_manage,
        mode: s(view, "mode").unwrap_or_else(|| "mesh".into()),
        partition_strategy: s(view, "partition_strategy").unwrap_or_else(|| "proportional".into()),
        imgsz: u(view, "imgsz").unwrap_or(0),
        batch_size: u(view, "batch_size").unwrap_or(0),
        total_samples: u(view, "total_samples").unwrap_or(0),
        has_artifact: view.get("has_artifact").and_then(Value::as_bool) == Some(true),
        efficiency: f(view, "efficiency").filter(|x| *x > 0.0),
        best_map50: f(view, "best_map50"),
        map50_95: f(view, "map50_95"),
        round,
        round_progress: if basis == "round" {
            round_progress as f32
        } else if basis == "last_round" {
            1.0
        } else {
            0.0
        },
        round_elapsed_seconds: if basis == "round" {
            devices
                .iter()
                .filter_map(|d| d.elapsed_seconds)
                .fold(None, |m: Option<f64>, x| Some(m.map_or(x, |m| m.max(x))))
        } else {
            None
        },
        predicted_round_seconds: f(&plan, "predicted_makespan_seconds").filter(|x| *x > 0.0),
        share_basis: basis.into(),
        devices,
        timeline,
        waiting_reasons,
        run,
    }
}

// ------------------------------------------------------------------ requests

/// `POST /runs` (coordinator `CreateRunRequest`). Optional fields left out
/// let the coordinator choose.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunRequest {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dataset_id: Option<String>,
    pub base_model: String,
    pub rounds: u32,
    pub imgsz: u32,
    pub batch_size: u32,
    #[serde(default = "mesh_mode")]
    pub mode: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backends: Option<Vec<String>>,
    #[serde(default = "proportional")]
    pub partition_strategy: String,
    #[serde(default = "first_round")]
    pub warmup_mode: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warmup_epochs: Option<f64>,
    #[serde(default = "auto")]
    pub optimizer: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lr0: Option<f64>,
    #[serde(default)]
    pub seed: u64,
    #[serde(default)]
    pub evaluate: bool,
    #[serde(default = "yes")]
    pub deterministic: bool,
    #[serde(default)]
    pub worker_validation: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dataloader_workers: Option<u32>,
}

fn mesh_mode() -> String {
    "mesh".into()
}
fn proportional() -> String {
    "proportional".into()
}
fn first_round() -> String {
    "first-round".into()
}
fn auto() -> String {
    "auto".into()
}
fn yes() -> bool {
    true
}

impl RunRequest {
    /// The coordinator's own limits, checked here so the form says what is
    /// wrong in words instead of a 422.
    pub fn validate(mut self) -> Res<Self> {
        self.name = self.name.trim().chars().take(80).collect();
        if self.name.is_empty() {
            self.name = "mesh-run".into();
        }
        self.notes = self
            .notes
            .map(|n| n.trim().to_string())
            .filter(|n| !n.is_empty());
        self.dataset_id = self.dataset_id.filter(|d| !d.is_empty());
        let check = |ok: bool, what: &str| if ok { Ok(()) } else { Err(what.to_string()) };
        check(
            (1..=200).contains(&self.rounds),
            "Rounds must be between 1 and 200.",
        )?;
        check(
            (32..=2048).contains(&self.imgsz),
            "Image size must be between 32 and 2048.",
        )?;
        check(
            (1..=128).contains(&self.batch_size),
            "Batch size must be between 1 and 128.",
        )?;
        check(
            ["mesh", "solo"].contains(&self.mode.as_str()),
            "Mode must be mesh or solo.",
        )?;
        check(
            ["proportional", "proportional-linear", "equal"]
                .contains(&self.partition_strategy.as_str()),
            "Unknown shard sizing.",
        )?;
        check(
            ["first-round", "none", "every-round"].contains(&self.warmup_mode.as_str()),
            "Unknown warmup mode.",
        )?;
        check(
            ["auto", "AdamW", "Adam", "SGD"].contains(&self.optimizer.as_str()),
            "Unknown optimiser.",
        )?;
        check(
            self.warmup_epochs.is_none_or(|w| (0.0..=10.0).contains(&w)),
            "Warmup epochs must be between 0 and 10.",
        )?;
        check(
            self.lr0.is_none_or(|l| l > 0.0 && l <= 1.0),
            "The learning rate must be above 0 and at most 1.",
        )?;
        check(
            self.dataloader_workers.is_none_or(|w| w <= 16),
            "Dataloader processes must be 16 or fewer.",
        )?;
        if let Some(backends) = &self.backends {
            check(
                backends
                    .iter()
                    .all(|b| ["cuda", "xpu", "mps", "cpu"].contains(&b.as_str())),
                "Unknown GPU vendor.",
            )?;
        }
        self.backends = self.backends.filter(|b| !b.is_empty());
        Ok(self)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ModelInfo {
    pub name: String,
    pub bytes: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct RunOptions {
    /// Base checkpoints on the host (`GET /models`).
    pub models: Vec<ModelInfo>,
    pub can_manage: bool,
}

// ------------------------------------------------------------------ commands

#[tauri::command]
pub async fn run_options(state: State<'_, AppBackend>) -> Res<RunOptions> {
    let (client, can_manage) = access(&state)?;
    let models = client
        .models()
        .await
        .map(|v| {
            arr(&v, "models")
                .iter()
                .filter_map(|m| {
                    Some(ModelInfo {
                        name: s(m, "name")?,
                        bytes: u(m, "bytes").unwrap_or(0),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(RunOptions { models, can_manage })
}

#[tauri::command]
pub async fn run_create(state: State<'_, AppBackend>, request: RunRequest) -> Res<RunSummary> {
    let client = host_access(&state)?;
    let request = request.validate()?;
    let body = serde_json::to_value(&request).map_err(|e| e.to_string())?;
    // Planning the first round lists the dataset; give a big one time.
    let view = client
        .with_timeout(std::time::Duration::from_secs(120))
        .create_run(&body)
        .await
        .map_err(|e| e.to_string())?;
    Ok(mesh::run(&view))
}

#[tauri::command]
pub async fn run_stop(state: State<'_, AppBackend>, id: String) -> Res<()> {
    let client = host_access(&state)?;
    client
        .stop_run(&id)
        .await
        .map(|_| ())
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn run_delete(state: State<'_, AppBackend>, id: String) -> Res<()> {
    let client = host_access(&state)?;
    client
        .delete_run(&id)
        .await
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// The live view of one run (host and contributors alike; contributors get
/// `can_manage: false`).
#[tauri::command]
pub async fn run_live(state: State<'_, AppBackend>, id: String) -> Res<LiveRun> {
    let (client, can_manage) = access(&state)?;
    let (view, mesh_raw) = tokio::join!(client.run(&id), client.mesh());
    let view = view.map_err(|e| format!("Could not read the run: {e}"))?;
    let me = state.self_info().await;
    Ok(live_run(&view, mesh_raw.ok().as_ref(), &me, can_manage))
}

/// Save a run's trained weights (`GET /runs/{id}/artifact`, the aggregated
/// global model as `.pt`, written after every round) into `dir`.
#[tauri::command]
pub async fn run_download_weights(
    app: AppHandle,
    state: State<'_, AppBackend>,
    id: String,
    name: String,
    dir: String,
) -> Res<String> {
    let (client, _) = access(&state)?;
    let fallback = format!("{name}-{id}.pt");
    let label = format!("Downloading weights of {name}");
    Ok(start_transfer(
        &app,
        "download",
        label,
        None,
        move |report| async move {
            let path = client
                .download_file(
                    &format!("/runs/{id}/artifact"),
                    std::path::Path::new(&dir),
                    &fallback,
                    move |done, total| report.progress(done, total),
                )
                .await
                .map_err(|e| e.to_string())?;
            Ok(Some(path.to_string_lossy().into_owned()))
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A run request with every default filled in, as the form starts.
    fn default_request() -> Value {
        json!({
            "name": "mesh-run", "base_model": "yolov8n.pt", "rounds": 4, "imgsz": 640, "batch_size": 8,
            "mode": "mesh", "partition_strategy": "proportional", "warmup_mode": "first-round",
            "warmup_epochs": 1.0, "optimizer": "auto", "seed": 0, "evaluate": true,
            "deterministic": true, "worker_validation": false
        })
    }

    fn me() -> SelfInfo {
        SelfInfo {
            node_id: Some("n-self".into()),
            contributing: true,
            names: vec![],
            port: Some("cuda-cu130".into()),
            torch: None,
        }
    }

    fn mesh_json() -> Value {
        json!({"nodes": [
            {"node_id": "n-self", "display_name": "MSI", "gpu": "NVIDIA GeForce RTX 3050 Laptop GPU", "backend": "cuda", "active": true,
             "throughput_sps": 0.0, "tier": "full", "software": {"torch_build": "cu130"}},
            {"node_id": "n-3090", "display_name": "lab", "gpu": "NVIDIA GeForce RTX 3090", "backend": "cuda", "active": true,
             "throughput_sps": 80.0, "tier": "full", "software": {"torch_build": "cu130"}},
            {"node_id": "n-m3", "display_name": "studio", "gpu": "Apple M3 Max", "backend": "mps", "active": true, "throughput_sps": 36.0, "tier": "full"},
            {"node_id": "n-cpu", "display_name": "host-cpu", "gpu": "No supported GPU", "backend": "cpu", "active": true, "tier": "rejected"},
            {"node_id": "n-off", "display_name": "old", "gpu": "Intel Arc A770", "backend": "xpu", "active": false, "tier": "probation"}
        ], "plan_preview": {"assignments": []}})
    }

    /// A `_run_view` mid-round, shaped like the real coordinator's.
    fn running_view() -> Value {
        let mut v = json!({
            "id": "r1", "name": "smoke", "status": "running", "mode": "mesh", "dataset_name": "COCO128", "base_model": "yolov8n.pt",
            "rounds": 4, "current_round": 1, "imgsz": 640, "batch_size": 8, "total_samples": 128, "live_workers": 3, "peak_workers": 3,
            "speedup": 2.4, "efficiency": 0.8, "has_artifact": true, "partition_strategy": "proportional",
            "accuracy_history": [], "map50": null, "best_map50": null
        });
        v["plan"] = json!({"total_samples": 128, "predicted_makespan_seconds": 30.0, "rejected": [], "assignments": [
            {"node_id": "n-3090", "samples": 72, "throughput_sps": 80.0, "predicted_seconds": 29.0},
            {"node_id": "n-m3", "samples": 34, "throughput_sps": 36.0, "predicted_seconds": 30.0},
            {"node_id": "n-self", "samples": 22, "throughput_sps": 20.0, "predicted_seconds": 30.0}]});
        let shards = json!([
            {"node_id": "n-3090", "node_name": "lab", "backend": "cuda", "samples": 64, "seconds": 36.0, "compute_seconds": 32.0, "metrics": {"loss": 1.2}},
            {"node_id": "n-m3", "node_name": "studio", "backend": "mps", "samples": 40, "seconds": 38.0, "compute_seconds": 34.0},
            {"node_id": "n-self", "node_name": "MSI", "backend": "cuda", "samples": 24, "seconds": 35.0, "compute_seconds": 30.0}]);
        v["round_history"] = json!([{"round": 0, "workers": 3, "samples": 128, "wall_clock_seconds": 40.0, "makespan_seconds": 38.0,
            "straggler_gap_seconds": 3.0, "aggregation_seconds": 0.4, "speedup": 2.4, "shards": shards}]);
        let training = json!({"batch_id": "b1", "node_id": "n-3090", "node_name": "lab", "backend": "cuda", "status": "assigned", "samples": 72,
            "phase": "training", "progress": {"batch": 6, "batches": 9}, "elapsed_seconds": 20.0, "soft_deadline_seconds": 45.0,
            "predicted_seconds": 29.0, "batch_size": 8, "speculative": false});
        let done = json!({"batch_id": "b2", "node_id": "n-m3", "node_name": "studio", "backend": "mps", "status": "done", "samples": 34,
            "elapsed_seconds": 25.0, "soft_deadline_seconds": 45.0, "speculative": false});
        let late = json!({"batch_id": "b3", "node_id": "n-self", "node_name": "MSI", "backend": "cuda", "status": "assigned", "samples": 22,
            "phase": "downloading", "progress": null, "elapsed_seconds": 50.0, "soft_deadline_seconds": 45.0, "speculative": false});
        let backup = json!({"batch_id": "b4", "node_id": "n-3090", "node_name": "lab", "backend": "cuda", "status": "queued", "samples": 22,
            "speculative": true});
        v["live_shards"] = json!([training, done, late, backup]);
        v
    }

    #[test]
    fn live_round_shares_come_from_the_shard_plan() {
        let live = live_run(&running_view(), Some(&mesh_json()), &me(), true);
        assert_eq!(live.share_basis, "round");
        assert_eq!(live.round, 2);
        let by = |id: &str, spec: bool| {
            live.devices
                .iter()
                .find(|d| d.node_id == id && d.speculative == spec)
                .unwrap()
        };
        let lab = by("n-3090", false);
        assert!((lab.share - 72.0 / 128.0).abs() < 1e-9);
        assert_eq!(lab.status, "training");
        assert_eq!(lab.progress, Some(6.0 / 9.0));
        assert_eq!(lab.images_per_sec, Some(2.0)); // 64 images / 32 s last round
        assert_eq!(lab.rate_source.as_deref(), Some("last round"));
        assert_eq!(lab.port.as_deref(), Some("cuda-cu130"));
        let studio = by("n-m3", false);
        assert_eq!(studio.status, "done");
        assert_eq!(studio.progress, Some(1.0));
        let mine = by("n-self", false);
        assert!(mine.is_self && mine.late);
        assert_eq!(mine.status, "downloading");
        // The backup copy is duplicate work: listed, no share.
        let backup = by("n-3090", true);
        assert_eq!(backup.share, 0.0);
        let total: f64 = live.devices.iter().map(|d| d.share).sum();
        assert!((total - 1.0).abs() < 1e-9);
        // Machines outside the round are listed with no share.
        assert_eq!(
            live.devices
                .iter()
                .find(|d| d.node_id == "n-cpu")
                .unwrap()
                .status,
            "idle"
        );
        assert_eq!(
            live.devices
                .iter()
                .find(|d| d.node_id == "n-off")
                .unwrap()
                .status,
            "offline"
        );
        // Largest share first.
        assert_eq!(live.devices[0].node_id, "n-3090");
        // 72·(6/9) + 34·1 + 22·0 over 128.
        assert!((f64::from(live.round_progress) - (48.0 + 34.0) / 128.0).abs() < 1e-6);
        assert_eq!(live.round_elapsed_seconds, Some(50.0));
        assert_eq!(live.predicted_round_seconds, Some(30.0));
        assert_eq!(live.timeline.len(), 1);
        let r1 = &live.timeline[0];
        assert_eq!(r1.round, 1);
        assert_eq!(r1.shards.len(), 3);
        assert!((r1.shards[0].share - 0.5).abs() < 1e-9);
        assert_eq!(r1.loss, Some(1.2));
    }

    #[test]
    fn finished_run_shows_last_round_and_waiting_run_shows_speed() {
        let mut done = running_view();
        done["status"] = json!("done");
        done["current_round"] = json!(4);
        done["live_shards"] = json!([]);
        let live = live_run(&done, Some(&mesh_json()), &me(), false);
        assert_eq!(live.share_basis, "last_round");
        assert_eq!(live.run.status, "completed");
        assert_eq!(live.round, 4);
        assert_eq!(live.round_progress, 1.0);
        assert!(!live.can_manage);
        let lab = live.devices.iter().find(|d| d.node_id == "n-3090").unwrap();
        assert!((lab.share - 0.5).abs() < 1e-9);

        let waiting = json!({"id": "r2", "name": "w", "status": "waiting", "rounds": 2, "current_round": 0,
            "plan": {"assignments": [], "rejected": [{"node_id": "n-cpu", "reason": "no supported accelerator was detected"}]},
            "round_history": [], "live_shards": []});
        let live = live_run(&waiting, Some(&mesh_json()), &me(), true);
        assert_eq!(live.share_basis, "throughput");
        assert_eq!(live.round, 1);
        assert_eq!(
            live.waiting_reasons,
            vec!["host-cpu: no supported accelerator was detected"]
        );
        let lab = live.devices.iter().find(|d| d.node_id == "n-3090").unwrap();
        assert!((lab.share - 80.0 / 116.0).abs() < 1e-9);
        // Without a mesh snapshot the run still renders.
        let bare = live_run(&waiting, None, &me(), true);
        assert!(bare.devices.is_empty());
    }

    #[test]
    fn run_requests_are_checked() {
        let ok: RunRequest = serde_json::from_value(default_request()).unwrap();
        let ok = ok.validate().unwrap();
        let body = serde_json::to_value(&ok).unwrap();
        assert_eq!(body["partition_strategy"], "proportional");
        assert!(body.get("dataset_id").is_none());
        assert!(body.get("lr0").is_none());
        let mut bad = default_request();
        bad["rounds"] = json!(0);
        let bad: RunRequest = serde_json::from_value(bad).unwrap();
        assert_eq!(
            bad.validate().unwrap_err(),
            "Rounds must be between 1 and 200."
        );
        let mut minimal = json!({"name": "  ", "base_model": "yolo11n.pt", "rounds": 3, "imgsz": 320, "batch_size": 4, "backends": []});
        minimal["dataset_id"] = json!("");
        let r: RunRequest = serde_json::from_value(minimal).unwrap();
        let r = r.validate().unwrap();
        assert_eq!(r.name, "mesh-run");
        assert_eq!(r.dataset_id, None);
        assert_eq!(r.backends, None);
        assert!(r.deterministic && !r.evaluate);
    }

    /// JSON captured from the real coordinator (`GET /runs/{id}` mid-round 2
    /// and `GET /mesh`), with four synthetic workers speaking the worker
    /// protocol: see docs/devlog/runs-datasets.md.
    #[test]
    fn real_coordinator_payloads() {
        let view: Value =
            serde_json::from_str(include_str!("../tests/fixtures/coordinator-run-view.json"))
                .unwrap();
        let mesh_raw: Value =
            serde_json::from_str(include_str!("../tests/fixtures/coordinator-mesh.json")).unwrap();
        let live = live_run(&view, Some(&mesh_raw), &SelfInfo::default(), true);
        assert_eq!(live.share_basis, "round");
        assert_eq!(live.run.status, "running");
        assert_eq!(live.round, 2);
        assert_eq!(live.devices.len(), 4);
        let names: Vec<&str> = live.devices.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names[0], "lab-rtx3090 (synthetic)");
        let samples: u64 = live.devices.iter().map(|d| d.samples).sum();
        assert_eq!(samples, live.total_samples);
        let shares: f64 = live.devices.iter().map(|d| d.share).sum();
        assert!((shares - 1.0).abs() < 1e-9);
        let status: Vec<&str> = live.devices.iter().map(|d| d.status.as_str()).collect();
        assert_eq!(status, ["training", "training", "uploading", "done"]);
        let lab = &live.devices[0];
        assert_eq!(lab.progress, Some(0.75));
        assert_eq!(lab.backend, Backend::Cuda);
        assert_eq!(lab.port.as_deref(), Some("cuda-cu130"));
        assert_eq!(lab.rate_source.as_deref(), Some("last round"));
        assert!(lab.images_per_sec.unwrap() > 0.0);
        assert!(live.round_progress > 0.5 && live.round_progress < 1.0);
        assert_eq!(live.timeline.len(), 1);
        assert_eq!(live.timeline[0].shards.len(), 4);
        assert!(live.timeline[0].speedup > 1.0);
        assert!(live.run.round_metrics[0].loss.is_some());
    }
}
