//! Text scenes and resources a headless script rewrote, and what the rewrite dropped.
//!
//! A `godot --headless -s` script that saves a `.tscn` or `.tres` runs without the editor's file
//! system, and Godot's text saver then writes no `uid=` at all: neither the file's own nor any
//! `[ext_resource]`'s. Without a renderer, a `ShaderMaterial` also reports `null` for every uniform
//! it never set, and the saver writes `shader_parameter/<name> = null`. Both are engine behaviour:
//! the saver asks for uids through a callback only the editor's file system installs. This module
//! finds them after the run, writes back what the editor would have written, and reports what is
//! still lost.

use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::paths::skipped_by_godot;

/// Files larger than this are only fingerprinted, never read.
const MAX_READ: u64 = 16 * 1024 * 1024;

/// One text scene or resource before the run.
#[derive(Debug, Clone)]
struct Before {
    len: u64,
    modified: Option<SystemTime>,
    uid: Option<String>,
    ext_uids: BTreeMap<String, String>,
}

/// The `.tscn` and `.tres` files of a project, keyed by `res://` path.
#[derive(Debug, Clone, Default)]
pub struct TextResourceSnapshot {
    files: BTreeMap<String, Before>,
}

/// What a rewrite of one file lost.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceDamage {
    /// `res://` path of the rewritten file.
    pub path: String,
    /// The file's own `uid://` that its header no longer carries.
    pub lost_uid: Option<String>,
    /// `(target path, uid)` of each `[ext_resource]` written without the uid its target has.
    pub ext_resources_without_uid: Vec<(String, String)>,
    /// Shader parameters written as `null`.
    pub null_shader_parameters: Vec<String>,
}

/// What a repair wrote back into one rewritten file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceRepair {
    /// `res://` path of the repaired file.
    pub path: String,
    /// The uid written into the file's header: the one it had before the run, or a new one for a
    /// file the run created.
    pub uid: Option<String>,
    /// Whether `uid` was created for a file that had none before the run.
    pub new_uid: bool,
    /// `(target path, uid)` of each `[ext_resource]` that got its uid back.
    pub ext_resource_uids: Vec<(String, String)>,
    /// Shader parameters whose `null` line was removed (an unset uniform reads its default).
    pub removed_null_shader_parameters: Vec<String>,
}

impl TextResourceSnapshot {
    /// Reads the header and the `[ext_resource]` lines of every text scene and resource.
    pub fn take(project: &Path) -> Self {
        let mut files = BTreeMap::new();
        walk(project, project, &mut |res, file| {
            let Ok(meta) = fs::metadata(file) else {
                return;
            };
            let (uid, ext_uids) = read_head(file).unwrap_or_default();
            files.insert(
                res,
                Before {
                    len: meta.len(),
                    modified: meta.modified().ok(),
                    uid,
                    ext_uids,
                },
            );
        });
        Self { files }
    }

    /// Text scenes and resources written since [`TextResourceSnapshot::take`] that lost a uid or
    /// gained a `null` shader parameter. Sorted by path.
    pub fn damage_since(&self, project: &Path) -> Vec<ResourceDamage> {
        let mut found = Vec::new();
        for (res, _, text) in self.changed_since(project) {
            if let Some(damage) = inspect(project, &res, &text, self.files.get(&res)) {
                found.push(damage);
            }
        }
        found.sort_by(|a, b| a.path.cmp(&b.path));
        found
    }

    /// Writes back into each text scene and resource rewritten since
    /// [`TextResourceSnapshot::take`] what the editor's saver would have written: the file's own
    /// uid (the one it had, or a new one for a new file), the uid of each `[ext_resource]` whose
    /// target has one, and no `shader_parameter/<name> = null` lines. A file that cannot be
    /// written is left as it is, so [`TextResourceSnapshot::damage_since`] still reports it.
    /// Sorted by path.
    pub fn repair_since(&self, project: &Path) -> Vec<ResourceRepair> {
        let changed = self.changed_since(project);
        // Every header uid first, so a scene that references another rewritten file gets that
        // file's uid even though its header on disk has none yet.
        let mut own: BTreeMap<String, (String, bool)> = BTreeMap::new();
        for (res, _, text) in &changed {
            let header = text.lines().next().unwrap_or("");
            if !is_resource_header(header) || attribute(header, "uid").is_some() {
                continue;
            }
            let entry = match self.files.get(res).and_then(|old| old.uid.clone()) {
                Some(uid) => (uid, false),
                None => (new_uid(), true),
            };
            own.insert(res.clone(), entry);
        }
        let mut repairs = Vec::new();
        for (res, file, text) in &changed {
            let before = self.files.get(res);
            let mut repair = ResourceRepair {
                path: res.clone(),
                uid: None,
                new_uid: false,
                ext_resource_uids: Vec::new(),
                removed_null_shader_parameters: Vec::new(),
            };
            let mut out = String::with_capacity(text.len() + 256);
            for (index, line) in text.split_inclusive('\n').enumerate() {
                let body = line.trim_end_matches(['\r', '\n']);
                let ending = &line[body.len()..];
                if index == 0 {
                    if let (Some((uid, fresh)), Some(open)) = (own.get(res), body.strip_suffix(']'))
                    {
                        out.push_str(&format!("{open} uid=\"{uid}\"]{ending}"));
                        repair.uid = Some(uid.clone());
                        repair.new_uid = *fresh;
                        continue;
                    }
                } else if body.starts_with("[ext_resource") && attribute(body, "uid").is_none() {
                    let known = attribute(body, "path").and_then(|target| {
                        own.get(&target)
                            .map(|(uid, _)| uid.clone())
                            .or_else(|| uid_of(project, &target))
                            .or_else(|| before.and_then(|old| old.ext_uids.get(&target).cloned()))
                            .filter(|uid| is_uid_text(uid))
                            .map(|uid| (target, uid))
                    });
                    if let (Some((target, uid)), Some(at)) = (known, body.find(" path=\"")) {
                        out.push_str(&format!(
                            "{} uid=\"{uid}\"{}{ending}",
                            &body[..at],
                            &body[at..]
                        ));
                        repair.ext_resource_uids.push((target, uid));
                        continue;
                    }
                } else if let Some(name) = null_shader_parameter(body) {
                    repair.removed_null_shader_parameters.push(name);
                    continue;
                }
                out.push_str(line);
            }
            if out != *text && fs::write(file, &out).is_ok() {
                repairs.push(repair);
            }
        }
        repairs.sort_by(|a, b| a.path.cmp(&b.path));
        repairs
    }

    /// `(res path, file, text)` of each text scene and resource whose size or modification time
    /// changed since the snapshot, or that is new. Files over [`MAX_READ`] are skipped.
    fn changed_since(&self, project: &Path) -> Vec<(String, PathBuf, String)> {
        let mut changed = Vec::new();
        walk(project, project, &mut |res, file| {
            let Ok(meta) = fs::metadata(file) else {
                return;
            };
            if self.files.get(&res).is_some_and(|old| {
                old.len == meta.len()
                    && old.modified.is_some()
                    && old.modified == meta.modified().ok()
            }) {
                return;
            }
            if meta.len() > MAX_READ {
                return;
            }
            let Ok(text) = fs::read_to_string(file) else {
                return;
            };
            changed.push((res, file.to_path_buf(), text));
        });
        changed
    }
}

fn inspect(
    project: &Path,
    res: &str,
    text: &str,
    before: Option<&Before>,
) -> Option<ResourceDamage> {
    let header = text.lines().next().unwrap_or("");
    let lost_uid = before
        .and_then(|old| old.uid.clone())
        .filter(|_| attribute(header, "uid").is_none());
    let mut ext_resources_without_uid = Vec::new();
    let mut null_shader_parameters = Vec::new();
    for line in text.lines() {
        if line.starts_with("[ext_resource") {
            if attribute(line, "uid").is_some() {
                continue;
            }
            let Some(target) = attribute(line, "path") else {
                continue;
            };
            let known = before
                .and_then(|old| old.ext_uids.get(&target).cloned())
                .or_else(|| uid_of(project, &target));
            if let Some(uid) = known {
                ext_resources_without_uid.push((target, uid));
            }
        } else if let Some(name) = null_shader_parameter(line) {
            null_shader_parameters.push(name);
        }
    }
    if lost_uid.is_none()
        && ext_resources_without_uid.is_empty()
        && null_shader_parameters.is_empty()
    {
        return None;
    }
    Some(ResourceDamage {
        path: res.to_string(),
        lost_uid,
        ext_resources_without_uid,
        null_shader_parameters,
    })
}

/// The uid Godot keeps for `target`: its `.uid` sidecar, the `uid=` of its `.import` file, or the
/// header of a text scene or resource.
fn uid_of(project: &Path, target: &str) -> Option<String> {
    let relative = target.strip_prefix("res://")?;
    if relative.is_empty()
        || relative
            .split('/')
            .any(|part| part.is_empty() || part == "..")
    {
        return None;
    }
    let file = project.join(relative);
    let name = file.file_name()?.to_str()?.to_string();
    if let Some(text) = read_small(&file.with_file_name(format!("{name}.uid"))) {
        let uid = text.trim();
        if uid.starts_with("uid://") {
            return Some(uid.to_string());
        }
    }
    if let Some(text) = read_small(&file.with_file_name(format!("{name}.import"))) {
        for line in text.lines() {
            if let Some(value) = line.trim().strip_prefix("uid=") {
                let uid = value.trim().trim_matches('"');
                if uid.starts_with("uid://") {
                    return Some(uid.to_string());
                }
            }
        }
    }
    if is_text_resource(&name) {
        return read_head(&file).and_then(|(uid, _)| uid);
    }
    None
}

/// The name of a `shader_parameter/<name> = null` line.
fn null_shader_parameter(line: &str) -> Option<String> {
    let (name, value) = line.strip_prefix("shader_parameter/")?.split_once('=')?;
    (value.trim() == "null").then(|| name.trim().to_string())
}

fn is_resource_header(line: &str) -> bool {
    line.starts_with("[gd_scene") || line.starts_with("[gd_resource")
}

/// `uid://` followed by the characters Godot's uid text uses.
fn is_uid_text(text: &str) -> bool {
    text.strip_prefix("uid://").is_some_and(|rest| {
        !rest.is_empty()
            && rest
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
    })
}

/// A new uid as Godot's `ResourceUID::create_id` makes one: 63 random bits, written with its
/// `id_to_text` alphabet (`a`-`y`, then `0`-`8`), most significant digit first.
fn new_uid() -> String {
    let bits = uuid::Uuid::new_v4().as_u128();
    uid_text(((bits >> 64) as u64 ^ bits as u64) & 0x7FFF_FFFF_FFFF_FFFF)
}

fn uid_text(mut id: u64) -> String {
    const DIGITS: &[u8; 34] = b"abcdefghijklmnopqrstuvwxy012345678";
    let mut digits = Vec::new();
    loop {
        digits.push(DIGITS[(id % 34) as usize]);
        id /= 34;
        if id == 0 {
            break;
        }
    }
    digits.reverse();
    format!("uid://{}", String::from_utf8_lossy(&digits))
}

fn read_small(path: &Path) -> Option<String> {
    let meta = fs::metadata(path).ok()?;
    if meta.len() > 64 * 1024 {
        return None;
    }
    fs::read_to_string(path).ok()
}

/// The header uid and the `path -> uid` of each `[ext_resource]`, which Godot writes before any
/// other section; reading stops at the first other section.
fn read_head(file: &Path) -> Option<(Option<String>, BTreeMap<String, String>)> {
    let reader = BufReader::new(fs::File::open(file).ok()?);
    let mut lines = reader.lines();
    let header = lines.next()?.ok()?;
    if !header.starts_with("[gd_") {
        return None;
    }
    let mut ext_uids = BTreeMap::new();
    for line in lines {
        let Ok(line) = line else {
            break;
        };
        if line.starts_with("[ext_resource") {
            if let (Some(path), Some(uid)) = (attribute(&line, "path"), attribute(&line, "uid")) {
                ext_uids.insert(path, uid);
            }
        } else if line.starts_with('[') {
            break;
        }
    }
    Some((attribute(&header, "uid"), ext_uids))
}

/// The quoted value of `key="..."` in a section header line.
fn attribute(line: &str, key: &str) -> Option<String> {
    let needle = format!(" {key}=\"");
    let start = line.find(&needle)? + needle.len();
    let end = line[start..].find('"')?;
    Some(line[start..start + end].to_string())
}

fn is_text_resource(name: &str) -> bool {
    name.ends_with(".tscn") || name.ends_with(".tres")
}

fn walk(project: &Path, dir: &Path, visit: &mut dyn FnMut(String, &Path)) {
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
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if skipped_by_godot(&path, kind.is_dir()) {
            continue;
        }
        if kind.is_dir() {
            walk(project, &path, visit);
            continue;
        }
        if !kind.is_file() || !is_text_resource(name) {
            continue;
        }
        let Ok(relative) = path.strip_prefix(project) else {
            continue;
        };
        let Some(relative) = relative.to_str() else {
            continue;
        };
        visit(format!("res://{}", relative.replace('\\', "/")), &path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EDITOR_SAVED: &str = r#"[gd_scene load_steps=3 format=3 uid="uid://cpond"]

[ext_resource type="Shader" uid="uid://dshader" path="res://water.gdshader" id="1_a"]
[ext_resource type="Texture2D" path="res://plain.png" id="2_b"]

[sub_resource type="ShaderMaterial" id="ShaderMaterial_a"]
shader = ExtResource("1_a")
shader_parameter/speed = 2.0

[node name="Pond" type="Node3D"]
"#;

    const HEADLESS_SAVED: &str = r#"[gd_scene format=3]

[ext_resource type="Shader" path="res://water.gdshader" id="1_a"]
[ext_resource type="Texture2D" path="res://plain.png" id="2_b"]
[ext_resource type="PackedScene" path="res://frog.tscn" id="3_c"]

[sub_resource type="ShaderMaterial" id="ShaderMaterial_a"]
shader = ExtResource("1_a")
shader_parameter/tint = null
shader_parameter/speed = 3.0

[node name="Pond" type="Node3D" unique_id=1]
"#;

    #[test]
    fn names_the_uids_and_null_parameters_a_headless_save_dropped() {
        let dir = tempfile::tempdir().expect("project");
        let root = dir.path();
        fs::write(root.join("pond.tscn"), EDITOR_SAVED).expect("scene");
        fs::write(root.join("untouched.tres"), "[gd_resource format=3]\n").expect("resource");
        fs::write(
            root.join("frog.tscn"),
            "[gd_scene format=3 uid=\"uid://bfrog\"]\n",
        )
        .expect("frog");
        let snapshot = TextResourceSnapshot::take(root);
        assert!(snapshot.damage_since(root).is_empty());

        fs::write(root.join("pond.tscn"), HEADLESS_SAVED).expect("rewrite");
        let damage = snapshot.damage_since(root);
        assert_eq!(
            damage,
            vec![ResourceDamage {
                path: "res://pond.tscn".into(),
                lost_uid: Some("uid://cpond".into()),
                // plain.png had no uid before and has no sidecar, so nothing was lost there.
                ext_resources_without_uid: vec![
                    ("res://water.gdshader".into(), "uid://dshader".into()),
                    ("res://frog.tscn".into(), "uid://bfrog".into()),
                ],
                null_shader_parameters: vec!["tint".into()],
            }]
        );
    }

    #[test]
    fn a_new_file_is_checked_against_the_uid_sidecars_of_its_targets() {
        let dir = tempfile::tempdir().expect("project");
        let root = dir.path();
        fs::create_dir_all(root.join("art")).expect("art");
        fs::write(root.join("art/water.gdshader.uid"), "uid://dshader\n").expect("uid");
        fs::write(
            root.join("art/rock.png.import"),
            "[remap]\n\nimporter=\"texture\"\nuid=\"uid://brock\"\n",
        )
        .expect("import");
        let snapshot = TextResourceSnapshot::take(root);
        fs::write(
            root.join("art/mat.tres"),
            "[gd_resource type=\"ShaderMaterial\" format=3]\n\n[ext_resource type=\"Shader\" path=\"res://art/water.gdshader\" id=\"1\"]\n[ext_resource type=\"Texture2D\" path=\"res://art/rock.png\" id=\"2\"]\n[ext_resource type=\"Texture2D\" uid=\"uid://ok\" path=\"res://art/ok.png\" id=\"3\"]\n",
        )
        .expect("new resource");
        let damage = snapshot.damage_since(root);
        assert_eq!(damage.len(), 1, "{damage:?}");
        assert_eq!(damage[0].path, "res://art/mat.tres");
        assert_eq!(damage[0].lost_uid, None);
        assert_eq!(
            damage[0].ext_resources_without_uid,
            vec![
                ("res://art/water.gdshader".into(), "uid://dshader".into()),
                ("res://art/rock.png".into(), "uid://brock".into()),
            ]
        );
    }

    #[test]
    fn a_save_that_keeps_every_uid_is_not_reported() {
        let dir = tempfile::tempdir().expect("project");
        let root = dir.path();
        fs::write(root.join("pond.tscn"), EDITOR_SAVED).expect("scene");
        let snapshot = TextResourceSnapshot::take(root);
        fs::write(
            root.join("pond.tscn"),
            EDITOR_SAVED.replace("speed = 2.0", "speed = 3.0\n"),
        )
        .expect("rewrite");
        assert!(snapshot.damage_since(root).is_empty());
    }

    #[test]
    fn repair_puts_back_what_the_editor_would_have_written() {
        let dir = tempfile::tempdir().expect("project");
        let root = dir.path();
        fs::write(root.join("pond.tscn"), EDITOR_SAVED).expect("scene");
        fs::write(
            root.join("frog.tscn"),
            "[gd_scene format=3 uid=\"uid://bfrog\"]\n",
        )
        .expect("frog");
        let snapshot = TextResourceSnapshot::take(root);
        fs::write(root.join("pond.tscn"), HEADLESS_SAVED.replace('\n', "\r\n")).expect("rewrite");

        let repairs = snapshot.repair_since(root);
        assert_eq!(
            repairs,
            vec![ResourceRepair {
                path: "res://pond.tscn".into(),
                uid: Some("uid://cpond".into()),
                new_uid: false,
                ext_resource_uids: vec![
                    ("res://water.gdshader".into(), "uid://dshader".into()),
                    ("res://frog.tscn".into(), "uid://bfrog".into()),
                ],
                removed_null_shader_parameters: vec!["tint".into()],
            }]
        );
        let repaired = fs::read_to_string(root.join("pond.tscn")).expect("repaired");
        assert_eq!(
            repaired,
            HEADLESS_SAVED
                .replace(
                    "[gd_scene format=3]",
                    "[gd_scene format=3 uid=\"uid://cpond\"]"
                )
                .replace(
                    "type=\"Shader\" path",
                    "type=\"Shader\" uid=\"uid://dshader\" path"
                )
                .replace(
                    "type=\"PackedScene\" path",
                    "type=\"PackedScene\" uid=\"uid://bfrog\" path"
                )
                .replace("shader_parameter/tint = null\n", "")
                .replace('\n', "\r\n")
        );
        assert!(snapshot.damage_since(root).is_empty());
        // A second pass finds nothing left to do.
        assert!(snapshot.repair_since(root).is_empty());
    }

    #[test]
    fn repair_gives_new_files_a_uid_that_their_references_share() {
        let dir = tempfile::tempdir().expect("project");
        let root = dir.path();
        fs::write(root.join("water.gdshader.uid"), "uid://dshader\n").expect("uid");
        let snapshot = TextResourceSnapshot::take(root);
        fs::write(
            root.join("mat.tres"),
            "[gd_resource type=\"ShaderMaterial\" format=3]\n\n[ext_resource type=\"Shader\" path=\"res://water.gdshader\" id=\"1\"]\n\n[resource]\nshader = ExtResource(\"1\")\n",
        )
        .expect("material");
        fs::write(
            root.join("level.tscn"),
            "[gd_scene format=3]\n\n[ext_resource type=\"Material\" path=\"res://mat.tres\" id=\"1\"]\n[ext_resource type=\"Texture2D\" path=\"res://plain.png\" id=\"2\"]\n\n[node name=\"Level\" type=\"Node3D\"]\n",
        )
        .expect("level");

        let repairs = snapshot.repair_since(root);
        assert_eq!(repairs.len(), 2, "{repairs:?}");
        let (level, material) = (&repairs[0], &repairs[1]);
        let material_uid = material.uid.clone().expect("material uid");
        assert!(
            material.new_uid && is_uid_text(&material_uid),
            "{material:?}"
        );
        assert_eq!(
            material.ext_resource_uids,
            vec![("res://water.gdshader".into(), "uid://dshader".into())]
        );
        assert!(level.new_uid && level.uid != material.uid, "{level:?}");
        // plain.png has no uid anywhere, so its line stays as the script wrote it.
        assert_eq!(
            level.ext_resource_uids,
            vec![("res://mat.tres".into(), material_uid.clone())]
        );
        let text = fs::read_to_string(root.join("level.tscn")).expect("level");
        assert!(
            text.contains(&format!("uid=\"{material_uid}\" path=\"res://mat.tres\""))
                && text.contains(
                    "[ext_resource type=\"Texture2D\" path=\"res://plain.png\" id=\"2\"]"
                ),
            "{text}"
        );
        assert_eq!(
            read_head(&root.join("mat.tres")).and_then(|(uid, _)| uid),
            Some(material_uid)
        );
    }

    #[test]
    fn uid_text_matches_godot() {
        // ResourceUID.id_to_text(5610405182049170494) in Godot 4.8.
        assert_eq!(uid_text(5_610_405_182_049_170_494), "uid://cl6ys3a78gc43");
        assert_eq!(uid_text(0x7FFF_FFFF_FFFF_FFFF), "uid://d4n4ub6itg400");
        assert_eq!(uid_text(0), "uid://a");
    }

    #[test]
    fn folders_godot_does_not_scan_are_not_audited() {
        let dir = tempfile::tempdir().expect("project");
        let root = dir.path();
        for folder in [".hidden", "docs", "nested"] {
            fs::create_dir(root.join(folder)).expect("mkdir");
            fs::write(root.join(folder).join("pond.tscn"), EDITOR_SAVED).expect("scene");
        }
        fs::write(root.join("docs/.gdignore"), "").expect("gdignore");
        fs::write(root.join("nested/project.godot"), "").expect("nested");
        let snapshot = TextResourceSnapshot::take(root);
        assert!(snapshot.files.is_empty());
        for folder in [".hidden", "docs", "nested"] {
            fs::write(root.join(folder).join("pond.tscn"), HEADLESS_SAVED).expect("rewrite");
        }
        assert!(snapshot.damage_since(root).is_empty());
    }
}
