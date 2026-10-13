//! Default locations and file-name hygiene.

use std::path::PathBuf;

/// `$HAUL_DATA_DIR`, else the platform's local data directory plus
/// `haul` (`%LOCALAPPDATA%\haul` on Windows, `~/.local/share/haul`
/// on Linux). It holds `config.toml`, the queue and the instance lock.
pub fn data_dir() -> PathBuf {
    std::env::var_os("HAUL_DATA_DIR")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| dirs::data_local_dir().map(|d| d.join("haul")))
        .unwrap_or_else(|| PathBuf::from("haul-data"))
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

#[cfg(test)]
mod tests {
    use super::safe;

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
}
