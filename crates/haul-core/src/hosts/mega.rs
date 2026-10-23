//! mega.nz: a file link's key decrypts the file's name and its bytes. MEGA's
//! API (`/cs`, no account) gives a file's size, its encrypted attributes and
//! an address for its encrypted bytes, which Haul decrypts as they arrive:
//! AES-128 in counter mode, so a download resumes at any offset. Folder links
//! still go to the external `megatools` program.

use std::{
    path::PathBuf,
    sync::{
        LazyLock,
        atomic::{AtomicU64, Ordering},
    },
};

use aes::{
    Aes128,
    cipher::{BlockCipherEncrypt, BlockModeDecrypt, KeyInit, KeyIvInit, block_padding::NoPadding},
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{Cx, Entry, Probed, ResolveError};

/// The key a file's bytes are decrypted with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Key {
    key: [u8; 16],
    nonce: [u8; 8],
}

impl Key {
    /// Decrypts `data` in place, its first byte being the file's byte at
    /// `offset`.
    pub fn apply(&self, data: &mut [u8], offset: u64) {
        let aes = Aes128::new_from_slice(&self.key).expect("a 16-byte key");
        let mut counter = offset / 16;
        let mut skip = (offset % 16) as usize;
        let mut at = 0;
        while at < data.len() {
            let mut block = aes::Block::default();
            block[..8].copy_from_slice(&self.nonce);
            block[8..].copy_from_slice(&counter.to_be_bytes());
            aes.encrypt_block(&mut block);
            for b in &block[skip..] {
                let Some(byte) = data.get_mut(at) else { break };
                *byte ^= b;
                at += 1;
            }
            skip = 0;
            counter += 1;
        }
    }
}

static FILE: LazyLock<[Regex; 2]> = LazyLock::new(|| {
    [
        Regex::new(r"/file/([A-Za-z0-9_-]{8})#([A-Za-z0-9_-]{43})").unwrap(),
        Regex::new(r"#!([A-Za-z0-9_-]{8})!([A-Za-z0-9_-]{43})").unwrap(),
    ]
});

/// A file link's id and its key, split into the AES key and the counter's
/// nonce. `None` for a folder link or one without its key.
fn file_link(url: &str) -> Option<(String, Key)> {
    let c = FILE.iter().find_map(|re| re.captures(url))?;
    let raw = URL_SAFE_NO_PAD.decode(&c[2]).ok()?;
    let raw: [u8; 32] = raw.try_into().ok()?;
    let mut key = [0; 16];
    for (i, k) in key.iter_mut().enumerate() {
        *k = raw[i] ^ raw[i + 16];
    }
    let nonce = raw[16..24].try_into().expect("8 bytes");
    Some((c[1].to_string(), Key { key, nonce }))
}

/// The name in a file's attributes: `MEGA{"n":"name",…}`, encrypted with its
/// key in CBC mode with a zero IV.
fn name_from(attributes: &str, key: &Key) -> Option<String> {
    let mut data = URL_SAFE_NO_PAD
        .decode(attributes.trim_end_matches('='))
        .ok()?;
    let plain = cbc::Decryptor::<Aes128>::new_from_slices(&key.key, &[0; 16])
        .ok()?
        .decrypt_padded::<NoPadding>(&mut data)
        .ok()?;
    let text = std::str::from_utf8(plain).ok()?.trim_end_matches('\0');
    let json: Value = serde_json::from_str(text.strip_prefix("MEGA")?).ok()?;
    json["n"].as_str().map(str::to_string)
}

/// What MEGA's error numbers mean for a download.
fn refusal(code: i64) -> ResolveError {
    match code {
        -9 => ResolveError::failed("mega: the file was deleted"),
        -16 => ResolveError::failed("mega: the file was taken down"),
        -11 => ResolveError::failed("mega: access to the file is denied"),
        -3 | -4 | -6 | -18 => ResolveError::Wait {
            seconds: 60,
            message: format!("mega: busy for now (error {code})"),
        },
        _ => ResolveError::Failed(format!("mega: error {code}")),
    }
}

/// The file's size, encrypted attributes and download address.
async fn file(cx: &Cx<'_>, id: &str) -> Result<(u64, String, String), ResolveError> {
    static SEQ: AtomicU64 = AtomicU64::new(1);
    let base = &cx.cfg.endpoints.mega;
    let answer: Value = cx
        .http
        .client
        .post(format!(
            "{base}/cs?id={}",
            SEQ.fetch_add(1, Ordering::Relaxed)
        ))
        .json(&json!([{ "a": "g", "g": 1, "ssl": 1, "p": id }]))
        .send()
        .await?
        .json()
        .await
        .map_err(|e| ResolveError::Failed(format!("mega: {e}")))?;
    // An error is a bare number, for the request or for this one command.
    let one = answer.get(0).unwrap_or(&answer);
    if let Some(code) = one.as_i64() {
        return Err(refusal(code));
    }
    match (one["s"].as_u64(), one["at"].as_str(), one["g"].as_str()) {
        (Some(size), Some(at), Some(url)) => Ok((size, at.to_string(), url.to_string())),
        _ => Err(ResolveError::failed("mega: no download address")),
    }
}

pub(super) async fn resolve(cx: &Cx<'_>, url: &str) -> Result<Vec<Entry>, ResolveError> {
    let Some((id, key)) = file_link(url) else {
        return with_megatools(cx, url);
    };
    let (size, at, address) = file(cx, &id).await?;
    Ok(vec![Entry {
        url: address,
        name: name_from(&at, &key),
        size: Some(size),
        decrypt: Some(key),
        ..Entry::default()
    }])
}

pub(super) async fn probe(cx: &Cx<'_>, url: &str) -> Option<Probed> {
    let (id, key) = file_link(url)?;
    let (size, at, _) = file(cx, &id).await.ok()?;
    Some(Probed {
        name: name_from(&at, &key)?,
        size: Some(size),
        checksum: None,
    })
}

/// A folder link, for the external `megatools` program to download.
fn with_megatools(cx: &Cx<'_>, url: &str) -> Result<Vec<Entry>, ResolveError> {
    let exe = cx
        .cfg
        .megatools
        .clone()
        .or_else(|| which("megatools"))
        .or_else(|| which("megadl"));
    let Some(exe) = exe else {
        return Err(ResolveError::failed(
            "mega.nz folders need megatools on PATH (https://megatools.megous.com); file links work without it",
        ));
    };
    // The old standalone `megadl` takes no subcommand.
    let standalone = exe
        .file_stem()
        .is_some_and(|s| s.eq_ignore_ascii_case("megadl"));
    let mut exec = vec![exe.to_string_lossy().into_owned()];
    if !standalone {
        exec.push("dl".into());
    }
    Ok(vec![Entry {
        url: url.into(),
        exec: Some(exec),
        ..Entry::default()
    }])
}

fn which(name: &str) -> Option<PathBuf> {
    let exts: Vec<String> = if cfg!(windows) {
        std::env::var("PATHEXT")
            .unwrap_or_else(|_| ".EXE;.CMD;.BAT".into())
            .split(';')
            .map(str::to_string)
            .collect()
    } else {
        vec![String::new()]
    };
    std::env::split_paths(&std::env::var_os("PATH")?)
        .flat_map(|dir| exts.iter().map(move |ext| dir.join(format!("{name}{ext}"))))
        .find(|p| p.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    const LINK: &str = "https://mega.nz/file/LrBUURwJ#yQvKtW0qGYmJGFoW_4g8NSiAS9IJ4ms2ROfv7FRpk54";

    #[test]
    fn reads_file_links_in_both_forms() {
        let (id, key) = file_link(LINK).unwrap();
        assert_eq!(id, "LrBUURwJ");
        let old =
            file_link("https://mega.nz/#!LrBUURwJ!yQvKtW0qGYmJGFoW_4g8NSiAS9IJ4ms2ROfv7FRpk54")
                .unwrap();
        assert_eq!(old, (id, key));
        assert!(file_link("https://mega.nz/folder/abcdefgh#0123456789abcdef012345").is_none());
        assert!(file_link("https://mega.nz/file/LrBUURwJ").is_none());
    }

    #[test]
    fn decrypts_from_any_offset() {
        let key = Key {
            key: *b"0123456789abcdef",
            nonce: *b"noncenon",
        };
        let plain: Vec<u8> = (0..1000u32).map(|i| (i * 7) as u8).collect();
        let mut whole = plain.clone();
        key.apply(&mut whole, 0);
        assert_ne!(whole, plain);
        // Decrypted in pieces that start mid-block, as a resumed download is.
        let mut pieces = whole.clone();
        for (start, end) in [(0, 5), (5, 37), (37, 600), (600, 1000)] {
            key.apply(&mut pieces[start..end], start as u64);
        }
        assert_eq!(pieces, plain);
    }
}
