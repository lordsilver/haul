//! Haul's own updates: the latest release on GitHub, when it is newer than
//! this build, with what the desktop app needs to install it (its setup
//! program, whose download is checked against the digest GitHub gives).

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
    /// What the release changed (Markdown), from its notes.
    pub notes: String,
    /// The release's page.
    pub page: String,
    /// The Windows setup program, when the release has one and GitHub gives
    /// its digest.
    pub installer: Option<Asset>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Asset {
    pub name: String,
    pub url: String,
    /// Its SHA-256 as GitHub reports it, in hexadecimal.
    pub sha256: String,
}

/// What the views show of an update.
#[derive(Debug, Clone, Serialize, PartialEq, ts_rs::TS)]
#[ts(export)]
pub struct UpdateView {
    pub version: String,
    pub notes: String,
    pub page: String,
    /// The desktop app can install it itself (on Windows, with its setup
    /// program published and its digest known).
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
    /// `sha256:<hex>`.
    #[serde(default)]
    digest: Option<String>,
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
    let installer = release
        .assets
        .iter()
        .find(|a| a.name.ends_with("-setup.exe") && a.name.contains("windows"))
        .and_then(|a| {
            Some(Asset {
                name: a.name.clone(),
                url: a.browser_download_url.clone(),
                sha256: sha256_of(a.digest.as_deref()?)?,
            })
        });
    Ok(Some(Update {
        version: release.tag_name.trim_start_matches('v').to_string(),
        notes: what_changed(&release.body).to_string(),
        page: release.html_url.clone(),
        installer,
    }))
}

/// A release's notes without the list of its files and how to check them,
/// which the release workflow adds under this heading: the release page's
/// business, not the app's.
fn what_changed(notes: &str) -> &str {
    notes
        .split_once("### Downloads")
        .map_or(notes, |(changes, _)| changes)
        .trim_end()
}

/// The hexadecimal SHA-256 in a digest as GitHub writes it (`sha256:<hex>`).
fn sha256_of(digest: &str) -> Option<String> {
    let hex = digest.strip_prefix("sha256:")?;
    (hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit()))
        .then(|| hex.to_ascii_lowercase())
}

/// Whether `bytes` are what the hexadecimal SHA-256 `want` says.
fn matches_sha256(bytes: &[u8], want: &str) -> bool {
    crate::hosts::to_hex(&Sha256::digest(bytes)) == want
}

/// Downloads `update`'s setup program into `dir` and checks it against the
/// digest GitHub gives for it. Returns where it is.
pub async fn fetch_installer(http: &Http, update: &Update, dir: &Path) -> Result<PathBuf, String> {
    let Some(installer) = &update.installer else {
        return Err("this release has no setup program to install".into());
    };
    let bytes = http
        .client
        .get(&installer.url)
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(|e| e.to_string())?
        .bytes()
        .await
        .map_err(|e| e.to_string())?;
    if !matches_sha256(&bytes, &installer.sha256) {
        return Err(format!("{} does not match its checksum", installer.name));
    }
    tokio::fs::create_dir_all(dir)
        .await
        .map_err(|e| e.to_string())?;
    // The name as GitHub sent it, made a plain file name in `dir`.
    let path = dir.join(crate::paths::safe(&installer.name));
    tokio::fs::write(&path, &bytes)
        .await
        .map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(path)
}

impl Update {
    pub fn view(&self) -> UpdateView {
        UpdateView {
            version: self.version.clone(),
            notes: self.notes.clone(),
            page: self.page.clone(),
            installable: cfg!(windows) && self.installer.is_some(),
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
    fn notes_leave_out_the_files() {
        let notes = "### Fixed\n\n- A fix.\n\n### Downloads\n\n| File |\n";
        assert_eq!(what_changed(notes), "### Fixed\n\n- A fix.");
        assert_eq!(what_changed("### Downloads\n\n| File |\n"), "");
        assert_eq!(what_changed("- A fix.\n"), "- A fix.");
    }

    #[test]
    fn reads_githubs_digest() {
        let hex = "ab".repeat(32);
        assert_eq!(sha256_of(&format!("sha256:{hex}")), Some(hex.clone()));
        let upper = hex.to_ascii_uppercase();
        assert_eq!(sha256_of(&format!("sha256:{upper}")), Some(hex.clone()));
        assert_eq!(sha256_of(&hex), None, "no algorithm");
        assert_eq!(sha256_of(&format!("sha512:{hex}")), None);
        assert_eq!(sha256_of("sha256:abc"), None, "too short");
    }

    #[test]
    fn a_wrong_file_is_not_taken_for_the_right_one() {
        // SHA-256 of "abc".
        let want = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        assert!(matches_sha256(b"abc", want));
        assert!(!matches_sha256(b"abd", want));
    }
}
