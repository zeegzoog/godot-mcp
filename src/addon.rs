//! The Godot addon (`addons/godot_mcp`) of the same build as this server, embedded by
//! `build.rs`, and its installation into a project.
//!
//! An installation never edits the addon in place: the new files are written to a hidden
//! staging folder next to it (Godot does not scan a folder whose name starts with `.`), which
//! then replaces the old folder in two renames.

use std::collections::BTreeMap;
use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};

/// Every file of the addon: its path inside `addons/godot_mcp` and its bytes.
pub const FILES: &[(&str, &[u8])] = include!(concat!(env!("OUT_DIR"), "/addon_files.rs"));

/// The version of this server, which its addon carries too.
pub const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

/// The addon's manifest, as `project.godot` names an enabled plugin.
pub const MANIFEST: &str = "res://addons/godot_mcp/plugin.cfg";

const ADDON_DIR: &str = "addons/godot_mcp";
const STAGING_DIR: &str = "addons/.godot_mcp.new";
const RETIRED_DIR: &str = "addons/.godot_mcp.old";

/// The addon a project holds on disk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Installed {
    Missing,
    Version(String),
    /// The manifest exists but names no version, or cannot be read.
    Unknown,
}

impl Installed {
    pub fn describe(&self) -> String {
        match self {
            Self::Missing => "not installed".into(),
            Self::Version(version) => version.clone(),
            Self::Unknown => "unknown".into(),
        }
    }
}

/// The version `plugin.cfg` text declares.
pub fn manifest_version(text: &str) -> Option<String> {
    text.lines()
        .map(str::trim)
        .find_map(|line| line.strip_prefix("version="))
        .map(|value| value.trim().trim_matches('"').to_string())
        .filter(|value| !value.is_empty())
}

/// The addon version installed in `project`.
pub fn installed(project: &Path) -> Installed {
    match std::fs::read_to_string(project.join(ADDON_DIR).join("plugin.cfg")) {
        Ok(text) => manifest_version(&text).map_or(Installed::Unknown, Installed::Version),
        Err(error) if error.kind() == ErrorKind::NotFound => Installed::Missing,
        Err(_) => Installed::Unknown,
    }
}

/// What an installation changed, by paths inside `addons/godot_mcp`.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Installation {
    pub added: Vec<String>,
    pub updated: Vec<String>,
    pub removed: Vec<String>,
    pub unchanged: usize,
}

impl Installation {
    pub fn changed(&self) -> bool {
        !(self.added.is_empty() && self.updated.is_empty() && self.removed.is_empty())
    }
}

/// What [`install`] would change in `project`, without writing anything.
pub fn plan(project: &Path) -> io::Result<Installation> {
    let addon = project.join(ADDON_DIR);
    if std::fs::symlink_metadata(&addon).is_ok_and(|meta| meta.file_type().is_symlink()) {
        return Err(io::Error::other(format!(
            "{} is a symbolic link; replace it with the addon by hand or remove the link",
            addon.display()
        )));
    }
    let old = read_tree(&addon)?;
    let mut installation = Installation::default();
    for (path, bytes) in FILES {
        match old.get(*path) {
            None => installation.added.push((*path).to_string()),
            Some(current) if current.as_slice() != *bytes => {
                installation.updated.push((*path).to_string());
            }
            Some(_) => installation.unchanged += 1,
        }
    }
    installation.removed = old
        .keys()
        .filter(|path| !FILES.iter().any(|(name, _)| name == path))
        .cloned()
        .collect();
    Ok(installation)
}

/// Makes `project`'s `addons/godot_mcp` hold exactly the embedded addon. Leaves the folder alone
/// when it already does. A file of the old folder that the addon no longer has is removed.
pub fn install(project: &Path) -> io::Result<Installation> {
    let installation = plan(project)?;
    if !installation.changed() {
        return Ok(installation);
    }
    let addon = project.join(ADDON_DIR);

    let staging = project.join(STAGING_DIR);
    let retired = project.join(RETIRED_DIR);
    remove_if_present(&staging)?;
    remove_if_present(&retired)?;
    for (path, bytes) in FILES {
        let target = staging.join(path);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&target, bytes)?;
    }
    if addon.exists() {
        std::fs::rename(&addon, &retired)?;
    }
    if let Err(error) = std::fs::rename(&staging, &addon) {
        // Put the old addon back so the project is not left without one.
        if retired.exists() {
            let _ = std::fs::rename(&retired, &addon);
        }
        return Err(error);
    }
    remove_if_present(&retired)?;
    Ok(installation)
}

/// `project.godot` text with the addon added to `[editor_plugins] enabled`, or `None` when it is
/// already enabled. Other enabled plugins are kept.
pub fn enable_in(project_godot: &str) -> Option<String> {
    let quoted = format!("\"{MANIFEST}\"");
    let lines: Vec<&str> = project_godot.lines().collect();
    let section = lines
        .iter()
        .position(|line| line.trim() == "[editor_plugins]");
    let Some(section) = section else {
        let mut text = project_godot.trim_end().to_string();
        text.push_str(&format!(
            "\n\n[editor_plugins]\n\nenabled=PackedStringArray({quoted})\n"
        ));
        return Some(text);
    };
    let end = lines[section + 1..]
        .iter()
        .position(|line| line.trim_start().starts_with('['))
        .map_or(lines.len(), |offset| section + 1 + offset);
    let mut out: Vec<String> = lines.iter().map(|line| (*line).to_string()).collect();
    match (section + 1..end).find(|index| lines[*index].trim_start().starts_with("enabled=")) {
        Some(index) => {
            let line = lines[index].trim();
            if line.contains(&quoted) {
                return None;
            }
            let inner = line
                .strip_prefix("enabled=PackedStringArray(")
                .and_then(|rest| rest.strip_suffix(')'))?
                .trim();
            out[index] = if inner.is_empty() {
                format!("enabled=PackedStringArray({quoted})")
            } else {
                format!("enabled=PackedStringArray({inner}, {quoted})")
            };
        }
        None => out.insert(section + 1, format!("enabled=PackedStringArray({quoted})")),
    }
    let mut text = out.join("\n");
    text.push('\n');
    Some(text)
}

fn remove_if_present(dir: &Path) -> io::Result<()> {
    match std::fs::remove_dir_all(dir) {
        Err(error) if error.kind() != ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
}

/// Every file under `dir` by its `/`-separated relative path; empty when `dir` does not exist.
fn read_tree(dir: &Path) -> io::Result<BTreeMap<String, Vec<u8>>> {
    let mut files = BTreeMap::new();
    let mut pending: Vec<PathBuf> = vec![dir.to_path_buf()];
    while let Some(current) = pending.pop() {
        let entries = match std::fs::read_dir(&current) {
            Ok(entries) => entries,
            Err(error) if error.kind() == ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        for entry in entries {
            let entry = entry?;
            let path = entry.path();
            if entry.file_type()?.is_dir() {
                pending.push(path);
            } else if let Ok(relative) = path.strip_prefix(dir) {
                let relative = relative
                    .components()
                    .map(|part| part.as_os_str().to_string_lossy().to_string())
                    .collect::<Vec<_>>()
                    .join("/");
                files.insert(relative, std::fs::read(&path)?);
            }
        }
    }
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_embedded_addon_is_this_build() {
        let manifest = FILES
            .iter()
            .find(|(path, _)| *path == "plugin.cfg")
            .map(|(_, bytes)| String::from_utf8_lossy(bytes).to_string())
            .expect("plugin.cfg embedded");
        assert_eq!(manifest_version(&manifest).as_deref(), Some(SERVER_VERSION));
        assert!(FILES.iter().any(|(path, _)| *path == "bridge_server.gd"));
        assert!(FILES.iter().all(|(path, _)| !path.contains('\\')));
    }

    #[test]
    fn the_plugin_is_enabled_next_to_others_once() {
        let bare = "config_version=5\n\n[application]\n\nconfig/name=\"Frog\"\n";
        let added = enable_in(bare).expect("added");
        assert!(added.ends_with(
            "[editor_plugins]\n\nenabled=PackedStringArray(\"res://addons/godot_mcp/plugin.cfg\")\n"
        ));
        assert_eq!(enable_in(&added), None);

        let other = "[editor_plugins]\n\nenabled=PackedStringArray(\"res://addons/other/plugin.cfg\")\n\n[rendering]\n\nx=1\n";
        let both = enable_in(other).expect("added");
        assert!(
            both.contains("enabled=PackedStringArray(\"res://addons/other/plugin.cfg\", \"res://addons/godot_mcp/plugin.cfg\")"),
            "{both}"
        );
        assert!(both.contains("[rendering]\n\nx=1"), "{both}");

        let empty = "[editor_plugins]\n\nenabled=PackedStringArray()\n";
        assert!(
            enable_in(empty)
                .expect("added")
                .contains("PackedStringArray(\"res://addons/godot_mcp/plugin.cfg\")")
        );
    }

    #[test]
    fn an_install_replaces_the_old_addon_and_reports_what_changed() {
        let project = tempfile::tempdir().expect("project");
        let addon = project.path().join(ADDON_DIR);
        std::fs::create_dir_all(addon.join("editor")).expect("old addon");
        std::fs::write(addon.join("plugin.cfg"), "version=\"0.11.1\"\n").expect("old manifest");
        std::fs::write(addon.join("editor/gone.gd"), "extends RefCounted\n").expect("old file");
        assert_eq!(
            installed(project.path()),
            Installed::Version("0.11.1".into())
        );

        let first = install(project.path()).expect("install");
        assert!(
            first.updated.contains(&"plugin.cfg".to_string()),
            "{first:?}"
        );
        assert_eq!(first.removed, vec!["editor/gone.gd".to_string()]);
        assert_eq!(first.added.len(), FILES.len() - 1);
        assert_eq!(
            installed(project.path()),
            Installed::Version(SERVER_VERSION.into())
        );
        assert!(!addon.join("editor/gone.gd").exists());
        assert!(!project.path().join(STAGING_DIR).exists());
        assert!(!project.path().join(RETIRED_DIR).exists());

        let again = install(project.path()).expect("again");
        assert!(!again.changed());
        assert_eq!(again.unchanged, FILES.len());
    }
}
