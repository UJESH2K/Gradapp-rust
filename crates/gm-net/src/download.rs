//! Streaming download to `<target>.part`, then an atomic rename (v1 `download`).

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use futures::StreamExt;
use tokio::io::AsyncWriteExt;

use crate::error::{NetError, Result};

/// Internet client: honours the system proxy (unlike [`crate::lan_client`]),
/// follows redirects (GitHub release assets redirect to a CDN).
fn web_client() -> reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(15))
                .redirect(reqwest::redirect::Policy::limited(10))
                .user_agent(concat!("GradApp/", env!("CARGO_PKG_VERSION")))
                .build()
                .expect("reqwest client")
        })
        .clone()
}

fn part_path(target: &Path) -> PathBuf {
    let mut name = target
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_default();
    name.push(".part");
    target.with_file_name(name)
}

/// Download `url` to `target`. `on_progress(received, total)` is called per
/// chunk (`total` from Content-Length when known). Writes `<target>.part` and
/// renames it into place only when complete, so `target` is never partial;
/// on failure or cancellation (dropping the future) the `.part` file is
/// removed. Whole transfer limited to 10 minutes, as in v1. Returns bytes written.
pub async fn download<F>(url: &str, target: &Path, on_progress: F) -> Result<u64>
where
    F: FnMut(u64, Option<u64>),
{
    download_with(
        &web_client(),
        url,
        target,
        Duration::from_secs(600),
        on_progress,
    )
    .await
}

/// [`download`] with your own client and overall timeout.
pub async fn download_with<F>(
    client: &reqwest::Client,
    url: &str,
    target: &Path,
    timeout: Duration,
    mut on_progress: F,
) -> Result<u64>
where
    F: FnMut(u64, Option<u64>),
{
    let response = client
        .get(url)
        .timeout(timeout)
        .send()
        .await
        .map_err(|e| NetError::from_reqwest(e, url))?;
    let status = response.status();
    if !status.is_success() {
        return Err(NetError::Http {
            status: status.as_u16(),
            detail: format!("HTTP {} for {url}", status.as_u16()),
        });
    }
    let total = response.content_length().filter(|n| *n > 0);
    if let Some(dir) = target.parent().filter(|d| !d.as_os_str().is_empty()) {
        tokio::fs::create_dir_all(dir).await?;
    }
    let temp = part_path(target);
    let mut cleanup = RemoveOnDrop(Some(temp.clone()));
    let mut file = tokio::fs::File::create(&temp).await?;
    let mut received = 0u64;
    let mut body = response.bytes_stream();
    while let Some(chunk) = body.next().await {
        let chunk = chunk.map_err(|e| NetError::from_reqwest(e, url))?;
        file.write_all(&chunk).await?;
        received += chunk.len() as u64;
        on_progress(received, total);
    }
    if let Some(expected) = total {
        if received != expected {
            return Err(NetError::Decode(format!(
                "short download: {received} of {expected} bytes"
            )));
        }
    }
    file.flush().await?;
    file.sync_all().await?;
    drop(file);
    tokio::fs::rename(&temp, target).await?;
    cleanup.0 = None;
    Ok(received)
}

struct RemoveOnDrop(Option<PathBuf>);

impl Drop for RemoveOnDrop {
    fn drop(&mut self) {
        if let Some(p) = self.0.take() {
            let _ = std::fs::remove_file(p);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn part_name() {
        assert_eq!(
            part_path(Path::new("/a/b/app.tar.gz")),
            PathBuf::from("/a/b/app.tar.gz.part")
        );
    }
}
