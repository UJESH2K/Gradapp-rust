use std::path::PathBuf;

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("port manifest {file}: {message}")]
    Manifest { file: String, message: String },

    #[error("invalid port registry:\n  - {}", .0.join("\n  - "))]
    Invalid(Vec<String>),

    #[error("unknown port '{0}'")]
    UnknownPort(String),

    /// The port has no PyTorch build for this OS/CPU (e.g. an x86-only CUDA
    /// build on an arm64 machine, anything on an Intel Mac).
    #[error("port '{port}' has no build for {platform} (it supports {supported})")]
    UnsupportedPlatform {
        port: String,
        platform: String,
        supported: String,
    },

    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("uv is not available for this platform ({0}); set GRADAPP_UV to a uv binary")]
    UvUnsupported(String),

    #[error("downloading {url} failed: {message}")]
    Download { url: String, message: String },

    #[error("checksum mismatch for {file}: expected {expected}, got {actual}")]
    Checksum {
        file: String,
        expected: String,
        actual: String,
    },

    #[error("{step} failed{}: {message}", .code.map(|c| format!(" (exit {c})")).unwrap_or_default())]
    Command {
        step: String,
        code: Option<i32>,
        message: String,
    },

    /// Not enough free disk for the port (checked before anything is
    /// downloaded, and recognised in uv's output if it runs out mid-install).
    #[error("not enough disk space in {path}: {needed_gb:.1} GB needed, {free_gb:.1} GB free. Free up space on that drive and try again.")]
    DiskFull {
        path: PathBuf,
        needed_gb: f64,
        free_gb: f64,
    },

    #[error("{0}")]
    Other(String),
}

impl Error {
    pub(crate) fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Error::Io {
            path: path.into(),
            source,
        }
    }
}
