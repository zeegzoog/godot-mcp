//! Embeds the Godot addon (`addons/godot_mcp`) in the server, so `godot_update_addon` installs
//! the addon of the same build as the running server.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

fn main() {
    let root = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default());
    let addon = root.join("addons").join("godot_mcp");
    println!("cargo:rerun-if-changed={}", addon.display());
    let mut files = Vec::new();
    collect(&addon, &addon, &mut files);
    files.sort();
    let mut source = String::from("&[\n");
    for (relative, absolute) in &files {
        println!("cargo:rerun-if-changed={}", absolute.display());
        let _ = writeln!(
            source,
            "    ({relative:?}, include_bytes!({:?}) as &[u8]),",
            absolute.display().to_string()
        );
    }
    source.push(']');
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap_or_default()).join("addon_files.rs");
    if let Err(error) = std::fs::write(&out, source) {
        panic!("cannot write {}: {error}", out.display());
    }
}

/// Every file under `dir`, as a `/`-separated path relative to `base` and its absolute path.
/// Directories are watched too, so an added or removed file rebuilds the list.
fn collect(base: &Path, dir: &Path, files: &mut Vec<(String, PathBuf)>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    println!("cargo:rerun-if-changed={}", dir.display());
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }
        if path.is_dir() {
            collect(base, &path, files);
        } else if let Ok(relative) = path.strip_prefix(base) {
            let relative = relative
                .components()
                .map(|part| part.as_os_str().to_string_lossy().to_string())
                .collect::<Vec<_>>()
                .join("/");
            files.push((relative, path));
        }
    }
}
