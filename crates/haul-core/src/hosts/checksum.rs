//! Checksums a host publishes, and reading one from a file or headers.

use super::*;

/// A hash algorithm that hosts publish checksums in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Algo {
    Md5,
    Sha256,
    Sha512,
}

impl Algo {
    /// Length of the digest in bytes.
    fn len(self) -> usize {
        match self {
            Algo::Md5 => 16,
            Algo::Sha256 => 32,
            Algo::Sha512 => 64,
        }
    }
}

/// A file's checksum as its host publishes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Checksum {
    pub algo: Algo,
    /// Lowercase hexadecimal.
    pub hex: String,
}

impl Checksum {
    /// From hexadecimal, as APIs send it; `None` unless it has the right length.
    pub fn from_hex(algo: Algo, hex: &str) -> Option<Self> {
        let hex = hex.trim().to_ascii_lowercase();
        (hex.len() == algo.len() * 2 && hex.bytes().all(|b| b.is_ascii_hexdigit()))
            .then_some(Self { algo, hex })
    }

    /// From base64, as HTTP headers send it.
    pub fn from_base64(algo: Algo, value: &str) -> Option<Self> {
        let bytes = STANDARD.decode(value.trim()).ok()?;
        (bytes.len() == algo.len()).then(|| Self {
            algo,
            hex: to_hex(&bytes),
        })
    }

    /// Whether the file at `path` has this checksum, reading all of it, and
    /// asking `steer` before every read whether to go
    /// on, wait (while downloads are paused, say) or give up, which fails
    /// with [`std::io::ErrorKind::Interrupted`].
    pub fn matches_steered(&self, path: &Path, steer: &dyn Fn() -> Steer) -> std::io::Result<bool> {
        let file = std::fs::File::open(path)?;
        let hex = match self.algo {
            Algo::Md5 => hash_reader::<md5::Md5>(file, steer)?,
            Algo::Sha256 => hash_reader::<sha2::Sha256>(file, steer)?,
            Algo::Sha512 => hash_reader::<sha2::Sha512>(file, steer)?,
        };
        Ok(hex == self.hex)
    }
}

/// A checksum worked out as the bytes come, for a download read from its
/// start.
pub enum Hashing {
    Md5(md5::Md5),
    Sha256(sha2::Sha256),
    Sha512(sha2::Sha512),
}

impl Hashing {
    pub fn new(algo: Algo) -> Self {
        match algo {
            Algo::Md5 => Self::Md5(Digest::new()),
            Algo::Sha256 => Self::Sha256(Digest::new()),
            Algo::Sha512 => Self::Sha512(Digest::new()),
        }
    }

    pub fn update(&mut self, bytes: &[u8]) {
        match self {
            Self::Md5(h) => h.update(bytes),
            Self::Sha256(h) => h.update(bytes),
            Self::Sha512(h) => h.update(bytes),
        }
    }

    /// The checksum, as lowercase hexadecimal.
    pub fn finish(self) -> String {
        match self {
            Self::Md5(h) => to_hex(&h.finalize()),
            Self::Sha256(h) => to_hex(&h.finalize()),
            Self::Sha512(h) => to_hex(&h.finalize()),
        }
    }
}

/// What a long checksum read does next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Steer {
    Go,
    Wait,
    Stop,
}

pub(super) fn hash_reader<D: Digest>(
    mut reader: impl Read,
    steer: &dyn Fn() -> Steer,
) -> std::io::Result<String> {
    let mut hasher = D::new();
    let mut buf = vec![0; 1 << 20];
    loop {
        loop {
            match steer() {
                Steer::Go => break,
                Steer::Wait => std::thread::sleep(Duration::from_millis(250)),
                Steer::Stop => return Err(std::io::ErrorKind::Interrupted.into()),
            }
        }
        match reader.read(&mut buf)? {
            0 => return Ok(to_hex(&hasher.finalize())),
            n => hasher.update(&buf[..n]),
        }
    }
}

pub(crate) fn to_hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    bytes
        .iter()
        .flat_map(|b| [DIGITS[usize::from(b >> 4)], DIGITS[usize::from(b & 0xf)]])
        .map(char::from)
        .collect()
}

/// The checksum a server publishes for the whole file in its headers, by
/// preference: `Repr-Digest` (RFC 9530), `Digest` (RFC 3230), Google Cloud
/// Storage's `x-goog-hash`, then `Content-MD5`. That last one covers only the
/// body of this response, so it counts only when `whole_body` says the body is
/// the whole file (a 200, not a 206).
pub(crate) fn header_checksum(headers: &HeaderMap, whole_body: bool) -> Option<Checksum> {
    // `name: key=value, key=value` across every line of the header, keys in
    // lowercase; values keep their `=` padding.
    let pairs = |name: &str| -> Vec<(String, String)> {
        headers
            .get_all(name)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .flat_map(|v| v.split(','))
            .filter_map(|p| p.trim().split_once('='))
            .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
            .collect()
    };
    let find = |pairs: &[(String, String)], key: &str, algo: Algo| {
        pairs
            .iter()
            .filter(|(k, _)| k == key)
            .find_map(|(_, v)| Checksum::from_base64(algo, v.trim_matches(':')))
    };
    let repr = pairs("repr-digest");
    let digest = pairs("digest");
    let goog = pairs("x-goog-hash");
    find(&repr, "sha-512", Algo::Sha512)
        .or_else(|| find(&repr, "sha-256", Algo::Sha256))
        .or_else(|| find(&digest, "sha-512", Algo::Sha512))
        .or_else(|| find(&digest, "sha-256", Algo::Sha256))
        .or_else(|| find(&digest, "md5", Algo::Md5))
        .or_else(|| find(&goog, "md5", Algo::Md5))
        .or_else(|| {
            let value = headers.get("content-md5")?.to_str().ok()?;
            whole_body
                .then(|| Checksum::from_base64(Algo::Md5, value))
                .flatten()
        })
}
