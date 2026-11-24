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
