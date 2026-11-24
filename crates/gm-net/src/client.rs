//! The coordinator API. Port of v1 `api.js`: same `X-Mesh-Token` header
//! (coordinator `require_mesh_token`), same path allowlist, same error
//! detail rule, and the `/events` server-sent-event stream with reconnects.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use futures::{Stream, StreamExt};
use gm_types::MeshHealth;
use reqwest::Method;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::AsyncReadExt;
use tokio_util::sync::CancellationToken;

use crate::download::save_response;
use crate::error::{detail_of, NetError, Result};
use crate::invite::{health_from_value, lan_client};

/// The auth header the coordinator checks (`require_mesh_token`).
pub const TOKEN_HEADER: &str = "X-Mesh-Token";

/// v1 api.js `ALLOWED`, without regexes. `[\w-]+` is one id segment.
fn allowed(path: &str) -> bool {
    let id = |s: &str| {
        !s.is_empty()
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    };
    let parts: Vec<&str> = match path.strip_prefix('/') {
        Some(rest) => rest.split('/').collect(),
        None => return false,
    };
    match parts.as_slice() {
        ["health"]
        | ["mesh"]
        | ["runs"]
        | ["datasets"]
        | ["models"]
        | ["policy"]
        | ["token", "rotate"] => true,
        ["runs", r] => id(r),
        ["runs", r, "stop"] => id(r),
        // Trained weights: the `.pt` artifact, and the raw current weights.
        ["runs", r, "artifact"] | ["runs", r, "weights.bin"] => id(r),
        ["datasets", d] => id(d), // includes "standard"
        ["datasets", d, "default"] => id(d),
        ["datasets", d, "download"] => id(d),
        ["nodes", n] => id(n),
        ["nodes", n, "reset"] => id(n),
        _ => false,
    }
}

/// Uploads and downloads may take this long in total (multi-GB datasets on WiFi).
const TRANSFER_TIMEOUT: Duration = Duration::from_secs(6 * 60 * 60);
/// Upload chunk size: progress is reported once per chunk.
const UPLOAD_CHUNK: usize = 256 * 1024;

/// A file name the server suggested (`Content-Disposition`), reduced to
/// characters that are safe on Windows, macOS and Linux. Never a path.
pub fn safe_file_name(name: &str) -> Option<String> {
    let base = name.rsplit(['/', '\\']).next().unwrap_or("");
    let cleaned: String = base
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | ' ' | '(' | ')') {
                c
            } else {
                '-'
            }
        })
        .collect();
    let cleaned = cleaned.trim_matches(|c: char| c == '.' || c == ' ' || c == '-');
    (!cleaned.is_empty()).then(|| cleaned.chars().take(120).collect())
}

/// `filename="x.zip"` (or `filename=x.zip`) from a `Content-Disposition` header.
fn disposition_name(header: &str) -> Option<String> {
    header.split(';').map(str::trim).find_map(|part| {
        let value = part.strip_prefix("filename=")?;
        Some(value.trim_matches('"').to_string())
    })
}

/// `dir/name`, or `dir/name (1).ext`, `(2)`... when that file already exists.
fn unique_path(dir: &Path, name: &str) -> PathBuf {
    let first = dir.join(name);
    if !first.exists() {
        return first;
    }
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    (1..10_000)
        .map(|n| dir.join(format!("{stem} ({n}){ext}")))
        .find(|p| !p.exists())
        .unwrap_or(first)
}

/// One event from `GET /events` (`event: <kind>\ndata: <json>\n\n`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SseEvent {
    /// The `event:` field; the coordinator sets it to the event kind.
    pub event: Option<String>,
    pub id: Option<String>,
    /// Parsed `data:` JSON: `{"id","kind","at","data"}` from the coordinator.
    pub data: Value,
}

impl SseEvent {
    /// The coordinator's `kind` (e.g. "run.started", "stream.ready").
    pub fn kind(&self) -> Option<&str> {
        self.data
            .get("kind")
            .and_then(Value::as_str)
            .or(self.event.as_deref())
    }
}

/// Incremental `text/event-stream` parser. Handles chunks split anywhere
/// (including inside UTF-8 sequences), CRLF, comments (`: keepalive`) and
/// multi-line data. Blocks whose data is not JSON are skipped, as in v1.
#[derive(Debug, Default)]
pub struct SseParser {
    buf: Vec<u8>,
}

impl SseParser {
    pub fn feed(&mut self, chunk: &[u8]) -> Vec<SseEvent> {
        self.buf
            .extend(chunk.iter().copied().filter(|b| *b != b'\r'));
        let mut out = Vec::new();
        while let Some(end) = self.buf.windows(2).position(|w| w == b"\n\n") {
            let block: Vec<u8> = self.buf.drain(..end + 2).collect();
            let text = String::from_utf8_lossy(&block[..end]);
            let (mut event, mut id, mut data) = (None, None, Vec::new());
            for line in text.split('\n') {
                if line.starts_with(':') {
                    continue;
                }
                let (field, value) = line.split_once(':').unwrap_or((line, ""));
                let value = value.strip_prefix(' ').unwrap_or(value);
                match field {
                    "event" => event = Some(value.to_string()),
                    "id" => id = Some(value.to_string()),
                    "data" => data.push(value.trim_start().to_string()),
                    _ => {}
                }
            }
            if data.is_empty() {
                continue;
            }
            if let Ok(value) = serde_json::from_str(&data.join("\n")) {
                out.push(SseEvent {
                    event,
                    id,
                    data: value,
                });
            }
        }
        out
    }
}

/// A coordinator, addressed by its base URL (`http://host:8000`).
/// Cheap to clone; clones share the token (so `rotate_token` updates all).
#[derive(Clone)]
pub struct CoordinatorClient {
    base: String,
    token: Arc<RwLock<Option<String>>>,
    http: reqwest::Client,
    timeout: Duration,
}

impl std::fmt::Debug for CoordinatorClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CoordinatorClient")
            .field("base", &self.base)
            .finish_non_exhaustive()
    }
}

impl CoordinatorClient {
    /// Uses the shared pooled LAN client (keep-alive, no system proxy).
    pub fn new(coordinator: impl Into<String>, token: Option<String>) -> Self {
        Self::with_http(lan_client(), coordinator, token)
    }

    pub fn with_http(
        http: reqwest::Client,
        coordinator: impl Into<String>,
        token: Option<String>,
    ) -> Self {
        Self {
            base: coordinator.into().trim_end_matches('/').to_string(),
            token: Arc::new(RwLock::new(token)),
            http,
            timeout: Duration::from_secs(20),
        }
    }

    /// Per-request timeout for JSON calls (default 20 s, as in v1).
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn base(&self) -> &str {
        &self.base
    }

    pub fn token(&self) -> Option<String> {
        self.token.read().ok().and_then(|t| t.clone())
    }

    pub fn set_token(&self, token: Option<String>) {
        if let Ok(mut t) = self.token.write() {
            *t = token;
        }
    }

    fn request(&self, method: Method, route: &str) -> reqwest::RequestBuilder {
        self.http
            .request(method, format!("{}{}", self.base, route))
            .header(TOKEN_HEADER, self.token().unwrap_or_default())
    }

    /// Generic allowlisted call (v1 `call`): `route` may carry a query string;
    /// its path must be one the app uses. Returns parsed JSON (or a JSON
    /// string for a non-JSON body, `null` for an empty one).
    pub async fn call(&self, method: Method, route: &str, body: Option<&Value>) -> Result<Value> {
        let path = route.split('?').next().unwrap_or("");
        let verb_ok = [Method::GET, Method::POST, Method::PUT, Method::DELETE].contains(&method);
        if !verb_ok || !allowed(path) {
            return Err(NetError::Blocked(format!("{method} {path}")));
        }
        let mut req = self.request(method, route).timeout(self.timeout);
        if let Some(body) = body {
            req = req.json(body);
        }
        let response = req
            .send()
            .await
            .map_err(|e| NetError::from_reqwest(e, &self.base))?;
        let status = response.status();
        let text = response
            .text()
            .await
            .map_err(|e| NetError::from_reqwest(e, &self.base))?;
        let data = if text.is_empty() {
            Value::Null
        } else {
            serde_json::from_str(&text).unwrap_or(Value::String(text))
        };
        if !status.is_success() {
            return Err(NetError::Http {
                status: status.as_u16(),
                detail: detail_of(Some(&data), status.as_u16()),
            });
        }
        Ok(data)
    }

    async fn get(&self, route: &str) -> Result<Value> {
        self.call(Method::GET, route, None).await
    }

    /// `GET /health` (unauthenticated).
    pub async fn health(&self) -> Result<MeshHealth> {
        let v = self.get("/health").await?;
        health_from_value(&v).ok_or_else(|| NetError::Decode("not a GradMesh /health".into()))
    }

    /// `GET /mesh`: everything the dashboard needs for a cold render.
    pub async fn mesh(&self) -> Result<Value> {
        self.get("/mesh").await
    }

    /// `GET /runs` → the `runs` array.
    pub async fn runs(&self) -> Result<Vec<Value>> {
        match self.get("/runs").await? {
            Value::Object(mut o) => match o.remove("runs") {
                Some(Value::Array(runs)) => Ok(runs),
                _ => Err(NetError::Decode("/runs without a runs array".into())),
            },
            Value::Array(runs) => Ok(runs),
            _ => Err(NetError::Decode("/runs is not an object".into())),
        }
    }

    pub async fn run(&self, id: &str) -> Result<Value> {
        self.get(&format!("/runs/{id}")).await
    }

    /// `POST /runs` with a `RunRequest` body.
    pub async fn create_run(&self, request: &Value) -> Result<Value> {
        self.call(Method::POST, "/runs", Some(request)).await
    }

    pub async fn stop_run(&self, id: &str) -> Result<Value> {
        self.call(Method::POST, &format!("/runs/{id}/stop"), None)
            .await
    }

    pub async fn delete_run(&self, id: &str) -> Result<Value> {
        self.call(Method::DELETE, &format!("/runs/{id}"), None)
            .await
    }

    pub async fn datasets(&self) -> Result<Value> {
        self.get("/datasets").await
    }

    pub async fn models(&self) -> Result<Value> {
        self.get("/models").await
    }

    /// `GET /datasets/standard` → `{"catalogue": [...], "import": {running, key, message, error}}`.
    pub async fn standard_datasets(&self) -> Result<Value> {
        self.get("/datasets/standard").await
    }

    /// `POST /datasets/standard {key, make_default}`: starts a background
    /// import on the host (progress arrives as `dataset.importing` events and
    /// in `standard_datasets().import`). Returns `{"status":"importing","key"}`.
    pub async fn import_standard(&self, key: &str, make_default: bool) -> Result<Value> {
        let body = json!({ "key": key, "make_default": make_default });
        self.call(Method::POST, "/datasets/standard", Some(&body))
            .await
    }

    pub async fn set_default_dataset(&self, id: &str) -> Result<Value> {
        self.call(Method::POST, &format!("/datasets/{id}/default"), None)
            .await
    }

    pub async fn delete_dataset(&self, id: &str) -> Result<Value> {
        self.call(Method::DELETE, &format!("/datasets/{id}"), None)
            .await
    }

    /// `POST /datasets` as `multipart/form-data` (`file`, `name`,
    /// `make_default`), streaming the zip from disk in 256 KB chunks so a
    /// multi-GB dataset is never held in memory. `on_progress(sent, total)` runs
    /// per chunk read. When every byte is sent the host still has to unpack and
    /// check the archive before it answers; the reply is the new dataset record.
    pub async fn upload_dataset<F>(
        &self,
        path: &Path,
        name: &str,
        make_default: bool,
        on_progress: F,
    ) -> Result<Value>
    where
        F: Fn(u64, u64) + Send + Sync + 'static,
    {
        let file = tokio::fs::File::open(path).await?;
        let total = file.metadata().await?.len();
        let file_name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "dataset.zip".into());
        let progress = Arc::new(on_progress);
        let chunks = futures::stream::unfold(
            (Some(file), 0u64, progress),
            move |(file, sent, progress)| async move {
                let mut file = file?;
                let mut buf = vec![0u8; UPLOAD_CHUNK];
                match file.read(&mut buf).await {
                    Ok(0) => None,
                    Ok(n) => {
                        buf.truncate(n);
                        let sent = sent + n as u64;
                        progress(sent, total);
                        Some((Ok::<_, std::io::Error>(buf), (Some(file), sent, progress)))
                    }
                    // Yield the error once, then end the body.
                    Err(e) => Some((Err(e), (None, sent, progress))),
                }
            },
        );
        let part =
            reqwest::multipart::Part::stream_with_length(reqwest::Body::wrap_stream(chunks), total)
                .file_name(file_name)
                .mime_str("application/zip")
                .map_err(|e| NetError::Decode(e.to_string()))?;
        let form = reqwest::multipart::Form::new()
            .text("name", name.to_string())
            .text("make_default", if make_default { "true" } else { "false" })
            .part("file", part);
        let response = self
            .request(Method::POST, "/datasets")
            .timeout(TRANSFER_TIMEOUT)
            .multipart(form)
            .send()
            .await
            .map_err(|e| NetError::from_reqwest(e, &self.base))?;
        let status = response.status();
        let text = response
            .text()
            .await
            .map_err(|e| NetError::from_reqwest(e, &self.base))?;
        let data = serde_json::from_str(&text).unwrap_or(Value::String(text));
        if !status.is_success() {
            return Err(NetError::Http {
                status: status.as_u16(),
                detail: detail_of(Some(&data), status.as_u16()),
            });
        }
        Ok(data)
    }

    /// Authenticated `GET route` saved into `dir` (created if needed) as
    /// `file_name` (made safe), or, when that is empty, under the server's
    /// `Content-Disposition` name; never overwriting an existing file. Written to `.part` and renamed when
    /// complete. Returns the final path. `route` must be on the allowlist
    /// (`/runs/{id}/artifact`, `/runs/{id}/weights.bin`, `/datasets/{id}/download`).
    pub async fn download_file<F>(
        &self,
        route: &str,
        dir: &Path,
        file_name: &str,
        on_progress: F,
    ) -> Result<PathBuf>
    where
        F: FnMut(u64, Option<u64>),
    {
        if !allowed(route) {
            return Err(NetError::Blocked(format!("GET {route}")));
        }
        let response = self
            .request(Method::GET, route)
            .timeout(TRANSFER_TIMEOUT)
            .send()
            .await
            .map_err(|e| NetError::from_reqwest(e, &self.base))?;
        let status = response.status();
        if status.as_u16() == 204 {
            return Err(NetError::Http {
                status: 204,
                detail: "There is nothing to download yet.".into(),
            });
        }
        if !status.is_success() {
            let body = response.json::<Value>().await.ok();
            return Err(NetError::Http {
                status: status.as_u16(),
                detail: detail_of(body.as_ref(), status.as_u16()),
            });
        }
        let name = safe_file_name(file_name)
            .or_else(|| {
                response
                    .headers()
                    .get(reqwest::header::CONTENT_DISPOSITION)
                    .and_then(|v| v.to_str().ok())
                    .and_then(disposition_name)
                    .and_then(|n| safe_file_name(&n))
            })
            .unwrap_or_else(|| "download.bin".into());
        tokio::fs::create_dir_all(dir).await?;
        let target = unique_path(dir, &name);
        save_response(response, &self.base, &target, on_progress).await?;
        Ok(target)
    }

    /// `GET /policy` → `{"policy": {...}, "defaults": {...}}`.
    pub async fn policy(&self) -> Result<Value> {
        self.get("/policy").await
    }

    /// `PUT /policy` with `{"values": values}` (coordinator `PolicyRequest`).
    pub async fn put_policy(&self, values: &Value) -> Result<Value> {
        self.call(Method::PUT, "/policy", Some(&json!({ "values": values })))
            .await
    }

    /// `POST /token/rotate`: mints a new token (and drops every node). The new
    /// token is stored in this client and returned.
    pub async fn rotate_token(&self) -> Result<String> {
        let v = self.call(Method::POST, "/token/rotate", None).await?;
        let token = v
            .get("token")
            .and_then(Value::as_str)
            .ok_or_else(|| NetError::Decode("no token in reply".into()))?;
        self.set_token(Some(token.to_string()));
        Ok(token.to_string())
    }

    /// One connection to `GET /events?replay=N` (server-sent events; the
    /// coordinator sends `: keepalive` every 15 s). Ends when the server
    /// closes; see [`follow_events`](Self::follow_events) for reconnects.
    pub async fn events(
        &self,
        replay: u32,
    ) -> Result<impl Stream<Item = Result<SseEvent>> + Send + 'static> {
        let response = self
            .request(Method::GET, &format!("/events?replay={}", replay.min(200)))
            .header(reqwest::header::ACCEPT, "text/event-stream")
            .send()
            .await
            .map_err(|e| NetError::from_reqwest(e, &self.base))?;
        let status = response.status();
        if !status.is_success() {
            let body = response.json::<Value>().await.ok();
            return Err(NetError::Http {
                status: status.as_u16(),
                detail: detail_of(body.as_ref(), status.as_u16()),
            });
        }
        let base = self.base.clone();
        let state = (
            response.bytes_stream().boxed(),
            SseParser::default(),
            VecDeque::<SseEvent>::new(),
            base,
        );
        Ok(futures::stream::unfold(
            state,
            |(mut bytes, mut parser, mut pending, base)| async move {
                loop {
                    if let Some(ev) = pending.pop_front() {
                        return Some((Ok(ev), (bytes, parser, pending, base)));
                    }
                    match bytes.next().await {
                        Some(Ok(chunk)) => pending.extend(parser.feed(&chunk)),
                        Some(Err(e)) => {
                            let err = NetError::from_reqwest(e, &base);
                            // Yield the error once, then end.
                            let empty = futures::stream::empty().boxed();
                            return Some((Err(err), (empty, parser, pending, base)));
                        }
                        None => return None,
                    }
                }
            },
        ))
    }

    /// Follow `/events` until `cancel`, reconnecting with backoff (1 s → 15 s,
    /// reset on a good connection), like v1 `followEvents`. Uses the current
    /// token on each reconnect, so it survives a rotation via this client.
    pub async fn follow_events<F>(&self, replay: u32, cancel: CancellationToken, mut on_event: F)
    where
        F: FnMut(SseEvent),
    {
        let mut delay = Duration::from_secs(1);
        while !cancel.is_cancelled() {
            let connected = cancel.run_until_cancelled(self.events(replay)).await;
            match connected {
                None => return,
                Some(Ok(stream)) => {
                    delay = Duration::from_secs(1);
                    let mut stream = std::pin::pin!(stream);
                    loop {
                        match cancel.run_until_cancelled(stream.next()).await {
                            None => return,
                            Some(Some(Ok(ev))) => on_event(ev),
                            Some(Some(Err(_))) | Some(None) => break,
                        }
                    }
                }
                Some(Err(e)) => tracing::debug!("events: {e}"),
            }
            if cancel
                .run_until_cancelled(tokio::time::sleep(delay))
                .await
                .is_none()
            {
                return;
            }
            delay = (delay * 2).min(Duration::from_secs(15));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allowlist_matches_v1() {
        for ok in [
            "/health",
            "/mesh",
            "/runs",
            "/runs/abc-1",
            "/runs/abc_1/stop",
            "/datasets",
            "/datasets/standard",
            "/datasets/d1",
            "/datasets/d1/default",
            "/datasets/d1/download",
            "/runs/r1/artifact",
            "/runs/r1/weights.bin",
            "/models",
            "/policy",
            "/nodes/n1",
            "/nodes/n1/reset",
            "/token/rotate",
        ] {
            assert!(allowed(ok), "{ok}");
        }
        for bad in [
            "",
            "health",
            "/runs/",
            "/runs/a/b",
            "/runs/../x",
            "/runs/a.b",
            "/events",
            "/register_node",
            "/nodes/n1/x",
            "/datasets/d1/bundle",
            "/runs/r1/weights",
            "/runs/r1/shards",
            "/token",
            "/policy/",
            "//health",
        ] {
            assert!(!allowed(bad), "{bad}");
        }
    }

    #[test]
    fn file_names_are_safe() {
        assert_eq!(safe_file_name("COCO8.zip").as_deref(), Some("COCO8.zip"));
        assert_eq!(
            safe_file_name("../../etc/passwd").as_deref(),
            Some("passwd")
        );
        assert_eq!(
            safe_file_name("C:\\x\\run 1.pt").as_deref(),
            Some("run 1.pt")
        );
        assert_eq!(safe_file_name("a:b*c?.zip").as_deref(), Some("a-b-c-.zip"));
        assert_eq!(safe_file_name(".."), None);
        assert_eq!(
            disposition_name(r#"attachment; filename="COCO8.zip""#).as_deref(),
            Some("COCO8.zip")
        );
        assert_eq!(disposition_name("attachment"), None);
        let dir = std::env::temp_dir().join(format!("gm-unique-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("w.pt"), b"x").unwrap();
        assert_eq!(unique_path(&dir, "w.pt"), dir.join("w (1).pt"));
        assert_eq!(unique_path(&dir, "new.pt"), dir.join("new.pt"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn sse_parser() {
        let mut p = SseParser::default();
        let full = "event: run.started\ndata: {\"id\":1,\"kind\":\"run.started\",\"at\":1.0,\"data\":{\"run_id\":\"r\"}}\n\n: keepalive\n\nevent: x\r\ndata: not json\r\n\r\ndata: {\"kind\":\"é\"}\n\n";
        let bytes = full.as_bytes();
        let mut events = Vec::new();
        // Feed one byte at a time to cover every split point.
        for b in bytes {
            events.extend(p.feed(std::slice::from_ref(b)));
        }
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].event.as_deref(), Some("run.started"));
        assert_eq!(events[0].kind(), Some("run.started"));
        assert_eq!(events[0].data["data"]["run_id"], "r");
        assert_eq!(events[1].kind(), Some("é"));
    }
}
