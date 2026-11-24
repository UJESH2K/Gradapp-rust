//! Update checks against GitHub Releases.
//!
//! Port of GradApp 1's `desktop/src/main/updates.js`, with what it lacked:
//!
//! * **Real semver.** v1 compared the first three dot/dash-separated numbers,
//!   so `2.1.0-beta.2` and `2.1.0` looked equal. Here the `semver` crate
//!   decides, pre-releases included.
//! * **Channels.** [`Channel::Stable`] only considers full releases;
//!   [`Channel::Beta`] also considers GitHub pre-releases. Drafts never count.
//! * **The highest version wins, not the newest release.** v1 asked
//!   `/releases/latest`, which is "most recently published non-prerelease";
//!   a v1.x hotfix published after 2.0 would then hide 2.0 from a 2.x app.
//!   We list releases and take the semver maximum instead.
//! * **Installable vs link-only.** A release that carries the Tauri updater
//!   manifest (`latest.json`) can be installed in place by the signed updater;
//!   anything else (every v1 release) is offered as a download link.
//! * **Rate-limit friendly.** At most one network check per
//!   [`Checker::min_interval`] unless forced, with ETag revalidation (a 304
//!   does not count against GitHub's unauthenticated 60 requests/hour), and
//!   the last good release list is reused when GitHub says no.
//! * **Never errors to the caller.** Every failure becomes
//!   `available = false` plus a human-readable [`Checked::reason`].

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use gm_types::UpdateInfo;
use semver::Version;
use serde::{Deserialize, Serialize};

/// GitHub's REST API root. Injectable via [`Checker::with_api_base`] for tests.
pub const GITHUB_API: &str = "https://api.github.com";
/// The repository GradApp publishes from.
pub const DEFAULT_REPO: &str = "UJESH2K/gradapp";
/// Name of the Tauri updater manifest asset that tauri-action uploads.
pub const MANIFEST_ASSET: &str = "latest.json";
/// Release notes longer than this are cut (on a char boundary) with an ellipsis.
pub const NOTES_MAX_CHARS: usize = 4000;
/// Same 8 s budget v1 used (`AbortSignal.timeout(8000)`).
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(8);
/// Default throttle between network checks.
pub const DEFAULT_MIN_INTERVAL: Duration = Duration::from_secs(30 * 60);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Channel {
    /// Full releases only.
    #[default]
    Stable,
    /// Full releases and GitHub pre-releases (e.g. `v2.1.0-beta.2`).
    Beta,
}

impl Channel {
    fn as_str(self) -> &'static str {
        match self {
            Channel::Stable => "stable",
            Channel::Beta => "beta",
        }
    }
}

// ---------------------------------------------------------------------------
// GitHub release JSON (only the fields we read)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Release {
    pub tag_name: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub draft: bool,
    #[serde(default)]
    pub prerelease: bool,
    #[serde(default)]
    pub html_url: Option<String>,
    #[serde(default)]
    pub published_at: Option<String>,
    #[serde(default)]
    pub assets: Vec<ReleaseAsset>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReleaseAsset {
    pub name: String,
    #[serde(default)]
    pub browser_download_url: Option<String>,
}

impl Release {
    /// The tag as semver, accepting a leading `v`/`V`. `None` for tags that are not semver.
    pub fn version(&self) -> Option<Version> {
        parse_version(&self.tag_name)
    }

    pub fn manifest_url(&self) -> Option<&str> {
        self.assets
            .iter()
            .find(|a| a.name == MANIFEST_ASSET)
            .and_then(|a| a.browser_download_url.as_deref())
    }

    pub fn has_manifest(&self) -> bool {
        self.assets.iter().any(|a| a.name == MANIFEST_ASSET)
    }
}

/// `v2.0.1`, `2.1.0-beta.2`, ` V2.0.0 ` -> Version. Anything else -> None.
pub fn parse_version(tag: &str) -> Option<Version> {
    let t = tag.trim();
    let t = t.strip_prefix(['v', 'V']).unwrap_or(t);
    Version::parse(t).ok()
}

/// Cut `text` to at most `max` chars, ending with `…` when cut.
pub fn truncate_notes(text: &str, max: usize) -> String {
    let text = text.trim();
    if text.chars().count() <= max {
        return text.to_string();
    }
    let cut: String = text.chars().take(max.saturating_sub(1)).collect();
    format!("{}…", cut.trim_end())
}

// ---------------------------------------------------------------------------
// Pure decision
// ---------------------------------------------------------------------------

/// What [`Checker::check_detailed`] returns: the UI payload plus why it is
/// not an update, when it is not.
#[derive(Debug, Clone, PartialEq)]
pub struct Checked {
    pub info: UpdateInfo,
    /// Set when the check could not be completed (offline, rate-limited,
    /// malformed answer, ...). `None` for a clean "up to date" or "update available".
    pub reason: Option<String>,
    /// The answer came from the on-disk cache rather than a fresh request.
    pub from_cache: bool,
    /// `latest.json` of the offered release, when installable. The Tauri
    /// updater's fixed endpoint (`/releases/latest/download/latest.json`)
    /// never serves pre-releases, so the Beta channel should point the
    /// updater here instead.
    pub manifest_url: Option<String>,
}

fn checked(
    current: &str,
    releases: &[Release],
    channel: Channel,
    reason: Option<String>,
    from_cache: bool,
) -> Checked {
    let (info, own) = evaluate(current, releases, channel);
    let manifest_url = if info.installable {
        best_release(releases, channel).and_then(|(_, r)| r.manifest_url().map(str::to_string))
    } else {
        None
    };
    Checked {
        info,
        reason: reason.or(own),
        from_cache,
        manifest_url,
    }
}

fn unavailable(current: &str) -> UpdateInfo {
    UpdateInfo {
        available: false,
        current: current.to_string(),
        latest: None,
        notes: None,
        url: None,
        published_at: None,
        installable: false,
        reason: None,
        manifest_url: None,
    }
}

/// The highest-semver non-draft release `channel` may see.
pub fn best_release(releases: &[Release], channel: Channel) -> Option<(Version, &Release)> {
    releases
        .iter()
        .filter(|r| !r.draft)
        .filter_map(|r| r.version().map(|v| (v, r)))
        .filter(|(v, r)| match channel {
            Channel::Stable => !r.prerelease && v.pre.is_empty(),
            Channel::Beta => true,
        })
        .max_by(|a, b| a.0.cmp(&b.0))
}

/// Pick the best release for `channel` and compare it with `current`.
/// Returns the info and, if nothing could be decided, a reason.
pub fn evaluate(
    current: &str,
    releases: &[Release],
    channel: Channel,
) -> (UpdateInfo, Option<String>) {
    let Some(current_v) = parse_version(current) else {
        return (
            unavailable(current),
            Some(format!("current version {current:?} is not semver")),
        );
    };
    let Some((latest_v, release)) = best_release(releases, channel) else {
        return (
            unavailable(current),
            Some(format!("no {} release found", channel.as_str())),
        );
    };
    let available = latest_v > current_v;
    let info = UpdateInfo {
        available,
        current: current.to_string(),
        latest: Some(latest_v.to_string()),
        notes: release
            .body
            .as_deref()
            .map(|b| truncate_notes(b, NOTES_MAX_CHARS))
            .filter(|b| !b.is_empty()),
        url: release.html_url.clone(),
        published_at: release.published_at.clone(),
        installable: available && release.has_manifest(),
        reason: None,
        manifest_url: None,
    };
    (info, None)
}

// ---------------------------------------------------------------------------
// Cache
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct CacheFile {
    /// Request URL the cached data belongs to (repo change => cache miss).
    url: String,
    etag: Option<String>,
    /// Unix seconds of the last network attempt (successful or not).
    checked_unix: u64,
    /// Last good release list (bodies already truncated).
    releases: Option<Vec<Release>>,
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn read_cache(path: &Path) -> Option<CacheFile> {
    let raw = std::fs::read(path).ok()?;
    serde_json::from_slice(&raw).ok()
}

fn write_cache(path: &Path, cache: &CacheFile) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let Ok(raw) = serde_json::to_vec_pretty(cache) else {
        return;
    };
    // Write-then-rename so a crash never leaves half a file.
    let tmp = path.with_extension("json.tmp");
    if std::fs::write(&tmp, raw).is_ok() && std::fs::rename(&tmp, path).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

// ---------------------------------------------------------------------------
// Checker
// ---------------------------------------------------------------------------

/// Configurable update checker. Cheap to build; holds no connection.
#[derive(Debug, Clone)]
pub struct Checker {
    api_base: String,
    cache_path: Option<PathBuf>,
    min_interval: Duration,
    timeout: Duration,
}

impl Default for Checker {
    fn default() -> Self {
        Self::new()
    }
}

impl Checker {
    pub fn new() -> Self {
        Checker {
            api_base: GITHUB_API.to_string(),
            cache_path: None,
            min_interval: DEFAULT_MIN_INTERVAL,
            timeout: DEFAULT_TIMEOUT,
        }
    }

    /// Point at another API root (tests use a local axum server).
    pub fn with_api_base(mut self, base: impl Into<String>) -> Self {
        self.api_base = base.into().trim_end_matches('/').to_string();
        self
    }

    /// Persist ETag + last good release list here (e.g. `<app data>/update-cache.json`).
    pub fn with_cache(mut self, path: impl Into<PathBuf>) -> Self {
        self.cache_path = Some(path.into());
        self
    }

    pub fn with_min_interval(mut self, interval: Duration) -> Self {
        self.min_interval = interval;
        self
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn min_interval(&self) -> Duration {
        self.min_interval
    }

    fn releases_url(&self, repo: &str) -> String {
        format!(
            "{}/repos/{}/releases?per_page=30",
            self.api_base,
            repo.trim_matches('/')
        )
    }

    /// Check for an update. Never fails; see [`Checked::reason`] for why not.
    pub async fn check(&self, current: &str, repo: &str, channel: Channel) -> UpdateInfo {
        self.check_detailed(current, repo, channel, false)
            .await
            .info
    }

    /// Full result. `force` skips the throttle (an explicit "Check now" click).
    pub async fn check_detailed(
        &self,
        current: &str,
        repo: &str,
        channel: Channel,
        force: bool,
    ) -> Checked {
        let url = self.releases_url(repo);
        let mut cache = self
            .cache_path
            .as_deref()
            .and_then(read_cache)
            .filter(|c| c.url == url)
            .unwrap_or_else(|| CacheFile {
                url: url.clone(),
                ..CacheFile::default()
            });

        let now = now_unix();
        let fresh = now.saturating_sub(cache.checked_unix) < self.min_interval.as_secs()
            && cache.checked_unix <= now;
        if !force && self.cache_path.is_some() && fresh {
            return match &cache.releases {
                Some(releases) => checked(current, releases, channel, None, true),
                None => Checked {
                    info: unavailable(current),
                    reason: Some("checked recently; try again later".into()),
                    from_cache: true,
                    manifest_url: None,
                },
            };
        }

        let outcome = self.fetch(&url, current, cache.etag.as_deref()).await;
        cache.checked_unix = now;
        let result = match outcome {
            Fetch::Fresh { releases, etag } => {
                let releases: Vec<Release> = releases
                    .into_iter()
                    .map(|mut r| {
                        r.body = r.body.map(|b| truncate_notes(&b, NOTES_MAX_CHARS));
                        r
                    })
                    .collect();
                let result = checked(current, &releases, channel, None, false);
                cache.etag = etag;
                cache.releases = Some(releases);
                result
            }
            Fetch::NotModified if cache.releases.is_some() => checked(
                current,
                cache.releases.as_deref().unwrap_or(&[]),
                channel,
                None,
                true,
            ),
            Fetch::NotModified => {
                cache.etag = None;
                Checked {
                    info: unavailable(current),
                    reason: Some("GitHub answered 304 but nothing is cached".into()),
                    from_cache: false,
                    manifest_url: None,
                }
            }
            Fetch::Failed(why) => {
                tracing::debug!(%why, "update check failed");
                match &cache.releases {
                    // Stale but known-good beats nothing; still say why it is stale.
                    Some(releases) => {
                        let why = format!("{why} (showing the last known release)");
                        checked(current, releases, channel, Some(why), true)
                    }
                    None => Checked {
                        info: unavailable(current),
                        reason: Some(why),
                        from_cache: false,
                        manifest_url: None,
                    },
                }
            }
        };
        if let Some(path) = &self.cache_path {
            write_cache(path, &cache);
        }
        result
    }

    async fn fetch(&self, url: &str, current: &str, etag: Option<&str>) -> Fetch {
        let client = match reqwest::Client::builder()
            .timeout(self.timeout)
            .connect_timeout(self.timeout)
            .user_agent(format!("GradApp/{current}"))
            .build()
        {
            Ok(c) => c,
            Err(e) => return Fetch::Failed(format!("http client: {e}")),
        };
        let mut req = client
            .get(url)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28");
        if let Some(etag) = etag {
            req = req.header("If-None-Match", etag);
        }
        let resp = match req.send().await {
            Ok(r) => r,
            Err(e) if e.is_timeout() => {
                return Fetch::Failed("GitHub did not answer in time".into())
            }
            Err(e) => return Fetch::Failed(format!("could not reach GitHub: {e}")),
        };
        let status = resp.status().as_u16();
        match status {
            200 => {
                let etag = resp
                    .headers()
                    .get("etag")
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_string);
                match resp.json::<Vec<Release>>().await {
                    Ok(releases) => Fetch::Fresh { releases, etag },
                    Err(e) => Fetch::Failed(format!("unexpected answer from GitHub: {e}")),
                }
            }
            304 => Fetch::NotModified,
            403 | 429 => {
                let reset = resp
                    .headers()
                    .get("x-ratelimit-reset")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.parse::<u64>().ok());
                Fetch::Failed(match reset {
                    Some(at) => format!(
                        "GitHub rate limit reached; retry in {} min",
                        at.saturating_sub(now_unix()).div_ceil(60)
                    ),
                    None => "GitHub rate limit reached".into(),
                })
            }
            404 => Fetch::Failed("no releases found (repository missing or private)".into()),
            other => Fetch::Failed(format!("GitHub answered HTTP {other}")),
        }
    }
}

enum Fetch {
    Fresh {
        releases: Vec<Release>,
        etag: Option<String>,
    },
    NotModified,
    Failed(String),
}

/// One-shot check with defaults and no cache. Prefer a [`Checker`] with
/// [`Checker::with_cache`] in the app so repeated checks stay within GitHub's limits.
pub async fn check(current: &str, repo: &str, channel: Channel) -> UpdateInfo {
    Checker::new().check(current, repo, channel).await
}

// ---------------------------------------------------------------------------
// Tauri updater manifest (latest.json)
// ---------------------------------------------------------------------------

/// The static JSON the Tauri updater reads, as written by `tauri-action`
/// with `includeUpdaterJson: true`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UpdaterManifest {
    pub version: String,
    #[serde(default)]
    pub notes: Option<String>,
    #[serde(default)]
    pub pub_date: Option<String>,
    pub platforms: BTreeMap<String, PlatformAsset>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlatformAsset {
    pub signature: String,
    pub url: String,
}

#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    #[error("latest.json is not valid: {0}")]
    Json(#[from] serde_json::Error),
    #[error("latest.json version {0:?} is not semver")]
    Version(String),
    #[error("latest.json lists no platforms")]
    NoPlatforms,
}

/// Parse and sanity-check a `latest.json`.
pub fn parse_latest_json(raw: &str) -> Result<UpdaterManifest, ManifestError> {
    let manifest: UpdaterManifest = serde_json::from_str(raw)?;
    if parse_version(&manifest.version).is_none() {
        return Err(ManifestError::Version(manifest.version));
    }
    if manifest.platforms.is_empty() {
        return Err(ManifestError::NoPlatforms);
    }
    Ok(manifest)
}

/// Tauri's platform key for an OS / arch pair, using Rust's `std::env::consts` names.
pub fn platform_key_for(os: &str, arch: &str) -> Option<String> {
    let os = match os {
        "windows" => "windows",
        "macos" => "darwin",
        "linux" => "linux",
        _ => return None,
    };
    let arch = match arch {
        "x86_64" => "x86_64",
        "aarch64" => "aarch64",
        "x86" => "i686",
        "arm" => "armv7",
        _ => return None,
    };
    Some(format!("{os}-{arch}"))
}

/// The key for the running build, e.g. `windows-x86_64` or `darwin-aarch64`.
pub fn platform_key() -> Option<String> {
    platform_key_for(std::env::consts::OS, std::env::consts::ARCH)
}

impl UpdaterManifest {
    pub fn version(&self) -> Option<Version> {
        parse_version(&self.version)
    }

    /// The asset for `key`. Tauri ≥2.1 may also write installer-specific
    /// keys (`windows-x86_64-nsis`, `linux-x86_64-appimage`, ...); the bare
    /// key wins, then the installer GradApp ships as primary.
    pub fn asset_for(&self, key: &str) -> Option<&PlatformAsset> {
        if let Some(a) = self.platforms.get(key) {
            return Some(a);
        }
        const PREFERRED: [&str; 6] = ["nsis", "msi", "app", "appimage", "deb", "rpm"];
        PREFERRED
            .iter()
            .find_map(|suffix| self.platforms.get(&format!("{key}-{suffix}")))
    }

    /// The asset for this machine.
    pub fn asset_for_this_platform(&self) -> Option<&PlatformAsset> {
        self.asset_for(&platform_key()?)
    }
}

#[cfg(test)]
mod unit {
    use super::*;

    fn rel(tag: &str, pre: bool, draft: bool, manifest: bool) -> Release {
        Release {
            tag_name: tag.into(),
            name: None,
            body: Some(format!("notes for {tag}")),
            draft,
            prerelease: pre,
            html_url: Some(format!("https://example/{tag}")),
            published_at: None,
            assets: if manifest {
                vec![ReleaseAsset {
                    name: MANIFEST_ASSET.into(),
                    browser_download_url: Some("u".into()),
                }]
            } else {
                vec![]
            },
        }
    }

    #[test]
    fn versions_parse() {
        assert_eq!(parse_version("v2.0.1"), Some(Version::new(2, 0, 1)));
        assert_eq!(
            parse_version("2.1.0-beta.2").unwrap().pre.as_str(),
            "beta.2"
        );
        assert!(parse_version("nightly").is_none());
        assert!(parse_version("v2.0").is_none());
    }

    #[test]
    fn highest_semver_wins_not_newest() {
        // v1 hotfix published after 2.0 must not hide 2.0.
        let rs = vec![
            rel("v1.0.5", false, false, false),
            rel("v2.0.0", false, false, true),
        ];
        let (info, _) = evaluate("1.0.4", &rs, Channel::Stable);
        assert_eq!(info.latest.as_deref(), Some("2.0.0"));
        assert!(info.available && info.installable);
    }

    #[test]
    fn prerelease_needs_beta_channel() {
        let rs = vec![
            rel("v2.0.0", false, false, true),
            rel("v2.1.0-beta.2", true, false, true),
        ];
        assert!(!evaluate("2.0.0", &rs, Channel::Stable).0.available);
        let (beta, _) = evaluate("2.0.0", &rs, Channel::Beta);
        assert_eq!(beta.latest.as_deref(), Some("2.1.0-beta.2"));
        // A semver pre-release tag not flagged as prerelease is still not stable.
        let rs = vec![rel("v2.2.0-rc.1", false, false, true)];
        assert!(!evaluate("2.0.0", &rs, Channel::Stable).0.available);
    }

    #[test]
    fn link_only_without_manifest() {
        let rs = vec![rel("v2.0.1", false, false, false)];
        let (info, _) = evaluate("2.0.0", &rs, Channel::Stable);
        assert!(info.available && !info.installable);
    }

    #[test]
    fn notes_truncate_on_char_boundary() {
        let s = "é".repeat(10);
        let t = truncate_notes(&s, 5);
        assert_eq!(t.chars().count(), 5);
        assert!(t.ends_with('…'));
        assert_eq!(truncate_notes("short", 5), "short");
    }

    #[test]
    fn platform_keys() {
        assert_eq!(
            platform_key_for("windows", "x86_64").as_deref(),
            Some("windows-x86_64")
        );
        assert_eq!(
            platform_key_for("macos", "aarch64").as_deref(),
            Some("darwin-aarch64")
        );
        assert_eq!(
            platform_key_for("macos", "x86_64").as_deref(),
            Some("darwin-x86_64")
        );
        assert_eq!(
            platform_key_for("linux", "x86_64").as_deref(),
            Some("linux-x86_64")
        );
        assert_eq!(platform_key_for("freebsd", "x86_64"), None);
        assert!(platform_key().is_some());
    }
}
