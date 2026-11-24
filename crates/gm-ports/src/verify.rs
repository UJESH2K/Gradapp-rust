//! Post-install verification: a real kernel launch on the port's backend.
//!
//! A Rust-embedded copy of `desktop/engine/setup_env.py::VERIFY_SNIPPET` and
//! `verify()`. `is_available()` alone is not enough: it reports True for a
//! Blackwell card on a build that has no kernels for it.

use gm_types::Backend;
use serde::{Deserialize, Serialize};

/// Runs inside the port's python. Prints one `GRADMESH_VERIFY {json}` line.
/// argv[1] is "1" when ultralytics should be imported too.
pub const VERIFY_SNIPPET: &str = r#"
import json, sys, os
os.environ.setdefault("PYTORCH_ENABLE_MPS_FALLBACK", "1")
out = {"python": sys.version.split()[0]}
check_ultralytics = len(sys.argv) > 1 and sys.argv[1] == "1"
try:
    import torch
    out["torch"] = torch.__version__
    out["cuda_build"] = getattr(torch.version, "cuda", None)
    out["hip_build"] = getattr(torch.version, "hip", None)
    def launch(device):
        x = torch.arange(1024, device=device, dtype=torch.float32)
        y = (x * 2.0 + 1.0).sum()
        return abs(float(y.item()) - (1023 * 1024 + 1024)) < 1e-2
    try:
        out["cpu_kernel"] = launch("cpu")
    except Exception as exc:
        out["cpu_kernel"] = False
        out["cpu_error"] = "%s: %s" % (type(exc).__name__, exc)
    out["cuda"] = bool(torch.cuda.is_available())
    if out["cuda"]:
        out["device"] = torch.cuda.get_device_name(0)
        major, minor = torch.cuda.get_device_capability(0)
        out["capability"] = "%d.%d" % (major, minor)
        out["arch_list"] = list(torch.cuda.get_arch_list())
        try:
            out["cuda_kernel"] = launch("cuda:0")
        except Exception as exc:
            out["cuda_kernel"] = False
            out["cuda_error"] = "%s: %s" % (type(exc).__name__, exc)
    out["xpu"] = bool(hasattr(torch, "xpu") and torch.xpu.is_available())
    if out["xpu"]:
        out["device"] = torch.xpu.get_device_name(0)
        try:
            out["xpu_kernel"] = launch("xpu:0")
        except Exception as exc:
            out["xpu_kernel"] = False
            out["xpu_error"] = "%s: %s" % (type(exc).__name__, exc)
    mps = getattr(torch.backends, "mps", None)
    out["mps"] = bool(mps is not None and mps.is_available())
    if out["mps"]:
        out["device"] = "Apple GPU (Metal)"
        try:
            out["mps_kernel"] = launch("mps")
        except Exception as exc:
            out["mps_kernel"] = False
            out["mps_error"] = "%s: %s" % (type(exc).__name__, exc)
except Exception as exc:
    out["error"] = "%s: %s" % (type(exc).__name__, exc)
if check_ultralytics:
    try:
        import ultralytics
        out["ultralytics"] = ultralytics.__version__
    except Exception as exc:
        out["ultralytics_error"] = "%s: %s" % (type(exc).__name__, exc)
print("GRADMESH_VERIFY " + json.dumps(out), flush=True)
"#;

pub(crate) const VERIFY_PREFIX: &str = "GRADMESH_VERIFY ";

/// The snippet's report, plus the verdict.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct VerifyReport {
    #[serde(default)]
    pub python: Option<String>,
    #[serde(default)]
    pub torch: Option<String>,
    #[serde(default)]
    pub cuda_build: Option<String>,
    #[serde(default)]
    pub hip_build: Option<String>,
    #[serde(default)]
    pub device: Option<String>,
    #[serde(default)]
    pub capability: Option<String>,
    #[serde(default)]
    pub ultralytics: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
    /// Everything the snippet printed, verbatim.
    #[serde(default)]
    pub raw: serde_json::Value,
    /// No problem found.
    #[serde(default)]
    pub ok: bool,
    #[serde(default)]
    pub problem: Option<String>,
}

fn flag(raw: &serde_json::Value, key: &str) -> bool {
    raw.get(key).and_then(|v| v.as_bool()).unwrap_or(false)
}

fn text(raw: &serde_json::Value, key: &str) -> Option<String> {
    raw.get(key).and_then(|v| v.as_str()).map(str::to_string)
}

/// Read the snippet's stdout (or a failure tail) and decide whether `backend` works.
pub fn interpret(
    stdout: &str,
    stderr_tail: &str,
    backend: Backend,
    check_ultralytics: bool,
) -> VerifyReport {
    let raw = stdout
        .lines()
        .rev()
        .find_map(|l| l.strip_prefix(VERIFY_PREFIX))
        .and_then(|j| serde_json::from_str::<serde_json::Value>(j).ok());
    let raw = match raw {
        Some(v) => v,
        None => {
            let tail: String = stderr_tail
                .chars()
                .rev()
                .take(800)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            serde_json::json!({ "error": if tail.trim().is_empty() { "the interpreter printed no report".to_string() } else { tail } })
        }
    };

    let err = |k: &str| text(&raw, k).unwrap_or_else(|| "unknown".into());
    let problem = if let Some(e) = text(&raw, "error") {
        Some(format!("PyTorch does not import: {e}"))
    } else {
        match backend {
            Backend::Cuda | Backend::Rocm => {
                let vendor = if backend == Backend::Rocm {
                    "AMD"
                } else {
                    "NVIDIA"
                };
                if !flag(&raw, "cuda") {
                    Some(format!(
                        "this PyTorch build cannot see the {vendor} GPU (torch.cuda.is_available() is False). \
                         The driver is usually missing or older than this build needs."
                    ))
                } else if !flag(&raw, "cuda_kernel") {
                    Some(format!(
                        "the GPU is visible but a kernel launch failed ({}). This is the 'no kernel image' failure: \
                         the build has no kernels for compute {}.",
                        err("cuda_error"),
                        text(&raw, "capability").unwrap_or_else(|| "unknown".into())
                    ))
                } else {
                    None
                }
            }
            Backend::Xpu => {
                if !flag(&raw, "xpu") {
                    Some(
                        "the Intel XPU runtime did not start (torch.xpu.is_available() is False). \
                          Install or update the Intel graphics driver."
                            .into(),
                    )
                } else if !flag(&raw, "xpu_kernel") {
                    Some(format!(
                        "the Intel GPU is visible but a kernel launch failed ({}).",
                        err("xpu_error")
                    ))
                } else {
                    None
                }
            }
            Backend::Mps => {
                if !flag(&raw, "mps") {
                    Some("the Metal backend is not available (torch.backends.mps.is_available() is False).".into())
                } else if !flag(&raw, "mps_kernel") {
                    Some(format!(
                        "Metal is visible but a kernel launch failed ({}).",
                        err("mps_error")
                    ))
                } else {
                    None
                }
            }
            Backend::Cpu => {
                if !flag(&raw, "cpu_kernel") {
                    Some(format!(
                        "a CPU tensor operation failed ({}).",
                        err("cpu_error")
                    ))
                } else {
                    None
                }
            }
        }
    };
    let problem = problem.or_else(|| {
        if check_ultralytics {
            text(&raw, "ultralytics_error").map(|e| format!("Ultralytics does not import: {e}"))
        } else {
            None
        }
    });

    VerifyReport {
        python: text(&raw, "python"),
        torch: text(&raw, "torch"),
        cuda_build: text(&raw, "cuda_build"),
        hip_build: text(&raw, "hip_build"),
        device: text(&raw, "device")
            .or_else(|| (backend == Backend::Cpu).then(|| "CPU".to_string())),
        capability: text(&raw, "capability"),
        ultralytics: text(&raw, "ultralytics"),
        error: text(&raw, "error"),
        ok: problem.is_none(),
        problem,
        raw,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cuda_ok_and_no_kernel_image() {
        let ok = r#"GRADMESH_VERIFY {"torch":"2.13.0+cu130","cuda":true,"cuda_kernel":true,"device":"RTX 5090","cpu_kernel":true}"#;
        let r = interpret(ok, "", Backend::Cuda, false);
        assert!(r.ok, "{r:?}");
        assert_eq!(r.device.as_deref(), Some("RTX 5090"));

        let bad = r#"noise
GRADMESH_VERIFY {"torch":"2.13.0+cu126","cuda":true,"cuda_kernel":false,"capability":"12.0","cuda_error":"RuntimeError: no kernel image"}"#;
        let r = interpret(bad, "", Backend::Cuda, false);
        assert!(!r.ok);
        assert!(r.problem.unwrap().contains("compute 12.0"));
    }

    #[test]
    fn import_failure_and_missing_report() {
        let r = interpret(
            r#"GRADMESH_VERIFY {"error":"ModuleNotFoundError: torch"}"#,
            "",
            Backend::Mps,
            false,
        );
        assert!(r.problem.unwrap().contains("does not import"));
        let r = interpret("", "Segmentation fault", Backend::Cpu, false);
        assert!(!r.ok);
        assert!(r.error.unwrap().contains("Segmentation"));
    }

    #[test]
    fn ultralytics_only_when_asked() {
        let out = r#"GRADMESH_VERIFY {"torch":"2.13.0","cpu_kernel":true,"ultralytics_error":"ImportError: x"}"#;
        assert!(interpret(out, "", Backend::Cpu, false).ok);
        assert!(!interpret(out, "", Backend::Cpu, true).ok);
    }
}
