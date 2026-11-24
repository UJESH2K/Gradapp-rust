//! gm-update against a local mock of the GitHub releases API.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use gm_update::{parse_latest_json, Channel, Checker, ManifestError};

#[derive(Clone, Default)]
struct Hits(Arc<AtomicUsize>);

fn release(tag: &str, pre: bool, draft: bool, manifest: bool) -> serde_json::Value {
    let mut assets = vec![
        serde_json::json!({"name": format!("GradApp_{tag}_x64-setup.exe"), "browser_download_url": "x"}),
    ];
    if manifest {
        assets.push(serde_json::json!({"name": "latest.json", "browser_download_url": format!("https://dl/{tag}/latest.json")}));
    }
    serde_json::json!({
        "tag_name": tag, "name": format!("GradApp {tag}"), "body": format!("## {tag}\n\nfixes"),
        "draft": draft, "prerelease": pre, "html_url": format!("https://github.com/o/r/releases/tag/{tag}"),
        "published_at": "2026-11-20T10:00:00Z", "assets": assets
    })
}

async fn releases(
    Path((_owner, repo)): Path<(String, String)>,
    State(hits): State<Hits>,
    headers: HeaderMap,
) -> Response {
    hits.0.fetch_add(1, Ordering::SeqCst);
    let json = |v: serde_json::Value| {
        let mut r = axum::Json(v).into_response();
        r.headers_mut().insert("etag", "\"v1\"".parse().unwrap());
        r
    };
    match repo.as_str() {
        "newer" => json(serde_json::json!([
            release("v2.0.1", false, false, true),
            release("v1.0.9", false, false, false)
        ])),
        "older" => json(serde_json::json!([release("v1.9.0", false, false, false)])),
        "equal" => json(serde_json::json!([release("v2.0.0", false, false, true)])),
        "prerelease" => json(serde_json::json!([
            release("v2.1.0-beta.2", true, false, true),
            release("v2.0.0", false, false, true)
        ])),
        "draft" => json(serde_json::json!([
            release("v9.0.0", false, true, true),
            release("v2.0.0", false, false, true)
        ])),
        "linkonly" => json(serde_json::json!([release("v2.0.5", false, false, false)])),
        "malformed" => (
            StatusCode::OK,
            [("content-type", "application/json")],
            "{not json",
        )
            .into_response(),
        "ratelimited" => (
            StatusCode::FORBIDDEN,
            [("x-ratelimit-remaining", "0"), ("x-ratelimit-reset", "0")],
            "{\"message\":\"API rate limit exceeded\"}",
        )
            .into_response(),
        "etag" => {
            if headers.get("if-none-match").and_then(|v| v.to_str().ok()) == Some("\"v1\"") {
                StatusCode::NOT_MODIFIED.into_response()
            } else {
                json(serde_json::json!([release("v2.3.0", false, false, true)]))
            }
        }
        "slow" => {
            tokio::time::sleep(Duration::from_secs(5)).await;
            json(serde_json::json!([]))
        }
        _ => (StatusCode::NOT_FOUND, "{\"message\":\"Not Found\"}").into_response(),
    }
}

async fn server() -> (String, Hits) {
    let hits = Hits::default();
    let app = Router::new()
        .route("/repos/:owner/:repo/releases", get(releases))
        .with_state(hits.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), hits)
}

#[tokio::test]
async fn newer_release_is_installable() {
    let (base, _) = server().await;
    let c = Checker::new()
        .with_api_base(&base)
        .check_detailed("2.0.0", "o/newer", Channel::Stable, false)
        .await;
    assert!(c.reason.is_none(), "{:?}", c.reason);
    assert!(c.info.available);
    assert!(c.info.installable);
    assert_eq!(c.info.latest.as_deref(), Some("2.0.1"));
    assert_eq!(c.info.current, "2.0.0");
    assert!(c.info.notes.as_deref().unwrap().contains("fixes"));
    assert_eq!(
        c.info.url.as_deref(),
        Some("https://github.com/o/r/releases/tag/v2.0.1")
    );
    assert_eq!(c.info.published_at.as_deref(), Some("2026-11-20T10:00:00Z"));
    assert_eq!(
        c.manifest_url.as_deref(),
        Some("https://dl/v2.0.1/latest.json")
    );
}

#[tokio::test]
async fn older_and_equal_are_not_updates() {
    let (base, _) = server().await;
    let k = Checker::new().with_api_base(&base);
    let older = k.check("2.0.0", "o/older", Channel::Stable).await;
    assert!(!older.available && !older.installable);
    assert_eq!(older.latest.as_deref(), Some("1.9.0"));
    let equal = k.check("2.0.0", "o/equal", Channel::Stable).await;
    assert!(!equal.available);
    // "v" prefix on current is accepted too.
    assert!(
        !k.check("v2.0.0", "o/equal", Channel::Stable)
            .await
            .available
    );
}

#[tokio::test]
async fn prerelease_only_on_beta() {
    let (base, _) = server().await;
    let k = Checker::new().with_api_base(&base);
    let stable = k.check("2.0.0", "o/prerelease", Channel::Stable).await;
    assert!(!stable.available);
    let beta = k.check("2.0.0", "o/prerelease", Channel::Beta).await;
    assert!(beta.available);
    assert_eq!(beta.latest.as_deref(), Some("2.1.0-beta.2"));
    let detailed = k
        .check_detailed("2.0.0", "o/prerelease", Channel::Beta, true)
        .await;
    assert_eq!(
        detailed.manifest_url.as_deref(),
        Some("https://dl/v2.1.0-beta.2/latest.json")
    );
    // A beta user on 2.1.0-beta.1 sees beta.2 as newer.
    assert!(
        k.check("2.1.0-beta.1", "o/prerelease", Channel::Beta)
            .await
            .available
    );
}

#[tokio::test]
async fn drafts_are_ignored() {
    let (base, _) = server().await;
    let i = Checker::new()
        .with_api_base(&base)
        .check("2.0.0", "o/draft", Channel::Beta)
        .await;
    assert!(!i.available);
    assert_eq!(i.latest.as_deref(), Some("2.0.0"));
}

#[tokio::test]
async fn release_without_manifest_is_link_only() {
    let (base, _) = server().await;
    let i = Checker::new()
        .with_api_base(&base)
        .check("2.0.0", "o/linkonly", Channel::Stable)
        .await;
    assert!(i.available && !i.installable);
    let c = Checker::new()
        .with_api_base(&base)
        .check_detailed("2.0.0", "o/linkonly", Channel::Stable, false)
        .await;
    assert!(c.manifest_url.is_none());
}

#[tokio::test]
async fn failures_never_error() {
    let (base, _) = server().await;
    let k = Checker::new().with_api_base(&base);
    for (repo, needle) in [
        ("o/malformed", "unexpected answer"),
        ("o/missing", "no releases"),
        ("o/ratelimited", "rate limit"),
    ] {
        let c = k
            .check_detailed("2.0.0", repo, Channel::Stable, false)
            .await;
        assert!(!c.info.available, "{repo}");
        assert_eq!(c.info.current, "2.0.0");
        let reason = c.reason.unwrap_or_default();
        assert!(reason.contains(needle), "{repo}: {reason}");
    }
    let bad = k
        .check_detailed("garbage", "o/newer", Channel::Stable, false)
        .await;
    assert!(!bad.info.available && bad.reason.unwrap().contains("not semver"));
    // Nothing listening: a network failure, not a panic.
    let dead = Checker::new()
        .with_api_base("http://127.0.0.1:9")
        .check_detailed("2.0.0", "o/newer", Channel::Stable, false)
        .await;
    assert!(!dead.info.available && dead.reason.is_some());
}

#[tokio::test]
async fn timeout_is_enforced() {
    let (base, _) = server().await;
    let started = std::time::Instant::now();
    let c = Checker::new()
        .with_api_base(&base)
        .with_timeout(Duration::from_millis(300))
        .check_detailed("2.0.0", "o/slow", Channel::Stable, false)
        .await;
    assert!(started.elapsed() < Duration::from_secs(3));
    assert!(c.reason.unwrap().contains("in time"));
}

#[tokio::test]
async fn throttle_and_etag() {
    let (base, hits) = server().await;
    let dir = tempfile::tempdir().unwrap();
    let cache = dir.path().join("update-cache.json");
    let k = Checker::new().with_api_base(&base).with_cache(&cache);

    let first = k
        .check_detailed("2.0.0", "o/etag", Channel::Stable, false)
        .await;
    assert!(first.info.available && !first.from_cache);
    assert_eq!(hits.0.load(Ordering::SeqCst), 1);
    assert!(cache.exists());

    // Within the interval: no request at all, same answer.
    let second = k
        .check_detailed("2.0.0", "o/etag", Channel::Stable, false)
        .await;
    assert!(second.from_cache && second.info.available);
    assert_eq!(second.info.latest.as_deref(), Some("2.3.0"));
    assert_eq!(hits.0.load(Ordering::SeqCst), 1);

    // Forced: request goes out with If-None-Match, 304 reuses the cache.
    let forced = k
        .check_detailed("2.0.0", "o/etag", Channel::Stable, true)
        .await;
    assert_eq!(hits.0.load(Ordering::SeqCst), 2);
    assert!(forced.from_cache && forced.info.available && forced.reason.is_none());

    // Interval elapsed (zero): checks again.
    let k0 = Checker::new()
        .with_api_base(&base)
        .with_cache(&cache)
        .with_min_interval(Duration::ZERO);
    k0.check("2.0.0", "o/etag", Channel::Stable).await;
    assert_eq!(hits.0.load(Ordering::SeqCst), 3);

    // Different repo => cache miss.
    k.check("2.0.0", "o/newer", Channel::Stable).await;
    assert_eq!(hits.0.load(Ordering::SeqCst), 4);
}

#[tokio::test]
async fn rate_limit_falls_back_to_last_known() {
    let (base, _) = server().await;
    let dir = tempfile::tempdir().unwrap();
    let cache = dir.path().join("c.json");
    // Seed a cache for o/ratelimited by hand-writing what a previous good check stored.
    let url = format!("{base}/repos/o/ratelimited/releases?per_page=30");
    let seeded = serde_json::json!({"url": url, "etag": null, "checked_unix": 0,
        "releases": [release("v2.0.2", false, false, true)]});
    std::fs::write(&cache, serde_json::to_vec(&seeded).unwrap()).unwrap();
    let c = Checker::new()
        .with_api_base(&base)
        .with_cache(&cache)
        .check_detailed("2.0.0", "o/ratelimited", Channel::Stable, false)
        .await;
    assert!(c.info.available);
    assert!(c.from_cache);
    assert!(c.reason.unwrap().contains("rate limit"));
}

const MANIFEST: &str = r#"{
  "version": "2.0.1",
  "notes": "Bug fixes",
  "pub_date": "2026-11-20T10:00:00Z",
  "platforms": {
    "darwin-aarch64": {"signature": "sig-mac", "url": "https://github.com/o/r/releases/download/v2.0.1/GradApp_universal.app.tar.gz"},
    "darwin-x86_64": {"signature": "sig-mac", "url": "https://github.com/o/r/releases/download/v2.0.1/GradApp_universal.app.tar.gz"},
    "linux-x86_64": {"signature": "sig-linux", "url": "https://github.com/o/r/releases/download/v2.0.1/GradApp_2.0.1_amd64.AppImage"},
    "windows-x86_64": {"signature": "sig-win", "url": "https://github.com/o/r/releases/download/v2.0.1/GradApp_2.0.1_x64-setup.exe"}
  }
}"#;

#[test]
fn latest_json_per_platform() {
    let m = parse_latest_json(MANIFEST).unwrap();
    assert_eq!(m.version().unwrap().to_string(), "2.0.1");
    assert_eq!(m.notes.as_deref(), Some("Bug fixes"));
    assert_eq!(m.asset_for("windows-x86_64").unwrap().signature, "sig-win");
    assert_eq!(m.asset_for("darwin-aarch64").unwrap().signature, "sig-mac");
    assert_eq!(m.asset_for("darwin-x86_64").unwrap().signature, "sig-mac");
    assert!(m
        .asset_for("linux-x86_64")
        .unwrap()
        .url
        .ends_with(".AppImage"));
    assert!(m.asset_for("windows-aarch64").is_none());
    assert!(
        m.asset_for_this_platform().is_some(),
        "CI runs on a platform we ship"
    );
}

#[test]
fn latest_json_installer_specific_keys() {
    let raw = r#"{"version":"v2.0.1","platforms":{
        "windows-x86_64-msi":{"signature":"m","url":"a.msi"},
        "windows-x86_64-nsis":{"signature":"n","url":"a.exe"},
        "linux-x86_64-deb":{"signature":"d","url":"a.deb"},
        "linux-x86_64-appimage":{"signature":"ai","url":"a.AppImage"}}}"#;
    let m = parse_latest_json(raw).unwrap();
    assert_eq!(m.asset_for("windows-x86_64").unwrap().signature, "n");
    assert_eq!(m.asset_for("linux-x86_64").unwrap().signature, "ai");
    assert!(m.pub_date.is_none());
}

#[test]
fn latest_json_rejects_bad_input() {
    assert!(matches!(
        parse_latest_json("{"),
        Err(ManifestError::Json(_))
    ));
    assert!(matches!(
        parse_latest_json(r#"{"version":"two","platforms":{"a":{"signature":"s","url":"u"}}}"#),
        Err(ManifestError::Version(_))
    ));
    assert!(matches!(
        parse_latest_json(r#"{"version":"2.0.0","platforms":{}}"#),
        Err(ManifestError::NoPlatforms)
    ));
    assert!(matches!(
        parse_latest_json(r#"{"version":"2.0.0"}"#),
        Err(ManifestError::Json(_))
    ));
}
