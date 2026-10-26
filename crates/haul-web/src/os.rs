//! Showing files in the system's file manager.

use std::{io, path::Path};

/// Opens the folder holding `path`, with the file selected where the file
/// manager supports it.
pub fn reveal(path: &Path) -> io::Result<()> {
    if !path.exists() {
        return Err(io::Error::new(io::ErrorKind::NotFound, "moved or deleted"));
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let target = path.display().to_string().replace('/', "\\");
        // Explorer wants the quotes around the path alone, where normal
        // argument quoting would wrap the whole switch.
        std::process::Command::new("explorer.exe")
            .raw_arg(format!("/select,\"{target}\""))
            .spawn()?;
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let folder = if path.is_dir() {
            path
        } else {
            path.parent().unwrap_or(path)
        };
        open::that_detached(folder)
    }
}
