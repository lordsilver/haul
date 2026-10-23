//! Unpacking an archive in parts (see [`crate::extract`]) once its last
//! part is done: on its own with `auto_extract` on, or when the user asks.
//! The parts of one archive are the queue's files whose names make them
//! parts of the same archive in the same package, as the list groups them.
//!
//! One archive is unpacked at a time, since each reads and writes as much
//! as it holds. Every part carries where its archive stands, so the state
//! survives a restart, and an archive Haul stopped in the middle of is
//! unpacked again from the start.

use std::{path::PathBuf, sync::Arc};

use indexmap::IndexMap;

use super::{BIN, Engine, Event, State, recycle};
use crate::{
    extract::{self, Part, Progress},
    item::{Extract, Item, Status},
    parts::{self as names, PartName},
};

/// The parts of one archive in the queue: its files whose names make them
/// parts of the same archive (see [`crate::parts`]), in the same package.
pub(super) struct ArchiveSet {
    /// What tells it from every other, for the views to keep its row by.
    pub key: String,
    pub name: String,
    /// Its rows, in part order.
    pub ids: Vec<String>,
    /// The parts its names show are missing, by name.
    pub missing: Vec<String>,
}

/// Every archive in parts in the queue, in the order of its first row.
pub(super) fn archives(items: &IndexMap<String, Item>) -> Vec<ArchiveSet> {
    let mut sets: IndexMap<(String, names::Form, String), Vec<(PartName, String)>> =
        IndexMap::new();
    for it in items.values().filter(|it| !it.wrapper) {
        let Some(part) = it.key_name().and_then(|n| PartName::parse(&n)) else {
            continue;
        };
        let (form, archive) = part.key();
        sets.entry((it.package.clone(), form, archive))
            .or_default()
            .push((part, it.id.clone()));
    }
    sets.into_iter()
        .filter_map(|((package, form, archive), mut parts)| {
            parts.sort_by_key(|(p, _)| p.number);
            // A part listed twice is the same file twice.
            parts.dedup_by_key(|(p, _)| p.number);
            if parts.len() < 2 {
                return None;
            }
            let found: Vec<PartName> = parts.iter().map(|(p, _)| p.clone()).collect();
            Some(ArchiveSet {
                key: format!("{package}\u{0}{form:?}\u{0}{archive}"),
                name: found[0].archive.clone(),
                missing: names::missing(&found),
                ids: parts.into_iter().map(|(_, id)| id).collect(),
            })
        })
        .collect()
}

impl Engine {
    /// The archive `id` is a part of, when the queue has two parts of it
    /// or more.
    fn archive_of(st: &State, id: &str) -> Option<ArchiveSet> {
        archives(&st.items)
            .into_iter()
            .find(|a| a.ids.iter().any(|i| i == id))
    }

    /// After row `id` is done: unpacks its archive when `auto_extract` is
    /// on, every part is done and none is missing from the list, and the
    /// archive is not unpacked or under way already. An archive all of
    /// whose parts were in the download folder before is left as it is:
    /// Haul downloaded none of it.
    pub(super) fn extract_when_complete(&self, st: &mut State, id: &str) {
        if !self.cfg().auto_extract {
            return;
        }
        let Some(set) = Self::archive_of(st, id).filter(|s| s.missing.is_empty()) else {
            return;
        };
        let parts: Vec<_> = set.ids.iter().filter_map(|i| st.items.get(i)).collect();
        if parts
            .iter()
            .all(|p| p.status == Status::Done && p.extract == Extract::No)
            && parts.iter().any(|p| !p.existing)
        {
            self.queue_extract(st, set.name, set.ids);
        }
    }

    /// Unpacks the archive `id` is a part of, because the user asked: again
    /// after it failed or was unpacked, and whatever `auto_extract` says.
    /// `Ok(false)` when there is no such row.
    pub fn extract(&self, id: &str) -> Result<bool, String> {
        let mut st = self.lock();
        if !st.items.contains_key(id) {
            return Ok(false);
        }
        let set = Self::archive_of(&st, id).ok_or("not a part of an archive in parts")?;
        if let Some(first) = set.missing.first() {
            return Err(match set.missing.len() {
                1 => format!("{first} is missing from the list"),
                n => format!("{first} and {} more are missing from the list", n - 1),
            });
        }
        let (archive, ids) = (set.name, set.ids);
        let parts: Vec<_> = ids.iter().filter_map(|i| st.items.get(i)).collect();
        if parts
            .iter()
            .any(|p| matches!(p.extract, Extract::Waiting | Extract::Extracting))
        {
            return Ok(true);
        }
        if let Some(p) = parts.iter().find(|p| p.status != Status::Done) {
            return Err(format!("{} is not downloaded yet", super::display_name(p)));
        }
        self.queue_extract(&mut st, archive, ids);
        Ok(true)
    }

    fn queue_extract(&self, st: &mut State, archive: String, ids: Vec<String>) {
        for id in &ids {
            if let Some(it) = st.items.get_mut(id) {
                it.extract = Extract::Waiting;
                it.extract_error.clear();
            }
        }
        self.touch(st);
        tokio::spawn(self.clone().unpack(archive, ids));
    }

    /// Takes up the archives the last run left waiting or under way.
    pub(super) fn resume_extractions(&self) {
        let mut st = self.lock();
        let mut seen = std::collections::HashSet::new();
        let waiting: Vec<String> = st
            .items
            .values()
            .filter(|it| it.extract == Extract::Waiting)
            .map(|it| it.id.clone())
            .collect();
        for id in waiting {
            if seen.contains(&id) {
                continue;
            }
            if let Some(set) = Self::archive_of(&st, &id) {
                seen.extend(set.ids.iter().cloned());
                self.queue_extract(&mut st, set.name, set.ids);
            }
        }
    }

    async fn unpack(self, archive: String, ids: Vec<String>) {
        let Ok(_turn) = self.0.extractor.acquire().await else {
            return;
        };
        let parts: Result<Vec<Part>, String> = {
            let mut st = self.lock();
            let mut parts = Vec::new();
            let mut missing = None;
            for id in &ids {
                let Some(it) = st.items.get_mut(id) else {
                    missing.get_or_insert_with(|| "a part was removed from the list".to_string());
                    continue;
                };
                it.extract = Extract::Extracting;
                match (&it.path, it.key_name()) {
                    (Some(path), Some(name)) if it.status == Status::Done => parts.push(Part {
                        path: PathBuf::from(path),
                        name,
                    }),
                    _ => {
                        missing.get_or_insert_with(|| {
                            format!("{} is not downloaded", super::display_name(it))
                        });
                    }
                }
            }
            self.touch(&mut st);
            match missing {
                Some(why) => Err(why),
                None => Ok(parts),
            }
        };
        tracing::info!("unpacking {archive}");
        let progress = Arc::new(Progress::default());
        *self.0.unpacking.lock().unwrap_or_else(|e| e.into_inner()) = Some(progress.clone());
        let result = match parts {
            Ok(parts) => {
                let progress = progress.clone();
                let files = parts.iter().map(|p| p.path.clone()).collect::<Vec<_>>();
                tokio::task::spawn_blocking(move || extract::extract(&parts, &progress))
                    .await
                    .unwrap_or_else(|e| Err(e.to_string()))
                    .map(|placed| (placed, files))
            }
            Err(why) => Err(why),
        };
        *self.0.unpacking.lock().unwrap_or_else(|e| e.into_inner()) = None;

        let notice = match &result {
            Ok((placed, _)) => {
                tracing::info!("unpacked {archive}: {}", placed.len());
                Event::Notice {
                    title: "Extracted".into(),
                    body: archive.clone(),
                }
            }
            Err(e) => {
                tracing::warn!("unpacking {archive}: {e}");
                Event::Notice {
                    title: "Extraction failed".into(),
                    body: format!("{archive}: {e}"),
                }
            }
        };
        {
            let mut st = self.lock();
            for id in &ids {
                if let Some(it) = st.items.get_mut(id) {
                    match &result {
                        Ok(_) => it.extract = Extract::Done,
                        Err(e) => {
                            it.extract = Extract::Failed;
                            it.extract_error = e.clone();
                        }
                    }
                }
            }
            self.touch(&mut st);
        }
        let _ = self.0.events.send(notice);

        if let Ok((_, files)) = result
            && self.cfg().delete_after_extract
        {
            self.recycle_parts(&ids, files).await;
        }
    }

    /// Moves an unpacked archive's parts to the [`BIN`]. A row whose part
    /// went keeps no path, since its file is not there any more.
    async fn recycle_parts(&self, ids: &[String], files: Vec<PathBuf>) {
        let bin = self.0.recycle_bin.clone();
        let results = tokio::task::spawn_blocking(move || {
            files
                .into_iter()
                .map(|f| {
                    let r = recycle(&f, bin.as_deref());
                    (f, r)
                })
                .collect::<Vec<_>>()
        })
        .await
        .unwrap_or_default();
        let mut st = self.lock();
        for (file, r) in results {
            match r {
                Ok(_) => {
                    tracing::info!("moved {} to the {BIN}", file.display());
                    let path = file.display().to_string();
                    for id in ids {
                        if let Some(it) = st.items.get_mut(id)
                            && it.path.as_deref() == Some(path.as_str())
                        {
                            it.path = None;
                        }
                    }
                }
                Err(e) => tracing::warn!("could not move {} to the {BIN}: {e}", file.display()),
            }
        }
        self.touch(&mut st);
    }

    /// How far the archive being unpacked is, between 0 and 1.
    pub(super) fn unpack_progress(&self) -> Option<f32> {
        self.0
            .unpacking
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|p| p.fraction())
    }
}
