use thiserror::Error;

/// Everything that can go wrong talking to the network.
#[derive(Debug, Error)]
pub enum NetError {
    /// Could not connect / DNS failed / connection dropped.
    #[error("could not reach {0}")]
    Unreachable(String),
    #[error("timed out talking to {0}")]
    Timeout(String),
    /// The server answered with a non-2xx status. `detail` is FastAPI's
    /// `detail` field when there is one (same rule as v1's `detailOf`).
    #[error("{detail}")]
    Http { status: u16, detail: String },
    /// Path or method refused by the client-side allowlist (v1 api.js `ALLOWED`).
    #[error("blocked: {0}")]
    Blocked(String),
    #[error("bad response: {0}")]
    Decode(String),
    #[error("invalid address: {0}")]
    InvalidAddress(String),
    #[error("cancelled")]
    Cancelled,
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub type Result<T, E = NetError> = std::result::Result<T, E>;

impl NetError {
    pub(crate) fn from_reqwest(err: reqwest::Error, what: &str) -> Self {
        if err.is_timeout() {
            NetError::Timeout(what.to_string())
        } else if err.is_decode() {
            NetError::Decode(err.to_string())
        } else if let Some(status) = err.status() {
            NetError::Http { status: status.as_u16(), detail: format!("HTTP {}", status.as_u16()) }
        } else {
            NetError::Unreachable(what.to_string())
        }
    }

    /// HTTP status, when the server answered.
    pub fn status(&self) -> Option<u16> {
        match self {
            NetError::Http { status, .. } => Some(*status),
            _ => None,
        }
    }
}

/// v1 `detailOf`: FastAPI puts a string or a list of `{msg}` in `detail`.
pub(crate) fn detail_of(body: Option<&serde_json::Value>, status: u16) -> String {
    let fallback = format!("HTTP {status}");
    let Some(body) = body else { return fallback };
    match body.get("detail") {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Array(items)) => items
            .iter()
            .map(|item| match item.get("msg").and_then(|m| m.as_str()) {
                Some(msg) => msg.to_string(),
                None => item.to_string(),
            })
            .collect::<Vec<_>>()
            .join("; "),
        _ => fallback,
    }
}
