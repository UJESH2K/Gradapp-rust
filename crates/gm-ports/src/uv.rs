//! Finding, or fetching, the uv binary.
//!
//! Search order: `GRADAPP_UV` → a copy bundled next to the app executable →
//! `<data>/bin/uv(.exe)` → `PATH`. If none is found, [`ensure`] downloads the
//! pinned release from GitHub into `<data>/bin`, checking the release's sha256.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use sha2::{Digest, Sha256};
use tokio::process::Command;

use crate::error::{Error, Result};

/// The uv release GradApp downloads when none is found. Tested against 0.11.x.
pub const UV_VERSION: &str = "0.11.32";

const EXE: &str = if cfg!(windows) { "uv.exe" } else { "uv" };

/// Where a uv binary came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UvSource {
    /// `GRADAPP_UV`.
    Env,
    /// Shipped next to the app executable.
    Bundled,
    /// `<data>/bin`, downloaded by an earlier run.
    DataDir,
    /// Found on `PATH`.
    Path,
    /// Downloaded by this call.
    Downloaded,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Uv {
    pub path: PathBuf,
    pub source: UvSource,
}

impl Uv {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Uv {
            path: path.into(),
            source: UvSource::Env,
        }
    }

    /// `uv --version`, e.g. "uv 0.11.32 (...)".
    pub async fn version(&self) -> Result<String> {
        let out = Command::new(&self.path)
            .arg("--version")
            .kill_on_drop(true)
            .output()
            .await
            .map_err(|e| Error::io(&self.path, e))?;
        if !out.status.success() {
            return Err(Error::Command {
                step: "uv --version".into(),
                code: out.status.code(),
                message: String::from_utf8_lossy(&out.stderr).trim().to_string(),
            });
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    }
}

/// uv's release target triple for an OS/arch (Rust `std::env::consts` names).
/// Linux uses the static musl build so it runs on any distribution.
pub fn target_for(os: &str, arch: &str) -> Option<&'static str> {
    Some(match (os, arch) {
        ("windows", "x86_64") => "x86_64-pc-windows-msvc",
        ("windows", "aarch64") => "aarch64-pc-windows-msvc",
        ("macos", "aarch64") => "aarch64-apple-darwin",
        ("macos", "x86_64") => "x86_64-apple-darwin",
        ("linux", "x86_64") => "x86_64-unknown-linux-musl",
        ("linux", "aarch64") => "aarch64-unknown-linux-musl",
        _ => return None,
    })
}

/// The target triple for this machine.
pub fn target() -> Option<&'static str> {
    target_for(std::env::consts::OS, std::env::consts::ARCH)
}

/// `uv-<target>.zip` on Windows, `.tar.gz` elsewhere.
pub fn archive_name(target: &str) -> String {
    if target.contains("windows") {
        format!("uv-{target}.zip")
    } else {
        format!("uv-{target}.tar.gz")
    }
}

/// `https://github.com/astral-sh/uv/releases/download/<version>/<archive>`.
pub fn release_url(version: &str, target: &str) -> String {
    format!(
        "https://github.com/astral-sh/uv/releases/download/{version}/{}",
        archive_name(target)
    )
}

fn is_file(p: &Path) -> bool {
    p.is_file()
}

/// Search without downloading, using `data_dir` as the app data dir.
pub fn find_in(data_dir: &Path) -> Option<Uv> {
    if let Some(p) = std::env::var_os("GRADAPP_UV").filter(|v| !v.is_empty()) {
        let p = PathBuf::from(p);
        if is_file(&p) {
            return Some(Uv {
                path: p,
                source: UvSource::Env,
            });
        }
        tracing::warn!(
            "GRADAPP_UV={} is not a file; searching elsewhere",
            p.display()
        );
    }
    if let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(Path::to_path_buf))
    {
        // Next to the executable (Windows, Linux, Tauri sidecar) and the macOS
        // bundle's Resources folder.
        for p in [dir.join(EXE), dir.join("..").join("Resources").join(EXE)] {
            if is_file(&p) {
                return Some(Uv {
                    path: p,
                    source: UvSource::Bundled,
                });
            }
        }
    }
    let p = data_dir.join("bin").join(EXE);
    if is_file(&p) {
        return Some(Uv {
            path: p,
            source: UvSource::DataDir,
        });
    }
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|d| d.join(EXE))
            .find(|p| is_file(p))
            .map(|p| Uv {
                path: p,
                source: UvSource::Path,
            })
    })
}

/// Search the default locations without downloading.
pub fn find() -> Option<Uv> {
    find_in(&crate::paths::data_dir())
}

/// Oldest uv whose CLI provisioning relies on (`uv python install --no-bin`
/// and `--no-registry` arrived in 0.8).
pub const MIN_UV_VERSION: (u64, u64, u64) = (0, 8, 0);

/// `"uv 0.11.32 (abc 2026-10-01)"` → `(0, 11, 32)`.
pub fn parse_uv_version(text: &str) -> Option<(u64, u64, u64)> {
    let version = text.trim().strip_prefix("uv ")?.split_whitespace().next()?;
    let mut parts = version.split('.').map(|p| p.parse::<u64>().ok());
    Some((
        parts.next()??,
        parts.next()??,
        parts.next().flatten().unwrap_or(0),
    ))
}

/// `uv --version` runs and is at least [`MIN_UV_VERSION`].
pub async fn is_usable(uv: &Uv) -> bool {
    match tokio::time::timeout(Duration::from_secs(20), uv.version()).await {
        Ok(Ok(v)) => parse_uv_version(&v).is_some_and(|v| v >= MIN_UV_VERSION),
        _ => false,
    }
}

/// Find uv, downloading the pinned release into `<data_dir>/bin` if needed.
///
/// A uv found on `PATH` or left in `<data>/bin` is used only if it is at
/// least [`MIN_UV_VERSION`]; an older one (a years-old Homebrew or pipx uv)
/// would fail on flags provisioning needs, so the pinned release is fetched
/// instead. `GRADAPP_UV` and a bundled copy are trusted as they are.
pub async fn ensure(data_dir: &Path) -> Result<Uv> {
    if let Some(uv) = find_in(data_dir) {
        let checked = matches!(uv.source, UvSource::Path | UvSource::DataDir);
        if !checked || is_usable(&uv).await {
            return Ok(uv);
        }
        tracing::warn!(
            "{} is older than uv {}.{}.{} or does not run; downloading uv {UV_VERSION}",
            uv.path.display(),
            MIN_UV_VERSION.0,
            MIN_UV_VERSION.1,
            MIN_UV_VERSION.2
        );
    }
    download(data_dir, UV_VERSION).await
}

async fn get_bytes(client: &reqwest::Client, url: &str) -> Result<Vec<u8>> {
    let mut last = String::new();
    for attempt in 0..4u32 {
        if attempt > 0 {
            tokio::time::sleep(Duration::from_secs(2u64.pow(attempt))).await;
        }
        match client
            .get(url)
            .send()
            .await
            .and_then(|r| r.error_for_status())
        {
            Ok(resp) => match resp.bytes().await {
                Ok(b) => return Ok(b.to_vec()),
                Err(e) => last = e.to_string(),
            },
            Err(e) => {
                last = e.to_string();
                // 4xx will not get better by retrying.
                if e.status().is_some_and(|s| s.is_client_error()) {
                    break;
                }
            }
        }
    }
    Err(Error::Download {
        url: url.to_string(),
        message: last,
    })
}

/// Download `version` of uv for this machine into `<data_dir>/bin`.
pub async fn download(data_dir: &Path, version: &str) -> Result<Uv> {
    let target = target().ok_or_else(|| {
        Error::UvUnsupported(format!(
            "{}-{}",
            std::env::consts::OS,
            std::env::consts::ARCH
        ))
    })?;
    download_target(data_dir, version, target, &data_dir.join("bin").join(EXE)).await
}

/// Download uv `version` for `target` and install the binary at `dest`.
pub async fn download_target(
    data_dir: &Path,
    version: &str,
    target: &str,
    dest: &Path,
) -> Result<Uv> {
    let url = release_url(version, target);
    let client = reqwest::Client::builder()
        .user_agent(concat!("gradapp/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(20))
        .timeout(Duration::from_secs(600))
        .build()
        .map_err(|e| Error::Download {
            url: url.clone(),
            message: e.to_string(),
        })?;

    let archive = get_bytes(&client, &url).await?;
    let sums = get_bytes(&client, &format!("{url}.sha256")).await?;
    let expected = String::from_utf8_lossy(&sums)
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let actual = hex::encode(Sha256::digest(&archive));
    if expected.len() != 64 || expected != actual {
        return Err(Error::Checksum {
            file: archive_name(target),
            expected,
            actual,
        });
    }

    let exe = if target.contains("windows") {
        "uv.exe"
    } else {
        "uv"
    };
    let binary = extract_binary(&archive, target.contains("windows"), exe)?;

    let bin_dir = dest.parent().unwrap_or(data_dir);
    std::fs::create_dir_all(bin_dir).map_err(|e| Error::io(bin_dir, e))?;
    let tmp = bin_dir.join(format!(".{exe}.download"));
    std::fs::write(&tmp, &binary).map_err(|e| Error::io(&tmp, e))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| Error::io(&tmp, e))?;
    }
    std::fs::rename(&tmp, dest).map_err(|e| Error::io(dest, e))?;
    Ok(Uv {
        path: dest.to_path_buf(),
        source: UvSource::Downloaded,
    })
}

/// Pull the `uv` binary out of a release archive (zip on Windows, tar.gz elsewhere).
pub fn extract_binary(archive: &[u8], is_zip: bool, exe: &str) -> Result<Vec<u8>> {
    let not_found = || Error::Other(format!("{exe} not found in the uv release archive"));
    let bad = |e: String| Error::Other(format!("cannot read the uv release archive: {e}"));
    if is_zip {
        let mut zip =
            zip::ZipArchive::new(std::io::Cursor::new(archive)).map_err(|e| bad(e.to_string()))?;
        for i in 0..zip.len() {
            let mut f = zip.by_index(i).map_err(|e| bad(e.to_string()))?;
            let name = f.name().map_err(|e| bad(e.to_string()))?.replace('\\', "/");
            if f.is_file() && name.rsplit('/').next() == Some(exe) {
                let mut buf = Vec::new();
                f.read_to_end(&mut buf).map_err(|e| bad(e.to_string()))?;
                return Ok(buf);
            }
        }
        Err(not_found())
    } else {
        let gz = flate2::read::GzDecoder::new(archive);
        let mut tar = tar::Archive::new(gz);
        for entry in tar.entries().map_err(|e| bad(e.to_string()))? {
            let mut entry = entry.map_err(|e| bad(e.to_string()))?;
            let is_exe = entry
                .path()
                .ok()
                .and_then(|p| p.file_name().map(|n| n == exe))
                .unwrap_or(false);
            if is_exe && entry.header().entry_type().is_file() {
                let mut buf = Vec::new();
                entry
                    .read_to_end(&mut buf)
                    .map_err(|e| bad(e.to_string()))?;
                return Ok(buf);
            }
        }
        Err(not_found())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn targets_and_urls() {
        assert_eq!(
            target_for("linux", "x86_64"),
            Some("x86_64-unknown-linux-musl")
        );
        assert_eq!(
            target_for("windows", "aarch64"),
            Some("aarch64-pc-windows-msvc")
        );
        assert_eq!(target_for("freebsd", "x86_64"), None);
        assert_eq!(
            release_url("0.11.32", "aarch64-apple-darwin"),
            "https://github.com/astral-sh/uv/releases/download/0.11.32/uv-aarch64-apple-darwin.tar.gz"
        );
        assert!(release_url(UV_VERSION, "x86_64-pc-windows-msvc")
            .ends_with("uv-x86_64-pc-windows-msvc.zip"));
    }

    #[test]
    fn extracts_from_tar_gz_and_zip() {
        // tar.gz with the layout uv releases use: uv-<target>/uv
        let mut tar_bytes = Vec::new();
        {
            let gz = flate2::write::GzEncoder::new(&mut tar_bytes, flate2::Compression::fast());
            let mut b = tar::Builder::new(gz);
            for (name, data) in [("uv-x/uvx", &b"uvx"[..]), ("uv-x/uv", &b"UVBIN"[..])] {
                let mut h = tar::Header::new_gnu();
                h.set_size(data.len() as u64);
                h.set_mode(0o755);
                h.set_cksum();
                b.append_data(&mut h, name, data).unwrap();
            }
            b.into_inner().unwrap().finish().unwrap();
        }
        assert_eq!(extract_binary(&tar_bytes, false, "uv").unwrap(), b"UVBIN");

        let mut zip_bytes = Vec::new();
        {
            let mut z = zip::ZipWriter::new(std::io::Cursor::new(&mut zip_bytes));
            let opts = zip::write::SimpleFileOptions::default();
            z.start_file("uvx.exe", opts).unwrap();
            std::io::Write::write_all(&mut z, b"no").unwrap();
            z.start_file("uv.exe", opts).unwrap();
            std::io::Write::write_all(&mut z, b"WINUV").unwrap();
            z.finish().unwrap();
        }
        assert_eq!(
            extract_binary(&zip_bytes, true, "uv.exe").unwrap(),
            b"WINUV"
        );
        assert!(extract_binary(&zip_bytes, true, "uv").is_err());
    }
}
