//! The port manifests: embedded defaults, a dev override, and validation.

use std::collections::HashSet;
use std::path::Path;

use gm_types::{Backend, PortSpec};

use crate::error::{Error, Result};

/// Mirrors `desktop/engine/version.py::REFERENCE_STACK`. Reference ports must pin these.
pub const REFERENCE_TORCH: &str = "2.13.0";
pub const REFERENCE_TORCHVISION: &str = "0.28.0";
pub const REFERENCE_ULTRALYTICS: &str = "8.4.46";

/// Every port id gm-probe can emit. The built-in registry has exactly these.
pub const PORT_IDS: &[&str] = &[
    "cuda-cu130",
    "cuda-cu126",
    "cuda-cu128",
    "xpu",
    "mps",
    "cpu",
    "rocm",
];

/// The OS/arch pairs a manifest may list.
pub const PLATFORMS: &[&str] = &[
    "windows-x86_64",
    "windows-arm64",
    "linux-x86_64",
    "linux-arm64",
    "macos-arm64",
];

/// CPython minors PyTorch 2.13 ships wheels for on every supported backend
/// (version.py SUPPORTED_PYTHON = 3.10 .. 3.13).
const SUPPORTED_PYTHON_MINORS: std::ops::RangeInclusive<u32> = 10..=13;

/// (file name, contents) of the manifests compiled into the app.
pub const BUILTIN_MANIFESTS: &[(&str, &str)] = &[
    (
        "cuda-cu130.toml",
        include_str!("../../../ports/cuda-cu130.toml"),
    ),
    (
        "cuda-cu126.toml",
        include_str!("../../../ports/cuda-cu126.toml"),
    ),
    (
        "cuda-cu128.toml",
        include_str!("../../../ports/cuda-cu128.toml"),
    ),
    ("xpu.toml", include_str!("../../../ports/xpu.toml")),
    ("mps.toml", include_str!("../../../ports/mps.toml")),
    ("cpu.toml", include_str!("../../../ports/cpu.toml")),
    ("rocm.toml", include_str!("../../../ports/rocm.toml")),
];

/// This machine's platform string (`linux-x86_64`, ...), if it is one ports support.
pub fn current_platform() -> Option<&'static str> {
    platform_for(std::env::consts::OS, std::env::consts::ARCH)
}

fn platform_for(os: &str, arch: &str) -> Option<&'static str> {
    Some(match (os, arch) {
        ("windows", "x86_64") => "windows-x86_64",
        ("windows", "aarch64") => "windows-arm64",
        ("linux", "x86_64") => "linux-x86_64",
        ("linux", "aarch64") => "linux-arm64",
        ("macos", "aarch64") => "macos-arm64",
        _ => return None,
    })
}

/// `Err(UnsupportedPlatform)` unless `spec` lists `platform`. `None` means
/// a machine no port supports (Intel Mac, 32-bit ARM, ...).
pub(crate) fn check_platform(spec: &PortSpec, platform: Option<&str>) -> Result<()> {
    match platform {
        Some(p) if spec.platforms.iter().any(|x| x == p) => Ok(()),
        _ => Err(Error::UnsupportedPlatform {
            port: spec.id.clone(),
            platform: platform
                .map(str::to_string)
                .unwrap_or_else(|| format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)),
            supported: spec.platforms.join(", "),
        }),
    }
}

/// The `--python-platform` value uv understands for a port platform. Used to
/// resolve (or build a wheelhouse for) a port on a machine other than this one.
pub fn uv_python_platform(platform: &str) -> Option<&'static str> {
    Some(match platform {
        "windows-x86_64" => "x86_64-pc-windows-msvc",
        "windows-arm64" => "aarch64-pc-windows-msvc",
        "linux-x86_64" => "x86_64-manylinux_2_28",
        "linux-arm64" => "aarch64-manylinux_2_28",
        "macos-arm64" => "aarch64-apple-darwin",
        _ => return None,
    })
}

/// The set of port manifests.
#[derive(Debug, Clone, PartialEq)]
pub struct Registry {
    ports: Vec<PortSpec>,
}

impl Default for Registry {
    fn default() -> Self {
        Self::builtin()
    }
}

impl Registry {
    /// The manifests compiled into this binary. They are checked by the crate's
    /// tests, so a parse failure here is a build defect, not a runtime condition.
    pub fn builtin() -> Self {
        Self::from_sources(BUILTIN_MANIFESTS.iter().copied())
            .expect("embedded ports/*.toml must parse (covered by gm-ports tests)")
    }

    /// Parse manifests from (name, toml) pairs. Does not validate; call [`Registry::validate`].
    pub fn from_sources<'a>(sources: impl IntoIterator<Item = (&'a str, &'a str)>) -> Result<Self> {
        let mut ports = Vec::new();
        for (name, text) in sources {
            let spec: PortSpec = toml::from_str(text).map_err(|e| Error::Manifest {
                file: name.to_string(),
                message: e.to_string(),
            })?;
            let stem = name.strip_suffix(".toml").unwrap_or(name);
            if stem != spec.id {
                return Err(Error::Manifest {
                    file: name.to_string(),
                    message: format!("file name must match the id '{}'", spec.id),
                });
            }
            ports.push(spec);
        }
        Ok(Registry { ports })
    }

    /// Development override: load every `*.toml` in `dir` instead of the
    /// embedded manifests. Files are read in name order.
    pub fn load_dir(dir: impl AsRef<Path>) -> Result<Self> {
        let dir = dir.as_ref();
        let mut files = Vec::new();
        for entry in std::fs::read_dir(dir).map_err(|e| Error::io(dir, e))? {
            let path = entry.map_err(|e| Error::io(dir, e))?.path();
            if path.extension().is_some_and(|x| x == "toml") && path.is_file() {
                let text = std::fs::read_to_string(&path).map_err(|e| Error::io(&path, e))?;
                let name = path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned();
                files.push((name, text));
            }
        }
        files.sort();
        Self::from_sources(files.iter().map(|(n, t)| (n.as_str(), t.as_str())))
    }

    /// `GRADAPP_PORTS_DIR` if set (development), else the embedded manifests.
    pub fn from_env_or_builtin() -> Result<Self> {
        match std::env::var_os("GRADAPP_PORTS_DIR") {
            Some(dir) if !dir.is_empty() => Self::load_dir(dir),
            _ => Ok(Self::builtin()),
        }
    }

    pub fn get(&self, id: &str) -> Option<&PortSpec> {
        self.ports.iter().find(|p| p.id == id)
    }

    /// Like [`Registry::get`] but an error naming the id.
    pub fn require(&self, id: &str) -> Result<&PortSpec> {
        self.get(id)
            .ok_or_else(|| Error::UnknownPort(id.to_string()))
    }

    /// Like [`Registry::require`], and the port must have a build for
    /// `platform` (e.g. [`current_platform`]).
    pub fn require_on(&self, id: &str, platform: &str) -> Result<&PortSpec> {
        let spec = self.require(id)?;
        check_platform(spec, Some(platform))?;
        Ok(spec)
    }

    pub fn all(&self) -> &[PortSpec] {
        &self.ports
    }

    /// Ports that can run on `platform` (e.g. [`current_platform`]).
    pub fn for_platform<'a>(
        &'a self,
        platform: &'a str,
    ) -> impl Iterator<Item = &'a PortSpec> + 'a {
        self.ports
            .iter()
            .filter(move |p| p.platforms.iter().any(|x| x == platform))
    }

    /// Every problem in the registry, or Ok.
    pub fn validate(&self) -> Result<()> {
        let mut problems = Vec::new();
        let mut seen = HashSet::new();
        for spec in &self.ports {
            if !seen.insert(spec.id.as_str()) {
                problems.push(format!("{}: duplicate port id", spec.id));
            }
            problems.extend(
                validate_spec(spec)
                    .into_iter()
                    .map(|p| format!("{}: {p}", spec.id)),
            );
        }
        if problems.is_empty() {
            Ok(())
        } else {
            Err(Error::Invalid(problems))
        }
    }
}

fn package_name(requirement: &str) -> String {
    requirement
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.'))
        .next()
        .unwrap_or("")
        .to_ascii_lowercase()
        .replace('_', "-")
}

/// Problems with one manifest (empty when it is valid).
pub fn validate_spec(spec: &PortSpec) -> Vec<String> {
    let mut p = Vec::new();

    if spec.id.is_empty()
        || !spec
            .id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        p.push("id must be lowercase letters, digits and '-'".into());
    }
    let family = spec.id.split('-').next().unwrap_or("");
    if family != spec.backend.as_str() {
        p.push(format!(
            "id must start with its backend '{}'",
            spec.backend.as_str()
        ));
    }
    if spec.label.trim().is_empty() {
        p.push("label is empty".into());
    }

    // python: exactly "3.N" with N in the supported range.
    match spec.python.split_once('.') {
        Some(("3", minor))
            if minor
                .parse::<u32>()
                .is_ok_and(|m| SUPPORTED_PYTHON_MINORS.contains(&m)) => {}
        _ => p.push(format!(
            "python '{}' must be \"3.N\" with N in {}..={}",
            spec.python,
            SUPPORTED_PYTHON_MINORS.start(),
            SUPPORTED_PYTHON_MINORS.end()
        )),
    }

    for (name, v) in [("torch", &spec.torch), ("torchvision", &spec.torchvision)] {
        let parts: Vec<_> = v.split('.').collect();
        if parts.len() != 3
            || !parts
                .iter()
                .all(|x| !x.is_empty() && x.chars().all(|c| c.is_ascii_digit()))
        {
            p.push(format!(
                "{name} '{v}' must be an exact X.Y.Z version without a suffix"
            ));
        }
    }

    // The local suffix and the index must name the same build.
    match (&spec.index_url, spec.local_suffix.as_str()) {
        (None, "") => {}
        (None, s) => p.push(format!(
            "local_suffix '{s}' needs an index_url that serves it"
        )),
        (Some(url), "") => p.push(format!("index_url {url} set but local_suffix is empty")),
        (Some(url), s) => {
            if !url.starts_with("https://") {
                p.push(format!("index_url {url} must be https"));
            }
            let last = url.trim_end_matches('/').rsplit('/').next().unwrap_or("");
            match s.strip_prefix('+') {
                Some(tag) if tag == last => {}
                Some(_) => p.push(format!("local_suffix '{s}' does not match index_url {url}")),
                None => p.push(format!("local_suffix '{s}' must start with '+'")),
            }
        }
    }
    if spec.id.starts_with("cuda-") {
        let tag = spec.id.trim_start_matches("cuda-");
        if spec.local_suffix != format!("+{tag}") {
            p.push(format!(
                "cuda port id says {tag} but local_suffix is '{}'",
                spec.local_suffix
            ));
        }
    }

    if spec.platforms.is_empty() {
        p.push("platforms is empty".into());
    }
    let mut seen = HashSet::new();
    for plat in &spec.platforms {
        if !PLATFORMS.contains(&plat.as_str()) {
            p.push(format!(
                "unknown platform '{plat}' (expected one of {})",
                PLATFORMS.join(", ")
            ));
        }
        if !seen.insert(plat) {
            p.push(format!("platform '{plat}' listed twice"));
        }
    }
    if spec.backend == Backend::Mps && spec.platforms.iter().any(|x| x != "macos-arm64") {
        p.push("mps runs on macos-arm64 only".into());
    }

    let mut names = HashSet::new();
    for req in &spec.packages {
        let name = package_name(req);
        if name.is_empty() {
            p.push(format!("package '{req}' has no name"));
        }
        if name == "torch" || name == "torchvision" {
            p.push(format!(
                "package '{req}': torch/torchvision are pinned by the torch/torchvision fields"
            ));
        }
        if !names.insert(name.clone()) {
            p.push(format!("package '{name}' listed twice"));
        }
    }

    if spec.min_driver.is_some() && spec.backend != Backend::Cuda {
        p.push("min_driver only applies to cuda ports".into());
    }
    if spec.backend == Backend::Cuda && spec.min_driver.is_none() {
        p.push("cuda ports must set min_driver".into());
    }
    if spec.reference && spec.experimental {
        p.push("an experimental port cannot be on the reference stack".into());
    }
    if spec.reference {
        if spec.torch != REFERENCE_TORCH {
            p.push(format!(
                "reference port pins torch {} but the reference stack is {REFERENCE_TORCH}",
                spec.torch
            ));
        }
        if spec.torchvision != REFERENCE_TORCHVISION {
            p.push(format!(
                "reference port pins torchvision {} but the reference stack is {REFERENCE_TORCHVISION}",
                spec.torchvision
            ));
        }
        let ultra = spec
            .packages
            .iter()
            .find(|r| package_name(r) == "ultralytics");
        let want = format!("ultralytics=={REFERENCE_ULTRALYTICS}");
        if ultra.map(|r| r.replace(' ', "")) != Some(want.clone()) {
            p.push(format!("reference port must pin {want}"));
        }
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_names() {
        assert_eq!(package_name("uvicorn[standard]>=0.30"), "uvicorn");
        assert_eq!(package_name("numpy>=1.26,<3"), "numpy");
        assert_eq!(package_name("python_multipart"), "python-multipart");
        assert_eq!(package_name("Torch==2"), "torch");
    }

    #[test]
    fn platform_mapping() {
        assert_eq!(platform_for("linux", "x86_64"), Some("linux-x86_64"));
        assert_eq!(platform_for("macos", "x86_64"), None);
        for plat in PLATFORMS {
            assert!(uv_python_platform(plat).is_some(), "{plat}");
        }
    }
}
