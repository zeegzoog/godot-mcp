//! Build-time inputs of the server.
//!
//! - The commit the server is built from, so a build between two releases reports more than the
//!   manifest version (see `src/version.rs`). A build without git, or from a source archive that
//!   has no `.git`, simply has no commit to name.
//! - The Godot addon (`addons/godot_mcp`), embedded so `godot_update_addon` installs the addon of
//!   the same build as the running server.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8(output.stdout).ok())
        .flatten()
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    name_the_commit();
    embed_the_addon();
}

fn name_the_commit() {
    println!("cargo:rerun-if-env-changed=GODOT_MCP_BUILD");

    // A commit given by the environment wins (a build from a tarball can pass it).
    let commit = std::env::var("GODOT_MCP_BUILD")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| git(&["rev-parse", "--short=9", "HEAD"]));
    if let Some(commit) = commit {
        println!("cargo:rustc-env=GODOT_MCP_BUILD={commit}");
    }

    // Build again when HEAD moves: its file, and the branch files it points to. `--git-path`
    // finds them in a worktree too, where `.git` is a file.
    for name in ["HEAD", "refs/heads", "packed-refs"] {
        if let Some(path) = git(&["rev-parse", "--git-path", name]) {
            let path = PathBuf::from(path);
            if path.exists() {
                println!("cargo:rerun-if-changed={}", path.display());
            }
        }
    }
}

fn embed_the_addon() {
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
