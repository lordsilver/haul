//! mega.nz: a link's key decrypts the file's name and its bytes. MEGA's API
//! (`/cs`, no account) gives a file's size, its encrypted attributes and an
//! address for its encrypted bytes, which Haul decrypts as they arrive:
//! AES-128 in counter mode, so a download resumes at any offset.
//!
//! A folder link's key unwraps the key of every file in it: the folder's
//! listing (`f` with the folder's handle) gives each node's key encrypted
//! with it, block by block. A folder becomes one row per file, each with
//! MEGA's own link to that file in the folder (`…/folder/F#key/file/H`),
//! which downloads like a file link.

use std::sync::{
    LazyLock,
    atomic::{AtomicU64, Ordering},
};

use aes::{
    Aes128,
    cipher::{
        BlockCipherDecrypt, BlockCipherEncrypt, BlockModeDecrypt, KeyInit, KeyIvInit,
        block_padding::NoPadding,
    },
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
    /// A file's 32-byte key: the AES key is its halves XORed together, the
    /// counter's nonce the 8 bytes after the first half.
    fn of_file(raw: &[u8]) -> Option<Self> {
        let raw: &[u8; 32] = raw.try_into().ok()?;
        let mut key = [0; 16];
        for (i, k) in key.iter_mut().enumerate() {
            *k = raw[i] ^ raw[i + 16];
        }
        let nonce = raw[16..24].try_into().expect("8 bytes");
        Some(Self { key, nonce })
    }

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

/// What a MEGA link points at.
#[derive(Debug, PartialEq)]
enum Link {
    File {
        id: String,
        key: Key,
    },
    /// A folder, and maybe one file or subfolder in it.
    Folder {
        id: String,
        key: [u8; 16],
        file: Option<String>,
        sub: Option<String>,
    },
}

const ID: &str = "[A-Za-z0-9_-]{8}";

static FILE: LazyLock<[Regex; 2]> = LazyLock::new(|| {
    [
        Regex::new(&format!(r"/file/({ID})#([A-Za-z0-9_-]{{43}})")).unwrap(),
        Regex::new(&format!(r"#!({ID})!([A-Za-z0-9_-]{{43}})")).unwrap(),
    ]
});
static FOLDER: LazyLock<[Regex; 2]> = LazyLock::new(|| {
    [
        Regex::new(&format!(
            r"/folder/({ID})#([A-Za-z0-9_-]{{22}})(?:/(file|folder)/({ID}))?"
        ))
        .unwrap(),
        Regex::new(&format!(r"#F!({ID})!([A-Za-z0-9_-]{{22}})(?:!({ID}))?")).unwrap(),
    ]
});

/// The link's ids and keys; `None` without its key.
fn link(url: &str) -> Option<Link> {
    if let Some(c) = FOLDER.iter().find_map(|re| re.captures(url)) {
        let key: [u8; 16] = URL_SAFE_NO_PAD.decode(&c[2]).ok()?.try_into().ok()?;
        // The new form names what it opens in the folder; the old one only
        // a subfolder.
        let (file, sub) = match (c.get(3).map(|m| m.as_str()), c.get(4)) {
            (Some("file"), Some(h)) => (Some(h.as_str().to_string()), None),
            (Some(_), Some(h)) => (None, Some(h.as_str().to_string())),
            (Some(h), None) => (None, Some(h.to_string())),
            _ => (None, None),
        };
        return Some(Link::Folder {
            id: c[1].to_string(),
            key,
            file,
            sub,
        });
    }
    let c = FILE.iter().find_map(|re| re.captures(url))?;
    let key = Key::of_file(&URL_SAFE_NO_PAD.decode(&c[2]).ok()?)?;
    Some(Link::File {
        id: c[1].to_string(),
        key,
    })
}

/// The name in a node's attributes: `MEGA{"n":"name",…}`, encrypted with its
/// AES key in CBC mode with a zero IV.
fn name_from(attributes: &str, key: &[u8; 16]) -> Option<String> {
    let mut data = URL_SAFE_NO_PAD
        .decode(attributes.trim_end_matches('='))
        .ok()?;
    let plain = cbc::Decryptor::<Aes128>::new_from_slices(key, &[0; 16])
        .ok()?
        .decrypt_padded::<NoPadding>(&mut data)
        .ok()?;
    let text = std::str::from_utf8(plain).ok()?.trim_end_matches('\0');
    let json: Value = serde_json::from_str(text.strip_prefix("MEGA")?).ok()?;
    json["n"].as_str().map(str::to_string)
}

/// A node's key in a folder's listing (`owner:key`), decrypted with the
/// folder's key one block at a time.
fn node_key(k: &str, folder: &[u8; 16]) -> Option<Vec<u8>> {
    let mut raw = URL_SAFE_NO_PAD.decode(k.rsplit(':').next()?).ok()?;
    if raw.is_empty() || raw.len() % 16 != 0 {
        return None;
    }
    let aes = Aes128::new_from_slice(folder).ok()?;
    for chunk in raw.chunks_mut(16) {
        let mut block = aes::Block::try_from(&*chunk).ok()?;
        aes.decrypt_block(&mut block);
        chunk.copy_from_slice(&block);
    }
    Some(raw)
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

/// One API command; `query` adds to the address (`&n=` a folder's handle).
async fn api(cx: &Cx<'_>, query: &str, command: Value) -> Result<Value, ResolveError> {
    static SEQ: AtomicU64 = AtomicU64::new(1);
    let base = &cx.cfg.endpoints.mega;
    let answer: Value = cx
        .http
        .client
        .post(format!(
            "{base}/cs?id={}{query}",
            SEQ.fetch_add(1, Ordering::Relaxed)
        ))
        .json(&json!([command]))
        .send()
        .await?
        .json()
        .await
        .map_err(|e| ResolveError::Failed(format!("mega: {e}")))?;
    // An error is a bare number, for the request or for this one command.
    let one = answer.get(0).cloned().unwrap_or(answer);
    match one.as_i64() {
        Some(code) => Err(refusal(code)),
        None => Ok(one),
    }
}

/// A file's size, encrypted attributes and download address: a public file
/// (`p`), or one in folder `folder` (`n`).
async fn file(
    cx: &Cx<'_>,
    id: &str,
    folder: Option<&str>,
) -> Result<(u64, String, String), ResolveError> {
    let (query, which) = match folder {
        Some(f) => (format!("&n={f}"), "n"),
        None => (String::new(), "p"),
    };
    let one = api(cx, &query, json!({ "a": "g", "g": 1, "ssl": 1, which: id })).await?;
    match (one["s"].as_u64(), one["g"].as_str()) {
        (Some(size), Some(url)) => Ok((
            size,
            one["at"].as_str().unwrap_or_default().to_string(),
            url.to_string(),
        )),
        _ => Err(ResolveError::failed("mega: no download address")),
    }
}

/// A file in a folder's listing.
#[derive(Debug)]
struct Node {
    id: String,
    name: String,
    size: u64,
    key: Key,
    /// The folders it is in, nearest first.
    within: Vec<String>,
}

/// The files in folder `id`, with their names and keys.
async fn listing(
    cx: &Cx<'_>,
    id: &str,
    key: &[u8; 16],
) -> Result<(Option<String>, Vec<Node>), ResolveError> {
    let one = api(
        cx,
        &format!("&n={id}"),
        json!({ "a": "f", "c": 1, "ca": 1, "r": 1 }),
    )
    .await?;
    let nodes = one["f"].as_array().cloned().unwrap_or_default();
    let parent: std::collections::HashMap<&str, &str> = nodes
        .iter()
        .filter_map(|n| Some((n["h"].as_str()?, n["p"].as_str()?)))
        .collect();
    let mut root = None;
    let mut files = Vec::new();
    for n in &nodes {
        let (Some(h), Some(k), Some(at)) = (n["h"].as_str(), n["k"].as_str(), n["a"].as_str())
        else {
            continue;
        };
        let Some(raw) = node_key(k, key) else {
            continue;
        };
        match n["t"].as_u64() {
            Some(1) if h == id => {
                root = raw
                    .as_slice()
                    .try_into()
                    .ok()
                    .and_then(|k| name_from(at, &k));
            }
            Some(0) => {
                let Some(file_key) = Key::of_file(&raw) else {
                    continue;
                };
                let Some(name) = name_from(at, &file_key.key) else {
                    continue;
                };
                let mut within = Vec::new();
                let mut at_node = h;
                while let Some(&up) = parent.get(at_node) {
                    within.push(up.to_string());
                    if up == id || within.len() > 64 {
                        break;
                    }
                    at_node = up;
                }
                files.push(Node {
                    id: h.to_string(),
                    name,
                    size: n["s"].as_u64().unwrap_or(0),
                    key: file_key,
                    within,
                });
            }
            _ => {}
        }
    }
    Ok((root, files))
}

/// MEGA's link to file `file` in folder `id`.
fn in_folder(id: &str, key: &[u8; 16], file: &str) -> String {
    format!(
        "https://mega.nz/folder/{id}#{}/file/{file}",
        URL_SAFE_NO_PAD.encode(key)
    )
}

pub(super) async fn resolve(cx: &Cx<'_>, url: &str) -> Result<Vec<Entry>, ResolveError> {
    match link(url) {
        Some(Link::File { id, key }) => {
            let (size, at, address) = file(cx, &id, None).await?;
            Ok(vec![Entry {
                url: address,
                name: name_from(&at, &key.key),
                size: Some(size),
                decrypt: Some(key),
                ..Entry::default()
            }])
        }
        Some(Link::Folder {
            id,
            key,
            file: Some(wanted),
            ..
        }) => {
            let (_, files) = listing(cx, &id, &key).await?;
            let node = files
                .into_iter()
                .find(|f| f.id == wanted)
                .ok_or_else(|| ResolveError::failed("mega: the file is no longer in the folder"))?;
            let (size, _, address) = file(cx, &node.id, Some(&id)).await?;
            Ok(vec![Entry {
                url: address,
                name: Some(node.name),
                size: Some(size),
                decrypt: Some(node.key),
                ..Entry::default()
            }])
        }
        Some(Link::Folder { id, key, sub, .. }) => {
            let (root, files) = listing(cx, &id, &key).await?;
            let within = sub.unwrap_or_else(|| id.clone());
            Ok(files
                .into_iter()
                .filter(|f| f.within.contains(&within))
                .map(|f| Entry {
                    url: in_folder(&id, &key, &f.id),
                    name: Some(f.name),
                    package: root.clone(),
                    size: Some(f.size),
                    deferred: true,
                    ..Entry::default()
                })
                .collect())
        }
        None => Err(ResolveError::failed(
            "mega: the link has no key (the part after #)",
        )),
    }
}

pub(super) async fn probe(cx: &Cx<'_>, url: &str) -> Option<Probed> {
    let (name, size) = match link(url)? {
        Link::File { id, key } => {
            let (size, at, _) = file(cx, &id, None).await.ok()?;
            (name_from(&at, &key.key)?, size)
        }
        Link::Folder {
            id,
            key,
            file: Some(wanted),
            ..
        } => {
            let (_, files) = listing(cx, &id, &key).await.ok()?;
            let f = files.into_iter().find(|f| f.id == wanted)?;
            (f.name, f.size)
        }
        // A folder is many files: its rows are made when it resolves.
        Link::Folder { .. } => return None,
    };
    Some(Probed {
        name,
        size: Some(size),
        checksum: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const LINK: &str = "https://mega.nz/file/LrBUURwJ#yQvKtW0qGYmJGFoW_4g8NSiAS9IJ4ms2ROfv7FRpk54";

    #[test]
    fn reads_file_and_folder_links_in_every_form() {
        let Some(Link::File { id, key }) = link(LINK) else {
            panic!("a file link");
        };
        assert_eq!(id, "LrBUURwJ");
        assert_eq!(
            link("https://mega.nz/#!LrBUURwJ!yQvKtW0qGYmJGFoW_4g8NSiAS9IJ4ms2ROfv7FRpk54"),
            Some(Link::File { id, key })
        );
        let folder = |file: Option<&str>, sub: Option<&str>| Link::Folder {
            id: "abcdefgh".into(),
            key: *b"0123456789abcdef",
            file: file.map(str::to_string),
            sub: sub.map(str::to_string),
        };
        let key = "MDEyMzQ1Njc4OWFiY2RlZg";
        assert_eq!(
            link(&format!("https://mega.nz/folder/abcdefgh#{key}")),
            Some(folder(None, None))
        );
        assert_eq!(
            link(&format!(
                "https://mega.nz/folder/abcdefgh#{key}/file/FILE1234"
            )),
            Some(folder(Some("FILE1234"), None))
        );
        assert_eq!(
            link(&format!(
                "https://mega.nz/folder/abcdefgh#{key}/folder/SUBF1234"
            )),
            Some(folder(None, Some("SUBF1234")))
        );
        assert_eq!(
            link(&format!("https://mega.nz/#F!abcdefgh!{key}")),
            Some(folder(None, None))
        );
        assert_eq!(link("https://mega.nz/file/LrBUURwJ"), None);
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
