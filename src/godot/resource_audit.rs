//! Text scenes and resources a headless script rewrote, and what the rewrite dropped.
//!
//! A `godot --headless -s` script that saves a `.tscn` or `.tres` runs without the editor's file
//! system, and Godot's text saver then writes no `uid=` at all: neither the file's own nor any
//! `[ext_resource]`'s. Without a renderer, a `ShaderMaterial` also reports `null` for every uniform
//! it never set, and the saver writes `shader_parameter/<name> = null`. Both are engine behaviour:
//! the saver asks for uids through a callback only the editor's file system installs. This module
//! finds them after the run, writes back what the editor would have written, and reports what is
//! still lost.
//!
//! Some data cannot be written back at all: a `MultiMesh` keeps its instance transforms and
//! colors only in the rendering server, which a headless Godot replaces with a dummy, so the
//! saver writes its `instance_count` and no `buffer`. Such a file loads with every instance at
//! the origin, scaled to zero. [`ResourceDamage::multimeshes_without_buffer`] names them.
//!
//! The binary saver (`.scn`, `.res`) drops the uid the same way. Its header is read here (see
//! `binary_resource`), and the uid is written back by Godot itself through `ResourceSaver.set_uid`,
//! so a compressed file stays compressed: [`TextResourceSnapshot::binary_uid_plan`] says which
//! files need it and [`TextResourceSnapshot::binary_repairs`] checks what took.

use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use super::binary_resource::{BinaryUid, binary_uid, uid_text};
use super::uid_cache::uids_by_path;
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

/// The `.tscn` and `.tres` files of a project, keyed by `res://` path, and a fingerprint of the
/// other files the editor loads as resources from their text: scripts and shaders.
#[derive(Debug, Clone, Default)]
pub struct TextResourceSnapshot {
    files: BTreeMap<String, Before>,
    loaded: BTreeMap<String, (u64, Option<SystemTime>)>,
    binaries: BTreeMap<String, BinaryBefore>,
    /// The uid the project's uid cache gives each path whose header carries no uid, unless
    /// another file's header carries that uid. A file another run rewrote and left without its
    /// uid (one that was stopped before its repair, or one in a parallel session) still has its
    /// uid there, and every `uid://` reference to it uses that one.
    cached: BTreeMap<String, String>,
}

/// One binary scene or resource before the run.
#[derive(Debug, Clone)]
struct BinaryBefore {
    len: u64,
    modified: Option<SystemTime>,
    uid: BinaryUid,
}

/// A uid to write into a binary scene or resource the run rewrote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BinaryUidFix {
    /// `res://` path of the file.
    pub path: String,
    /// The uid it had before the run (its header's, or the project's uid cache's when the header
    /// had none), or a new one for a file that had none.
    pub uid: String,
    /// Whether `uid` was created for a file that had none before the run.
    pub new_uid: bool,
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
    /// `(id, instance_count)` of each `MultiMesh` written with instances but without their
    /// `buffer`: the `[sub_resource]` id, or `resource` for the file's own `[resource]`.
    pub multimeshes_without_buffer: Vec<(String, u64)>,
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
    /// Reads the header and the `[ext_resource]` lines of every text scene and resource, the
    /// header uid of every binary one, and the size and time of every script and shader, in one
    /// walk of the project.
    pub fn take(project: &Path) -> Self {
        let mut files = BTreeMap::new();
        let mut loaded = BTreeMap::new();
        let mut binaries = BTreeMap::new();
        walk_project(project, is_audited, &mut |res, file, name| {
            let Ok(meta) = fs::metadata(file) else {
                return;
            };
            if is_text_resource(name) {
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
            } else if is_loaded_text(name) {
                loaded.insert(res, (meta.len(), meta.modified().ok()));
            } else {
                binaries.insert(
                    res,
                    BinaryBefore {
                        len: meta.len(),
                        modified: meta.modified().ok(),
                        uid: binary_uid(file),
                    },
                );
            }
        });
        let cached = cached_uids(project, &files, &binaries);
        Self {
            files,
            loaded,
            binaries,
            cached,
        }
    }

    /// The files written or created since the snapshot, found in one walk of the project. Only
    /// sizes and times are read here; [`ProjectChanges::read_texts`] reads the text files.
    pub fn changes_since(&self, project: &Path) -> ProjectChanges {
        self.changes_among(project, is_audited)
    }

    /// [`TextResourceSnapshot::changes_since`] among the files whose name is `wanted`.
    fn changes_among(&self, project: &Path, wanted: fn(&str) -> bool) -> ProjectChanges {
        let mut changes = ProjectChanges::default();
        walk_project(project, wanted, &mut |res, file, name| {
            let Ok(meta) = fs::metadata(file) else {
                return;
            };
            let same = |len: u64, modified: Option<SystemTime>| {
                len == meta.len() && modified.is_some() && modified == meta.modified().ok()
            };
            if is_text_resource(name) {
                let unchanged = self
                    .files
                    .get(&res)
                    .is_some_and(|old| same(old.len, old.modified));
                if !unchanged && meta.len() <= MAX_READ {
                    changes.text.push((res, file.to_path_buf()));
                }
            } else if is_loaded_text(name) {
                let unchanged = self
                    .loaded
                    .get(&res)
                    .is_some_and(|(len, modified)| same(*len, *modified));
                if !unchanged {
                    changes.loaded.push(res);
                }
            } else {
                let unchanged = self
                    .binaries
                    .get(&res)
                    .is_some_and(|old| same(old.len, old.modified));
                if !unchanged {
                    changes.binaries.push((res, file.to_path_buf()));
                }
            }
        });
        changes
    }

    /// The uid `res` had before the run: its header's, or the uid cache's when the header had
    /// none (or the file did not exist).
    fn uid_before(&self, res: &str) -> Option<String> {
        let header = match self.binaries.get(res).map(|old| &old.uid) {
            Some(BinaryUid::Uid(uid)) => Some(uid.clone()),
            Some(_) => None,
            None => self.files.get(res).and_then(|old| old.uid.clone()),
        };
        header.or_else(|| self.cached.get(res).cloned())
    }

    /// The uid each binary scene or resource rewritten since the snapshot needs back: the one its
    /// header had, when the rewrite dropped or changed it, or a new one for a file without any.
    /// A header this server cannot read is left alone. Sorted by path.
    pub fn binary_uid_plan(&self, project: &Path) -> Vec<BinaryUidFix> {
        self.binary_uid_plan_of(&self.changes_since(project))
    }

    /// [`TextResourceSnapshot::binary_uid_plan`] for changes already found.
    pub fn binary_uid_plan_of(&self, changes: &ProjectChanges) -> Vec<BinaryUidFix> {
        let mut plan = Vec::new();
        for (res, file) in &changes.binaries {
            let now = binary_uid(file);
            if now == BinaryUid::Unreadable {
                continue;
            }
            if self
                .binaries
                .get(res)
                .is_some_and(|old| old.uid == BinaryUid::Unreadable)
            {
                continue;
            }
            let fix = match self.uid_before(res) {
                Some(old) if now != BinaryUid::Uid(old.clone()) => BinaryUidFix {
                    path: res.clone(),
                    uid: old,
                    new_uid: false,
                },
                Some(_) => continue,
                None if now == BinaryUid::Missing => BinaryUidFix {
                    path: res.clone(),
                    uid: new_uid(),
                    new_uid: true,
                },
                _ => continue,
            };
            plan.push(fix);
        }
        plan.sort_by(|a, b| a.path.cmp(&b.path));
        plan
    }

    /// The fixes of `plan` whose uid the file's header now carries, as repairs.
    pub fn binary_repairs(&self, project: &Path, plan: &[BinaryUidFix]) -> Vec<ResourceRepair> {
        plan.iter()
            .filter(|fix| {
                res_file(project, &fix.path)
                    .is_some_and(|file| binary_uid(&file) == BinaryUid::Uid(fix.uid.clone()))
            })
            .map(|fix| ResourceRepair {
                path: fix.path.clone(),
                uid: Some(fix.uid.clone()),
                new_uid: fix.new_uid,
                ext_resource_uids: Vec::new(),
                removed_null_shader_parameters: Vec::new(),
            })
            .collect()
    }

    /// Every text scene, resource, script and shader written or created since
    /// [`TextResourceSnapshot::take`]: the files an editor holding the project must refresh, and
    /// reload where it has them loaded. Sorted by path.
    pub fn written_since(&self, project: &Path) -> Vec<String> {
        let changes = self.changes_since(project);
        written_in(&changes, &changes.read_texts())
    }

    /// Text scenes and resources written since [`TextResourceSnapshot::take`] that lost a uid,
    /// gained a `null` shader parameter or hold a `MultiMesh` saved without its instance data,
    /// and binary ones that lost their uid. Sorted by path.
    pub fn damage_since(&self, project: &Path) -> Vec<ResourceDamage> {
        // Scripts and shaders carry no uid, so they are not looked at.
        let changes = self.changes_among(project, |name| {
            is_text_resource(name) || is_binary_resource(name)
        });
        let mut found = Vec::new();
        for (res, _, text) in changes.read_texts() {
            let uid = self.uid_before(&res);
            if let Some(damage) = inspect(project, &res, &text, self.files.get(&res), uid) {
                found.push(damage);
            }
        }
        for (res, file) in changes.binaries {
            if self
                .binaries
                .get(&res)
                .is_some_and(|old| old.uid == BinaryUid::Unreadable)
            {
                continue;
            }
            let Some(old) = self.uid_before(&res) else {
                continue;
            };
            if binary_uid(&file) != BinaryUid::Uid(old.clone()) {
                found.push(ResourceDamage {
                    path: res,
                    lost_uid: Some(old.clone()),
                    ext_resources_without_uid: Vec::new(),
                    null_shader_parameters: Vec::new(),
                    multimeshes_without_buffer: Vec::new(),
                });
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
        self.repair_texts(project, &self.changes_since(project).read_texts())
    }

    /// [`TextResourceSnapshot::repair_since`] for the changed text files already read, as
    /// [`ProjectChanges::read_texts`] returns them.
    pub fn repair_texts(&self, project: &Path, changed: &[ChangedText]) -> Vec<ResourceRepair> {
        // Every header uid first, so a scene that references another rewritten file gets that
        // file's uid even though its header on disk has none yet.
        let mut own: BTreeMap<String, (String, bool)> = BTreeMap::new();
        for (res, _, text) in changed {
            let header = text.lines().next().unwrap_or("");
            if !is_resource_header(header) || attribute(header, "uid").is_some() {
                continue;
            }
            let entry = match self.uid_before(res) {
                Some(uid) => (uid, false),
                None => (new_uid(), true),
            };
            own.insert(res.clone(), entry);
        }
        let mut repairs = Vec::new();
        for (res, file, text) in changed {
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
}

/// `(res path, file, text)` of a changed text scene or resource.
pub type ChangedText = (String, PathBuf, String);

/// The scenes, resources, scripts and shaders whose size or modification time differs from the
/// snapshot, or that are new, as [`TextResourceSnapshot::changes_since`] found them in one walk.
#[derive(Debug, Clone, Default)]
pub struct ProjectChanges {
    /// Text scenes and resources, with their files. Files over [`MAX_READ`] are left out.
    text: Vec<(String, PathBuf)>,
    /// Binary scenes and resources, with their files.
    binaries: Vec<(String, PathBuf)>,
    /// Scripts and shaders.
    loaded: Vec<String>,
}

impl ProjectChanges {
    /// The changed text scenes and resources with their text, each read once. A file that
    /// cannot be read as text is left out.
    pub fn read_texts(&self) -> Vec<ChangedText> {
        self.text
            .iter()
            .filter_map(|(res, file)| {
                let text = fs::read_to_string(file).ok()?;
                Some((res.clone(), file.clone(), text))
            })
            .collect()
    }
}

/// Every text scene, resource, script and shader among `changes`, the text files as `texts`
/// (from [`ProjectChanges::read_texts`]) has them. Sorted by path.
pub fn written_in(changes: &ProjectChanges, texts: &[ChangedText]) -> Vec<String> {
    let mut written: Vec<String> = texts.iter().map(|(res, _, _)| res.clone()).collect();
    written.extend(changes.binaries.iter().map(|(res, _)| res.clone()));
    written.extend(changes.loaded.iter().cloned());
    written.sort();
    written
}

fn inspect(
    project: &Path,
    res: &str,
    text: &str,
    before: Option<&Before>,
    uid_before: Option<String>,
) -> Option<ResourceDamage> {
    let header = text.lines().next().unwrap_or("");
    let lost_uid = uid_before.filter(|_| attribute(header, "uid").is_none());
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
    let multimeshes_without_buffer = multimeshes_without_buffer(text);
    if lost_uid.is_none()
        && ext_resources_without_uid.is_empty()
        && null_shader_parameters.is_empty()
        && multimeshes_without_buffer.is_empty()
    {
        return None;
    }
    Some(ResourceDamage {
        path: res.to_string(),
        lost_uid,
        ext_resources_without_uid,
        null_shader_parameters,
        multimeshes_without_buffer,
    })
}

/// `(id, instance_count)` of each `MultiMesh` in a text scene or resource that has instances and
/// no instance data: no `buffer` line, or an empty one. The id is the `[sub_resource]`'s, or
/// `resource` for the file's own `[resource]` section.
fn multimeshes_without_buffer(text: &str) -> Vec<(String, u64)> {
    let mut lines = text.lines();
    let header = lines.next().unwrap_or("");
    let own_is_multimesh =
        is_resource_header(header) && attribute(header, "type").as_deref() == Some("MultiMesh");
    let mut found = Vec::new();
    // (id, instance_count, has instance data) of the MultiMesh section being read.
    let mut current: Option<(String, u64, bool)> = None;
    let mut close = |current: &mut Option<(String, u64, bool)>| {
        if let Some((id, count, buffer)) = current.take()
            && count > 0
            && !buffer
        {
            found.push((id, count));
        }
    };
    for line in lines {
        if is_section_line(line) {
            close(&mut current);
            current = if line.starts_with("[sub_resource")
                && attribute(line, "type").as_deref() == Some("MultiMesh")
            {
                Some((attribute(line, "id").unwrap_or_default(), 0, false))
            } else if own_is_multimesh && line.trim_end() == "[resource]" {
                Some(("resource".into(), 0, false))
            } else {
                None
            };
            continue;
        }
        let Some((_, count, buffer)) = current.as_mut() else {
            continue;
        };
        if let Some((key, value)) = line.split_once('=') {
            let value = value.trim();
            match key.trim() {
                "instance_count" => *count = value.parse().unwrap_or(0),
                "buffer" => *buffer = value != "PackedFloat32Array()",
                _ => {}
            }
        }
    }
    close(&mut current);
    found
}

/// A section header of a text scene or resource. A line of a multi-line value never starts with
/// one of these.
fn is_section_line(line: &str) -> bool {
    [
        "[sub_resource ",
        "[ext_resource ",
        "[resource]",
        "[node ",
        "[connection ",
        "[editable ",
    ]
    .iter()
    .any(|start| line.starts_with(start))
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
    if is_binary_resource(&name)
        && let BinaryUid::Uid(uid) = binary_uid(&file)
    {
        return Some(uid);
    }
    None
}

/// The uids the project's uid cache gives the paths whose header has none (or that do not exist
/// yet), leaving out a uid another file's header carries: that file owns it now.
fn cached_uids(
    project: &Path,
    files: &BTreeMap<String, Before>,
    binaries: &BTreeMap<String, BinaryBefore>,
) -> BTreeMap<String, String> {
    let mut cached = uids_by_path(project);
    if cached.is_empty() {
        return cached;
    }
    let mut owners: BTreeMap<&str, &str> = BTreeMap::new();
    for (res, old) in files {
        if let Some(uid) = &old.uid {
            owners.insert(uid, res);
        }
    }
    for (res, old) in binaries {
        if let BinaryUid::Uid(uid) = &old.uid {
            owners.insert(uid, res);
        }
    }
    cached.retain(|res, uid| {
        let has_own = files.get(res).is_some_and(|old| old.uid.is_some())
            || binaries
                .get(res)
                .is_some_and(|old| old.uid != BinaryUid::Missing);
        !has_own && owners.get(uid.as_str()).is_none_or(|owner| owner == res)
    });
    cached
}

/// The file of a `res://` path inside `project`, refusing `..` and empty parts.
fn res_file(project: &Path, res: &str) -> Option<PathBuf> {
    let relative = res.strip_prefix("res://")?;
    if relative.is_empty()
        || relative
            .split('/')
            .any(|part| part.is_empty() || part == "..")
    {
        return None;
    }
    Some(project.join(relative))
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

fn is_binary_resource(name: &str) -> bool {
    name.ends_with(".scn") || name.ends_with(".res")
}

/// Scripts and shaders: text files the editor loads as resources but never saves with a uid.
fn is_loaded_text(name: &str) -> bool {
    [".gd", ".gdshader", ".gdshaderinc"]
        .iter()
        .any(|extension| name.ends_with(extension))
}

/// The scenes, resources, scripts and shaders the audit looks at.
fn is_audited(name: &str) -> bool {
    is_text_resource(name) || is_binary_resource(name) || is_loaded_text(name)
}

#[cfg(test)]
thread_local! {
    /// Walks of a project started on this thread, so tests can pin how often one is walked.
    // Clippy 1.100 asks for `const` here on windows-gnu although the initializer is one.
    #[allow(clippy::missing_const_for_thread_local)]
    static WALKS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Calls `visit` with the `res://` path, the file and the file name of every file under
/// `project` whose name is `wanted`, as Godot's scan sees the project.
fn walk_project(
    project: &Path,
    wanted: fn(&str) -> bool,
    visit: &mut dyn FnMut(String, &Path, &str),
) {
    #[cfg(test)]
    WALKS.with(|walks| walks.set(walks.get() + 1));
    walk(project, project, wanted, visit);
}

fn walk(
    project: &Path,
    dir: &Path,
    wanted: fn(&str) -> bool,
    visit: &mut dyn FnMut(String, &Path, &str),
) {
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
            walk(project, &path, wanted, visit);
            continue;
        }
        if !kind.is_file() || !wanted(name) {
            continue;
        }
        let Ok(relative) = path.strip_prefix(project) else {
            continue;
        };
        let Some(relative) = relative.to_str() else {
            continue;
        };
        visit(
            format!("res://{}", relative.replace('\\', "/")),
            &path,
            name,
        );
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
                multimeshes_without_buffer: Vec::new(),
            }]
        );
    }

    #[test]
    fn a_multimesh_saved_without_its_instance_data_is_damage() {
        let dir = tempfile::tempdir().expect("project");
        let root = dir.path();
        let snapshot = TextResourceSnapshot::take(root);
        // What a headless ResourceSaver.save writes for a MultiMesh of 12 instances: the
        // dummy rendering server holds no buffer, so none is written.
        fs::write(
            root.join("grass.tres"),
            "[gd_resource type=\"MultiMesh\" format=3]\n\n[resource]\ntransform_format = 1\nuse_colors = true\ninstance_count = 12\n",
        )
        .expect("grass");
        fs::write(
            root.join("field.tscn"),
            concat!(
                "[gd_scene format=3]\n\n",
                "[sub_resource type=\"MultiMesh\" id=\"MultiMesh_lost\"]\n",
                "transform_format = 1\ninstance_count = 3\nbuffer = PackedFloat32Array()\n\n",
                "[sub_resource type=\"MultiMesh\" id=\"MultiMesh_kept\"]\n",
                "instance_count = 1\nbuffer = PackedFloat32Array(1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0)\n\n",
                "[sub_resource type=\"MultiMesh\" id=\"MultiMesh_empty\"]\n",
                "instance_count = 0\n\n",
                "[node name=\"Field\" type=\"MultiMeshInstance3D\"]\n",
                "multimesh = SubResource(\"MultiMesh_lost\")\ninstance_count = 9\n",
            ),
        )
        .expect("field");
        fs::write(
            root.join("other.tres"),
            "[gd_resource type=\"Resource\" format=3]\n\n[resource]\ninstance_count = 4\n",
        )
        .expect("other");
        let damage = snapshot.damage_since(root);
        let lost: Vec<(&str, &[(String, u64)])> = damage
            .iter()
            .map(|item| {
                (
                    item.path.as_str(),
                    item.multimeshes_without_buffer.as_slice(),
                )
            })
            .collect();
        assert_eq!(
            lost,
            vec![
                ("res://field.tscn", &[("MultiMesh_lost".to_string(), 3)][..]),
                ("res://grass.tres", &[("resource".to_string(), 12)][..]),
            ]
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
    fn written_lists_rewritten_shaders_and_scripts_with_the_text_resources() {
        let dir = tempfile::tempdir().expect("project");
        let root = dir.path();
        let shader = "shader_type spatial;\nuniform float foam_pulse = 0.35;\n";
        fs::write(root.join("water.gdshader"), shader).expect("shader");
        fs::write(root.join("foam.gdshaderinc"), "// foam\n").expect("include");
        fs::write(root.join("helper.gd"), "extends Node\n").expect("script");
        fs::write(root.join("still.gd"), "extends Node\n").expect("script");
        fs::write(root.join("pond.tscn"), EDITOR_SAVED).expect("scene");
        fs::write(root.join("notes.txt"), "a").expect("notes");
        let snapshot = TextResourceSnapshot::take(root);
        fs::write(
            root.join("water.gdshader"),
            "shader_type spatial;\nuniform float foam_pulse = 0.5;\nuniform float foam_speed = 1.0;\n",
        )
        .expect("rewrite shader");
        fs::write(root.join("foam.gdshaderinc"), "// foam, faster\n").expect("rewrite include");
        fs::write(root.join("helper.gd"), "extends Node3D\n").expect("rewrite script");
        fs::write(root.join("new.gdshader"), "shader_type canvas_item;\n").expect("new shader");
        fs::write(root.join("pond.tscn"), HEADLESS_SAVED).expect("rewrite scene");
        fs::write(root.join("notes.txt"), "ab").expect("rewrite notes");
        assert_eq!(
            snapshot.written_since(root),
            [
                "res://foam.gdshaderinc",
                "res://helper.gd",
                "res://new.gdshader",
                "res://pond.tscn",
                "res://water.gdshader",
            ]
        );
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

    #[test]
    fn a_binary_scene_rewritten_without_its_uid_is_planned_and_checked() {
        use super::super::binary_resource::tests::{rscc, rsrc};
        let project = tempfile::tempdir().expect("project");
        let root = project.path();
        let id = 0x63cf_e4ba_4e3a_8225_i64;
        fs::write(root.join("gate.scn"), rscc(&rsrc(Some(id)))).expect("gate");
        fs::write(root.join("kept.res"), rsrc(Some(5))).expect("kept");
        fs::write(root.join("bare.res"), rsrc(None)).expect("bare");
        let snapshot = TextResourceSnapshot::take(root);
        assert!(
            snapshot.binary_uid_plan(root).is_empty(),
            "nothing written yet"
        );

        // The run rewrites gate.scn without its uid, rewrites kept.res with it, creates new.scn.
        fs::write(
            root.join("gate.scn"),
            rscc(&[rsrc(None), vec![1; 50]].concat()),
        )
        .expect("gate");
        fs::write(root.join("kept.res"), [rsrc(Some(5)), vec![1; 50]].concat()).expect("kept");
        fs::write(root.join("new.scn"), rsrc(None)).expect("new");
        let plan = snapshot.binary_uid_plan(root);
        let paths: Vec<&str> = plan.iter().map(|fix| fix.path.as_str()).collect();
        assert_eq!(paths, ["res://gate.scn", "res://new.scn"], "{plan:?}");
        assert_eq!(plan[0].uid, "uid://dap714cpjw8pd");
        assert!(!plan[0].new_uid);
        assert!(plan[1].new_uid && is_uid_text(&plan[1].uid), "{plan:?}");
        let damage = snapshot.damage_since(root);
        assert_eq!(damage.len(), 1, "{damage:?}");
        assert_eq!(damage[0].lost_uid.as_deref(), Some("uid://dap714cpjw8pd"));
        assert!(
            snapshot
                .written_since(root)
                .contains(&"res://kept.res".to_string())
        );

        // Godot wrote the uid back into gate.scn only.
        fs::write(
            root.join("gate.scn"),
            rscc(&[rsrc(Some(id)), vec![1; 50]].concat()),
        )
        .expect("gate");
        let repairs = snapshot.binary_repairs(root, &plan);
        assert_eq!(repairs.len(), 1, "{repairs:?}");
        assert_eq!(repairs[0].path, "res://gate.scn");
        assert!(snapshot.damage_since(root).is_empty());
    }

    /// Writes `.godot/uid_cache.bin` with `(uid, res path)` entries, in Godot's format.
    fn write_uid_cache(root: &Path, entries: &[(&str, &str)]) {
        let mut bytes = (entries.len() as u32).to_le_bytes().to_vec();
        for (uid, path) in entries {
            let id = super::super::uid_cache::uid_id(uid).expect("uid");
            bytes.extend_from_slice(&id.to_le_bytes());
            bytes.extend_from_slice(&(path.len() as u32).to_le_bytes());
            bytes.extend_from_slice(path.as_bytes());
        }
        fs::create_dir_all(root.join(".godot")).expect("dir");
        fs::write(root.join(".godot/uid_cache.bin"), bytes).expect("cache");
    }

    #[test]
    fn a_file_left_without_its_uid_by_an_earlier_write_gets_the_cached_uid_back() {
        use super::super::binary_resource::tests::rsrc;
        let project = tempfile::tempdir().expect("project");
        let root = project.path();
        // An earlier headless write (a run stopped before its repair, or one in another session)
        // left both files without their uid; the uid cache and every reference still use it.
        fs::write(root.join("zone.tscn"), HEADLESS_SAVED).expect("zone");
        fs::write(root.join("zone_mesh.res"), rsrc(None)).expect("mesh");
        write_uid_cache(
            root,
            &[
                ("uid://cpond", "res://zone.tscn"),
                ("uid://dap714cpjw8pd", "res://zone_mesh.res"),
            ],
        );
        let snapshot = TextResourceSnapshot::take(root);

        fs::write(root.join("zone.tscn"), HEADLESS_SAVED.replace("3.0", "4.0")).expect("zone");
        fs::write(
            root.join("zone_mesh.res"),
            [rsrc(None), vec![1; 50]].concat(),
        )
        .expect("mesh");
        let plan = snapshot.binary_uid_plan(root);
        assert_eq!(
            plan,
            vec![BinaryUidFix {
                path: "res://zone_mesh.res".into(),
                uid: "uid://dap714cpjw8pd".into(),
                new_uid: false,
            }]
        );
        let damage = snapshot.damage_since(root);
        let lost: Vec<_> = damage.iter().map(|item| item.lost_uid.as_deref()).collect();
        assert_eq!(lost, [Some("uid://cpond"), Some("uid://dap714cpjw8pd")]);
        let repairs = snapshot.repair_since(root);
        assert_eq!(repairs.len(), 1, "{repairs:?}");
        assert_eq!(repairs[0].uid.as_deref(), Some("uid://cpond"));
        assert!(!repairs[0].new_uid);
        let text = fs::read_to_string(root.join("zone.tscn")).expect("zone");
        assert!(
            text.starts_with("[gd_scene format=3 uid=\"uid://cpond\"]"),
            "{text}"
        );
    }

    #[test]
    fn a_cached_uid_another_file_now_carries_is_not_reused() {
        let project = tempfile::tempdir().expect("project");
        let root = project.path();
        fs::write(root.join("old.tscn"), HEADLESS_SAVED).expect("old");
        fs::write(
            root.join("other.tscn"),
            "[gd_scene format=3 uid=\"uid://cpond\"]\n",
        )
        .expect("other");
        write_uid_cache(root, &[("uid://cpond", "res://old.tscn")]);
        let snapshot = TextResourceSnapshot::take(root);
        fs::write(root.join("old.tscn"), HEADLESS_SAVED.replace("3.0", "4.0")).expect("old");
        let repairs = snapshot.repair_since(root);
        assert_eq!(repairs.len(), 1, "{repairs:?}");
        assert!(repairs[0].new_uid, "{repairs:?}");
        assert_ne!(repairs[0].uid.as_deref(), Some("uid://cpond"));
    }

    fn walks() -> usize {
        WALKS.with(|walks| walks.get())
    }

    /// A project with every kind of file the audit looks at, and files it does not.
    fn mixed_project(root: &Path) {
        use super::super::binary_resource::tests::{rscc, rsrc};
        fs::create_dir_all(root.join("art/deep")).expect("dirs");
        fs::create_dir_all(root.join(".godot")).expect(".godot");
        fs::write(root.join("pond.tscn"), EDITOR_SAVED).expect("scene");
        fs::write(
            root.join("art/frog.tscn"),
            "[gd_scene format=3 uid=\"uid://bfrog\"]\n",
        )
        .expect("frog");
        fs::write(root.join("art/deep/mat.tres"), "[gd_resource format=3]\n").expect("mat");
        fs::write(root.join("art/water.gdshader"), "shader_type spatial;\n").expect("shader");
        fs::write(root.join("art/foam.gdshaderinc"), "// foam\n").expect("include");
        fs::write(root.join("main.gd"), "extends Node\n").expect("script");
        fs::write(root.join("art/gate.scn"), rscc(&rsrc(Some(77)))).expect("gate");
        fs::write(root.join("art/deep/bare.res"), rsrc(None)).expect("bare");
        fs::write(root.join("art/deep/text.res"), "[gd_resource]\n").expect("text res");
        fs::write(root.join("art/rock.png"), [0_u8; 8]).expect("png");
        fs::write(root.join(".godot/hidden.tscn"), EDITOR_SAVED).expect("hidden");
    }

    /// What one kind of file looks like to a walk of its own, as the audit walked them before.
    fn walk_kind(root: &Path, wanted: fn(&str) -> bool) -> Vec<String> {
        let mut found = Vec::new();
        walk(root, root, wanted, &mut |res, _, _| found.push(res));
        found.sort();
        found
    }

    #[test]
    fn one_walk_takes_the_snapshot_and_one_finds_what_a_run_changed() {
        use super::super::binary_resource::tests::rsrc;
        let project = tempfile::tempdir().expect("project");
        let root = project.path();
        mixed_project(root);
        let start = walks();
        let snapshot = TextResourceSnapshot::take(root);
        assert_eq!(walks() - start, 1, "the snapshot walks the project once");

        fs::write(root.join("pond.tscn"), HEADLESS_SAVED).expect("rewrite");
        fs::write(root.join("main.gd"), "extends Node3D\n").expect("script");
        fs::write(
            root.join("art/deep/bare.res"),
            [rsrc(None), vec![1; 9]].concat(),
        )
        .expect("bare");
        let start = walks();
        let changes = snapshot.changes_since(root);
        let plan = snapshot.binary_uid_plan_of(&changes);
        let texts = changes.read_texts();
        let repairs = snapshot.repair_texts(root, &texts);
        let written = written_in(&changes, &texts);
        assert_eq!(
            walks() - start,
            1,
            "the post-run audit walks the project once"
        );
        assert_eq!(
            plan.iter().map(|fix| fix.path.as_str()).collect::<Vec<_>>(),
            ["res://art/deep/bare.res"]
        );
        assert_eq!(
            repairs
                .iter()
                .map(|item| item.path.as_str())
                .collect::<Vec<_>>(),
            ["res://pond.tscn"]
        );
        assert_eq!(
            written,
            [
                "res://art/deep/bare.res",
                "res://main.gd",
                "res://pond.tscn"
            ]
        );
        let start = walks();
        let damage = snapshot.damage_since(root);
        assert_eq!(
            walks() - start,
            1,
            "the damage check walks the project once"
        );
        // bare.res never had a uid, so only the plan names it; the text repair took.
        assert!(damage.is_empty(), "{damage:?}");
    }

    #[test]
    fn the_single_walk_finds_what_a_walk_per_kind_found() {
        let project = tempfile::tempdir().expect("project");
        let root = project.path();
        mixed_project(root);
        let snapshot = TextResourceSnapshot::take(root);
        assert_eq!(
            snapshot.files.keys().cloned().collect::<Vec<_>>(),
            walk_kind(root, is_text_resource)
        );
        assert_eq!(
            snapshot.loaded.keys().cloned().collect::<Vec<_>>(),
            walk_kind(root, is_loaded_text)
        );
        assert_eq!(
            snapshot.binaries.keys().cloned().collect::<Vec<_>>(),
            walk_kind(root, is_binary_resource)
        );
        assert_eq!(
            snapshot.files.len() + snapshot.loaded.len() + snapshot.binaries.len(),
            9
        );
        assert_eq!(
            snapshot.files["res://art/frog.tscn"].uid.as_deref(),
            Some("uid://bfrog")
        );
        assert_eq!(
            snapshot.files["res://pond.tscn"].ext_uids["res://water.gdshader"],
            "uid://dshader"
        );
        assert_eq!(
            snapshot.binaries["res://art/gate.scn"].uid,
            BinaryUid::Uid(uid_text(77))
        );
        assert_eq!(
            snapshot.binaries["res://art/deep/bare.res"].uid,
            BinaryUid::Missing
        );
        assert_eq!(
            snapshot.binaries["res://art/deep/text.res"].uid,
            BinaryUid::Unreadable
        );

        // Every file of every kind is new to an empty snapshot.
        let changes = TextResourceSnapshot::default().changes_since(root);
        let mut text: Vec<String> = changes.text.iter().map(|(res, _)| res.clone()).collect();
        text.sort();
        let mut binaries: Vec<String> = changes
            .binaries
            .iter()
            .map(|(res, _)| res.clone())
            .collect();
        binaries.sort();
        let mut loaded = changes.loaded.clone();
        loaded.sort();
        assert_eq!(text, walk_kind(root, is_text_resource));
        assert_eq!(binaries, walk_kind(root, is_binary_resource));
        assert_eq!(loaded, walk_kind(root, is_loaded_text));
        assert!(snapshot.changes_since(root).text.is_empty());
    }

    /// Writes a synthetic project of `count` files: text scenes and resources, scripts, shaders,
    /// binary resources (half of them compressed) and imported assets, in folders of 200.
    fn synthetic_project(root: &Path, count: usize) {
        use super::super::binary_resource::tests::{rscc, rsrc};
        for index in 0..count {
            let dir = root.join(format!("d{}", index / 200));
            if index % 200 == 0 {
                fs::create_dir_all(&dir).expect("dir");
            }
            let (name, bytes): (String, Vec<u8>) = match index % 10 {
                0 | 1 => (
                    format!("s{index}.tscn"),
                    EDITOR_SAVED.replace("uid://cpond", &uid_text(index as u64)).into_bytes(),
                ),
                2 => (
                    format!("r{index}.tres"),
                    format!("[gd_resource type=\"Resource\" format=3 uid=\"{}\"]\n\n[resource]\nvalue = 1\n{}", uid_text(index as u64), "# padding\n".repeat(50)).into_bytes(),
                ),
                3 | 4 => (
                    format!("c{index}.gd"),
                    format!("extends Node\nclass_name C{index}\n\n{}", "func f() -> int:\n\treturn 1\n".repeat(40)).into_bytes(),
                ),
                5 => (format!("h{index}.gdshader"), b"shader_type spatial;\n".to_vec()),
                6 if index % 20 == 6 => (format!("b{index}.res"), rsrc(Some(index as i64))),
                6 => (
                    format!("b{index}.scn"),
                    rscc(&[rsrc(Some(index as i64)), vec![3; 9000]].concat()),
                ),
                _ => (format!("t{index}.png"), vec![0; 256]),
            };
            fs::write(dir.join(name), bytes).expect("file");
        }
    }

    /// How long the snapshot and the post-run audit of a large project take:
    /// GODOT_MCP_BENCH_FILES=<file count, default 20000>.
    #[test]
    #[ignore = "timing probe on a synthetic project"]
    fn audit_time_of_a_synthetic_project() {
        let count = std::env::var("GODOT_MCP_BENCH_FILES")
            .ok()
            .and_then(|text| text.parse().ok())
            .unwrap_or(20_000);
        let project = tempfile::tempdir().expect("project");
        let root = project.path();
        synthetic_project(root, count);
        // The least time of several rounds, so other work on the machine counts less.
        let (mut take, mut audit, mut damage_time) = (None, None, None);
        let least = |best: &mut Option<std::time::Duration>, time| {
            *best = Some(best.map_or(time, |old: std::time::Duration| old.min(time)));
        };
        let mut counts = (0, 0, 0, 0);
        for _ in 0..7 {
            let started = std::time::Instant::now();
            let snapshot = TextResourceSnapshot::take(root);
            least(&mut take, started.elapsed());
            // The run rewrites a few files the way a headless save does.
            fs::write(root.join("d0/s0.tscn"), HEADLESS_SAVED).expect("rewrite");
            fs::write(root.join("d0/s1.tscn"), HEADLESS_SAVED).expect("rewrite");
            fs::write(root.join("d0/c3.gd"), "extends Node\n").expect("rewrite");
            fs::write(root.join("d1/new.tscn"), HEADLESS_SAVED).expect("new");
            let started = std::time::Instant::now();
            let (plan, repairs, written) = post_run_audit(&snapshot, root);
            least(&mut audit, started.elapsed());
            let started = std::time::Instant::now();
            let damage = snapshot.damage_since(root);
            least(&mut damage_time, started.elapsed());
            counts = (plan.len(), repairs.len(), written.len(), damage.len());
            fs::remove_file(root.join("d1/new.tscn")).expect("remove");
        }
        eprintln!(
            "{count} files, least of 7: take {take:?}; repair, plan and written {audit:?} ({} planned, {} repaired, {} written); damage {damage_time:?} ({} damaged)",
            counts.0, counts.1, counts.2, counts.3
        );
    }

    /// How long a snapshot of a large project takes: GODOT_MCP_TIME_SNAPSHOT=<project dir>.
    #[test]
    #[ignore = "timing probe for a real project"]
    fn snapshot_time_of_a_large_project() {
        let Ok(dir) = std::env::var("GODOT_MCP_TIME_SNAPSHOT") else {
            return;
        };
        let started = std::time::Instant::now();
        let snapshot = TextResourceSnapshot::take(Path::new(&dir));
        let uids = snapshot
            .binaries
            .values()
            .filter(|old| matches!(old.uid, BinaryUid::Uid(_)))
            .count();
        eprintln!(
            "{} binaries, {uids} with a uid, {} text, in {:?}",
            snapshot.binaries.len(),
            snapshot.files.len(),
            started.elapsed()
        );
    }

    /// What `repair_files` does after a run, without the Godot pass for binary files.
    fn post_run_audit(
        snapshot: &TextResourceSnapshot,
        root: &Path,
    ) -> (Vec<BinaryUidFix>, Vec<ResourceRepair>, Vec<String>) {
        let changes = snapshot.changes_since(root);
        let plan = snapshot.binary_uid_plan_of(&changes);
        let texts = changes.read_texts();
        let repairs = snapshot.repair_texts(root, &texts);
        (plan, repairs, written_in(&changes, &texts))
    }
}
