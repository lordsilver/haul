//! Embeds the built web UI (`ui/dist`, which `pnpm build` in `ui/` writes and
//! git keeps, so a build needs no Node) as a table of routes, content types
//! and bytes.

use std::{
    env,
    fmt::Write,
    fs,
    path::{Path, PathBuf},
};

fn files(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = fs::read_dir(dir).unwrap_or_else(|e| {
        panic!(
            "{}: {e}; build the UI with `pnpm build` in crates/haul-web/ui",
            dir.display()
        )
    });
    for entry in entries {
        let path = entry.expect("UI file").path();
        if path.is_dir() {
            files(&path, out);
        } else {
            out.push(path);
        }
    }
}

fn content_type(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("js") => "text/javascript; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("woff2") => "font/woff2",
        Some("png") => "image/png",
        Some("json") => "application/json",
        _ => "application/octet-stream",
    }
}

fn main() {
    let root = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("manifest dir"));
    let dist = root.join("ui").join("dist");
    println!("cargo::rerun-if-changed={}", dist.display());
    let mut all = Vec::new();
    files(&dist, &mut all);
    all.sort();
    let mut table = String::from(
        "/// The UI, built into the binary: route, file in `ui/dist`, content type, body.\n\
         pub(crate) const ASSETS: &[(&str, &str, &str, &[u8])] = &[\n",
    );
    for path in &all {
        let file = path
            .strip_prefix(&dist)
            .expect("under dist")
            .to_string_lossy()
            .replace('\\', "/");
        let route = if file == "index.html" {
            "/".to_string()
        } else {
            format!("/{file}")
        };
        writeln!(
            table,
            "    ({route:?}, {file:?}, {:?}, include_bytes!({:?})),",
            content_type(path),
            path.display().to_string()
        )
        .expect("write to a string");
    }
    table.push_str("];\n");
    let out = PathBuf::from(env::var("OUT_DIR").expect("out dir")).join("assets.rs");
    fs::write(out, table).expect("write the asset table");
}
