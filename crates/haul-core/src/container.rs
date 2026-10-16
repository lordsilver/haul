//! Link containers: plain text (any file with links in it), JDownloader
//! `.dlc` files and Click'n'Load data.
//!
//! DLC uses the same scheme as pyLoad: the file's last 88 characters go to
//! JDownloader's key service, whose answer decrypts the file key, which in
//! turn decrypts the link list. Import therefore needs that service online.
//! Click'n'Load data carries its key, so it needs nothing online.

use std::sync::LazyLock;

use aes::cipher::{BlockModeDecrypt, KeyIvInit, block_padding::NoPadding};
use base64::{
    Engine as _, alphabet,
    engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig},
};
use regex::Regex;

/// A link from a container, with the name and package it carried.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    pub url: String,
    pub name: Option<String>,
    pub package: String,
}

static URL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"https?://[^\s"'<>]+"#).unwrap());

/// Every http(s) link in `text`, without trailing punctuation.
pub fn extract_urls(text: &str) -> Vec<String> {
    URL.find_iter(text)
        .map(|m| m.as_str().trim_end_matches(['.', ',', ')']).to_string())
        .collect()
}

/// Links from a container file, recognised by its name.
pub async fn read(
    http: &reqwest::Client,
    dlc_service: &str,
    filename: &str,
    bytes: &[u8],
) -> Result<Vec<Link>, String> {
    if filename.to_ascii_lowercase().ends_with(".dlc") {
        return parse_dlc(http, dlc_service, bytes).await;
    }
    let text = String::from_utf8_lossy(bytes);
    Ok(extract_urls(&text)
        .into_iter()
        .map(|url| Link {
            url,
            name: None,
            package: String::new(),
        })
        .collect())
}

const KEY: &[u8; 16] = b"cb99b5cbc24db398";
const IV: &[u8; 16] = b"9bc24cb995cb8db3";

/// Base64 that, like Python's decoder, skips characters outside the alphabet
/// and does not insist on padding.
const LENIENT: GeneralPurpose = GeneralPurpose::new(
    &alphabet::STANDARD,
    GeneralPurposeConfig::new()
        .with_decode_padding_mode(DecodePaddingMode::Indifferent)
        .with_decode_allow_trailing_bits(true),
);

fn b64(s: &str) -> Result<Vec<u8>, String> {
    let clean: String = s
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '/'))
        .collect();
    LENIENT
        .decode(clean)
        .map_err(|e| format!("bad base64 in DLC: {e}"))
}

fn b64_text(s: &str) -> String {
    b64(s)
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .unwrap_or_default()
}

fn decrypt(key: &[u8], iv: &[u8], data: &mut [u8]) -> Result<usize, String> {
    let dec = cbc::Decryptor::<aes::Aes128>::new_from_slices(key, iv).map_err(|e| e.to_string())?;
    let plain = dec
        .decrypt_padded::<NoPadding>(data)
        .map_err(|_| "DLC data is not a whole number of blocks".to_string())?;
    Ok(plain.len())
}

/// The links in Click'n'Load data: `crypted` is base64 of the list, one
/// link per line, encrypted with AES-128-CBC under `jk`, a key in hexadecimal
/// that is its IV as well. The protocol sends `jk` as a JavaScript function
/// returning that hexadecimal; a page that gives the hexadecimal itself, as
/// filecrypt.cc's does, leaves nothing to run.
pub fn parse_cnl(jk: &str, crypted: &str) -> Result<Vec<String>, String> {
    let jk = jk.trim();
    let key = (0..jk.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(jk.get(i..i + 2)?, 16).ok())
        .collect::<Option<Vec<u8>>>()
        .filter(|key| key.len() == 16)
        .ok_or("the Click'n'Load key is not 16 bytes in hexadecimal")?;
    let mut data = b64(crypted).map_err(|_| "bad base64 in the Click'n'Load data".to_string())?;
    let n = decrypt(&key, &key, &mut data)
        .map_err(|_| "the Click'n'Load data is not a whole number of blocks".to_string())?;
    Ok(extract_urls(
        String::from_utf8_lossy(&data[..n]).trim_end_matches('\0'),
    ))
}

static RC: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<rc>(.+?)</rc>").unwrap());
static PACKAGE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?s)<package\b(.*?)</package>").unwrap());
static PACKAGE_NAME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"^[^>]*name="([^"]*)""#).unwrap());
static FILE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?s)<file>(.*?)</file>").unwrap());
static FILE_URL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?s)<url>(.*?)</url>").unwrap());
static FILE_NAME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?s)<filename>(.*?)</filename>").unwrap());

pub async fn parse_dlc(
    http: &reqwest::Client,
    service: &str,
    raw: &[u8],
) -> Result<Vec<Link>, String> {
    let data: String = raw
        .iter()
        .filter(|b| b.is_ascii())
        .map(|&b| b as char)
        .collect();
    let data = data.trim();
    if data.len() < 100 {
        return Err("not a DLC file".into());
    }
    let (body, dlc_key) = data.split_at(data.len() - 88);
    let response = http
        .get(format!(
            "{service}?srcType=dlc&destType=pylo&data={dlc_key}"
        ))
        .timeout(std::time::Duration::from_secs(20))
        .send()
        .await
        .map_err(|e| format!("JDownloader key service: {}", e.without_url()))?
        .text()
        .await
        .map_err(|e| format!("JDownloader key service: {}", e.without_url()))?;
    let Some(rc) = RC.captures(&response) else {
        let preview: String = response.chars().take(120).collect();
        return Err(format!(
            "JDownloader key service refused/unavailable: {preview}"
        ));
    };
    let mut key = b64(&rc[1])?;
    let n = decrypt(KEY, IV, &mut key)?;
    if n < 16 {
        return Err("JDownloader key service sent a short key".into());
    }
    let key = key[..16].to_vec();
    let mut xml = b64(body)?;
    let n = decrypt(&key, &key, &mut xml)?;
    let xml_b64 = String::from_utf8_lossy(&xml[..n])
        .trim_end_matches('\0')
        .trim()
        .to_string();
    let xml = b64_text(&xml_b64);

    let mut out = Vec::new();
    for pkg in PACKAGE.captures_iter(&xml) {
        let pkg = &pkg[1];
        let package = PACKAGE_NAME
            .captures(pkg)
            .map(|m| b64_text(&m[1]))
            .unwrap_or_default();
        for file in FILE.captures_iter(pkg) {
            let file = &file[1];
            if let Some(url) = FILE_URL.captures(file) {
                // "n.A." is how a container says it does not know the name:
                // taken as a name, it would merge unrelated files into one row.
                let name = FILE_NAME
                    .captures(file)
                    .map(|n| b64_text(&n[1]).trim().to_string())
                    .filter(|n| !n.is_empty() && n != "n.A.");
                out.push(Link {
                    url: b64_text(&url[1]).trim().to_string(),
                    name,
                    package: package.clone(),
                });
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use aes::cipher::BlockModeEncrypt;
    use base64::engine::general_purpose::STANDARD;

    #[test]
    fn urls_in_text() {
        let text = "see https://a.example/x.rar, (https://b.example/y.zip) and http://c.example/z.7z.\nnot ftp://d";
        assert_eq!(
            extract_urls(text),
            [
                "https://a.example/x.rar",
                "https://b.example/y.zip",
                "http://c.example/z.7z"
            ]
        );
    }

    fn encrypt(key: &[u8], iv: &[u8], plain: &[u8]) -> Vec<u8> {
        let mut buf = plain.to_vec();
        buf.resize(plain.len().div_ceil(16) * 16, 0);
        let len = buf.len();
        cbc::Encryptor::<aes::Aes128>::new_from_slices(key, iv)
            .unwrap()
            .encrypt_padded::<NoPadding>(&mut buf, len)
            .unwrap()
            .to_vec()
    }

    #[test]
    fn click_n_load_data_decrypts_with_its_key() {
        let key = b"6229761275967424";
        let jk: String = key.iter().map(|b| format!("{b:02x}")).collect();
        let plain = "https://a.example/one.part1.rar\r\nhttps://b.example/d/xyz\r\n";
        let crypted = STANDARD.encode(encrypt(key, key, plain.as_bytes()));
        assert_eq!(
            parse_cnl(&jk, &crypted).unwrap(),
            ["https://a.example/one.part1.rar", "https://b.example/d/xyz"]
        );
        // As a page may break it: across lines.
        let (head, tail) = crypted.split_at(20);
        assert_eq!(
            parse_cnl(&jk.to_uppercase(), &format!("{head}\n{tail}"))
                .unwrap()
                .len(),
            2
        );
        assert!(parse_cnl("0a0", &crypted).is_err(), "odd hexadecimal");
        assert!(parse_cnl("0a0b", &crypted).is_err(), "a short key");
        assert!(parse_cnl(&jk, "c2hvcnQ=").is_err(), "not whole blocks");
    }

    /// Builds a DLC the way JDownloader does, around a fixed file key.
    fn sample_dlc(file_key: &[u8; 16]) -> (Vec<u8>, String) {
        let e = |s: &str| STANDARD.encode(s);
        let xml = format!(
            r#"<dlc><header/><content><package name="{}"><file><url>{}</url><filename>{}</filename></file><file><url>{}</url></file><file><url>{}</url><filename>{}</filename></file></package></content></dlc>"#,
            e("My Package"),
            e("https://host.example/f/abc"),
            e("abc.part1.rar"),
            e("https://other.example/x.zip"),
            e("https://third.example/d/abc"),
            e("n.A."),
        );
        let body = STANDARD.encode(encrypt(file_key, file_key, STANDARD.encode(xml).as_bytes()));
        let dlc_key = "K".repeat(88);
        let rc = STANDARD.encode(encrypt(KEY, IV, file_key));
        (format!("{body}{dlc_key}").into_bytes(), rc)
    }

    #[tokio::test]
    async fn decrypts_dlc_through_the_key_service() {
        use axum::{Router, extract::Query, routing::get};
        use std::collections::HashMap;

        let file_key = *b"0123456789abcdef";
        let (dlc, rc) = sample_dlc(&file_key);
        let app = Router::new().route(
            "/service.php",
            get(move |Query(q): Query<HashMap<String, String>>| {
                let rc = rc.clone();
                async move {
                    assert_eq!(q.get("srcType").map(String::as_str), Some("dlc"));
                    assert_eq!(q.get("data").map(String::len), Some(88));
                    format!("<rc>{rc}</rc>")
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let links = parse_dlc(
            &reqwest::Client::new(),
            &format!("http://{addr}/service.php"),
            &dlc,
        )
        .await
        .unwrap();
        assert_eq!(
            links,
            [
                Link {
                    url: "https://host.example/f/abc".into(),
                    name: Some("abc.part1.rar".into()),
                    package: "My Package".into()
                },
                Link {
                    url: "https://other.example/x.zip".into(),
                    name: None,
                    package: "My Package".into()
                },
                // A name the container marks unknown is no name.
                Link {
                    url: "https://third.example/d/abc".into(),
                    name: None,
                    package: "My Package".into()
                },
            ]
        );
        assert!(
            parse_dlc(&reqwest::Client::new(), "http://127.0.0.1:9/", b"short")
                .await
                .is_err()
        );
    }
}
