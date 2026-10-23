//! A zip split by zip itself into pieces (`name.z01`, `name.z02` … and
//! `name.zip` last), read as the one zip they make. The pieces hold the
//! archive's bytes in order, but the central directory gives each file's
//! place as a piece (a "disk") and an offset inside it, which no single-file
//! reader takes. So the pieces are read one after the other, as they are,
//! and followed by a copy of the central directory whose places are offsets
//! into all of them together, with a record of its end to match: what
//! `zip -s 0` writes, without writing the files again.
//!
//! The local headers in front of each file name no piece, so they stay as
//! they are. Zip64 fields are read where the directory has them, and
//! written where the new offsets need them.

use std::io::{Read, Seek, SeekFrom};

use super::{Joined, Part};

const CENTRAL: &[u8; 4] = b"PK\x01\x02";
const END: &[u8; 4] = b"PK\x05\x06";
const END64: &[u8; 4] = b"PK\x06\x06";
const LOCATOR64: &[u8; 4] = b"PK\x06\x07";
/// The extra field that holds a file's Zip64 sizes, offset and disk.
const ZIP64_EXTRA: u16 = 0x0001;
const FULL16: u16 = 0xffff;
const FULL32: u32 = 0xffff_ffff;

fn u16_at(b: &[u8], at: usize) -> Result<u16, String> {
    b.get(at..at + 2)
        .map(|s| u16::from_le_bytes([s[0], s[1]]))
        .ok_or_else(damaged)
}

fn u32_at(b: &[u8], at: usize) -> Result<u32, String> {
    b.get(at..at + 4)
        .map(|s| u32::from_le_bytes(s.try_into().expect("four bytes")))
        .ok_or_else(damaged)
}

fn u64_at(b: &[u8], at: usize) -> Result<u64, String> {
    b.get(at..at + 8)
        .map(|s| u64::from_le_bytes(s.try_into().expect("eight bytes")))
        .ok_or_else(damaged)
}

fn damaged() -> String {
    "the split zip's directory is damaged".into()
}

fn read_at(j: &mut Joined, at: u64, len: u64) -> Result<Vec<u8>, String> {
    let mut buf = vec![0; usize::try_from(len).map_err(|_| damaged())?];
    j.seek(SeekFrom::Start(at)).map_err(|e| e.to_string())?;
    j.read_exact(&mut buf).map_err(|_| damaged())?;
    Ok(buf)
}

/// Where the central directory is, as the end of the last piece says.
struct Directory {
    disk: u64,
    offset: u64,
    size: u64,
    entries: u64,
}

fn directory(j: &mut Joined, len: u64) -> Result<Directory, String> {
    // The end record is the last thing in the zip, before a comment of at
    // most 65535 bytes.
    let tail = len.min(22 + 65535);
    let from = len - tail;
    let bytes = read_at(j, from, tail)?;
    let at = bytes
        .windows(4)
        .rposition(|w| w == END)
        .ok_or("no zip directory at the end of the last piece")?;
    let end = &bytes[at..];
    let mut dir = Directory {
        disk: u64::from(u16_at(end, 6)?),
        entries: u64::from(u16_at(end, 10)?),
        size: u64::from(u32_at(end, 12)?),
        offset: u64::from(u32_at(end, 16)?),
    };
    // A Zip64 end record, which the locator just before this one points at.
    let end_at = from + at as u64;
    if end_at >= 20 {
        let locator = read_at(j, end_at - 20, 20)?;
        if &locator[..4] == LOCATOR64 {
            let disk = u64::from(u32_at(&locator, 4)?);
            let offset = u64_at(&locator, 8)?;
            let record_at = j.disk_start(disk).ok_or_else(damaged)? + offset;
            let record = read_at(j, record_at, 56)?;
            if &record[..4] != END64 {
                return Err(damaged());
            }
            dir = Directory {
                disk: u64::from(u32_at(&record, 20)?),
                entries: u64_at(&record, 32)?,
                size: u64_at(&record, 40)?,
                offset: u64_at(&record, 48)?,
            };
        }
    }
    Ok(dir)
}

/// The directory entry at the start of `b` with its file's place made an
/// offset into the pieces together, and how many bytes it took in `b`.
fn rewrite(b: &[u8], j: &Joined) -> Result<(Vec<u8>, usize), String> {
    if b.get(..4) != Some(CENTRAL) {
        return Err(damaged());
    }
    let name_len = usize::from(u16_at(b, 28)?);
    let extra_len = usize::from(u16_at(b, 30)?);
    let comment_len = usize::from(u16_at(b, 32)?);
    let total = 46 + name_len + extra_len + comment_len;
    if b.len() < total {
        return Err(damaged());
    }
    let compressed = u32_at(b, 20)?;
    let size = u32_at(b, 24)?;
    let disk16 = u16_at(b, 34)?;
    let offset32 = u32_at(b, 42)?;

    // The extra fields, with Zip64's taken apart: its values come in this
    // order, each only when the field it stands for is full.
    let extra = &b[46 + name_len..46 + name_len + extra_len];
    let mut kept = Vec::new();
    let (mut size64, mut compressed64, mut offset, mut disk) = (
        u64::from(size),
        u64::from(compressed),
        u64::from(offset32),
        u64::from(disk16),
    );
    let mut at = 0;
    while at + 4 <= extra.len() {
        let id = u16_at(extra, at)?;
        let len = usize::from(u16_at(extra, at + 2)?);
        let data = extra.get(at + 4..at + 4 + len).ok_or_else(damaged)?;
        if id == ZIP64_EXTRA {
            let mut k = 0;
            let mut next = || -> Result<u64, String> {
                let v = u64_at(data, k)?;
                k += 8;
                Ok(v)
            };
            if size == FULL32 {
                size64 = next()?;
            }
            if compressed == FULL32 {
                compressed64 = next()?;
            }
            if offset32 == FULL32 {
                offset = next()?;
            }
            if disk16 == FULL16 {
                disk = u64::from(u32_at(data, k)?);
            }
        } else {
            kept.extend_from_slice(&extra[at..at + 4 + len]);
        }
        at += 4 + len;
    }

    let absolute = j.disk_start(disk).ok_or_else(damaged)? + offset;
    let mut zip64 = Vec::new();
    if size == FULL32 {
        zip64.extend_from_slice(&size64.to_le_bytes());
    }
    if compressed == FULL32 {
        zip64.extend_from_slice(&compressed64.to_le_bytes());
    }
    let offset_field = if absolute >= u64::from(FULL32) {
        zip64.extend_from_slice(&absolute.to_le_bytes());
        FULL32
    } else {
        absolute as u32
    };
    if !zip64.is_empty() {
        kept.extend_from_slice(&ZIP64_EXTRA.to_le_bytes());
        kept.extend_from_slice(&(zip64.len() as u16).to_le_bytes());
        kept.extend_from_slice(&zip64);
    }

    let mut out = Vec::with_capacity(46 + name_len + kept.len() + comment_len);
    out.extend_from_slice(&b[..30]);
    out.extend_from_slice(
        &u16::try_from(kept.len())
            .map_err(|_| damaged())?
            .to_le_bytes(),
    );
    out.extend_from_slice(&b[32..34]);
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&b[36..42]);
    out.extend_from_slice(&offset_field.to_le_bytes());
    out.extend_from_slice(&b[46..46 + name_len]);
    out.extend_from_slice(&kept);
    out.extend_from_slice(&b[46 + name_len + extra_len..total]);
    Ok((out, total))
}

/// The pieces of a split zip, in order, read as one ordinary zip.
pub(super) fn unspan(parts: &[Part]) -> Result<Joined, String> {
    let mut j = Joined::open(parts)?;
    let len = j.len();
    let dir = directory(&mut j, len)?;
    let at = j.disk_start(dir.disk).ok_or_else(damaged)? + dir.offset;
    let old = read_at(&mut j, at, dir.size)?;

    let mut central = Vec::with_capacity(old.len());
    let mut rest = &old[..];
    let mut entries = 0u64;
    while rest.starts_with(CENTRAL) {
        let (entry, used) = rewrite(rest, &j)?;
        central.extend_from_slice(&entry);
        rest = &rest[used..];
        entries += 1;
    }
    if entries != dir.entries {
        return Err(damaged());
    }

    // The new directory comes after every piece; the old one stays where it
    // was, unread.
    let start = len;
    let size = central.len() as u64;
    let mut tail = central;
    if entries >= u64::from(FULL16) || size >= u64::from(FULL32) || start >= u64::from(FULL32) {
        let record_at = start + size;
        tail.extend_from_slice(END64);
        tail.extend_from_slice(&44u64.to_le_bytes());
        tail.extend_from_slice(&45u16.to_le_bytes());
        tail.extend_from_slice(&45u16.to_le_bytes());
        tail.extend_from_slice(&0u32.to_le_bytes());
        tail.extend_from_slice(&0u32.to_le_bytes());
        tail.extend_from_slice(&entries.to_le_bytes());
        tail.extend_from_slice(&entries.to_le_bytes());
        tail.extend_from_slice(&size.to_le_bytes());
        tail.extend_from_slice(&start.to_le_bytes());
        tail.extend_from_slice(LOCATOR64);
        tail.extend_from_slice(&0u32.to_le_bytes());
        tail.extend_from_slice(&record_at.to_le_bytes());
        tail.extend_from_slice(&1u32.to_le_bytes());
    }
    let count = u16::try_from(entries).unwrap_or(FULL16);
    tail.extend_from_slice(END);
    tail.extend_from_slice(&0u16.to_le_bytes());
    tail.extend_from_slice(&0u16.to_le_bytes());
    tail.extend_from_slice(&count.to_le_bytes());
    tail.extend_from_slice(&count.to_le_bytes());
    tail.extend_from_slice(&u32::try_from(size).unwrap_or(FULL32).to_le_bytes());
    tail.extend_from_slice(&u32::try_from(start).unwrap_or(FULL32).to_le_bytes());
    tail.extend_from_slice(&0u16.to_le_bytes());
    j.append(tail);
    Ok(j)
}
