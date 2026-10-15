//! Files in the data directory: the queue, what Haul learned about hosts,
//! per-host routes from older versions, and the lock that keeps a second
//! instance from loading and rewriting the same queue.

use std::{
    collections::BTreeMap,
    fs::{File, OpenOptions, TryLockError},
    io::{self, Write},
    path::{Path, PathBuf},
};

use indexmap::IndexMap;
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::{config::Route, engine::StartError, item::Saved};

pub(crate) const QUEUE: &str = "queue.json";
pub(crate) const ROUTES: &str = "routes.json";
pub(crate) const HOSTS: &str = "hosts.json";
/// Present while downloads are paused, so a restart keeps them paused.
const PAUSED: &str = "paused";
const LOCK: &str = "haul.lock";

pub(crate) fn load_paused(dir: &Path) -> bool {
    dir.join(PAUSED).exists()
}

pub(crate) fn save_paused(dir: &Path, paused: bool) -> io::Result<()> {
    let path = dir.join(PAUSED);
    if paused {
        std::fs::write(path, b"")
    } else {
        match std::fs::remove_file(path) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }
}

/// Holds the data directory for this process until the file is dropped.
pub(crate) fn lock(dir: &Path) -> Result<File, StartError> {
    let path = dir.join(LOCK);
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .map_err(|source| StartError::Io {
            path: path.clone(),
            source,
        })?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(TryLockError::WouldBlock) => Err(StartError::Locked(dir.to_path_buf())),
        Err(TryLockError::Error(source)) => Err(StartError::Io { path, source }),
    }
}

fn read<T: DeserializeOwned + Default>(path: PathBuf) -> Result<T, StartError> {
    match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| StartError::Corrupt {
            path,
            message: e.to_string(),
        }),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(T::default()),
        Err(source) => Err(StartError::Io { path, source }),
    }
}

pub(crate) fn load_queue(dir: &Path) -> Result<IndexMap<String, Saved>, StartError> {
    read(dir.join(QUEUE))
}

pub(crate) fn load_routes(dir: &Path) -> Result<BTreeMap<String, Route>, StartError> {
    read(dir.join(ROUTES))
}

/// What Haul learned about a host, kept across restarts.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct SavedHost {
    /// Smoothed bytes per second of one stream.
    pub speed: u64,
    /// When `speed` was last measured, in seconds since 1970.
    pub measured: u64,
    pub ok: u32,
    pub fails: u32,
    pub bytes: u64,
}

/// The hosts file is only a head start, so a damaged one is set aside.
pub(crate) fn load_hosts(dir: &Path) -> BTreeMap<String, SavedHost> {
    read(dir.join(HOSTS)).unwrap_or_else(|e| {
        tracing::warn!("starting without the saved host speeds: {e}");
        BTreeMap::new()
    })
}

/// Writes `value` as JSON to `dir/name`; see [`write_atomic`].
pub(crate) fn write(dir: &Path, name: &str, value: &impl Serialize) -> io::Result<()> {
    let bytes = serde_json::to_vec(value).map_err(io::Error::other)?;
    write_atomic(&dir.join(name), &bytes)
}

/// Replaces `path` through a temporary file, synced before the rename, so a
/// crash leaves the old file or the new one, never half of one.
pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    let mut f = File::create(&tmp)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    drop(f);
    std::fs::rename(&tmp, path)
}
