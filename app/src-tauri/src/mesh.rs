//! The coordinator's JSON (`GET /mesh`, `/runs`, `/runs/{id}`, `/datasets`,
//! `/events`) turned into the shapes the interface draws.
//!
//! Everything here is pure, so it is unit tested against payloads shaped like
//! the real Python coordinator's (`desktop/engine/coordinator/app.py`:
//! `mesh_state`, `_run_summary`, `_run_view`, `store.list_datasets`,
//! `events.EventBus.publish`).

use gm_types::{Backend, MeshEvent};
use serde_json::Value;

use crate::model::{Dataset, MeshNode, MeshSnapshot, RoundMetric, RunSummary};

/// What the app knows about its own worker, to mark "This computer".
#[derive(Debug, Clone, Default)]
pub struct SelfInfo {
    /// From the worker's `joined <server> as <node id>` line.
    pub node_id: Option<String>,
    /// The worker is running (so a co-located node is probably us).
    pub contributing: bool,
    /// The names this machine registers under (device name, hostname).
    pub names: Vec<String>,
    pub port: Option<String>,
    pub torch: Option<String>,
}

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

fn b(v: &Value, key: &str) -> bool {
    v.get(key).and_then(Value::as_bool).unwrap_or(false)
}

pub fn backend_of(text: &str) -> Backend {
    match text {
        "cuda" => Backend::Cuda,
        "xpu" => Backend::Xpu,
        "mps" => Backend::Mps,
        "rocm" => Backend::Rocm,
        _ => Backend::Cpu,
    }
}

/// The port a remote node most likely runs, from its backend and wheel build
/// (`software.torch_build`, e.g. "cu130", "xpu", "cpu", "macOS").
fn port_of(backend: Backend, build: &str) -> Option<String> {
    Some(match backend {
        Backend::Cuda if build.starts_with("cu") => format!("cuda-{build}"),
        Backend::Rocm => "rocm".into(),
        Backend::Xpu => "xpu".into(),
        Backend::Mps => "mps".into(),
        Backend::Cpu => "cpu".into(),
        Backend::Cuda => return None,
    })
}

/// Progress of the current shard as 0..=1, from the heartbeat's `progress`
/// (`{"batch": n, "batches": m}`) or the epoch counters.
fn progress_of(node: &Value) -> Option<f32> {
    if let Some(p) = node.get("progress").filter(|p| p.is_object()) {
        if let (Some(done), Some(total)) = (f(p, "batch"), f(p, "batches")) {
            if total > 0.0 {
                return Some((done / total).clamp(0.0, 1.0) as f32);
            }
        }
    }
    let (epoch, total) = (
        f(node, "training_epoch")?,
        f(node, "training_total_epochs")?,
    );
    (total > 0.0 && epoch > 0.0).then(|| (epoch / total).clamp(0.0, 1.0) as f32)
}

pub fn node(v: &Value, me: &SelfInfo) -> MeshNode {
    let node_id = s(v, "node_id").unwrap_or_default();
    let display_name = s(v, "display_name")
        .or_else(|| s(v, "gpu"))
        .unwrap_or_else(|| node_id.clone());
    let backend = backend_of(&s(v, "backend").unwrap_or_default());
    let software = v.get("software").cloned().unwrap_or(Value::Null);
    let capability = v.get("capability").cloned().unwrap_or(Value::Null);
    let phase = s(v, "phase").filter(|p| p != "idle");
    let busy = u(v, "active_batches").unwrap_or(0) > 0;
    let training = busy
        || matches!(
            phase.as_deref(),
            Some("downloading" | "loading" | "training" | "uploading")
        );
    let build = s(&software, "torch_build").unwrap_or_default();
    let torch = s(&software, "torch").map(|t| {
        if build.is_empty() || build == "macOS" {
            t
        } else {
            format!("{t}+{build}")
        }
    });

    let is_self = match &me.node_id {
        Some(id) => *id == node_id,
        None => {
            me.contributing
                && b(v, "co_located")
                && me
                    .names
                    .iter()
                    .any(|n| n.eq_ignore_ascii_case(&display_name))
        }
    };
    let mut warnings: Vec<String> = v
        .get("warnings")
        .and_then(Value::as_array)
        .map(|w| {
            w.iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    if b(&software, "agent_behind") {
        warnings.push(format!(
            "Runs GradMesh {}, older than the host; rejoin to update.",
            s(&software, "agent").unwrap_or_default()
        ));
    }
    MeshNode {
        gpu: s(v, "gpu").unwrap_or_else(|| "No supported GPU".into()),
        backend,
        gpu_memory_mb: u(v, "gpu_memory_mb").filter(|m| *m > 0),
        gflops: f(&capability, "gflops").filter(|g| *g > 0.0),
        throughput_sps: f(v, "throughput_sps").filter(|t| *t > 0.0),
        active: b(v, "active"),
        training,
        progress: if training { progress_of(v) } else { None },
        phase,
        completed_rounds: u(v, "completed_rounds").unwrap_or(0) as u32,
        tier: s(v, "tier").unwrap_or_else(|| "probation".into()),
        fitness: f(v, "fitness").unwrap_or(0.0).clamp(0.0, 1.0) as f32,
        port: if is_self {
            me.port.clone()
        } else {
            port_of(backend, &build)
        },
        torch: if is_self {
            me.torch.clone().or(torch)
        } else {
            torch
        },
        os: s(&software, "os"),
        is_self,
        warnings,
        node_id,
        display_name,
    }
}

/// `GET /mesh` → snapshot (events are filled in by the caller from the feed).
pub fn snapshot(v: &Value, coordinator: &str, me: &SelfInfo) -> MeshSnapshot {
    let nodes = v
        .get("nodes")
        .and_then(Value::as_array)
        .map(|n| n.iter().map(|x| node(x, me)).collect())
        .unwrap_or_default();
    MeshSnapshot {
        mesh_id: s(v, "mesh_id").unwrap_or_default(),
        mesh_name: s(v, "mesh_name"),
        coordinator: coordinator.to_string(),
        version: s(v, "version").unwrap_or_default(),
        torch_ready: b(v, "torch_ready"),
        nodes,
        events: Vec::new(),
    }
}

/// The coordinator says "done"; the interface says "completed".
fn run_status(raw: &str) -> String {
    match raw {
        "done" | "completed" => "completed",
        "planning" | "running" | "waiting" | "stopped" | "failed" => raw,
        "stopping" => "stopped",
        _ => "running",
    }
    .to_string()
}

/// `round_history` records (0-based `round`) plus `accuracy_history` → per-round metrics.
pub fn round_metrics(run: &Value) -> Vec<RoundMetric> {
    let accuracy: Vec<&Value> = run
        .get("accuracy_history")
        .and_then(Value::as_array)
        .map(|a| a.iter().collect())
        .unwrap_or_default();
    let Some(history) = run.get("round_history").and_then(Value::as_array) else {
        return Vec::new();
    };
    history
        .iter()
        .map(|r| {
            let index = u(r, "round").unwrap_or(0);
            let map50 = r.get("accuracy").and_then(|a| f(a, "map50")).or_else(|| {
                accuracy
                    .iter()
                    .find(|a| u(a, "round") == Some(index))
                    .and_then(|a| f(a, "map50"))
            });
            let losses: Vec<f64> = r
                .get("shards")
                .and_then(Value::as_array)
                .map(|shards| {
                    shards
                        .iter()
                        .filter_map(|sh| {
                            sh.get("metrics")
                                .and_then(|m| f(m, "loss").or_else(|| f(m, "train_loss")))
                        })
                        .collect()
                })
                .unwrap_or_default();
            RoundMetric {
                round: index as u32 + 1,
                wall_clock_seconds: f(r, "wall_clock_seconds").unwrap_or(0.0),
                straggler_gap_seconds: f(r, "straggler_gap_seconds").unwrap_or(0.0),
                aggregation_seconds: f(r, "aggregation_seconds").unwrap_or(0.0),
                speedup: f(r, "speedup").unwrap_or(0.0),
                workers: u(r, "workers").unwrap_or(0) as u32,
                map50,
                loss: (!losses.is_empty())
                    .then(|| losses.iter().sum::<f64>() / losses.len() as f64),
            }
        })
        .collect()
}

/// A `/runs` summary (or a `/runs/{id}` view, which adds `round_history`).
pub fn run(v: &Value) -> RunSummary {
    let workers = u(v, "live_workers")
        .filter(|w| *w > 0)
        .or_else(|| u(v, "peak_workers"))
        .unwrap_or(0) as u32;
    RunSummary {
        id: s(v, "id").unwrap_or_default(),
        name: s(v, "name").unwrap_or_else(|| "Untitled run".into()),
        status: run_status(&s(v, "status").unwrap_or_default()),
        dataset: s(v, "dataset_name")
            .or_else(|| s(v, "dataset_id"))
            .unwrap_or_default(),
        base_model: s(v, "base_model").unwrap_or_default(),
        rounds: u(v, "rounds").unwrap_or(0) as u32,
        current_round: u(v, "current_round").unwrap_or(0) as u32,
        workers,
        speedup: f(v, "speedup").filter(|x| *x > 0.0),
        map50: f(v, "map50").or_else(|| f(v, "best_map50")),
        wall_clock_seconds: f(v, "wall_clock_seconds").filter(|x| *x > 0.0),
        started_at: f(v, "started_at").or_else(|| f(v, "created_at")),
        error: s(v, "error"),
        round_metrics: round_metrics(v),
    }
}

/// `GET /datasets` → `{"datasets": [...]}` (or a bare array).
pub fn datasets(v: &Value) -> Vec<Dataset> {
    let list = v
        .get("datasets")
        .and_then(Value::as_array)
        .or_else(|| v.as_array());
    list.map(|items| {
        items
            .iter()
            .map(|d| Dataset {
                id: s(d, "id").unwrap_or_default(),
                name: s(d, "name").unwrap_or_else(|| "Dataset".into()),
                images: u(d, "train_count").unwrap_or(0) as u32,
                classes: d
                    .get("class_names")
                    .and_then(Value::as_array)
                    .map(|c| c.len() as u32)
                    .unwrap_or(0),
                is_default: b(d, "is_default"),
                size_mb: u(d, "bytes")
                    .filter(|x| *x > 0)
                    .map(|bytes| (bytes as f64 / 1_048_576.0 * 10.0).round() / 10.0),
            })
            .collect()
    })
    .unwrap_or_default()
}

fn name_of(data: &Value) -> String {
    s(data, "name")
        .or_else(|| s(data, "node_id"))
        .unwrap_or_else(|| "A machine".into())
}

fn backend_label(data: &Value) -> String {
    match s(data, "backend").as_deref() {
        Some("cuda") => "NVIDIA (CUDA)".into(),
        Some("xpu") => "Intel (XPU)".into(),
        Some("mps") => "Apple (Metal)".into(),
        Some("rocm") => "AMD (ROCm)".into(),
        Some(other) => other.to_uppercase(),
        None => "CPU".into(),
    }
}

/// One `/events` payload (`{"id","kind","at","data"}`) in words. `None` for
/// bookkeeping events the feed does not show (`stream.ready`, ...).
pub fn describe(event: &Value) -> Option<MeshEvent> {
    let kind = s(event, "kind")?;
    let at = f(event, "at").unwrap_or_else(crate::backend::now);
    let d = event.get("data").cloned().unwrap_or(Value::Null);
    let round = |key: &str| {
        u(&d, key)
            .map(|r| r + 1)
            .map(|r| r.to_string())
            .unwrap_or_else(|| "?".into())
    };
    let text = match kind.as_str() {
        "stream.ready" | "internal.error" | "network.scanned" => return None,
        "coordinator.ready" => format!(
            "Coordinator {} is up.",
            s(&d, "version").unwrap_or_default()
        ),
        "node.joined" => {
            let mut t = format!("{} joined on {}", name_of(&d), backend_label(&d));
            if let Some(g) = f(&d, "gflops").filter(|g| *g > 0.0) {
                t.push_str(&format!(" at {g:.0} GFLOP/s"));
            }
            t.push('.');
            match (s(&d, "tier").as_deref(), s(&d, "reason")) {
                (Some("full"), _) => t.push_str(" Admitted."),
                (Some(tier), Some(reason)) => {
                    t.push_str(&format!(" {}: {reason}", capitalise(tier)))
                }
                (Some(tier), None) => t.push_str(&format!(" {}.", capitalise(tier))),
                _ => {}
            }
            t
        }
        "node.left" => format!("{} left the mesh.", name_of(&d)),
        "node.offline" => format!("{} stopped answering.", name_of(&d)),
        "node.evicted" => format!("{} was removed by the mesh owner.", name_of(&d)),
        "node.reset" => format!("{} was reset.", name_of(&d)),
        "node.version" => format!("{}: {}", name_of(&d), s(&d, "detail").unwrap_or_default()),
        "node.batch_capped" => format!(
            "{}'s batch is now capped at {} after running out of memory.",
            name_of(&d),
            s(&d, "batch_size").unwrap_or_default()
        ),
        "run.created" => format!(
            "Run {} created ({} rounds).",
            s(&d, "name").unwrap_or_default(),
            s(&d, "rounds").unwrap_or_else(|| "?".into())
        ),
        "run.waiting" => format!(
            "Run waiting: {}",
            s(&d, "reason").unwrap_or_else(|| "not enough machines".into())
        ),
        "run.stopped" => "Run stopped.".into(),
        "run.deleted" => "Run deleted.".into(),
        "run.failed" => format!("Run failed: {}", s(&d, "error").unwrap_or_default()),
        "run.completed" => {
            let summary = d.get("summary").cloned().unwrap_or(Value::Null);
            let mut t = format!("Run {} completed", s(&summary, "name").unwrap_or_default());
            if let Some(x) = f(&summary, "speedup").filter(|x| *x > 0.0) {
                t.push_str(&format!(" at {x:.2}x"));
            }
            t.push('.');
            t
        }
        "round.started" => {
            let plan = d.get("plan").cloned().unwrap_or(Value::Null);
            let machines = plan
                .get("assignments")
                .and_then(Value::as_array)
                .map(|a| a.len());
            let mut t = format!(
                "Round {} of {} planned",
                round("round"),
                s(&d, "total_rounds").unwrap_or_else(|| "?".into())
            );
            if let Some(m) = machines {
                t.push_str(&format!(
                    " across {m} machine{}",
                    if m == 1 { "" } else { "s" }
                ));
            }
            if let Some(p) = f(&plan, "predicted_makespan_seconds").filter(|p| *p > 0.0) {
                t.push_str(&format!(", predicted {p:.0} s"));
            }
            t.push('.');
            t
        }
        "round.completed" => format!(
            "Round {} done in {:.1} s. Straggler gap {:.1} s, aggregation {:.2} s, speedup {:.2}x.",
            round("round"),
            f(&d, "wall_clock_seconds").unwrap_or(0.0),
            f(&d, "straggler_gap_seconds").unwrap_or(0.0),
            f(&d, "aggregation_seconds").unwrap_or(0.0),
            f(&d, "speedup").unwrap_or(0.0)
        ),
        "round.evaluated" => match f(&d, "map50") {
            Some(m) => format!("Round {} scored mAP50 {m:.4}.", round("round")),
            None => format!(
                "Round {} could not be scored: {}",
                round("round"),
                s(&d, "error").unwrap_or_default()
            ),
        },
        "round.failed" => format!(
            "Round failed: {}{}",
            s(&d, "reason").unwrap_or_default(),
            if b(&d, "retrying") { " Retrying." } else { "" }
        ),
        "shard.assigned" => format!(
            "{} took a shard of round {}{}.",
            name_of(&d),
            round("round"),
            s(&d, "samples")
                .map(|n| format!(", {n} images"))
                .unwrap_or_default()
        ),
        "shard.completed" => format!(
            "{} finished its shard of round {} in {:.1} s.",
            name_of(&d),
            round("round"),
            f(&d, "seconds").unwrap_or(0.0)
        ),
        "shard.failed" => format!("A shard failed: {}", s(&d, "error").unwrap_or_default()),
        "shard.superseded" => "A slow shard was overtaken by its speculative copy.".into(),
        "mesh.token_rotated" => format!(
            "The mesh token was rotated; {} machine(s) must rejoin with the new invite.",
            s(&d, "removed").unwrap_or_else(|| "0".into())
        ),
        "policy.updated" => "The mesh policy was changed.".into(),
        "dataset.added"
        | "dataset.default"
        | "dataset.removed"
        | "dataset.subset"
        | "dataset.importing"
        | "dataset.import_failed" => {
            let what = kind.trim_start_matches("dataset.").replace('_', " ");
            let name = s(&d, "name")
                .or_else(|| s(&d, "key"))
                .or_else(|| s(&d, "dataset_id"))
                .unwrap_or_default();
            format!("Dataset {name}: {what}.")
        }
        other => {
            let words = other.replace(['.', '_'], " ");
            match s(&d, "name") {
                Some(n) => format!("{}: {n}", capitalise(&words)),
                None => capitalise(&words),
            }
        }
    };
    Some(MeshEvent { at, kind, text })
}

fn capitalise(text: &str) -> String {
    let mut c = text.chars();
    match c.next() {
        Some(first) => first.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn mesh_json() -> Value {
        json!({
            "mesh_name": "Lab", "mesh_id": "6a23583a1707", "version": "5.1.1", "protocol": 5, "torch_ready": false,
            "nodes": [{
                "node_id": "abcdef0123", "display_name": "lab-pc", "gpu": "NVIDIA GeForce RTX 3090", "backend": "cuda",
                "gpu_memory_mb": 24576, "capability": {"gflops": 29400.0}, "throughput_sps": 0.0, "active": true,
                "active_batches": 1, "phase": "training", "progress": {"batch": 3, "batches": 12}, "completed_rounds": 4,
                "tier": "full", "fitness": 0.91, "co_located": false, "warnings": ["on battery"],
                "software": {"torch": "2.13.0", "torch_build": "cu130", "os": "Ubuntu 24.04", "agent": "5.1.1", "agent_behind": false}
            }, {
                "node_id": "0123456789", "display_name": "mac", "gpu": "Apple M3", "backend": "mps", "active": false,
                "phase": "idle", "tier": "probation", "fitness": 0.4, "software": {"torch": "2.13.0", "torch_build": "macOS"}
            }]
        })
    }

    #[test]
    fn mesh_maps_nodes() {
        let me = SelfInfo {
            node_id: Some("0123456789".into()),
            port: Some("mps".into()),
            ..Default::default()
        };
        let snap = snapshot(&mesh_json(), "http://10.0.0.2:8000", &me);
        assert_eq!(snap.mesh_id, "6a23583a1707");
        assert_eq!(snap.mesh_name.as_deref(), Some("Lab"));
        let a = &snap.nodes[0];
        assert_eq!(a.backend, Backend::Cuda);
        assert!(a.training && a.active && !a.is_self);
        assert_eq!(a.progress, Some(0.25));
        assert_eq!(a.gflops, Some(29400.0));
        assert_eq!(a.throughput_sps, None);
        assert_eq!(a.port.as_deref(), Some("cuda-cu130"));
        assert_eq!(a.torch.as_deref(), Some("2.13.0+cu130"));
        assert_eq!(a.warnings, vec!["on battery".to_string()]);
        let m = &snap.nodes[1];
        assert!(m.is_self && !m.training && m.phase.is_none());
        assert_eq!(m.port.as_deref(), Some("mps"));
        assert_eq!(m.torch.as_deref(), Some("2.13.0"));
    }

    #[test]
    fn runs_map_status_and_rounds() {
        let v = json!({
            "id": "r1", "name": "smoke", "status": "done", "dataset_name": "COCO128", "base_model": "yolo11n.pt",
            "rounds": 2, "current_round": 2, "live_workers": 0, "peak_workers": 3, "speedup": 2.5, "wall_clock_seconds": 100.0,
            "created_at": 10.0, "map50": null, "best_map50": 0.5,
            "accuracy_history": [{"round": 1, "map50": 0.5}],
            "round_history": [
                {"round": 0, "workers": 3, "wall_clock_seconds": 50.0, "straggler_gap_seconds": 2.0, "aggregation_seconds": 0.5, "speedup": 2.4,
                 "shards": [{"metrics": {"loss": 1.0}}, {"metrics": {"loss": 2.0}}]},
                {"round": 1, "workers": 3, "wall_clock_seconds": 50.0, "straggler_gap_seconds": 1.0, "aggregation_seconds": 0.5, "speedup": 2.6}
            ]
        });
        let r = run(&v);
        assert_eq!(r.status, "completed");
        assert_eq!(r.workers, 3);
        assert_eq!(r.map50, Some(0.5));
        assert_eq!(r.round_metrics.len(), 2);
        assert_eq!(r.round_metrics[0].round, 1);
        assert_eq!(r.round_metrics[0].loss, Some(1.5));
        assert_eq!(r.round_metrics[1].map50, Some(0.5));
    }

    #[test]
    fn datasets_map() {
        let v = json!({"datasets": [{"id": "d1", "name": "parts", "train_count": 4812, "class_names": ["a", "b"], "is_default": true, "bytes": 1048576 * 3}]});
        let d = datasets(&v);
        assert_eq!(d[0].images, 4812);
        assert_eq!(d[0].classes, 2);
        assert_eq!(d[0].size_mb, Some(3.0));
    }

    #[test]
    fn events_in_words() {
        let joined = json!({"id": 3, "kind": "node.joined", "at": 5.0, "data": {"name": "lab-pc", "backend": "cuda", "gflops": 29400.0, "tier": "full"}});
        let e = describe(&joined).unwrap();
        assert_eq!(
            e.text,
            "lab-pc joined on NVIDIA (CUDA) at 29400 GFLOP/s. Admitted."
        );
        assert_eq!(e.at, 5.0);
        let round = json!({"kind": "round.completed", "at": 1.0, "data": {"round": 0, "wall_clock_seconds": 10.0, "straggler_gap_seconds": 1.0, "aggregation_seconds": 0.5, "speedup": 2.0}});
        assert!(describe(&round)
            .unwrap()
            .text
            .starts_with("Round 1 done in 10.0 s"));
        assert!(describe(&json!({"kind": "stream.ready", "data": {}})).is_none());
        assert_eq!(
            describe(&json!({"kind": "suite.started", "data": {}}))
                .unwrap()
                .text,
            "Suite started"
        );
    }
}
