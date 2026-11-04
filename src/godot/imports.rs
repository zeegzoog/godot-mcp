//! Imported sources whose cache in `.godot/imported` is older than the file on disk.
//!
//! Godot 4 writes a `.import` sidecar next to each imported source. Its `path=` line names the
//! cache file (`res://.godot/imported/...`). A process run loads that cache and does not import
//! again, so a source saved while the editor was open stays stale until something reimports it.
//! A source rewritten with the same bytes is not stale: Godot compares the `source_md5` it keeps
//! in `.godot/imported/<name>-<hash>.md5` and skips the reimport, so this check does the same.
//!
//! Godot's own scan trusts its file system cache: when a source's modification time in whole
//! seconds and its sidecar's are the ones it recorded, it does not read the source again. A source
//! rewritten within the second its last scan saw is therefore never reimported, and its cache is
//! newer than the source. [`stale_reason`] compares the bytes for the paths a caller names, and
//! [`force_reimport`] makes the next scan test and reimport a source whatever its times say.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use md5::{Digest, Md5};

use crate::paths::skipped_by_godot;

/// Extensions the editor imports (a `.import` file is generated next to them).
/// Kept in step with `IMPORTED_EXTENSIONS` in the editor addon.
pub const IMPORTED_EXTENSIONS: &[&str] = &[
    "png", "jpg", "jpeg", "webp", "svg", "bmp", "tga", "exr", "hdr", "dds", "ktx", "wav", "ogg",
    "mp3", "glb", "gltf", "fbx", "obj", "ttf", "otf", "woff", "woff2",
];

/// `res://` paths of imported sources newer than their cache, or whose cache file is missing.
/// Only files with a `.import` sidecar are considered, in the folders Godot scans
/// ([`skipped_by_godot`]). The result is sorted and has no duplicates.
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
        if skipped_by_godot(&path, kind.is_dir()) {
            continue;
        }
        if kind.is_dir() {
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

fn stale_source(project: &Path, import_file: &Path) -> Option<String> {
    let source = source_of(import_file)?;
    if !source.is_file() {
        return None;
    }
    let res = res_path(project, &source)?;
    (!import_current(project, &source)?).then_some(res)
}

/// Whether the cache of an imported `source` is current: `false` when the cache is missing, or
/// older than the source while the source's bytes differ from the `source_md5` Godot recorded for
/// that cache. `None` when the source has no readable `.import` sidecar.
pub fn import_current(project: &Path, source: &Path) -> Option<bool> {
    let name = source.file_name()?.to_str()?;
    let import_file = source.with_file_name(format!("{name}.import"));
    let text = read_sidecar(&import_file)?;
    let stale = match cache_path(project, &text) {
        Some(cache) => {
            modified(&cache).is_none()
                || (newer_than(source, &cache) && !same_recorded_md5(source, name, &cache))
        }
        None => newer_than(source, &import_file),
    };
    Some(!stale)
}

/// True when Godot's `<name>-<hash>.md5` next to `cache` records the md5 of `source` as it is now,
/// which is when Godot itself skips the reimport. False when the file is missing or differs.
fn same_recorded_md5(source: &Path, name: &str, cache: &Path) -> bool {
    let Some(md5_file) = md5_file(name, cache) else {
        return false;
    };
    let Some(recorded) = read_sidecar(&md5_file)
        .and_then(|text| text.lines().find_map(|line| assignment(line, "source_md5")))
    else {
        return false;
    };
    let Ok(bytes) = fs::read(source) else {
        return false;
    };
    recorded.eq_ignore_ascii_case(&md5_hex(&bytes))
}

fn md5_hex(bytes: &[u8]) -> String {
    Md5::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Fingerprint of each `.import` sidecar: the `res://` source and a hash of the sidecar text, of
/// the `.md5` file Godot records and of the times of it and of the cache. A new or rewritten
/// sidecar compares unequal, and so does a reimport that leaves the sidecar text as it was (a
/// model reimported with the same options).
pub fn import_fingerprints(project: &Path) -> Vec<(String, u64)> {
    let mut found = Vec::new();
    walk_sidecars(project, project, &mut found);
    found.sort();
    found.dedup();
    found
}

fn walk_sidecars(project: &Path, dir: &Path, found: &mut Vec<(String, u64)>) {
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
        if skipped_by_godot(&path, kind.is_dir()) {
            continue;
        }
        if kind.is_dir() {
            walk_sidecars(project, &path, found);
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
        let Some(source) = source_of(&path) else {
            continue;
        };
        let Some(res) = res_path(project, &source) else {
            continue;
        };
        let Some(text) = read_sidecar(&path) else {
            continue;
        };
        let mut state = text.into_bytes();
        state.extend(import_state(project, &source));
        found.push((res, fnv_hash(&state)));
    }
}

/// What a reimport of `source` rewrites besides its sidecar, whose text stays the same when the
/// import options do: the recorded `.md5` file and the modification times of it and of the cache.
fn import_state(project: &Path, source: &Path) -> Vec<u8> {
    let mut state = Vec::new();
    let Some(files) = cache_files(project, source) else {
        return state;
    };
    for file in [Some(files.cache), files.md5].into_iter().flatten() {
        if let Some(time) = modified(&file) {
            let nanos = time
                .duration_since(SystemTime::UNIX_EPOCH)
                .map_or(0, |since| since.as_nanos());
            state.extend(nanos.to_le_bytes());
        }
        if let Ok(bytes) = fs::read(&file) {
            if bytes.len() <= 64 * 1024 {
                state.extend(fnv_hash(&bytes).to_le_bytes());
            }
        }
    }
    state
}

/// The cache file a sidecar names and the `.md5` file Godot keeps next to it.
struct CacheFiles {
    cache: PathBuf,
    md5: Option<PathBuf>,
}

fn cache_files(project: &Path, source: &Path) -> Option<CacheFiles> {
    let name = source.file_name()?.to_str()?;
    let text = read_sidecar(&source.with_file_name(format!("{name}.import")))?;
    let cache = cache_path(project, &text)?;
    let md5 = md5_file(name, &cache);
    Some(CacheFiles { cache, md5 })
}

/// `<name>-<hash>.md5` next to the cache `<name>-<hash>.<ext>` of a source called `name`.
fn md5_file(name: &str, cache: &Path) -> Option<PathBuf> {
    let cache_name = cache.file_name()?.to_str()?;
    let hash = cache_name
        .strip_prefix(name)?
        .strip_prefix('-')?
        .split('.')
        .next()?;
    if hash.is_empty() {
        return None;
    }
    Some(cache.with_file_name(format!("{name}-{hash}.md5")))
}

/// The importer (`scene`, `texture`, `wav`...) and the imported resource class from the sidecar
/// of `source`. `None` when it has no readable sidecar or names no importer.
pub fn import_kind(source: &Path) -> Option<(String, Option<String>)> {
    let name = source.file_name()?.to_str()?;
    let text = read_sidecar(&source.with_file_name(format!("{name}.import")))?;
    let importer = text.lines().find_map(|line| assignment(line, "importer"))?;
    let class = text.lines().find_map(|line| assignment(line, "type"));
    Some((importer, class))
}

/// Why the imported data of `source` does not match its bytes, as a reason code and a sentence,
/// or `None` when it does, when the source has no sidecar, or when its importer keeps no cache.
/// Unlike [`import_current`] this reads the source whatever the modification times say.
pub fn stale_reason(project: &Path, source: &Path) -> Option<(&'static str, String)> {
    let name = source.file_name()?.to_str()?;
    let text = read_sidecar(&source.with_file_name(format!("{name}.import")))?;
    if text.lines().any(|line| {
        line.trim()
            .strip_prefix("valid")
            .and_then(|rest| rest.trim_start().strip_prefix('='))
            .is_some_and(|value| value.trim() == "false")
    }) {
        return Some((
            "import_failed",
            "the sidecar says the last import failed (valid=false), so there is no imported data"
                .into(),
        ));
    }
    let cache = cache_path(project, &text)?;
    if !cache.is_file() {
        return Some((
            "cache_missing",
            "the imported file in .godot/imported does not exist".into(),
        ));
    }
    let recorded = md5_file(name, &cache)
        .and_then(|file| read_sidecar(&file))
        .and_then(|text| text.lines().find_map(|line| assignment(line, "source_md5")));
    let Some(recorded) = recorded else {
        return Some((
            "md5_missing",
            "Godot recorded no source_md5 for the imported data, so it is not known which bytes it came from".into(),
        ));
    };
    let bytes = fs::read(source).ok()?;
    if recorded.eq_ignore_ascii_case(&md5_hex(&bytes)) {
        return None;
    }
    let detail = if newer_than(source, &cache) {
        "the source changed after its imported data was written"
    } else {
        "the source's bytes differ from the ones its imported data was made from, although the imported data is newer: \
         Godot skipped the import because the source's modification time, in whole seconds, \
         is the one its last scan recorded"
    };
    Some(("source_changed", detail.into()))
}

/// Makes Godot's next scan test `source` and reimport it, as when its cache is fooled: removes the
/// `.md5` file Godot recorded for it and moves its sidecar's modification time back one second,
/// so the file system cache no longer matches. `Ok(false)` when it has no sidecar.
pub fn force_reimport(project: &Path, source: &Path) -> std::io::Result<bool> {
    let Some(name) = source.file_name().and_then(|name| name.to_str()) else {
        return Ok(false);
    };
    let sidecar = source.with_file_name(format!("{name}.import"));
    if !sidecar.is_file() {
        return Ok(false);
    }
    if let Some(md5) = cache_files(project, source).and_then(|files| files.md5) {
        match fs::remove_file(&md5) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    let time = fs::metadata(&sidecar)?.modified()?;
    let earlier = time
        .checked_sub(std::time::Duration::from_secs(1))
        .unwrap_or(time);
    fs::File::options()
        .write(true)
        .open(&sidecar)?
        .set_modified(earlier)?;
    Ok(true)
}

fn fnv_hash(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    hash
}

/// Sources whose sidecar appeared, or that were imported again, between two
/// [`import_fingerprints`] snapshots.
pub fn changed_imports(before: &[(String, u64)], after: &[(String, u64)]) -> Vec<String> {
    let mut changed = Vec::new();
    for (path, hash) in after {
        let same = before
            .iter()
            .any(|(old, old_hash)| old == path && old_hash == hash);
        if !same {
            changed.push(path.clone());
        }
    }
    changed.sort();
    changed.dedup();
    changed
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

    /// A model as Godot leaves it after an import: the sidecar, the cache and the `.md5` file
    /// that records the source's bytes. Returns the project and the source.
    fn imported_model(bytes: &[u8]) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("project");
        let root = dir.path();
        fs::create_dir_all(root.join("art")).unwrap();
        fs::create_dir_all(root.join(".godot/imported")).unwrap();
        let source = root.join("art/m.glb");
        fs::write(&source, bytes).unwrap();
        fs::write(
            root.join("art/m.glb.import"),
            "[remap]\n\nimporter=\"scene\"\nimporter_version=1\ntype=\"PackedScene\"\n\
             uid=\"uid://b1\"\npath=\"res://.godot/imported/m.glb-abc.scn\"\n\n[deps]\n\n\
             source_file=\"res://art/m.glb\"\ndest_files=[\"res://.godot/imported/m.glb-abc.scn\"]\n",
        )
        .unwrap();
        record_import(root, bytes);
        (dir, source)
    }

    /// Writes the cache and the `.md5` file of `art/m.glb` the way an import of `bytes` does.
    fn record_import(root: &Path, bytes: &[u8]) {
        fs::write(root.join(".godot/imported/m.glb-abc.scn"), bytes).unwrap();
        fs::write(
            root.join(".godot/imported/m.glb-abc.md5"),
            format!("source_md5=\"{}\"\ndest_md5=\"0\"\n", md5_hex(bytes)),
        )
        .unwrap();
    }

    #[test]
    fn a_reimport_that_keeps_the_sidecar_text_is_a_changed_import() {
        let (project, _) = imported_model(b"glb one");
        let root = project.path();
        let before = import_fingerprints(root);
        // A model reimported with the same options: Godot writes the same sidecar text and
        // only the cache and its .md5 change.
        record_import(root, b"glb two");
        let after = import_fingerprints(root);
        assert_eq!(
            changed_imports(&before, &after),
            vec!["res://art/m.glb".to_string()]
        );
        assert!(changed_imports(&after, &import_fingerprints(root)).is_empty());
    }

    #[test]
    fn import_kind_names_the_importer_and_the_resource_class() {
        let (_project, source) = imported_model(b"glb");
        assert_eq!(
            import_kind(&source),
            Some(("scene".to_string(), Some("PackedScene".to_string())))
        );
        assert_eq!(import_kind(&source.with_file_name("none.glb")), None);
    }

    #[test]
    fn a_source_rewritten_in_the_same_second_is_stale_although_the_cache_is_newer() {
        let (project, source) = imported_model(b"glb one");
        let root = project.path();
        assert_eq!(stale_reason(root, &source), None);
        // New bytes of the same size, older than the cache: Godot's scan trusted its
        // whole-second times and did not import them.
        fs::write(&source, b"glb two").unwrap();
        touch(&source, Duration::from_secs(5));
        assert_eq!(import_current(root, &source), Some(true));
        let (reason, detail) = stale_reason(root, &source).expect("stale");
        assert_eq!(reason, "source_changed");
        assert!(detail.contains("whole seconds"), "{detail}");
        // Newer bytes say so plainly.
        touch(
            &root.join(".godot/imported/m.glb-abc.scn"),
            Duration::from_secs(60),
        );
        touch(&source, Duration::from_secs(1));
        let (_, detail) = stale_reason(root, &source).expect("stale");
        assert!(detail.contains("changed after"), "{detail}");
    }

    #[test]
    fn stale_reason_names_a_missing_cache_md5_and_a_failed_import() {
        let (project, source) = imported_model(b"glb");
        let root = project.path();
        fs::remove_file(root.join(".godot/imported/m.glb-abc.md5")).unwrap();
        assert_eq!(
            stale_reason(root, &source).map(|(reason, _)| reason),
            Some("md5_missing")
        );
        fs::remove_file(root.join(".godot/imported/m.glb-abc.scn")).unwrap();
        assert_eq!(
            stale_reason(root, &source).map(|(reason, _)| reason),
            Some("cache_missing")
        );
        fs::write(
            root.join("art/m.glb.import"),
            "[remap]\n\nimporter=\"scene\"\nvalid=false\n",
        )
        .unwrap();
        assert_eq!(
            stale_reason(root, &source).map(|(reason, _)| reason),
            Some("import_failed")
        );
        assert_eq!(stale_reason(root, &root.join("art/none.glb")), None);
    }

    #[test]
    fn force_reimport_drops_the_recorded_md5_and_moves_the_sidecar_time() {
        let (project, source) = imported_model(b"glb");
        let root = project.path();
        let sidecar = root.join("art/m.glb.import");
        let before = modified(&sidecar).expect("mtime");
        assert!(force_reimport(root, &source).expect("force"));
        assert!(!root.join(".godot/imported/m.glb-abc.md5").exists());
        let after = modified(&sidecar).expect("mtime");
        assert!(after < before, "{after:?} {before:?}");
        // The sidecar and the cache stay, so the next import reuses the uid and the cache name.
        assert!(sidecar.is_file());
        assert!(root.join(".godot/imported/m.glb-abc.scn").is_file());
        // Twice in a row is fine, and a file without a sidecar is left alone.
        assert!(force_reimport(root, &source).expect("force again"));
        assert!(!force_reimport(root, &root.join("art/none.glb")).expect("none"));
    }

    #[test]
    fn a_new_or_rewritten_sidecar_is_a_changed_import() {
        let before = vec![
            ("res://a.glb".to_string(), 1u64),
            ("res://c.glb".to_string(), 1u64),
        ];
        let after = vec![
            ("res://a.glb".to_string(), 2u64),
            ("res://b.glb".to_string(), 3u64),
            ("res://c.glb".to_string(), 1u64),
        ];
        assert_eq!(
            changed_imports(&before, &after),
            vec!["res://a.glb".to_string(), "res://b.glb".to_string()]
        );
        assert!(changed_imports(&after, &after).is_empty());
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
    fn import_current_reads_one_source() {
        let stale = project_with(true);
        let source = stale.path().join("shaders/flow.glsl");
        assert_eq!(import_current(stale.path(), &source), Some(false));
        let fresh = project_with(false);
        let source = fresh.path().join("shaders/flow.glsl");
        assert_eq!(import_current(fresh.path(), &source), Some(true));
        let unimported = fresh.path().join("shaders/other.glsl");
        fs::write(&unimported, "void main() {}\n").unwrap();
        assert_eq!(import_current(fresh.path(), &unimported), None);
    }

    #[test]
    fn a_newer_source_with_the_md5_godot_recorded_is_left_alone() {
        let project = project_with(true);
        let root = project.path();
        let md5_file = root.join(".godot/imported/flow.glsl-abc.md5");
        // The md5 of the source's bytes, as Godot records it: Godot skips this reimport.
        let actual = md5_hex(b"void main() {}\n");
        fs::write(
            &md5_file,
            format!("source_md5=\"{actual}\"\ndest_md5=\"0\"\n"),
        )
        .unwrap();
        assert!(stale_imports(root).is_empty());
        let source = root.join("shaders/flow.glsl");
        assert_eq!(import_current(root, &source), Some(true));
        // Changed bytes are stale again, as they are for Godot.
        fs::write(&source, "void main() { discard; }\n").unwrap();
        assert_eq!(
            stale_imports(root),
            vec!["res://shaders/flow.glsl".to_string()]
        );
    }

    #[test]
    fn a_missing_cache_is_stale_even_when_the_md5_matches() {
        let project = project_with(false);
        let root = project.path();
        let actual = md5_hex(b"void main() {}\n");
        fs::write(
            root.join(".godot/imported/flow.glsl-abc.md5"),
            format!("source_md5=\"{actual}\"\n"),
        )
        .unwrap();
        fs::remove_file(root.join(".godot/imported/flow.glsl-abc.res")).unwrap();
        assert_eq!(
            stale_imports(root),
            vec!["res://shaders/flow.glsl".to_string()]
        );
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

    #[test]
    fn folders_godot_does_not_scan_are_not_walked() {
        let project = project_with(true);
        let root = project.path();
        // A stale copy of the same source in each folder Godot skips.
        for folder in [
            ".hidden",
            "docs",
            "nested",
            ".claude/worktrees/copy/shaders",
        ] {
            let dir = root.join(folder);
            fs::create_dir_all(&dir).unwrap();
            fs::copy(root.join("shaders/flow.glsl"), dir.join("flow.glsl")).unwrap();
            fs::copy(
                root.join("shaders/flow.glsl.import"),
                dir.join("flow.glsl.import"),
            )
            .unwrap();
            touch(&dir.join("flow.glsl"), Duration::from_secs(10));
        }
        fs::write(root.join("docs/.gdignore"), "").unwrap();
        fs::write(root.join("nested/project.godot"), "config_version=5\n").unwrap();
        // A hidden source and its sidecar next to a scanned one.
        fs::copy(
            root.join("shaders/flow.glsl"),
            root.join("shaders/.flow.glsl"),
        )
        .unwrap();
        fs::copy(
            root.join("shaders/flow.glsl.import"),
            root.join("shaders/.flow.glsl.import"),
        )
        .unwrap();
        assert_eq!(
            stale_imports(root),
            vec!["res://shaders/flow.glsl".to_string()]
        );
        let fingerprints: Vec<String> = import_fingerprints(root)
            .into_iter()
            .map(|(res, _)| res)
            .collect();
        assert_eq!(fingerprints, vec!["res://shaders/flow.glsl".to_string()]);
    }
}
