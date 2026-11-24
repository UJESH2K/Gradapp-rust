//! Probing a coordinator's `/health` and reading the token a host publishes on
//! its join port. Port of v1 `network.js` `probeHost` / `readInvite`.

use std::sync::OnceLock;
use std::time::{Duration, Instant};

use gm_types::MeshHealth;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A client for talking to machines on the LAN: never through the system
/// HTTP proxy (a proxy cannot reach 192.168.x.x, and would leak the token),
/// pooled, with a short connect timeout.
pub fn lan_client() -> reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            reqwest::Client::builder()
                .no_proxy()
                .connect_timeout(Duration::from_millis(1500))
                .pool_idle_timeout(Duration::from_secs(30))
                .tcp_nodelay(true)
                .user_agent(concat!("GradApp/", env!("CARGO_PKG_VERSION")))
                .build()
                .expect("reqwest client")
        })
        .clone()
}

/// A coordinator that answered `/health` with status "ok" and a mesh id.
#[derive(Debug, Clone, PartialEq)]
pub struct HealthHit {
    pub hostname: String,
    pub port: u16,
    pub health: MeshHealth,
    pub latency_ms: u32,
}

/// Read a `/health` body leniently: v1 only required `status == "ok"` and a
/// `mesh_id`; older coordinators may omit the rest.
pub fn health_from_value(v: &Value) -> Option<MeshHealth> {
    if v.get("status").and_then(Value::as_str) != Some("ok") {
        return None;
    }
    let mesh_id = v
        .get("mesh_id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())?;
    Some(MeshHealth {
        status: "ok".into(),
        version: v
            .get("version")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        protocol: v.get("protocol").and_then(Value::as_u64).unwrap_or(0) as u32,
        mesh_id: mesh_id.to_string(),
        torch_ready: v
            .get("torch_ready")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        nodes_active: v.get("nodes_active").and_then(Value::as_u64).unwrap_or(0) as u32,
    })
}

/// `GET http://host:port/health` within `timeout`; `None` for anything that
/// is not a GradMesh coordinator. Latency is the full request round-trip.
pub async fn probe_health(
    client: &reqwest::Client,
    hostname: &str,
    port: u16,
    timeout: Duration,
) -> Option<HealthHit> {
    let url = format!("http://{hostname}:{port}/health");
    let started = Instant::now();
    let response = client.get(&url).timeout(timeout).send().await.ok()?;
    if !response.status().is_success() {
        return None;
    }
    let body: Value = response.json().await.ok()?;
    let latency_ms = started.elapsed().as_millis().min(u32::MAX as u128) as u32;
    Some(HealthHit {
        hostname: hostname.to_string(),
        port,
        health: health_from_value(&body)?,
        latency_ms,
    })
}

/// Where an invite came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InviteKind {
    /// A GradApp host's `/gradapp.json`.
    Gradapp,
    /// GradMesh's dashboard (or a GradApp host): `MESH_TOKEN="…"` in `/join.sh`.
    Dashboard,
}

/// What a host publishes on its join port. All `None` when nothing answered.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InviteInfo {
    pub web: String,
    pub token: Option<String>,
    pub mesh_name: Option<String>,
    pub kind: Option<InviteKind>,
}

/// The token and name a host publishes on its join port: `/gradapp.json`
/// first, then `MESH_TOKEN` out of `/join.sh`.
pub async fn read_invite(hostname: &str, web_port: u16) -> InviteInfo {
    read_invite_with(&lan_client(), hostname, web_port).await
}

pub async fn read_invite_with(
    client: &reqwest::Client,
    hostname: &str,
    web_port: u16,
) -> InviteInfo {
    let web = format!("http://{hostname}:{web_port}");
    // Nothing listening on the join port is the common case (a bare GradMesh
    // coordinator, a firewall). Find that out with one short TCP connect
    // instead of two HTTP requests: on Windows a refused connect takes ~2 s
    // per attempt, which held every sweep result back by 3+ s.
    if let Ok(ip) = hostname.parse::<std::net::IpAddr>() {
        let reachable = tokio::time::timeout(
            Duration::from_millis(800),
            tokio::net::TcpStream::connect((ip, web_port)),
        )
        .await;
        if !matches!(reachable, Ok(Ok(_))) {
            return InviteInfo {
                web,
                token: None,
                mesh_name: None,
                kind: None,
            };
        }
    }
    if let Ok(response) = client
        .get(format!("{web}/gradapp.json"))
        .timeout(Duration::from_millis(2500))
        .send()
        .await
    {
        if response.status().is_success() {
            if let Ok(info) = response.json::<Value>().await {
                if let Some(token) = info
                    .get("token")
                    .and_then(Value::as_str)
                    .filter(|t| !t.is_empty())
                {
                    let mesh_name = info
                        .get("meshName")
                        .and_then(Value::as_str)
                        .filter(|n| !n.is_empty())
                        .map(str::to_string);
                    return InviteInfo {
                        web,
                        token: Some(token.to_string()),
                        mesh_name,
                        kind: Some(InviteKind::Gradapp),
                    };
                }
            }
        }
    }
    if let Ok(response) = client
        .get(format!("{web}/join.sh"))
        .timeout(Duration::from_millis(3000))
        .send()
        .await
    {
        if response.status().is_success() {
            if let Ok(text) = response.text().await {
                if let Some(token) = mesh_token_from_script(&text) {
                    return InviteInfo {
                        web,
                        token: Some(token),
                        mesh_name: None,
                        kind: Some(InviteKind::Dashboard),
                    };
                }
            }
        }
    }
    InviteInfo {
        web,
        token: None,
        mesh_name: None,
        kind: None,
    }
}

/// v1: `text.match(/^MESH_TOKEN="([^"]+)"/m)?.[1]`
pub fn mesh_token_from_script(text: &str) -> Option<String> {
    const PREFIX: &str = "MESH_TOKEN=\"";
    let line_starts = std::iter::once(0).chain(
        text.char_indices()
            .filter(|(_, c)| matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}'))
            .map(|(i, c)| i + c.len_utf8()),
    );
    for start in line_starts {
        let Some(rest) = text[start..].strip_prefix(PREFIX) else {
            continue;
        };
        if let Some(end) = rest.find('"') {
            if end > 0 {
                return Some(rest[..end].to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_from_script() {
        assert_eq!(
            mesh_token_from_script("#!/bin/sh\nMESH_TOKEN=\"abc\"\n").as_deref(),
            Some("abc")
        );
        assert_eq!(
            mesh_token_from_script("MESH_TOKEN=\"x-1\"").as_deref(),
            Some("x-1")
        );
        assert_eq!(
            mesh_token_from_script("a\r\nMESH_TOKEN=\"crlf\"\r\n").as_deref(),
            Some("crlf")
        );
        assert_eq!(mesh_token_from_script("  MESH_TOKEN=\"indented\""), None);
        assert_eq!(mesh_token_from_script("MESH_TOKEN=\"\"\n"), None);
        assert_eq!(mesh_token_from_script("MESH_TOKEN=unquoted\n"), None);
        let golden = include_str!("../tests/fixtures/join.sh");
        assert_eq!(
            mesh_token_from_script(golden).as_deref(),
            Some("tok-ABC_123")
        );
    }

    #[test]
    fn lenient_health() {
        let v = serde_json::json!({"status":"ok","mesh_id":"m"});
        assert_eq!(health_from_value(&v).unwrap().protocol, 0);
        assert!(health_from_value(&serde_json::json!({"status":"ok"})).is_none());
        assert!(health_from_value(&serde_json::json!({"status":"down","mesh_id":"m"})).is_none());
    }
}
