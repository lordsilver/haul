//! What one release's rows are, as sets of files (plan §4.2): one pure
//! function over what is known of each row, with no engine state and no
//! clock, that gives the same answer whatever order the rows came in.
//!
//! A release's sets are its archives in parts, by their name's archive and
//! form, and each single file. Sets of one release with different part
//! counts are splits: the same release cut differently. One of them is the
//! main split, the one whose parts are on the most preferred, switched-on
//! hosts (D-9); the others stand by, and a split every link of which is gone
//! is dead, a note on the card rather than rows.

use std::collections::{BTreeMap, BTreeSet};

use crate::parts::{Form, PartName};

/// What is known of one row of a release, for [`resolve`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Known<'a> {
    pub id: &'a str,
    /// Its file's name, when known.
    pub name: Option<&'a str>,
    /// The hosts of its links that are neither gone nor failed for good.
    pub live: Vec<&'a str>,
    /// The hosts of all its links.
    pub hosts: Vec<&'a str>,
}

/// One set: an archive in parts, or a single file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Set {
    /// The archive's name (or the file's).
    pub name: String,
    /// Its parts as its rows number them; 1 for a single file.
    pub parts: u32,
    /// Its rows, in part order.
    pub ids: Vec<String>,
    /// Every host any of its rows has a link on.
    pub hosts: BTreeSet<String>,
    /// Some part has a link not gone.
    pub alive: bool,
}

/// What a release's rows are.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Plan {
    /// Its archives in parts, by name; single files are not sets here.
    pub sets: Vec<Set>,
    /// Of the sets that are splits of one another (two or more archives
    /// with different part counts), the main one's place in `sets`.
    pub main: Option<usize>,
}

impl Plan {
    /// Whether row `id` belongs to a split that stands by: starting the
    /// release leaves it alone (D-9).
    pub fn stands_by(&self, id: &str) -> bool {
        let Some(main) = self.main else {
            return false;
        };
        self.sets
            .iter()
            .enumerate()
            .any(|(at, s)| at != main && s.ids.iter().any(|i| i == id))
    }

    /// The splits every link of which is gone.
    pub fn dead_splits(&self) -> impl Iterator<Item = &Set> {
        self.sets.iter().filter(|s| self.main.is_some() && !s.alive)
    }
}

/// The sets `rows` make, and the main split among them. `rank` gives a
/// host's place among the preferred hosts (lower first), `on` whether it is
/// switched on. The rows are sorted first, so their order can't matter.
pub(crate) fn resolve(
    rows: &[Known<'_>],
    rank: impl Fn(&str) -> Option<usize>,
    on: impl Fn(&str) -> bool,
) -> Plan {
    let mut sorted: Vec<&Known<'_>> = rows.iter().collect();
    sorted.sort_by(|a, b| (a.name, a.id).cmp(&(b.name, b.id)));
    let by_id: BTreeMap<&str, &Known<'_>> = sorted.iter().map(|r| (r.id, *r)).collect();
    let mut by_archive: BTreeMap<(String, Form), Vec<(PartName, &Known<'_>)>> = BTreeMap::new();
    for row in sorted {
        let Some(part) = row.name.and_then(PartName::parse) else {
            continue;
        };
        let (form, archive) = part.key();
        by_archive
            .entry((archive, form))
            .or_default()
            .push((part, row));
    }
    let mut sets: Vec<Set> = by_archive
        .into_values()
        .filter(|parts| parts.len() >= 2)
        .map(|mut parts| {
            parts.sort_by_key(|(p, r)| (p.number, r.id));
            Set {
                name: parts[0].0.archive.clone(),
                parts: parts.iter().map(|(p, _)| p.number).max().unwrap_or(0),
                ids: parts.iter().map(|(_, r)| r.id.to_string()).collect(),
                hosts: parts
                    .iter()
                    .flat_map(|(_, r)| r.hosts.iter().map(|h| h.to_string()))
                    .collect(),
                alive: parts.iter().any(|(_, r)| !r.live.is_empty()),
            }
        })
        .collect();
    sets.sort_by(|a, b| (a.parts, &a.name).cmp(&(b.parts, &b.name)));
    let counts: BTreeSet<u32> = sets.iter().map(|s| s.parts).collect();
    if counts.len() < 2 {
        return Plan { sets, main: None };
    }
    // The main split: the most parts a switched-on host serves, then the
    // best preferred host among its hosts, then the set's own order.
    let score = |s: &Set| {
        let served = s
            .ids
            .iter()
            .filter(|id| by_id[id.as_str()].live.iter().any(|h| on(h)))
            .count();
        let best = s
            .hosts
            .iter()
            .filter(|h| on(h))
            .filter_map(|h| rank(h))
            .min()
            .unwrap_or(usize::MAX);
        (served, std::cmp::Reverse(best))
    };
    let main = sets
        .iter()
        .enumerate()
        .filter(|(_, s)| s.alive)
        .max_by(|(i, a), (j, b)| score(a).cmp(&score(b)).then(j.cmp(i)))
        .map(|(at, _)| at);
    Plan { sets, main }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row<'a>(id: &'a str, name: &'a str, host: &'a str, alive: bool) -> Known<'a> {
        Known {
            id,
            name: Some(name),
            live: if alive { vec![host] } else { Vec::new() },
            hosts: vec![host],
        }
    }

    fn release() -> Vec<Known<'static>> {
        vec![
            row("a1", "Example.Set.part1.rar", "pixeldrain.com", true),
            row("a2", "Example.Set.part2.rar", "pixeldrain.com", true),
            row("a3", "Example.Set.part3.rar", "pixeldrain.com", true),
            row("a4", "Example.Set.part4.rar", "pixeldrain.com", true),
            row("b1", "Example.Set.part1.rar", "rapidgator.net", true),
            row("b2", "Example.Set.part2.rar", "rapidgator.net", true),
        ]
    }

    /// Two live cuts of one release: the one on the preferred host is the
    /// main split, the other stands by.
    #[test]
    fn of_two_live_splits_the_preferred_hosts_one_is_main() {
        // Both name their archive alike: the two-part one is its own set
        // only because its numbers say so; here they share a name, so the
        // rows make one archive of four with doubled parts 1 and 2.
        let rows = release();
        let plan = resolve(&rows, |_| None, |_| true);
        assert_eq!(plan.sets.len(), 1);

        let rows = vec![
            row("a1", "Example.Set.part1.rar", "pixeldrain.com", true),
            row("a2", "Example.Set.part2.rar", "pixeldrain.com", true),
            row("a3", "Example.Set.part3.rar", "pixeldrain.com", true),
            row("a4", "Example.Set.part4.rar", "pixeldrain.com", true),
            row("b1", "0123abcd.part1.rar", "rapidgator.net", true),
            row("b2", "0123abcd.part2.rar", "rapidgator.net", true),
        ];
        let rank = |h: &str| (h == "rapidgator.net").then_some(0);
        let plan = resolve(&rows, rank, |_| true);
        let main = &plan.sets[plan.main.unwrap()];
        assert_eq!(
            main.parts, 4,
            "more parts served wins over the preferred host"
        );
        assert!(plan.stands_by("b1") && !plan.stands_by("a1"));

        // With pixeldrain switched off, the other cut serves more.
        let plan = resolve(&rows, rank, |h| h != "pixeldrain.com");
        assert_eq!(plan.sets[plan.main.unwrap()].parts, 2);
    }

    /// A split whose every link is gone is dead, never the main one.
    #[test]
    fn a_dead_split_is_never_main() {
        let rows = vec![
            row("a1", "Example.Set.part1.rar", "pixeldrain.com", true),
            row("a2", "Example.Set.part2.rar", "pixeldrain.com", true),
            row("a3", "Example.Set.part3.rar", "pixeldrain.com", true),
            row("b1", "0123abcd.part1.rar", "1fichier.com", false),
            row("b2", "0123abcd.part2.rar", "1fichier.com", false),
        ];
        let plan = resolve(&rows, |_| Some(0), |_| true);
        assert_eq!(plan.sets[plan.main.unwrap()].parts, 3);
        let dead: Vec<u32> = plan.dead_splits().map(|s| s.parts).collect();
        assert_eq!(dead, [2]);
    }

    /// Any order of the rows gives the same plan.
    #[test]
    fn the_order_of_the_rows_does_not_matter() {
        let rows = vec![
            row("a1", "Example.Set.part1.rar", "pixeldrain.com", true),
            row("a2", "Example.Set.part2.rar", "pixeldrain.com", true),
            row("a3", "Example.Set.part3.rar", "ddownload.com", true),
            row("b1", "0123abcd.part1.rar", "rapidgator.net", true),
            row("b2", "0123abcd.part2.rar", "rapidgator.net", false),
            row("c", "readme.txt", "pixeldrain.com", true),
        ];
        let first = resolve(&rows, |_| None, |_| true);
        // Every rotation and its reverse.
        for turn in 0..rows.len() {
            let mut r = rows.clone();
            r.rotate_left(turn);
            assert_eq!(resolve(&r, |_| None, |_| true), first);
            r.reverse();
            assert_eq!(resolve(&r, |_| None, |_| true), first);
        }
    }
}
