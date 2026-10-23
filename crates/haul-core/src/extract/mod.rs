//! Unpacking an archive that came in parts, once every part is downloaded:
//! RAR volumes, 7-Zip and zip archives, a zip split by zip itself into
//! `.zNN` pieces, any of them cut into numbered pieces by a file splitter
//! (`name.7z.001`), and pieces of a file that is no archive at all
//! (`movie.mkv.001`), joined into it. What an archive is comes from its
//! first bytes; which parts make it, from their names (see
//! [`crate::parts`]).
//!
//! Everything is unpacked into a hidden folder next to the first part and
//! only then moved beside it, each top-level entry under a name nothing has
//! yet (`Release (1)`), so a failed or interrupted run leaves nothing half
//! written among the user's files and never overwrites one.

use std::{
    fs::File,
    io::{self, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use crate::{
    parts::{Form, PartName},
    paths,
};

mod rar;
mod spanned;

/// The hidden folder an archive is unpacked in, next to its first part.
const STAGING: &str = ".haul-extract-";

/// One downloaded part: where it is, and the name it has on its host, which
/// RAR volumes are found by even when the file on disk got another
/// (`name.part2 (1).rar`) or went to another host's folder.
#[derive(Debug, Clone)]
pub struct Part {
    pub path: PathBuf,
    pub name: String,
}

/// How far an extraction is, in bytes of unpacked files.
#[derive(Debug, Default)]
pub struct Progress {
    pub done: AtomicU64,
    pub total: AtomicU64,
}

impl Progress {
    /// Between 0 and 1; 0 until the size is known.
    pub fn fraction(&self) -> f32 {
        let total = self.total.load(Ordering::Relaxed);
        if total == 0 {
            return 0.0;
        }
        (self.done.load(Ordering::Relaxed) as f64 / total as f64).min(1.0) as f32
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Rar,
    SevenZip,
    Zip,
    /// A zip split by zip itself, which starts with a mark of its own.
    SpannedZip,
}

/// Unpacks the archive whose parts are `parts`, in part order, beside the
/// first one. Returns what it put there: the top-level files and folders.
pub fn extract(parts: &[Part], progress: &Progress) -> Result<Vec<PathBuf>, String> {
    let first = parts.first().ok_or("no parts")?;
    let dest = first
        .path
        .parent()
        .ok_or("the first part has no folder")?
        .to_path_buf();
    let name = PartName::parse(&first.name);
    let kind = match kind_of(&first.path)? {
        Some(kind) => kind,
        // Pieces of a file that is no archive: the file, joined again.
        None if name.as_ref().is_some_and(|n| n.form == Form::Pieces) => {
            let name = name.expect("checked above");
            return join(parts, &dest, &name.archive, progress).map(|file| vec![file]);
        }
        None => return Err("not a RAR, 7-Zip or zip archive".into()),
    };
    // Left behind by a run Haul did not get to finish.
    clear_staging(&dest);
    let staging = Staging(dest.join(format!(
        "{STAGING}{}",
        &uuid::Uuid::new_v4().simple().to_string()[..10]
    )));
    let out = staging.0.join("out");
    std::fs::create_dir_all(&out).map_err(|e| format!("{}: {e}", out.display()))?;
    match kind {
        Kind::Rar => rar::rar(parts, &staging.0, &out, progress)?,
        Kind::SevenZip => seven_zip(Joined::open(parts)?, &out, progress)?,
        Kind::Zip => zip(Joined::open(parts)?, &out, progress)?,
        Kind::SpannedZip => zip(spanned::unspan(parts)?, &out, progress)?,
    }
    place(&out, &dest)
}

/// Takes away hidden extraction folders a run that did not finish left in
/// `dir`. Only one extraction runs at a time, so none of them is in use.
fn clear_staging(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        if e.file_name().to_string_lossy().starts_with(STAGING) {
            let _ = std::fs::remove_dir_all(e.path());
        }
    }
}

/// The hidden folder, taken away however the extraction ends.
struct Staging(PathBuf);

impl Drop for Staging {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// What the first part's first bytes say it is; `None` for no archive.
fn kind_of(path: &Path) -> Result<Option<Kind>, String> {
    let mut head = [0u8; 8];
    let n = File::open(path)
        .and_then(|mut f| f.read(&mut head))
        .map_err(|e| format!("{}: {e}", path.display()))?;
    let head = &head[..n];
    Ok(if head.starts_with(b"Rar!\x1a\x07") {
        Some(Kind::Rar)
    } else if head.starts_with(&[0x37, 0x7a, 0xbc, 0xaf, 0x27, 0x1c]) {
        Some(Kind::SevenZip)
    } else if head.starts_with(b"PK\x07\x08") {
        Some(Kind::SpannedZip)
    } else if head.starts_with(b"PK\x03\x04") || head.starts_with(b"PK\x05\x06") {
        Some(Kind::Zip)
    } else {
        None
    })
}

/// Writes the pieces one after the other into `name` beside them, under a
/// name nothing has yet, by way of a hidden file so a failure leaves none.
fn join(parts: &[Part], dest: &Path, name: &str, progress: &Progress) -> Result<PathBuf, String> {
    let mut from = Joined::open(parts)?;
    progress.total.store(from.len(), Ordering::Relaxed);
    clear_staging(dest);
    let staging = Staging(dest.join(format!(
        "{STAGING}{}",
        &uuid::Uuid::new_v4().simple().to_string()[..10]
    )));
    let mut to = File::create(&staging.0).map_err(|e| format!("{}: {e}", staging.0.display()))?;
    io::copy(&mut Counted(&mut from, &progress.done), &mut to)
        .map_err(|e| format!("joining the pieces: {e}"))?;
    drop(to);
    let target = paths::unused_name(dest, &paths::safe(name));
    std::fs::rename(&staging.0, &target).map_err(|e| format!("{}: {e}", target.display()))?;
    Ok(target)
}

/// `name` from an archive as a path inside the folder it unpacks to, or
/// `None` when it would lead out of it (`..`, a root or a drive).
fn enclosed(name: &str) -> Option<PathBuf> {
    use std::path::Component;

    let mut rel = PathBuf::new();
    for c in Path::new(&name.replace('\\', "/")).components() {
        match c {
            Component::Normal(part) => rel.push(part),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    (!rel.as_os_str().is_empty()).then_some(rel)
}

fn seven_zip(mut joined: Joined, out: &Path, progress: &Progress) -> Result<(), String> {
    let seven_error = |e: sevenz_rust2::Error| match e {
        sevenz_rust2::Error::PasswordRequired | sevenz_rust2::Error::MaybeBadPassword(_) => {
            "the archive needs a password".to_string()
        }
        e => e.to_string(),
    };
    let archive = sevenz_rust2::Archive::read(&mut joined, &sevenz_rust2::Password::empty())
        .map_err(seven_error)?;
    let total = archive.files.iter().map(|f| f.size()).sum();
    progress.total.store(total, Ordering::Relaxed);
    joined.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
    sevenz_rust2::decompress_with_extract_fn(joined, out, |entry, reader, dest| {
        let mut counted = Counted(reader, &progress.done);
        sevenz_rust2::default_entry_extract_fn(entry, &mut counted, dest)
    })
    .map_err(seven_error)
}

fn zip(joined: Joined, out: &Path, progress: &Progress) -> Result<(), String> {
    let mut archive = zip::ZipArchive::new(joined).map_err(|e| e.to_string())?;
    let total = (0..archive.len())
        .filter_map(|i| archive.by_index_raw(i).ok().map(|f| f.size()))
        .sum();
    progress.total.store(total, Ordering::Relaxed);
    for i in 0..archive.len() {
        if archive.by_index_raw(i).is_ok_and(|f| f.encrypted()) {
            return Err("the archive needs a password".into());
        }
        let mut file = archive.by_index(i).map_err(|e| e.to_string())?;
        // A name that would lead out of the folder is left out.
        let Some(rel) = file.enclosed_name() else {
            tracing::warn!(
                "left out of the zip: {}",
                String::from_utf8_lossy(file.name_raw())
            );
            continue;
        };
        let target = out.join(&rel);
        if file.is_dir() {
            std::fs::create_dir_all(&target).map_err(|e| format!("{}: {e}", target.display()))?;
            continue;
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
        let mut to = File::create(&target).map_err(|e| format!("{}: {e}", target.display()))?;
        io::copy(&mut Counted(&mut file, &progress.done), &mut to)
            .map_err(|e| format!("{}: {e}", rel.display()))?;
    }
    Ok(())
}

/// Moves what was unpacked from the hidden folder beside the parts.
fn place(out: &Path, dest: &Path) -> Result<Vec<PathBuf>, String> {
    let mut placed = Vec::new();
    for e in std::fs::read_dir(out).map_err(|e| e.to_string())? {
        let e = e.map_err(|e| e.to_string())?;
        let target = paths::unused_name(dest, &e.file_name().to_string_lossy());
        std::fs::rename(e.path(), &target).map_err(|e| format!("{}: {e}", target.display()))?;
        placed.push(target);
    }
    placed.sort();
    Ok(placed)
}

/// Counts the bytes read through it.
struct Counted<'a, R: ?Sized>(&'a mut R, &'a AtomicU64);

impl<R: Read + ?Sized> Read for Counted<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.0.read(buf)?;
        self.1.fetch_add(n as u64, Ordering::Relaxed);
        Ok(n)
    }
}

enum Piece {
    File(File),
    Bytes(Vec<u8>),
}

/// The parts read one after the other as one file, wherever each is: what
/// pieces a file splitter cut are, and a whole archive is one piece of.
/// Bytes of Haul's own can follow them (see [`spanned`]).
struct Joined {
    pieces: Vec<(Piece, u64)>,
    len: u64,
    pos: u64,
}

impl Joined {
    fn open(parts: &[Part]) -> Result<Self, String> {
        let mut pieces = Vec::with_capacity(parts.len());
        for p in parts {
            let f = File::open(&p.path).map_err(|e| format!("{}: {e}", p.path.display()))?;
            let len = f.metadata().map_err(|e| e.to_string())?.len();
            pieces.push((Piece::File(f), len));
        }
        let len = pieces.iter().map(|(_, l)| l).sum();
        Ok(Self {
            pieces,
            len,
            pos: 0,
        })
    }

    fn len(&self) -> u64 {
        self.len
    }

    /// Where piece `n` (a split zip's disk) starts in the whole.
    fn disk_start(&self, n: u64) -> Option<u64> {
        let n = usize::try_from(n).ok()?;
        (n < self.pieces.len()).then(|| self.pieces[..n].iter().map(|(_, l)| l).sum())
    }

    /// Adds `bytes` after everything else.
    fn append(&mut self, bytes: Vec<u8>) {
        let len = bytes.len() as u64;
        self.pieces.push((Piece::Bytes(bytes), len));
        self.len += len;
    }
}

impl Read for Joined {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let mut start = 0;
        for (piece, len) in &mut self.pieces {
            if self.pos < start + *len {
                let within = self.pos - start;
                let room = (*len - within).min(buf.len() as u64) as usize;
                let n = match piece {
                    Piece::File(f) => {
                        f.seek(SeekFrom::Start(within))?;
                        f.read(&mut buf[..room])?
                    }
                    Piece::Bytes(b) => {
                        let within = within as usize;
                        buf[..room].copy_from_slice(&b[within..within + room]);
                        room
                    }
                };
                self.pos += n as u64;
                return Ok(n);
            }
            start += *len;
        }
        Ok(0)
    }
}

impl Seek for Joined {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        let pos = match to {
            SeekFrom::Start(n) => Some(n),
            SeekFrom::End(d) => self.len.checked_add_signed(d),
            SeekFrom::Current(d) => self.pos.checked_add_signed(d),
        };
        self.pos =
            pos.ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "before the start"))?;
        Ok(self.pos)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/archives")
            .join(name)
    }

    /// Copies fixtures into `dir`, each under the name given.
    fn parts_in(dir: &Path, names: &[(&str, &str)]) -> Vec<Part> {
        names
            .iter()
            .map(|(from, to)| {
                let path = dir.join(to);
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::copy(fixture(from), &path).unwrap();
                Part {
                    path,
                    name: from.to_string(),
                }
            })
            .collect()
    }

    /// The release every fixture holds: `Release/movie.bin` (60000 bytes)
    /// and `Release/sub/readme.txt`.
    fn assert_release(dir: &Path) {
        assert_eq!(
            std::fs::metadata(dir.join("movie.bin")).unwrap().len(),
            60000
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("sub/readme.txt")).unwrap(),
            "hello from haul\n"
        );
    }

    fn names_in(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn unpacks_rar_volumes_where_they_are() {
        let dir = tempfile::tempdir().unwrap();
        let names = ["R5.part1.rar", "R5.part2.rar", "R5.part3.rar"];
        let parts = parts_in(dir.path(), &names.map(|n| (n, n)));
        let progress = Progress::default();
        let placed = extract(&parts, &progress).unwrap();
        assert_eq!(placed, [dir.path().join("Release")]);
        assert_release(&dir.path().join("Release"));
        assert_eq!(progress.fraction(), 1.0);
        let mut left = names.map(String::from).to_vec();
        left.push("Release".into());
        left.sort();
        assert_eq!(names_in(dir.path()), left, "no hidden folder stays");
    }

    #[test]
    fn rar_volumes_apart_or_renamed_are_gathered() {
        let dir = tempfile::tempdir().unwrap();
        let parts = parts_in(
            dir.path(),
            &[
                ("R5.part1.rar", "a/R5.part1.rar"),
                ("R5.part2.rar", "b/R5.part2 (1).rar"),
                ("R5.part3.rar", "a/R5.part3.rar"),
            ],
        );
        // Something by the same name is there already, and stays.
        std::fs::create_dir_all(dir.path().join("a/Release")).unwrap();
        let placed = extract(&parts, &Progress::default()).unwrap();
        assert_eq!(placed, [dir.path().join("a/Release (1)")]);
        assert_release(&dir.path().join("a/Release (1)"));
        assert_eq!(
            names_in(&dir.path().join("a/Release")),
            Vec::<String>::new()
        );
    }

    #[test]
    fn unpacks_pieces_of_a_7z_and_a_zip() {
        for set in [
            ["S.7z.001", "S.7z.002", "S.7z.003"],
            ["Z.zip.001", "Z.zip.002", "Z.zip.003"],
        ] {
            let dir = tempfile::tempdir().unwrap();
            // Wherever the pieces went.
            let parts = parts_in(
                dir.path(),
                &[
                    (set[0], set[0]),
                    (set[1], &format!("x/{}", set[1])),
                    (set[2], set[2]),
                ],
            );
            let progress = Progress::default();
            extract(&parts, &progress).unwrap();
            assert_release(&dir.path().join("Release"));
            assert_eq!(progress.fraction(), 1.0, "{set:?}");
        }
    }

    /// `zip -s 64k` of `Release/sub/readme.txt`, `Release/movie.bin` (70000
    /// bytes counting 0 to 250 over and over) and `Release/zlast.txt`, which
    /// begins in the second piece; `Z64` is the same made with `-fz`, so
    /// its directory has Zip64 fields.
    #[test]
    fn unpacks_a_zip_split_into_z01_pieces() {
        for name in ["ZS", "Z64"] {
            let dir = tempfile::tempdir().unwrap();
            let (z01, zip) = (format!("{name}.z01"), format!("{name}.zip"));
            // In different folders, as two hosts' would be.
            let parts = parts_in(
                dir.path(),
                &[(&z01, &format!("a/{z01}")), (&zip, &format!("b/{zip}"))],
            );
            let progress = Progress::default();
            extract(&parts, &progress).unwrap();
            let release = dir.path().join("a/Release");
            let movie = std::fs::read(release.join("movie.bin")).unwrap();
            assert_eq!(movie.len(), 70000, "{name}");
            assert!(movie.iter().enumerate().all(|(i, &b)| b == (i % 251) as u8));
            assert_eq!(
                std::fs::read_to_string(release.join("sub/readme.txt")).unwrap(),
                "hello from haul\n"
            );
            assert_eq!(
                std::fs::read_to_string(release.join("zlast.txt")).unwrap(),
                "after the split\n"
            );
            assert_eq!(progress.fraction(), 1.0, "{name}");
        }
    }

    #[test]
    fn joins_pieces_of_a_file_that_is_no_archive() {
        let dir = tempfile::tempdir().unwrap();
        let whole: Vec<u8> = (0..30000u32).map(|i| (i * 7 % 256) as u8).collect();
        let parts: Vec<Part> = whole
            .chunks(12000)
            .enumerate()
            .map(|(i, chunk)| {
                let name = format!("movie.mkv.{:03}", i + 1);
                let path = dir.path().join(&name);
                std::fs::write(&path, chunk).unwrap();
                Part { path, name }
            })
            .collect();
        std::fs::write(dir.path().join("movie.mkv"), "another").unwrap();
        let progress = Progress::default();
        let placed = extract(&parts, &progress).unwrap();
        assert_eq!(placed, [dir.path().join("movie (1).mkv")]);
        assert_eq!(std::fs::read(&placed[0]).unwrap(), whole);
        assert_eq!(progress.fraction(), 1.0);
        assert!(!names_in(dir.path()).iter().any(|n| n.starts_with(STAGING)));
    }

    #[test]
    fn says_why_it_cannot() {
        let dir = tempfile::tempdir().unwrap();
        let parts = parts_in(dir.path(), &[("E.rar", "E.rar")]);
        assert_eq!(
            extract(&parts, &Progress::default()).unwrap_err(),
            "the archive needs a password"
        );

        let parts = parts_in(
            dir.path(),
            &[
                ("R5.part1.rar", "R5.part1.rar"),
                ("R5.part2.rar", "R5.part2.rar"),
            ],
        );
        assert_eq!(
            extract(&parts, &Progress::default()).unwrap_err(),
            "a part is missing"
        );

        // A split zip without its last piece has no directory to read.
        let parts = parts_in(dir.path(), &[("ZS.z01", "ZS.z01")]);
        assert!(extract(&parts, &Progress::default()).is_err());

        let junk = dir.path().join("J.part1.rar");
        std::fs::write(&junk, b"not an archive").unwrap();
        let err = extract(
            &[Part {
                path: junk,
                name: "J.part1.rar".into(),
            }],
            &Progress::default(),
        );
        assert_eq!(err.unwrap_err(), "not a RAR, 7-Zip or zip archive");
        assert!(
            !names_in(dir.path()).iter().any(|n| n.starts_with(STAGING)),
            "no hidden folder stays"
        );
    }
}
