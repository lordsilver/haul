//! Files in the data directory: the queue, per-host routes, and the lock that
//! keeps a second instance from loading and rewriting the same queue.

use std::{
    collections::BTreeMap,
    fs::{File, OpenOptions, TryLockError},
    io::{self, Write},
    path::{Path, PathBuf},
};

use indexmap::IndexMap;
use serde::{Serialize, de::DeserializeOwned};

use crate::{config::Route, engine::StartError, item::Saved};

pub(crate) const QUEUE: &str = "queue.json";
pub(crate) const ROUTES: &str = "routes.json";
const LOCK: &str = "haul.lock";

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

/// Writes through a temporary file and a rename, so a crash never leaves a
/// half-written file behind.
pub(crate) fn write(dir: &Path, name: &str, value: &impl Serialize) -> io::Result<()> {
    let bytes = serde_json::to_vec(value).map_err(io::Error::other)?;
    let path = dir.join(name);
    let tmp = dir.join(format!("{name}.tmp"));
    let mut f = File::create(&tmp)?;
    f.write_all(&bytes)?;
    f.sync_all()?;
    drop(f);
    std::fs::rename(&tmp, &path)
}
