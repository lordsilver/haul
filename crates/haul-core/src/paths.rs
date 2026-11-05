//! Default locations and file-name hygiene.

use std::path::{Path, PathBuf};

/// The desktop app's identifier, as in `tauri.conf.json`.
pub const APP_ID: &str = "io.github.lordsilver.haul";

/// `$HAUL_DATA_DIR`, else Haul's folder in the platform's local data
/// directory: `%LOCALAPPDATA%\io.github.lordsilver.haul` on Windows, named
/// after the app identifier like the WebView2 data next to it, and
/// `~/.local/share/haul` on Linux. It holds `config.toml`, the queue and the
/// instance lock.
pub fn data_dir() -> PathBuf {
    let name = if cfg!(windows) { APP_ID } else { "haul" };
    std::env::var_os("HAUL_DATA_DIR")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| dirs::data_local_dir().map(|d| d.join(name)))
        .unwrap_or_else(|| PathBuf::from("haul-data"))
}

/// The system's Downloads folder, where a report Haul makes is saved; its
/// data folder when the system has none.
pub fn downloads_folder() -> PathBuf {
    dirs::download_dir().unwrap_or_else(data_dir)
}

/// `dir/name` unless something is there already; then the first free
/// `name (1).ext`, `name (2).ext` and so on, as browsers number their
/// downloads. A `.tar.*` archive keeps its double extension together.
pub fn unused_name(dir: &Path, name: &str) -> PathBuf {
    let first = dir.join(name);
    if !first.exists() {
        return first;
    }
    let lower = name.to_ascii_lowercase();
    let split = ["tar.gz", "tar.bz2", "tar.xz", "tar.zst"]
        .iter()
        .find(|ext| lower.ends_with(&format!(".{ext}")) && name.len() > ext.len() + 1)
        .map(|ext| name.len() - ext.len() - 1)
        .or_else(|| name.rfind('.').filter(|&i| i > 0))
        .unwrap_or(name.len());
    let (stem, ext) = name.split_at(split);
    (1..)
        .map(|n| dir.join(format!("{stem} ({n}){ext}")))
        .find(|path| !path.exists())
        .expect("some number is free")
}

/// The user's Downloads folder plus `Haul`.
pub fn download_dir() -> PathBuf {
    dirs::download_dir()
        .or_else(|| dirs::home_dir().map(|h| h.join("Downloads")))
        .map(|d| d.join("Haul"))
        .unwrap_or_else(|| PathBuf::from("downloads"))
}

/// Device names Windows refuses as file names, with or without an extension.
const RESERVED: [&str; 22] = [
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

const MAX_NAME: usize = 200;

/// Turns a name a host sent into a single path component that is valid on
/// every platform and cannot climb out of the download folder.
pub fn safe(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') || c < ' ' {
                '_'
            } else {
                c
            }
        })
        .collect();
    let trimmed = cleaned.trim_matches([' ', '.']);
    if trimmed.is_empty() {
        return "file".into();
    }
    let stem = trimmed.split('.').next().unwrap_or_default();
    let mut out = if RESERVED.iter().any(|r| r.eq_ignore_ascii_case(stem)) {
        format!("_{trimmed}")
    } else {
        trimmed.to_string()
    };
    if out.len() > MAX_NAME {
        let ext = out
            .rfind('.')
            .map(|i| out[i..].to_string())
            .filter(|e| e.len() <= 16)
            .unwrap_or_default();
        let mut cut = MAX_NAME - ext.len();
        while !out.is_char_boundary(cut) {
            cut -= 1;
        }
        out = format!("{}{ext}", &out[..cut]);
    }
    out
}

/// Turns a folder the user named, such as `Hosts/pixeldrain`, into a path
/// inside the download folder: each part made [`safe`], and empty parts and
/// parts of only dots (`.`, `..`) dropped, so it cannot lead anywhere else.
pub fn subfolder(value: &str) -> PathBuf {
    value
        .split(['/', '\\'])
        .map(str::trim)
        .filter(|part| !part.chars().all(|c| c == '.'))
        .map(safe)
        .collect()
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::{safe, subfolder, unused_name};

    #[test]
    fn safe_names() {
        assert_eq!(safe("a/b\\c:d*e?.rar"), "a_b_c_d_e_.rar");
        assert_eq!(safe(" ..hidden. "), "hidden");
        assert_eq!(safe(".."), "file");
        assert_eq!(safe("../../etc/passwd"), "_.._etc_passwd");
        assert_eq!(safe("con.txt"), "_con.txt");
        assert_eq!(safe("tab\there"), "tab_here");
        let long = format!("{}.part01.rar", "x".repeat(300));
        let s = safe(&long);
        assert!(s.len() <= 200 && s.ends_with(".rar"), "{s}");
    }

    #[test]
    fn subfolders_stay_inside() {
        assert_eq!(subfolder("pixeldrain"), Path::new("pixeldrain"));
        assert_eq!(
            subfolder(" Hosts\\pixel:drain/ "),
            Path::new("Hosts").join("pixel_drain")
        );
        assert_eq!(subfolder("../../etc"), Path::new("etc"));
        assert_eq!(subfolder("/abs/./x/"), Path::new("abs").join("x"));
        assert_eq!(
            subfolder("C:\\Downloads"),
            Path::new("C_").join("Downloads")
        );
        assert_eq!(subfolder(" / .. / ... "), PathBuf::new());
    }

    #[test]
    fn taken_names_are_numbered() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        let next = |name: &str| unused_name(d, name).file_name().unwrap().to_owned();
        assert_eq!(next("a.iso"), "a.iso");
        for f in [
            "a.iso",
            "a (1).iso",
            "b.tar.gz",
            "README",
            ".env",
            "c.part1.rar",
        ] {
            std::fs::write(d.join(f), "").unwrap();
        }
        assert_eq!(next("a.iso"), "a (2).iso");
        assert_eq!(next("b.tar.gz"), "b (1).tar.gz");
        assert_eq!(next("README"), "README (1)");
        assert_eq!(next(".env"), ".env (1)");
        assert_eq!(next("c.part1.rar"), "c.part1 (1).rar");
    }
}
