//! The project's uid cache, `.godot/uid_cache.bin`: the `uid:// -> res://` map a game or a
//! headless script loads at startup to resolve the uids in `[ext_resource]` lines.
//!
//! Only the editor writes it (`EditorFileSystem` after a scan, `ResourceUID::update_cache`). A
//! headless script that saves new files never adds their uids, so after the server writes new
//! uids back into those files, the next run that loads them prints "ext_resource, invalid UID"
//! once per reference. [`record`] adds them the way the editor would.
//!
//! The format is `ResourceUID::save_to_cache`'s: a little-endian `u32` entry count, then per
//! entry a `u64` id, a `u32` byte length and that many bytes of the UTF-8 `res://` path. When an
//! id appears twice, Godot keeps the last one.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use super::binary_resource::uid_text;

/// Where Godot keeps the uid cache of `project`.
pub fn uid_cache_file(project: &Path) -> PathBuf {
    project.join(".godot").join("uid_cache.bin")
}

/// What [`record`] did with the cache.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UidCacheUpdate {
    /// The project has no uid cache yet (the editor never scanned it); nothing was written.
    Missing,
    /// The number of `(path, uid)` pairs that were added or moved; 0 when all were known.
    Recorded(usize),
}

/// Makes the uid cache map each `uid` to its `res://` path, as the editor's file system does
/// when it rescans a file: an id already known for another path is moved, and an entry that gave
/// the same path another id is dropped. The file is replaced atomically; a cache that does not
/// parse is left as it is and reported as an error.
pub fn record(project: &Path, entries: &[(String, String)]) -> io::Result<UidCacheUpdate> {
    let file = uid_cache_file(project);
    let bytes = match fs::read(&file) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(UidCacheUpdate::Missing);
        }
        Err(error) => return Err(error),
    };
    let mut cache = parse(&bytes).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{} does not parse as a Godot uid cache", file.display()),
        )
    })?;
    let mut changed = 0;
    for (path, uid) in entries {
        let Some(id) = uid_id(uid) else {
            continue;
        };
        let path = path.as_bytes();
        let mut matching = cache
            .iter()
            .filter(|(known, known_path)| *known == id || known_path == path);
        let first = matching.next();
        if first.is_some_and(|(known, known_path)| *known == id && known_path == path)
            && matching.next().is_none()
        {
            continue;
        }
        cache.retain(|(known, known_path)| *known != id && known_path != path);
        cache.push((id, path.to_vec()));
        changed += 1;
    }
    if changed == 0 {
        return Ok(UidCacheUpdate::Recorded(0));
    }
    let temporary = file.with_extension("bin.godot-mcp");
    fs::write(&temporary, encode(&cache)?)?;
    if let Err(error) = fs::rename(&temporary, &file) {
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }
    Ok(UidCacheUpdate::Recorded(changed))
}

/// The uid the cache gives each `res://` path, as Godot resolves it: when an id appears twice the
/// last entry wins, and when a path has two ids the later entry wins. Empty when the project has
/// no cache or it does not parse.
pub(super) fn uids_by_path(project: &Path) -> BTreeMap<String, String> {
    let Some(entries) = fs::read(uid_cache_file(project))
        .ok()
        .and_then(|bytes| parse(&bytes))
    else {
        return BTreeMap::new();
    };
    let mut last_of_id: BTreeMap<u64, usize> = BTreeMap::new();
    for (index, (id, _)) in entries.iter().enumerate() {
        last_of_id.insert(*id, index);
    }
    let mut by_path = BTreeMap::new();
    for (index, (id, path)) in entries.iter().enumerate() {
        if last_of_id.get(id) != Some(&index) {
            continue;
        }
        if let Ok(path) = String::from_utf8(path.clone()) {
            by_path.insert(path, uid_text(*id));
        }
    }
    by_path
}

/// The entries of a cache file in their order, or `None` when it is cut short or has bytes left.
fn parse(bytes: &[u8]) -> Option<Vec<(u64, Vec<u8>)>> {
    let mut rest = bytes;
    let mut take = |count: usize| -> Option<&[u8]> {
        if rest.len() < count {
            return None;
        }
        let (head, tail) = rest.split_at(count);
        rest = tail;
        Some(head)
    };
    let count = u32::from_le_bytes(take(4)?.try_into().ok()?);
    let mut entries = Vec::new();
    for _ in 0..count {
        let id = u64::from_le_bytes(take(8)?.try_into().ok()?);
        let length = u32::from_le_bytes(take(4)?.try_into().ok()?);
        let path = take(usize::try_from(length).ok()?)?.to_vec();
        entries.push((id, path));
    }
    rest.is_empty().then_some(entries)
}

fn encode(entries: &[(u64, Vec<u8>)]) -> io::Result<Vec<u8>> {
    let too_large = || io::Error::new(io::ErrorKind::InvalidData, "uid cache entry too large");
    let mut bytes = Vec::new();
    bytes.extend_from_slice(
        &u32::try_from(entries.len())
            .map_err(|_| too_large())?
            .to_le_bytes(),
    );
    for (id, path) in entries {
        bytes.extend_from_slice(&id.to_le_bytes());
        bytes.extend_from_slice(
            &u32::try_from(path.len())
                .map_err(|_| too_large())?
                .to_le_bytes(),
        );
        bytes.extend_from_slice(path);
    }
    Ok(bytes)
}

/// The id of a `uid://` text, as Godot's `ResourceUID::text_to_id` reads it: base 34, `a`-`z`
/// count from 0 and `0`-`9` from 25, and the result keeps 63 bits.
pub(super) fn uid_id(text: &str) -> Option<u64> {
    let digits = text.strip_prefix("uid://")?;
    if digits.is_empty() {
        return None;
    }
    let mut id: u64 = 0;
    for byte in digits.bytes() {
        let digit = match byte {
            b'a'..=b'z' => byte - b'a',
            b'0'..=b'9' => byte - b'0' + 25,
            _ => return None,
        };
        id = id.wrapping_mul(34).wrapping_add(u64::from(digit));
    }
    Some(id & 0x7FFF_FFFF_FFFF_FFFF)
}

#[cfg(test)]
mod tests {
    use super::super::binary_resource::uid_text;
    use super::*;

    fn cache_with(project: &Path, entries: &[(u64, &str)]) {
        fs::create_dir_all(project.join(".godot")).expect("dir");
        let entries: Vec<(u64, Vec<u8>)> = entries
            .iter()
            .map(|(id, path)| (*id, path.as_bytes().to_vec()))
            .collect();
        fs::write(uid_cache_file(project), encode(&entries).expect("encode")).expect("cache");
    }

    fn cache_of(project: &Path) -> Vec<(u64, String)> {
        parse(&fs::read(uid_cache_file(project)).expect("cache"))
            .expect("parses")
            .into_iter()
            .map(|(id, path)| (id, String::from_utf8(path).expect("utf8")))
            .collect()
    }

    #[test]
    fn uid_id_reads_what_uid_text_writes() {
        for id in [
            0,
            1,
            33,
            34,
            5_610_405_182_049_170_494,
            0x7FFF_FFFF_FFFF_FFFF,
        ] {
            assert_eq!(uid_id(&uid_text(id)), Some(id));
        }
        // ResourceUID.text_to_id("uid://cl6ys3a78gc43") in Godot 4.8.
        assert_eq!(
            uid_id("uid://cl6ys3a78gc43"),
            Some(5_610_405_182_049_170_494)
        );
        assert_eq!(uid_id("uid://"), None);
        assert_eq!(uid_id("uid://<invalid>"), None);
        assert_eq!(uid_id("res://a.tres"), None);
    }

    #[test]
    fn uids_by_path_follow_the_last_entry_of_an_id() {
        let dir = tempfile::tempdir().expect("project");
        let project = dir.path();
        assert!(uids_by_path(project).is_empty());
        cache_with(
            project,
            &[
                (7, "res://moved_from.tscn"),
                (9, "res://kept.res"),
                (7, "res://moved_to.tscn"),
            ],
        );
        let by_path = uids_by_path(project);
        assert_eq!(by_path.len(), 2, "{by_path:?}");
        assert_eq!(by_path["res://kept.res"], uid_text(9));
        assert_eq!(by_path["res://moved_to.tscn"], uid_text(7));
    }

    #[test]
    fn record_adds_new_uids_and_keeps_the_rest() {
        let dir = tempfile::tempdir().expect("dir");
        let root = dir.path();
        cache_with(root, &[(7, "res://seed.tres"), (8, "res://icon.svg")]);
        let entries = vec![
            ("res://zones/zone.tscn".to_string(), uid_text(100)),
            ("res://zones/mesh.res".to_string(), uid_text(101)),
            ("res://seed.tres".to_string(), uid_text(7)),
        ];
        assert_eq!(
            record(root, &entries).expect("record"),
            UidCacheUpdate::Recorded(2)
        );
        assert_eq!(
            cache_of(root),
            vec![
                (7, "res://seed.tres".to_string()),
                (8, "res://icon.svg".to_string()),
                (100, "res://zones/zone.tscn".to_string()),
                (101, "res://zones/mesh.res".to_string()),
            ]
        );
        assert_eq!(
            record(root, &entries).expect("again"),
            UidCacheUpdate::Recorded(0)
        );
    }

    #[test]
    fn record_moves_an_id_and_drops_the_old_id_of_a_path() {
        let dir = tempfile::tempdir().expect("dir");
        let root = dir.path();
        cache_with(root, &[(1, "res://old_place.tres"), (2, "res://zone.tscn")]);
        let entries = vec![
            ("res://new_place.tres".to_string(), uid_text(1)),
            ("res://zone.tscn".to_string(), uid_text(3)),
        ];
        assert_eq!(
            record(root, &entries).expect("record"),
            UidCacheUpdate::Recorded(2)
        );
        assert_eq!(
            cache_of(root),
            vec![
                (1, "res://new_place.tres".to_string()),
                (3, "res://zone.tscn".to_string()),
            ]
        );
    }

    #[test]
    fn a_missing_cache_is_not_created_and_a_broken_one_is_left_alone() {
        let dir = tempfile::tempdir().expect("dir");
        let root = dir.path();
        let entries = vec![("res://a.tres".to_string(), uid_text(5))];
        assert_eq!(
            record(root, &entries).expect("missing"),
            UidCacheUpdate::Missing
        );
        assert!(!uid_cache_file(root).exists());

        fs::create_dir_all(root.join(".godot")).expect("dir");
        fs::write(uid_cache_file(root), [3, 0, 0, 0, 1]).expect("broken");
        let error = record(root, &entries).expect_err("broken cache");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert_eq!(
            fs::read(uid_cache_file(root)).expect("kept"),
            vec![3, 0, 0, 0, 1]
        );
    }
}
