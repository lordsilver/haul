//! Default locations and file-name hygiene.

use std::path::{Path, PathBuf};

/// `$HAUL_DATA_DIR`, else Haul's folder in the platform's local data
/// directory: `%LOCALAPPDATA%\Haul` on Windows, which is also the folder the
/// uninstaller deletes when asked to remove the app's data (a per-user install
/// puts the program in `%LOCALAPPDATA%\Programs\Haul`), and
/// `~/.local/share/haul` on Linux. It holds `config.toml`, the queue, the logs,
/// the instance lock and every browser profile ([`webview`]).
pub fn data_dir() -> PathBuf {
    let name = if cfg!(windows) { "Haul" } else { "haul" };
    std::env::var_os("HAUL_DATA_DIR")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| dirs::data_local_dir().map(|d| d.join(name)))
        .unwrap_or_else(|| PathBuf::from("haul-data"))
}

/// Where the desktop app keeps its browser data: the main window's under
/// `main`, and the profiles it passes pages in under `chromium`
/// ([`pass_profiles`]).
pub fn webview(data_dir: &Path) -> PathBuf {
    data_dir.join("webview")
}

/// The main window's browser data.
pub fn main_webview(data_dir: &Path) -> PathBuf {
    webview(data_dir).join("main")
}

/// Where the desktop app keeps the browser profiles it passes pages in, one
/// per site, beside the last picture of each site's page that wanted a person
/// ([`wants_person_shot`]), which a report carries.
pub fn pass_profiles(data_dir: &Path) -> PathBuf {
    webview(data_dir).join("chromium")
}

/// The profile `site`'s pages are passed in: the first browser's among
/// [`pass_profiles`], and the other's (`browser`, as `chrome` or `edge`)
/// in a folder of its own there, since one browser's profile is not the
/// other's to open.
pub fn pass_profile(data_dir: &Path, browser: Option<&str>, site: &str) -> PathBuf {
    match browser {
        None => pass_profiles(data_dir).join(site),
        Some(browser) => pass_profiles(data_dir).join(browser).join(site),
    }
}

/// The picture of `site`'s last page that wanted a person, among the
/// profiles ([`pass_profiles`]).
pub fn wants_person_shot(data_dir: &Path, site: &str) -> PathBuf {
    pass_profiles(data_dir).join(format!("{site}{WANTS_PERSON}"))
}

/// How such a picture's file name ends.
pub const WANTS_PERSON: &str = "-wants-person.png";

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
