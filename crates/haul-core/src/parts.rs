//! Which archive in parts a file belongs to, read from its name: the forms
//! archives name their parts in, as JDownloader's extraction recognises
//! them (its `ArchiveType`), and what a set of parts lacks.
//!
//! | Form          | Parts                                              |
//! | ------------- | -------------------------------------------------- |
//! | `RarPart`     | `name.part1.rar`, `name.part01.rar`, `name.p1.rar`  |
//! | `RarNumbered` | `name.000.rar` or `name.001.rar`, then on           |
//! | `RarOld`      | `name.rar`, `name.r00` … `name.r99`, `name.s00` … `name.y99` |
//! | `SevenZip`    | `name.7z.001`, `name.7z.002` …                      |
//! | `ZipNumbered` | `name.zip.001`, `name.zip.002` …                    |
//! | `ZipSplit`    | `name.z01`, `name.z02` … and `name.zip` last         |
//! | `Pieces`      | `name.001`, `name.002` … of any file                |
//!
//! Only a set of two parts or more is an archive in parts; `name.rar` or
//! `name.zip` alone is a file like any other. Old-style RAR volumes would
//! reach `name.z00` after 2600 parts, so `name.zNN` is read as a zip's piece:
//! what an archive really is comes from its bytes when it is unpacked.

use std::{collections::BTreeSet, sync::LazyLock};

use regex::Regex;
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Form {
    RarPart,
    RarNumbered,
    RarOld,
    SevenZip,
    ZipNumbered,
    ZipSplit,
    Pieces,
}

impl Form {
    /// Pieces a file splitter cut, which joined together are the file they
    /// were (an archive or anything else), rather than volumes an archiver
    /// wrote.
    pub fn is_pieces(self) -> bool {
        matches!(self, Form::SevenZip | Form::ZipNumbered | Form::Pieces)
    }
}

/// What a file's name says about the archive it is a part of.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartName {
    /// The archive's name: `name.rar`, `name.7z`, `name.zip`, or for
    /// pieces of a file the file's own (`video.mkv`).
    pub archive: String,
    pub form: Form,
    /// Its place among the parts. `ZipSplit`'s `name.zip` comes last.
    pub number: u32,
    /// How the name spells the parts it numbers, to name a missing one:
    /// everything before the number, its digits, and everything after.
    stem: String,
    width: usize,
    tail: String,
}

/// The number `ZipSplit`'s `name.zip` has: after every `name.zNN`.
const LAST: u32 = u32::MAX;

struct Pattern {
    re: Regex,
    read: fn(&regex::Captures) -> Option<PartName>,
}

fn part(
    archive: String,
    form: Form,
    number: u32,
    stem: &str,
    width: usize,
    tail: &str,
) -> Option<PartName> {
    Some(PartName {
        archive,
        form,
        number,
        stem: stem.to_string(),
        width,
        tail: tail.to_string(),
    })
}

/// In the order they are tried: a name the earlier ones take is not read by
/// the later ones (`name.part1.rar` is not `name.rar`'s first volume).
static PATTERNS: LazyLock<Vec<Pattern>> = LazyLock::new(|| {
    let p = |re: &str, read: fn(&regex::Captures) -> Option<PartName>| Pattern {
        re: Regex::new(re).expect("valid pattern"),
        read,
    };
    vec![
        p(
            r"(?i)^(.+)\.(part|p)(\.?)(\d{1,3})((?:\..*?)?\.rar)$",
            |c| {
                part(
                    format!("{}.rar", &c[1]),
                    Form::RarPart,
                    c[4].parse().ok()?,
                    &format!("{}.{}{}", &c[1], &c[2], &c[3]),
                    c[4].len(),
                    &c[5],
                )
            },
        ),
        p(r"(?i)^(.+)\.(\d{3})(\.rar)$", |c| {
            part(
                format!("{}.rar", &c[1]),
                Form::RarNumbered,
                c[2].parse().ok()?,
                &format!("{}.", &c[1]),
                3,
                &c[3],
            )
        }),
        p(r"(?i)^(.+)\.([r-y])(\d{2})$", |c| {
            let letter = c[2].to_ascii_lowercase().as_bytes()[0];
            let number = u32::from(letter - b'r') * 100 + c[3].parse::<u32>().ok()? + 1;
            part(format!("{}.rar", &c[1]), Form::RarOld, number, &c[1], 2, "")
        }),
        p(r"(?i)^(.+)(\.rar)$", |c| {
            part(format!("{}.rar", &c[1]), Form::RarOld, 0, &c[1], 2, "")
        }),
        p(r"(?i)^(.+\.7z)\.(\d{1,3})$", |c| {
            part(
                c[1].to_string(),
                Form::SevenZip,
                c[2].parse().ok()?,
                &format!("{}.", &c[1]),
                c[2].len(),
                "",
            )
        }),
        p(r"(?i)^(.+\.zip)\.(\d{1,3})$", |c| {
            part(
                c[1].to_string(),
                Form::ZipNumbered,
                c[2].parse().ok()?,
                &format!("{}.", &c[1]),
                c[2].len(),
                "",
            )
        }),
        p(r"(?i)^(.+)\.z(\d{1,3})$", |c| {
            part(
                format!("{}.zip", &c[1]),
                Form::ZipSplit,
                c[2].parse().ok()?,
                &format!("{}.z", &c[1]),
                c[2].len(),
                "",
            )
        }),
        p(r"(?i)^(.+)\.zip$", |c| {
            part(
                format!("{}.zip", &c[1]),
                Form::ZipSplit,
                LAST,
                &format!("{}.z", &c[1]),
                2,
                "",
            )
        }),
        p(r"^(.+)\.(\d{3})$", |c| {
            part(
                c[1].to_string(),
                Form::Pieces,
                c[2].parse().ok()?,
                &format!("{}.", &c[1]),
                3,
                "",
            )
        }),
    ]
});

impl PartName {
    /// What `name` says, or `None` when it names no part of anything.
    pub fn parse(name: &str) -> Option<Self> {
        PATTERNS
            .iter()
            .find_map(|p| p.re.captures(name).and_then(|c| (p.read)(&c)))
    }

    /// What tells this archive's parts from every other's: its form, and
    /// its name the way hosts that change letter case still agree on.
    pub fn key(&self) -> (Form, String) {
        (self.form, self.archive.to_lowercase())
    }

    /// The name part `number` of this archive has.
    fn name_of(&self, number: u32) -> String {
        match (self.form, number) {
            (Form::RarOld, 0) => self.archive.clone(),
            (Form::RarOld, n) => {
                let n = n - 1;
                let letter = char::from(b'r' + (n / 100) as u8);
                format!("{}.{letter}{:02}", self.stem, n % 100)
            }
            (Form::ZipSplit, LAST) => self.archive.clone(),
            (_, n) => format!("{}{n:0w$}{}", self.stem, self.tail, w = self.width),
        }
    }
}

/// Part `name` of an archive under the archive name `base` instead of its
/// own: `a.part01.rar` with `b` is `b.part01.rar`. `None` when `name`
/// names no part.
pub fn rebased(name: &str, base: &str) -> Option<String> {
    let part = PartName::parse(name)?;
    let own = part
        .archive
        .rsplit_once('.')
        .map_or(part.archive.as_str(), |(stem, _)| stem);
    let head = name.get(..own.len())?;
    let rest = name.get(own.len()..)?;
    head.eq_ignore_ascii_case(own)
        .then(|| format!("{base}{rest}"))
}

/// The names of the parts missing from a set whose names are `parts` (one
/// archive's, as [`PartName::key`] gathers them): every number below the
/// highest one there, from the first a form starts at; `name.zip` of a zip
/// split into `.zNN` pieces, which ends it; and `name.rar` of old-style
/// volumes, which begins them. Parts after the highest are not known to
/// be missing from the names alone.
pub fn missing(parts: &[PartName]) -> Vec<String> {
    let Some(any) = parts.first() else {
        return Vec::new();
    };
    let numbers: BTreeSet<u32> = parts.iter().map(|p| p.number).collect();
    let first = match any.form {
        Form::RarOld => 0,
        // `name.000.rar` or `name.001.rar`: either may begin them.
        Form::RarNumbered => numbers.first().copied().unwrap_or(0).min(1),
        _ => 1,
    };
    let highest = numbers.range(..LAST).next_back().copied().unwrap_or(first);
    let mut out: Vec<String> = (first..=highest)
        .filter(|n| !numbers.contains(n))
        .map(|n| any.name_of(n))
        .collect();
    if any.form == Form::ZipSplit && !numbers.contains(&LAST) {
        out.push(any.name_of(LAST));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_part_takes_another_archive_name() {
        assert_eq!(
            rebased("a1b2c3.part01.rar", "Example.Set").as_deref(),
            Some("Example.Set.part01.rar")
        );
        assert_eq!(rebased("X.7z.002", "Y").as_deref(), Some("Y.7z.002"));
        assert_eq!(rebased("notes.txt", "Y"), None);
    }

    fn read(name: &str) -> Option<(String, Form, u32)> {
        PartName::parse(name).map(|p| (p.archive, p.form, p.number))
    }

    #[test]
    fn names_say_which_archive_and_part() {
        let cases = [
            ("R.part1.rar", Some(("R.rar", Form::RarPart, 1))),
            ("R.part01.rar", Some(("R.rar", Form::RarPart, 1))),
            ("R.PART12.RAR", Some(("R.rar", Form::RarPart, 12))),
            ("R.p3.rar", Some(("R.rar", Form::RarPart, 3))),
            ("R.part.2.rar", Some(("R.rar", Form::RarPart, 2))),
            ("R.000.rar", Some(("R.rar", Form::RarNumbered, 0))),
            ("R.rar", Some(("R.rar", Form::RarOld, 0))),
            ("R.r00", Some(("R.rar", Form::RarOld, 1))),
            ("R.r99", Some(("R.rar", Form::RarOld, 100))),
            ("R.s00", Some(("R.rar", Form::RarOld, 101))),
            ("R.7z.003", Some(("R.7z", Form::SevenZip, 3))),
            ("R.zip.002", Some(("R.zip", Form::ZipNumbered, 2))),
            ("R.z01", Some(("R.zip", Form::ZipSplit, 1))),
            ("R.z101", Some(("R.zip", Form::ZipSplit, 101))),
            ("R.zip", Some(("R.zip", Form::ZipSplit, LAST))),
            ("video.mkv.002", Some(("video.mkv", Form::Pieces, 2))),
            ("R.mkv", None),
            ("R.7z", None),
        ];
        for (name, want) in cases {
            let got = read(name);
            assert_eq!(
                got.as_ref().map(|(a, f, n)| (a.as_str(), *f, *n)),
                want,
                "{name}"
            );
        }
    }

    #[test]
    fn says_which_parts_are_missing() {
        let set = |names: &[&str]| -> Vec<PartName> {
            names.iter().map(|n| PartName::parse(n).unwrap()).collect()
        };
        assert_eq!(
            missing(&set(&["R.part01.rar", "R.part02.rar", "R.part04.rar"])),
            ["R.part03.rar"]
        );
        assert!(missing(&set(&["R.part1.rar", "R.part2.rar"])).is_empty());
        assert_eq!(
            missing(&set(&["R.part2.rar", "R.part3.rar"])),
            ["R.part1.rar"]
        );
        assert_eq!(missing(&set(&["R.r00", "R.r02"])), ["R.rar", "R.r01"]);
        assert_eq!(
            missing(&set(&["R.rar", "R.r98", "R.s00"]))[..2],
            ["R.r00", "R.r01"]
        );
        assert!(missing(&set(&["R.rar", "R.r99", "R.s00"])).len() == 99);
        assert_eq!(missing(&set(&["Z.z01", "Z.z03"])), ["Z.z02", "Z.zip"]);
        assert!(missing(&set(&["Z.z01", "Z.zip"])).is_empty());
        assert!(missing(&set(&["R.000.rar", "R.001.rar"])).is_empty());
        assert!(missing(&set(&["R.001.rar", "R.002.rar"])).is_empty());
        assert_eq!(missing(&set(&["S.7z.001", "S.7z.003"])), ["S.7z.002"]);
        assert_eq!(missing(&set(&["m.mkv.002", "m.mkv.003"])), ["m.mkv.001"]);
    }
}
