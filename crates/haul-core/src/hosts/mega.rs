//! mega.nz: handed to the external `megatools` program, which does MEGA's
//! client-side decryption.

use std::path::PathBuf;

use super::{Cx, Entry, ResolveError};

pub(super) fn resolve(cx: &Cx<'_>, url: &str) -> Result<Vec<Entry>, ResolveError> {
    let exe = cx
        .cfg
        .megatools
        .clone()
        .or_else(|| which("megatools"))
        .or_else(|| which("megadl"));
    let Some(exe) = exe else {
        return Err(ResolveError::failed(
            "mega.nz needs megatools on PATH (https://megatools.megous.com)",
        ));
    };
    // The old standalone `megadl` takes no subcommand.
    let standalone = exe
        .file_stem()
        .is_some_and(|s| s.eq_ignore_ascii_case("megadl"));
    let mut exec = vec![exe.to_string_lossy().into_owned()];
    if !standalone {
        exec.push("dl".into());
    }
    Ok(vec![Entry {
        url: url.into(),
        exec: Some(exec),
        ..Entry::default()
    }])
}

fn which(name: &str) -> Option<PathBuf> {
    let exts: Vec<String> = if cfg!(windows) {
        std::env::var("PATHEXT")
            .unwrap_or_else(|_| ".EXE;.CMD;.BAT".into())
            .split(';')
            .map(str::to_string)
            .collect()
    } else {
        vec![String::new()]
    };
    std::env::split_paths(&std::env::var_os("PATH")?)
        .flat_map(|dir| exts.iter().map(move |ext| dir.join(format!("{name}{ext}"))))
        .find(|p| p.is_file())
}
