//! Default locations and file-name hygiene.

use std::path::{Path, PathBuf};

use crate::{config, store};

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

/// Up to 0.3, Haul kept its data in `%LOCALAPPDATA%\haul` on Windows, which is
/// also the folder the installer puts the program in (Windows ignores case).
/// When `dir` is the default data folder, this moves that data over, once.
pub fn adopt_old_data(dir: &Path) {
    if !cfg!(windows) {
        return;
    }
    if let Some(local) = dirs::data_local_dir()
        && dir == local.join(APP_ID)
    {
        move_data(&local.join("haul"), dir);
    }
}

/// Haul's own files in its data folder, besides the log and the lock.
const DATA: [&str; 6] = [
    config::FILE,
    store::QUEUE,
    store::HOSTS,
    store::ROUTES,
    "routes.json.migrated",
    // The in-app browser's profiles.
    "webview",
];

/// Moves Haul's data from `old` to `dir` unless `dir` has settings or a queue
/// already, or an older Haul still holds `old`. Only Haul's own files move:
/// `old` may be the program folder.
fn move_data(old: &Path, dir: &Path) {
    let has_data = |d: &Path| {
        [config::FILE, store::QUEUE]
            .iter()
            .any(|f| d.join(f).exists())
    };
    if !has_data(old) || has_data(dir) {
        return;
    }
    let lock = match store::lock(old) {
        Ok(lock) => lock,
        Err(e) => {
            tracing::warn!("leaving the data in {} where it is: {e}", old.display());
            return;
        }
    };
    if let Err(e) = std::fs::create_dir_all(dir) {
        tracing::warn!("not moving the data in {}: {e}", old.display());
        return;
    }
    for name in DATA {
        let from = old.join(name);
        if from.exists()
            && let Err(e) = std::fs::rename(&from, dir.join(name))
        {
            tracing::warn!("moving {} to {}: {e}", from.display(), dir.display());
        }
    }
    tracing::info!("moved the data in {} to {}", old.display(), dir.display());
    // The old log and lock go too, and the folder itself unless the program
    // is in it.
    drop(lock);
    for name in ["haul.log", "haul.lock"] {
        let _ = std::fs::remove_file(old.join(name));
    }
    let _ = std::fs::remove_dir(old);
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
    use std::{
        collections::BTreeSet,
        path::{Path, PathBuf},
    };

    use super::{APP_ID, move_data, safe, subfolder, unused_name};

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
    fn data_moves_out_of_the_program_folder() {
        let tmp = tempfile::tempdir().unwrap();
        let (old, dir) = (tmp.path().join("Haul"), tmp.path().join(APP_ID));
        std::fs::create_dir_all(old.join("webview")).unwrap();
        for f in [
            "Haul.exe",
            "uninstall.exe",
            "config.toml",
            "queue.json",
            "haul.log",
        ] {
            std::fs::write(old.join(f), f).unwrap();
        }
        let running = crate::store::lock(&old).unwrap();
        move_data(&old, &dir);
        assert!(!dir.exists(), "an older Haul still holds the folder");
        drop(running);

        move_data(&old, &dir);
        assert_eq!(
            std::fs::read_to_string(dir.join("queue.json")).unwrap(),
            "queue.json"
        );
        assert!(dir.join("config.toml").is_file() && dir.join("webview").is_dir());
        let left: BTreeSet<String> = std::fs::read_dir(&old)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        assert_eq!(left, ["Haul.exe", "uninstall.exe"].map(String::from).into());

        std::fs::write(old.join("queue.json"), "older").unwrap();
        move_data(&old, &dir);
        assert_eq!(
            std::fs::read_to_string(dir.join("queue.json")).unwrap(),
            "queue.json",
            "moved once only"
        );
    }

    #[test]
    fn an_emptied_data_folder_goes() {
        let tmp = tempfile::tempdir().unwrap();
        let (old, dir) = (tmp.path().join("haul"), tmp.path().join(APP_ID));
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(old.join("queue.json"), "{}").unwrap();
        move_data(&old, &dir);
        assert!(!old.exists() && dir.join("queue.json").is_file());
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
