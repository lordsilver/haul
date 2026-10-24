//! mediafire.com: its API (`file/get_info.php`, no account) gives a file's
//! exact name, size and SHA-256; the file's page holds the download address
//! on its download button (`href`, or base64 in `data-scrambled-url` on
//! newer pages). A file mediafire took down or deleted sends its page to
//! `error.php?errno=…`.

use std::sync::LazyLock;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use regex::Regex;
use serde_json::Value;

use super::{Algo, Checksum, Cx, Entry, Probed, ResolveError};

static KEY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"/(?:file|download|view)/([A-Za-z0-9]{8,20})").unwrap());
static BUTTON: LazyLock<[Regex; 3]> = LazyLock::new(|| {
    [
        Regex::new(r#"href\s*=\s*"(https?://[^"]+)"\s*id\s*=\s*"downloadButton""#).unwrap(),
        Regex::new(r#"id\s*=\s*"downloadButton"[^>]*?href\s*=\s*"(https?://[^"]+)""#).unwrap(),
        Regex::new(r#"data-scrambled-url\s*=\s*"([A-Za-z0-9+/=]+)""#).unwrap(),
    ]
});

/// What the API says of the file.
struct Info {
    name: String,
    size: u64,
    checksum: Option<Checksum>,
}

async fn info(cx: &Cx<'_>, key: &str) -> Result<Info, ResolveError> {
    let base = &cx.cfg.endpoints.mediafire;
    let answer: Value = cx
        .http
        .client
        .get(format!("{base}/api/1.5/file/get_info.php"))
        .query(&[("quick_key", key), ("response_format", "json")])
        .send()
        .await?
        .json()
        .await
        .map_err(|e| ResolveError::Failed(format!("mediafire: {e}")))?;
    let f = &answer["response"]["file_info"];
    match (
        f["filename"].as_str(),
        f["size"].as_str().and_then(|s| s.parse().ok()),
    ) {
        (Some(name), Some(size)) => Ok(Info {
            name: name.to_string(),
            size,
            checksum: f["hash"]
                .as_str()
                .and_then(|h| Checksum::from_hex(Algo::Sha256, h)),
        }),
        _ => Err(ResolveError::failed("mediafire: the file was deleted")),
    }
}

fn key_of(url: &str) -> Result<String, ResolveError> {
    KEY.captures(url)
        .map(|c| c[1].to_string())
        .ok_or_else(|| ResolveError::failed(format!("mediafire: not a file link: {url}")))
}

/// The download address on the file's page.
fn button(page: &str) -> Option<String> {
    BUTTON.iter().enumerate().find_map(|(i, re)| {
        let found = re.captures(page)?[1].to_string();
        if i < 2 {
            return Some(found.replace("&amp;", "&"));
        }
        String::from_utf8(STANDARD.decode(found).ok()?)
            .ok()
            .filter(|u| u.starts_with("http"))
    })
}

pub(super) async fn resolve(cx: &Cx<'_>, url: &str) -> Result<Vec<Entry>, ResolveError> {
    let key = key_of(url)?;
    let info = info(cx, &key).await?;
    let base = &cx.cfg.endpoints.mediafire;
    let page = cx
        .http
        .client
        .get(format!("{base}/file/{key}"))
        .send()
        .await?;
    // A file mediafire took down still has its info; its page says why.
    if page.url().path().contains("error.php") {
        let errno = page
            .url()
            .query_pairs()
            .find(|(k, _)| k == "errno")
            .map(|(_, v)| v.into_owned())
            .unwrap_or_default();
        return Err(ResolveError::Failed(format!(
            "mediafire: the file is not available (mediafire error {errno})"
        )));
    }
    let page = page.text().await?;
    let address = button(&page).ok_or_else(|| {
        ResolveError::NeedsUser("mediafire: its page gave no download button: open it".into())
    })?;
    Ok(vec![Entry {
        url: address,
        name: Some(info.name),
        size: Some(info.size),
        checksum: info.checksum,
        ..Entry::default()
    }])
}

pub(super) async fn probe(cx: &Cx<'_>, url: &str) -> Option<Probed> {
    let info = info(cx, &key_of(url).ok()?).await.ok()?;
    Some(Probed {
        name: info.name,
        size: Some(info.size),
        checksum: info.checksum,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_download_button_in_each_form() {
        let plain = r#"<a class="input popsok" aria-label="Download file" href="https://download2.mediafire.com/x/R.rar" id="downloadButton">"#;
        assert_eq!(
            button(plain).as_deref(),
            Some("https://download2.mediafire.com/x/R.rar")
        );
        let scrambled = format!(
            r#"<a id="downloadButton" href="javascript:void(0)" data-scrambled-url="{}">"#,
            STANDARD.encode("https://download9.mediafire.com/y/R.rar")
        );
        assert_eq!(
            button(&scrambled).as_deref(),
            Some("https://download9.mediafire.com/y/R.rar")
        );
        assert_eq!(button("<p>nothing</p>"), None);
    }

    #[test]
    fn reads_the_files_key() {
        assert_eq!(
            key_of("https://www.mediafire.com/file/ohi5vdv1nx2fqsu/R.part1.rar/file").unwrap(),
            "ohi5vdv1nx2fqsu"
        );
    }
}
