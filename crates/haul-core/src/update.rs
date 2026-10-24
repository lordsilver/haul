//! Haul's own updates: the latest release on GitHub, when it is newer than
//! this build, with what the desktop app needs to install it (its setup
//! program and `SHA256SUMS`, which the download is checked against).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::hosts::Http;

/// Where releases are published: the repository this build came from.
pub const REPOSITORY: &str = "lordsilver/haul";

/// A release newer than this build.
#[derive(Debug, Clone, PartialEq)]
pub struct Update {
    pub version: String,
    /// The release notes, as written (Markdown).
    pub notes: String,
    /// The release's page.
    pub page: String,
    /// The Windows setup program, when the release has one.
    pub installer: Option<Asset>,
    pub sums: Option<Asset>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Asset {
    pub name: String,
    pub url: String,
}

/// What the views show of an update.
#[derive(Debug, Clone, Serialize, PartialEq, ts_rs::TS)]
#[ts(export)]
pub struct UpdateView {
    pub version: String,
    pub notes: String,
    pub page: String,
    /// The desktop app can install it itself (on Windows, with its setup
    /// program and checksums published).
    pub installable: bool,
}

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    #[serde(default)]
    body: String,
    html_url: String,
    #[serde(default)]
    assets: Vec<ReleaseAsset>,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
}

#[derive(Deserialize)]
struct ReleaseAsset {
    name: String,
    browser_download_url: String,
}

/// `a.b.c` as numbers, a leading `v` and anything after `-` or `+` left out.
fn version(v: &str) -> Option<(u64, u64, u64)> {
    let core = v.trim().trim_start_matches('v').split(['-', '+']).next()?;
    let mut parts = core.split('.').map(|p| p.parse::<u64>().ok());
    Some((
        parts.next()??,
        parts.next().flatten().unwrap_or(0),
        parts.next().flatten().unwrap_or(0),
    ))
}

/// Whether release `tag` is newer than `current`.
pub fn newer(tag: &str, current: &str) -> bool {
    matches!((version(tag), version(current)), (Some(a), Some(b)) if a > b)
}

/// The latest release, when it is newer than `current`. `api` is GitHub's
/// API (`https://api.github.com`).
pub async fn latest(http: &Http, api: &str, current: &str) -> Result<Option<Update>, String> {
    let r = http
        .client
        .get(format!("{api}/repos/{REPOSITORY}/releases/latest"))
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !r.status().is_success() {
        return Err(format!("GitHub answered {}", r.status()));
    }
    let release: Release = r.json().await.map_err(|e| e.to_string())?;
    if release.draft || release.prerelease || !newer(&release.tag_name, current) {
        return Ok(None);
    }
    let asset = |pick: &dyn Fn(&str) -> bool| {
        release
            .assets
            .iter()
            .find(|a| pick(&a.name))
            .map(|a| Asset {
                name: a.name.clone(),
                url: a.browser_download_url.clone(),
            })
    };
    Ok(Some(Update {
        version: release.tag_name.trim_start_matches('v').to_string(),
        notes: release.body.clone(),
        page: release.html_url.clone(),
        installer: asset(&|n| n.ends_with("-setup.exe") && n.contains("windows")),
        sums: asset(&|n| n == "SHA256SUMS"),
    }))
}

/// The SHA-256 `SHA256SUMS` gives for file `name` (`<hex>  <name>` lines).
fn sum_for<'a>(sums: &'a str, name: &str) -> Option<&'a str> {
    sums.lines().find_map(|line| {
        let (hex, file) = line.split_once(char::is_whitespace)?;
        (file.trim().trim_start_matches('*') == name).then_some(hex.trim())
    })
}

/// Downloads `update`'s setup program into `dir` and checks it against the
/// release's `SHA256SUMS`. Returns where it is.
pub async fn fetch_installer(http: &Http, update: &Update, dir: &Path) -> Result<PathBuf, String> {
    let (Some(installer), Some(sums)) = (&update.installer, &update.sums) else {
        return Err("this release has no setup program to install".into());
    };
    let get = |url: &str| http.client.get(url).send();
    let sums = get(&sums.url)
        .await
        .and_then(|r| r.error_for_status())
        .map_err(|e| e.to_string())?
        .text()
        .await
        .map_err(|e| e.to_string())?;
    let want = sum_for(&sums, &installer.name)
        .ok_or_else(|| format!("SHA256SUMS lists no {}", installer.name))?
        .to_ascii_lowercase();
    let bytes = get(&installer.url)
        .await
        .and_then(|r| r.error_for_status())
        .map_err(|e| e.to_string())?
        .bytes()
        .await
        .map_err(|e| e.to_string())?;
    let got = Sha256::digest(&bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    if got != want {
        return Err(format!("{} does not match its checksum", installer.name));
    }
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let path = dir.join(&installer.name);
    std::fs::write(&path, &bytes).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(path)
}

impl Update {
    pub fn view(&self) -> UpdateView {
        UpdateView {
            version: self.version.clone(),
            notes: self.notes.clone(),
            page: self.page.clone(),
            installable: cfg!(windows) && self.installer.is_some() && self.sums.is_some(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_versions() {
        assert!(newer("v0.15.4", "0.15.3"));
        assert!(newer("v1.0.0", "0.99.9"));
        assert!(!newer("v0.15.3", "0.15.3"));
        assert!(!newer("v0.15.2", "0.15.3"));
        assert!(!newer("nightly", "0.15.3"));
        assert!(newer("v0.16", "0.15.9"));
    }

    #[test]
    fn reads_sha256sums() {
        let sums = "aa11  haul-v0.15.4-x86_64-unknown-linux-gnu.tar.gz\nbb22 *haul-desktop-v0.15.4-x86_64-pc-windows-msvc-setup.exe\n";
        assert_eq!(
            sum_for(
                sums,
                "haul-desktop-v0.15.4-x86_64-pc-windows-msvc-setup.exe"
            ),
            Some("bb22")
        );
        assert_eq!(sum_for(sums, "missing.exe"), None);
    }
}
