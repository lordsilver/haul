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
    extract::{self, Failure, Part, Progress},
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

/// The release a row came from, when it says: its package, or the name of
/// the wrapper it was unwrapped from.
fn release(items: &IndexMap<String, Item>, it: &Item) -> Option<String> {
    (!it.package.is_empty())
        .then(|| it.package.clone())
        .or_else(|| items.get(it.source.as_ref()?)?.title.clone())
}

/// Pairs of archives that are the same files on other hosts, uploaded under
/// other names: as many parts, none missing, every part the same size as its
/// counterpart to the byte, and not from two releases that say otherwise.
/// The first of a pair is the one higher in the queue, which keeps its rows.
/// A copy that has started downloading is left as it is.
fn copies(items: &IndexMap<String, Item>) -> Vec<(Vec<String>, Vec<String>)> {
    let sets = archives(items);
    let sizes = |set: &ArchiveSet| -> Option<Vec<u64>> {
        set.ids
            .iter()
            .map(|id| items.get(id).map(|it| it.size).filter(|s| *s > 0))
            .collect()
    };
    let untouched = |set: &ArchiveSet| {
        set.ids.iter().all(|id| {
            items
                .get(id)
                .is_some_and(|it| it.done == 0 && !it.running() && it.status != Status::Done)
        })
    };
    let mut pairs = Vec::new();
    let mut taken = std::collections::HashSet::new();
    for (i, keep) in sets.iter().enumerate() {
        if taken.contains(&i) || !keep.missing.is_empty() {
            continue;
        }
        let Some(want) = sizes(keep) else { continue };
        let keep_release = release(items, &items[&keep.ids[0]]);
        for (j, copy) in sets.iter().enumerate().skip(i + 1) {
            if taken.contains(&j) || !copy.missing.is_empty() || copy.ids.len() != keep.ids.len() {
                continue;
            }
            let copy_release = release(items, &items[&copy.ids[0]]);
            if keep_release.is_some() && copy_release.is_some() && keep_release != copy_release {
                continue;
            }
            if sizes(copy).as_ref() == Some(&want) && untouched(copy) {
                taken.insert(j);
                pairs.push((keep.ids.clone(), copy.ids.clone()));
            }
        }
    }
    pairs
}

impl Engine {
    /// Folds archives that are copies of one another on other hosts (see
    /// [`copies`]) into one: each part of the copy becomes more links of the
    /// same part, so the release downloads once, from whichever host serves
    /// each part, the others standing by.
    pub(super) fn merge_copies(&self, st: &mut State) {
        for (keep, copy) in copies(&st.items) {
            for (k, c) in keep.iter().zip(&copy) {
                let Some(gone) = st.items.shift_remove(c) else {
                    continue;
                };
                tracing::info!(item = %k, "{} is a copy of it on another host", gone.key_name().unwrap_or_default());
                let Some(it) = st.items.get_mut(k) else {
                    continue;
                };
                for m in gone.mirrors {
                    if !it.mirrors.contains(&m) {
                        it.mirrors.push(m);
                    }
                }
                it.manual_urls.extend(gone.manual_urls);
                it.merge_wrappers(gone.source.as_ref(), &gone.also_from);
                if it.checksum.is_none() {
                    it.checksum = gone.checksum;
                }
                let _ = std::fs::remove_file(self.part_path(c));
            }
        }
    }

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
    /// A `password` given is kept with the archive and tried first.
    /// `Ok(false)` when there is no such row.
    pub fn extract(&self, id: &str, password: Option<&str>) -> Result<bool, String> {
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
        if let Some(password) = password.filter(|p| !p.is_empty()) {
            for id in &ids {
                if let Some(it) = st.items.get_mut(id) {
                    it.extract_password = Some(password.to_string());
                }
            }
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
        let mut own = None;
        let parts: Result<Vec<Part>, Failure> = {
            let mut st = self.lock();
            let mut parts = Vec::new();
            let mut missing = None;
            for id in &ids {
                let Some(it) = st.items.get_mut(id) else {
                    missing.get_or_insert_with(|| "a part was removed from the list".to_string());
                    continue;
                };
                it.extract = Extract::Extracting;
                own = own.or_else(|| it.extract_password.clone());
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
                Some(why) => Err(Failure::Other(why)),
                None => Ok(parts),
            }
        };
        // The archive's own password first, then the list of the settings.
        let mut passwords: Vec<String> = own.into_iter().collect();
        for p in &self.cfg().archive_passwords {
            if !passwords.contains(p) {
                passwords.push(p.clone());
            }
        }
        tracing::info!("unpacking {archive}");
        let progress = Arc::new(Progress::default());
        *self.0.unpacking.lock().unwrap_or_else(|e| e.into_inner()) = Some(progress.clone());
        let result = match parts {
            Ok(parts) => {
                let progress = progress.clone();
                let files = parts.iter().map(|p| p.path.clone()).collect::<Vec<_>>();
                tokio::task::spawn_blocking(move || extract::extract(&parts, &passwords, &progress))
                    .await
                    .unwrap_or_else(|e| Err(Failure::Other(e.to_string())))
                    .map(|done| (done, files))
            }
            Err(why) => Err(why),
        };
        *self.0.unpacking.lock().unwrap_or_else(|e| e.into_inner()) = None;

        let notice = match &result {
            Ok((done, _)) => {
                tracing::info!("unpacked {archive}: {}", done.placed.len());
                Event::Notice {
                    title: "Extracted".into(),
                    body: archive.clone(),
                }
            }
            Err(e @ Failure::Password { .. }) => {
                tracing::info!("unpacking {archive}: {e}");
                Event::Notice {
                    title: "Archive needs a password".into(),
                    body: format!("{archive}: {e}"),
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
                        Ok((done, _)) => {
                            it.extract = Extract::Done;
                            // The one that fit, to open it again.
                            if done.password.is_some() {
                                it.extract_password.clone_from(&done.password);
                            }
                        }
                        Err(e @ Failure::Password { .. }) => {
                            it.extract = Extract::Password;
                            it.extract_error = e.to_string();
                        }
                        Err(e) => {
                            it.extract = Extract::Failed;
                            it.extract_error = e.to_string();
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
