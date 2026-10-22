//! Imported sources whose cache in `.godot/imported` is older than the file on disk.
//!
//! Godot 4 writes a `.import` sidecar next to each imported source. Its `path=` line names the
//! cache file (`res://.godot/imported/...`). A process run loads that cache and does not import
//! again, so a source saved while the editor was open stays stale until something reimports it.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// `res://` paths of imported sources newer than their cache, or whose cache file is missing.
/// Only files with a `.import` sidecar are considered. The result is sorted and has no duplicates.
pub fn stale_imports(project: &Path) -> Vec<String> {
    let mut found = Vec::new();
    walk(project, project, &mut found);
    found.sort();
    found.dedup();
    found
}

fn walk(project: &Path, dir: &Path, found: &mut Vec<String>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_symlink() {
            continue;
        }
        let path = entry.path();
        if kind.is_dir() {
            if is_skipped_dir(&path) {
                continue;
            }
            walk(project, &path, found);
            continue;
        }
        if !kind.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !name.ends_with(".import") {
            continue;
        }
        if let Some(res) = stale_source(project, &path) {
            found.push(res);
        }
    }
}

fn is_skipped_dir(path: &Path) -> bool {
    matches!(
        path.file_name().and_then(|name| name.to_str()),
        Some(".godot" | ".git")
    )
}

fn stale_source(project: &Path, import_file: &Path) -> Option<String> {
    let source = source_of(import_file)?;
    if !source.is_file() {
        return None;
    }
    let res = res_path(project, &source)?;
    let text = read_sidecar(import_file)?;
    let cache = cache_path(project, &text);
    let stale = match &cache {
        Some(cache) => newer_than(&source, cache),
        None => newer_than(&source, import_file),
    };
    stale.then_some(res)
}

fn source_of(import_file: &Path) -> Option<PathBuf> {
    let name = import_file.file_name()?.to_str()?;
    let source_name = name.strip_suffix(".import")?;
    if source_name.is_empty() {
        return None;
    }
    Some(import_file.with_file_name(source_name))
}

fn read_sidecar(path: &Path) -> Option<String> {
    let meta = fs::metadata(path).ok()?;
    if meta.len() > 64 * 1024 {
        return None;
    }
    fs::read_to_string(path).ok()
}

/// The `path=` cache from a Godot 4 `.import` file, or the first `dest_files` entry.
fn cache_path(project: &Path, text: &str) -> Option<PathBuf> {
    let mut dest = None;
    for line in text.lines() {
        if let Some(value) = assignment(line, "path") {
            if let Some(path) = res_to_file(project, &value) {
                return Some(path);
            }
        }
        if dest.is_none() && line.trim().starts_with("dest_files") {
            dest = first_res(line).and_then(|value| res_to_file(project, &value));
        }
    }
    dest
}

fn assignment(line: &str, key: &str) -> Option<String> {
    let rest = line.trim().strip_prefix(key)?;
    let rest = rest.trim_start().strip_prefix('=')?.trim_start();
    unquote(rest)
}

fn first_res(line: &str) -> Option<String> {
    let start = line.find("\"res://")?;
    unquote(&line[start..])
}

fn unquote(text: &str) -> Option<String> {
    let rest = text.trim_start().strip_prefix('"')?;
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

fn res_to_file(project: &Path, res: &str) -> Option<PathBuf> {
    let relative = res.strip_prefix("res://")?;
    if relative.is_empty()
        || relative
            .split(['/', '\\'])
            .any(|part| part.is_empty() || part == "..")
    {
        return None;
    }
    Some(project.join(relative.replace('\\', "/")))
}

fn res_path(project: &Path, file: &Path) -> Option<String> {
    let relative = file.strip_prefix(project).ok()?;
    let text = relative.to_str()?.replace('\\', "/");
    if text.is_empty() || text.split('/').any(|part| part.is_empty() || part == "..") {
        return None;
    }
    Some(format!("res://{text}"))
}

/// True when `source` is strictly newer, or when `cache` cannot be read (it is missing).
fn newer_than(source: &Path, cache: &Path) -> bool {
    let Some(source_time) = modified(source) else {
        return false;
    };
    match modified(cache) {
        Some(cache_time) => source_time > cache_time,
        None => true,
    }
}

fn modified(path: &Path) -> Option<SystemTime> {
    fs::metadata(path).and_then(|meta| meta.modified()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn touch(path: &Path, age: Duration) {
        let file = fs::File::options()
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
            .expect("touch");
        file.set_modified(SystemTime::now() - age).expect("mtime");
    }

    fn project_with(stale_source: bool) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("project");
        let root = dir.path();
        fs::create_dir_all(root.join("shaders")).unwrap();
        fs::create_dir_all(root.join(".godot/imported")).unwrap();
        fs::write(root.join("project.godot"), "config_version=5\n").unwrap();
        let source = root.join("shaders/flow.glsl");
        fs::write(&source, "void main() {}\n").unwrap();
        let cache = root.join(".godot/imported/flow.glsl-abc.res");
        fs::write(&cache, "cache").unwrap();
        fs::write(
            root.join("shaders/flow.glsl.import"),
            "path=\"res://.godot/imported/flow.glsl-abc.res\"\n\
             dest_files=[\"res://.godot/imported/flow.glsl-abc.res\"]\n",
        )
        .unwrap();
        if stale_source {
            touch(&cache, Duration::from_secs(3600));
            touch(&source, Duration::from_secs(10));
        } else {
            touch(&source, Duration::from_secs(3600));
            touch(&cache, Duration::from_secs(10));
        }
        dir
    }

    #[test]
    fn a_source_newer_than_its_import_cache_is_stale() {
        let project = project_with(true);
        assert_eq!(
            stale_imports(project.path()),
            vec!["res://shaders/flow.glsl".to_string()]
        );
    }

    #[test]
    fn a_cache_newer_than_its_source_is_left_alone() {
        let project = project_with(false);
        assert!(stale_imports(project.path()).is_empty());
    }

    #[test]
    fn a_missing_cache_is_stale_and_a_file_without_a_sidecar_is_not() {
        let project = project_with(false);
        let root = project.path();
        fs::remove_file(root.join(".godot/imported/flow.glsl-abc.res")).unwrap();
        fs::write(root.join("shaders/plain.glsl"), "void main() {}\n").unwrap();
        touch(&root.join("shaders/plain.glsl"), Duration::from_secs(1));
        assert_eq!(
            stale_imports(root),
            vec!["res://shaders/flow.glsl".to_string()]
        );
    }

    #[test]
    fn a_cache_path_that_leaves_the_project_is_ignored() {
        let project = project_with(false);
        let root = project.path();
        fs::write(
            root.join("shaders/flow.glsl.import"),
            "path=\"res://../outside.res\"\n",
        )
        .unwrap();
        // The sidecar itself is not older than the source, so there is nothing to report.
        touch(&root.join("shaders/flow.glsl"), Duration::from_secs(3600));
        touch(
            &root.join("shaders/flow.glsl.import"),
            Duration::from_secs(10),
        );
        assert!(stale_imports(root).is_empty());
    }

    #[test]
    fn dest_files_is_used_when_path_is_absent_and_the_source_is_newer() {
        let project = project_with(true);
        let root = project.path();
        fs::write(
            root.join("shaders/flow.glsl.import"),
            "dest_files=[\"res://.godot/imported/flow.glsl-abc.res\"]\n",
        )
        .unwrap();
        assert_eq!(
            stale_imports(root),
            vec!["res://shaders/flow.glsl".to_string()]
        );
    }
}
