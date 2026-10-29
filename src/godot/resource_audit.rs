//! Text scenes and resources a headless script rewrote, and what the rewrite dropped.
//!
//! A `godot --headless -s` script that saves a `.tscn` or `.tres` runs without the editor's file
//! system, and Godot's text saver then writes no `uid=` at all: neither the file's own nor any
//! `[ext_resource]`'s. Without a renderer, a `ShaderMaterial` also reports `null` for every uniform
//! it never set, and the saver writes `shader_parameter/<name> = null`. Both are engine behaviour;
//! this module only finds them after the run so the answer can say what was lost.

use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::time::SystemTime;

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
        walk(project, project, &mut |res, file| {
            let Ok(meta) = fs::metadata(file) else {
                return;
            };
            let before = self.files.get(&res);
            if before.is_some_and(|old| {
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
            if let Some(damage) = inspect(project, &res, &text, before) {
                found.push(damage);
            }
        });
        found.sort_by(|a, b| a.path.cmp(&b.path));
        found
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
        } else if let Some(rest) = line.strip_prefix("shader_parameter/") {
            if let Some((name, value)) = rest.split_once('=') {
                if value.trim() == "null" {
                    null_shader_parameters.push(name.trim().to_string());
                }
            }
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
        if kind.is_dir() {
            if !matches!(name, ".godot" | ".git") {
                walk(project, &path, visit);
            }
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
}
