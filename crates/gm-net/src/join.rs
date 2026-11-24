//! The join surface a hosting GradApp offers its network, on port 3000.
//! Port of v1 `desktop/src/main/beacon.js`, byte-compatible:
//!
//! * another GradApp finds the host and reads `/gradapp.json`,
//! * someone without the app pastes `irm http://host:3000/join.ps1 | iex`
//!   or `curl -fsSL http://host:3000/join.sh | sh`,
//! * `worker --server host` reads the token out of `/join.sh`.
//!
//! Same trust model as GradMesh: on a LAN, being on the network is the
//! boundary and the token is shown to anyone who can reach this port. Only an
//! explicit allowlist of agent files is served; nothing else on disk is.

use std::io;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::extract::State;
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode, Uri};
use axum::response::Response;
use axum::Router;
use percent_encoding::percent_decode_str;
use tokio::sync::oneshot;

/// The files a joining machine needs. Mirrors v1 `agent.js` / GradMesh `lib/agent.ts`.
pub const AGENT_FILES: &[&str] = &[
    "version.py",
    "hardware.py",
    "setup_env.py",
    "accelerator.py",
    "probe.py",
    "trainers.py",
    "federated_training.py",
    "ultralytics_xpu.py",
    "worker.py",
    "requirements-control.txt",
    "requirements-common.txt",
    "requirements-train-cu130.txt",
    "requirements-train-cu126.txt",
    "requirements-train-cu128.txt",
    "requirements-train-cpu.txt",
    "requirements-xpu.txt",
    "requirements-mps.txt",
];

// Generated from v1 beacon.js with placeholders (see tests/fixtures for the
// rendered golden copies the tests compare against).
const SH_TEMPLATE: &str = include_str!("templates/join.sh.tpl");
const PS1_TEMPLATE: &str = include_str!("templates/join.ps1.tpl");
const HTML_TEMPLATE: &str = include_str!("templates/join.html.tpl");

/// Single-pass `@@KEY@@` substitution, so a value can never inject a placeholder.
fn render(template: &str, values: &[(&str, &str)]) -> String {
    let mut out = String::with_capacity(template.len() + 256);
    let mut rest = template;
    while let Some(start) = rest.find("@@") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        match after.find("@@").and_then(|end| values.iter().find(|(k, _)| *k == &after[..end]).map(|(_, v)| (end, v))) {
            Some((end, value)) => {
                out.push_str(value);
                rest = &after[end + 2..];
            }
            None => {
                out.push_str("@@");
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// v1 `shellScript`.
pub fn shell_script(web: &str, coordinator: &str, token: &str, files: &[&str]) -> String {
    let files = files.join(" ");
    render(SH_TEMPLATE, &[("WEB", web), ("COORDINATOR", coordinator), ("TOKEN", token), ("FILES", &files)])
}

/// v1 `powershellScript`.
pub fn powershell_script(web: &str, coordinator: &str, token: &str, files: &[&str]) -> String {
    // The template holds `'@@FILES@@'`; v1 renders `'a', 'b', ...`.
    let files = files.join("', '");
    render(PS1_TEMPLATE, &[("WEB", web), ("COORDINATOR", coordinator), ("TOKEN", token), ("FILES", &files)])
}

fn html_escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            c => out.push(c),
        }
    }
    out
}

/// v1 `joinPage`.
pub fn join_page(web: &str, mesh_name: &str) -> String {
    render(HTML_TEMPLATE, &[("WEB", &html_escape(web)), ("MESH_NAME", &html_escape(mesh_name))])
}

/// What the surface publishes, read per request so a rotated token or a new
/// address is picked up without a restart (v1 `info()`).
#[derive(Debug, Clone, PartialEq)]
pub struct JoinInfo {
    /// `None` while the mesh is starting: every route answers 503.
    pub token: Option<String>,
    pub coordinator_port: u16,
    pub mesh_name: Option<String>,
    pub mesh_id: Option<String>,
    pub version: String,
    /// Used when the request's Host is localhost/127.x or missing.
    pub lan_address: String,
}

pub type InfoFn = Arc<dyn Fn() -> JoinInfo + Send + Sync>;

#[derive(Clone)]
pub struct JoinConfig {
    pub info: InfoFn,
    /// Where the agent files live (v1 `engineDir()`).
    pub engine_dir: PathBuf,
    /// Where `logo.png` lives (v1 `assetsDir()`); `None` → 404.
    pub assets_dir: Option<PathBuf>,
    /// Defaults to [`AGENT_FILES`].
    pub agent_files: Vec<String>,
    /// When set, `GET /health` is proxied to `<coordinator>/health`.
    pub health_upstream: Option<String>,
    /// The port written into URLs; defaults to the bound port.
    pub public_port: Option<u16>,
}

impl JoinConfig {
    pub fn new(engine_dir: impl Into<PathBuf>, info: impl Fn() -> JoinInfo + Send + Sync + 'static) -> Self {
        Self {
            info: Arc::new(info),
            engine_dir: engine_dir.into(),
            assets_dir: None,
            agent_files: AGENT_FILES.iter().map(|s| s.to_string()).collect(),
            health_upstream: None,
            public_port: None,
        }
    }
}

struct Shared {
    cfg: JoinConfig,
    port: u16,
}

/// A running join surface. Shuts down gracefully on [`JoinSurface::shutdown`]
/// or when dropped.
pub struct JoinSurface {
    addr: SocketAddr,
    stop: Option<oneshot::Sender<()>>,
    task: Option<tokio::task::JoinHandle<io::Result<()>>>,
}

impl JoinSurface {
    /// Bind `addr` (v1: 0.0.0.0:3000) and serve. Fails when the port is taken.
    pub async fn serve(addr: SocketAddr, cfg: JoinConfig) -> io::Result<Self> {
        let listener = tokio::net::TcpListener::bind(addr).await?;
        let bound = listener.local_addr()?;
        let shared = Arc::new(Shared { port: cfg.public_port.unwrap_or(bound.port()), cfg });
        let app = Router::new().fallback(handle).with_state(shared);
        let (stop, stopped) = oneshot::channel::<()>();
        let task = tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    let _ = stopped.await;
                })
                .await
        });
        Ok(Self { addr: bound, stop: Some(stop), task: Some(task) })
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.addr
    }

    /// Stop accepting, let in-flight requests finish, and wait.
    pub async fn shutdown(mut self) -> io::Result<()> {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        match self.task.take() {
            Some(task) => task.await.map_err(io::Error::other)?,
            None => Ok(()),
        }
    }
}

impl Drop for JoinSurface {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
    }
}

fn send(status: StatusCode, content_type: &'static str, body: impl Into<Body>) -> Response {
    let mut r = Response::new(body.into());
    *r.status_mut() = status;
    let h = r.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    h.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    r
}

/// v1: Host header's name unless it is localhost/127.x, else the LAN address.
fn public_hostname(headers: &HeaderMap, lan_address: &str) -> String {
    let host_header = headers.get(header::HOST).and_then(|h| h.to_str().ok()).unwrap_or("");
    let name = host_header.split(':').next().unwrap_or("");
    if !name.is_empty() && !host_header.starts_with("localhost") && !host_header.starts_with("127.") {
        name.to_string()
    } else {
        lan_address.to_string()
    }
}

async fn handle(State(shared): State<Arc<Shared>>, method: Method, headers: HeaderMap, uri: Uri) -> Response {
    let cfg = &shared.cfg;
    if method != Method::GET && method != Method::HEAD {
        return send(StatusCode::METHOD_NOT_ALLOWED, "text/plain", "GET only\n");
    }
    let trimmed = uri.path().trim_end_matches('/');
    let route = if trimmed.is_empty() { "/" } else { trimmed };

    if route == "/health" {
        if let Some(upstream) = &cfg.health_upstream {
            return proxy_health(upstream).await;
        }
    }

    let info = (cfg.info)();
    let Some(token) = info.token.as_deref().filter(|t| !t.is_empty()) else {
        return send(StatusCode::SERVICE_UNAVAILABLE, "text/plain", "# The mesh is still starting. Try again in a moment.\n");
    };
    let hostname = public_hostname(&headers, &info.lan_address);
    let web = format!("http://{hostname}:{}", shared.port);
    let coordinator = format!("http://{hostname}:{}", info.coordinator_port);
    let files: Vec<&str> = cfg.agent_files.iter().map(String::as_str).collect();

    match route {
        "/join.ps1" => {
            return send(StatusCode::OK, "text/plain; charset=utf-8", powershell_script(&web, &coordinator, token, &files))
        }
        "/join.sh" => return send(StatusCode::OK, "text/plain; charset=utf-8", shell_script(&web, &coordinator, token, &files)),
        "/gradapp.json" => {
            // Built by hand to keep v1's key order (JSON.stringify).
            let body = format!(
                "{{\"app\":\"gradapp\",\"meshName\":{},\"meshId\":{},\"version\":{},\"coordinator\":{},\"web\":{},\"token\":{},\"coordinatorPort\":{}}}",
                j(&info.mesh_name),
                j(&info.mesh_id),
                j(&info.version),
                j(&coordinator),
                j(&web),
                j(&token),
                info.coordinator_port
            );
            return send(StatusCode::OK, "application/json", body);
        }
        "/api/health" => return send(StatusCode::OK, "application/json", r#"{"status":"ok","app":"gradapp"}"#),
        "/" | "/join" => {
            return send(StatusCode::OK, "text/html; charset=utf-8", join_page(&web, info.mesh_name.as_deref().unwrap_or("GradMesh")))
        }
        "/logo.png" | "/favicon.ico" => {
            let Some(dir) = &cfg.assets_dir else { return send(StatusCode::NOT_FOUND, "text/plain", "Missing\n") };
            return match tokio::fs::read(dir.join("logo.png")).await {
                Ok(bytes) => {
                    let mut r = Response::new(Body::from(bytes));
                    r.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("image/png"));
                    r.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("public, max-age=86400"));
                    r
                }
                Err(_) => send(StatusCode::NOT_FOUND, "text/plain", "Missing\n"),
            };
        }
        _ => {}
    }

    if let Some(raw) = route.strip_prefix("/api/agent/") {
        // Decode, then require an exact allowlist match: "../x", "a/b",
        // "%2e%2e%2f..." and invalid UTF-8 can never name a file.
        let file = match percent_decode_str(raw).decode_utf8() {
            Ok(f) => f.into_owned(),
            Err(_) => return send(StatusCode::NOT_FOUND, "text/plain", "Not an agent file\n"),
        };
        if !cfg.agent_files.contains(&file) || file.contains(['/', '\\']) || file.starts_with('.') {
            return send(StatusCode::NOT_FOUND, "text/plain", "Not an agent file\n");
        }
        let content_type = if file.ends_with(".py") { "text/x-python; charset=utf-8" } else { "text/plain; charset=utf-8" };
        return match tokio::fs::read(cfg.engine_dir.join(&file)).await {
            Ok(bytes) => send(StatusCode::OK, content_type, bytes),
            Err(_) => send(StatusCode::NOT_FOUND, "text/plain", "Missing\n"),
        };
    }
    send(StatusCode::NOT_FOUND, "text/plain", "Not found\n")
}

async fn proxy_health(upstream: &str) -> Response {
    let url = format!("{}/health", upstream.trim_end_matches('/'));
    let result = crate::invite::lan_client().get(&url).timeout(Duration::from_secs(3)).send().await;
    match result {
        Ok(r) => {
            let status = StatusCode::from_u16(r.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
            match r.bytes().await {
                Ok(body) => send(status, "application/json", body),
                Err(_) => send(StatusCode::BAD_GATEWAY, "application/json", r#"{"status":"unreachable"}"#),
            }
        }
        Err(_) => send(StatusCode::BAD_GATEWAY, "application/json", r#"{"status":"unreachable"}"#),
    }
}

fn j<T: serde::Serialize + ?Sized>(value: &T) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "null".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scripts_match_v1_golden() {
        let (web, coord, tok) = ("http://192.168.1.20:3000", "http://192.168.1.20:8000", "tok-ABC_123");
        assert_eq!(shell_script(web, coord, tok, AGENT_FILES), include_str!("../tests/fixtures/join.sh"));
        assert_eq!(powershell_script(web, coord, tok, AGENT_FILES), include_str!("../tests/fixtures/join.ps1"));
        assert_eq!(join_page(web, "Lab <\"A\"> & co"), include_str!("../tests/fixtures/join.html"));
    }

    #[test]
    fn render_is_single_pass() {
        assert_eq!(render("a@@X@@b@@Y@@", &[("X", "@@Y@@"), ("Y", "1")]), "a@@Y@@b1");
        assert_eq!(render("keep @@ alone", &[]), "keep @@ alone");
    }

    #[test]
    fn hostname_rule() {
        let mut h = HeaderMap::new();
        assert_eq!(public_hostname(&h, "10.0.0.2"), "10.0.0.2");
        h.insert(header::HOST, HeaderValue::from_static("localhost:3000"));
        assert_eq!(public_hostname(&h, "10.0.0.2"), "10.0.0.2");
        h.insert(header::HOST, HeaderValue::from_static("127.0.0.1:3000"));
        assert_eq!(public_hostname(&h, "10.0.0.2"), "10.0.0.2");
        h.insert(header::HOST, HeaderValue::from_static("192.168.1.20:3000"));
        assert_eq!(public_hostname(&h, "10.0.0.2"), "192.168.1.20");
        h.insert(header::HOST, HeaderValue::from_static("gradmesh.local"));
        assert_eq!(public_hostname(&h, "10.0.0.2"), "gradmesh.local");
    }
}
