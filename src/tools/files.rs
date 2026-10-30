//! Deleting project files with their companions (`.uid`, `.import` and the imported data).
//!
//! With the editor connected the addon deletes them the way the FileSystem dock does: it closes
//! their tabs, takes them out of the editor's caches and file system, and moves them to the
//! system trash unless `trash` is false. Without an editor the files are deleted on disk and the
//! answer says a rescan is needed. Either way a file that another project file still names by
//! path or UID is refused unless `force` is set, and nothing is deleted while any path is refused.

use super::{EditorChoice, ToolManager};
use crate::error::McpError;
use crate::paths::{res_display, resolve_in_project};
use crate::protocol::{Tool, ToolResult};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const DELETE: &str = "godot_delete_file";

const MAX_PATHS: usize = 64;
/// The editor searches every project file for references, which takes a while in a big project.
const EDITOR_WAIT: Duration = Duration::from_secs(60);
/// Text files searched for a path or UID of a file being deleted; the addon uses the same list.
const TEXT_EXTENSIONS: &[&str] = &[
    "tscn",
    "tres",
    "gd",
    "gdshader",
    "gdshaderinc",
    "godot",
    "cfg",
    "import",
    "cs",
    "json",
];
/// Binary resources: only the editor can read their dependencies.
const BINARY_EXTENSIONS: &[&str] = &["scn", "res"];
const MAX_SCAN_BYTES: u64 = 16 * 1024 * 1024;
const MAX_LISTED_REFERENCES: usize = 50;
const BRIDGE_ADDON: &str = "res://addons/godot_mcp/";
const IMPORTED_PREFIX: &str = "res://.godot/imported/";

pub fn handles(name: &str) -> bool {
    name == DELETE
}

pub fn catalog() -> Vec<Tool> {
    vec![super::tool(
        DELETE,
        "Deletes files of a Godot project (scripts, scenes, resources, imported assets, any project file) together with their companions: the .uid file, the .import file and the imported data in .godot/imported. Before deleting it searches the project's text files (.tscn, .tres, .gd, shaders, project.godot, .cfg, .import, .cs, .json; with the editor also binary .scn/.res) for the file's res:// path and uid:// and refuses a file that is still referenced unless force is true. Refuses project.godot, anything in .godot/, the Godot MCP addon itself, a .uid or .import file whose own file exists, and directories (an empty one only with allow_empty_dir). All or nothing: while any path is refused nothing is deleted, and the answer lists each refusal with its reason. With the editor connected the files are deleted through it: open scene and script tabs of them are closed (one with unsaved changes is refused), the FileSystem dock is updated, and the files go to the system trash unless trash is false. Without an editor they are deleted permanently on disk and the answer says the editor needs a rescan (godot_resave_resources). dry_run reports what would be deleted and the references without deleting. Mentions by class_name or a path built at run time are not found.",
        json!({
            "paths": {
                "type": "array",
                "items": {"type": "string"},
                "description": "res:// paths of the files to delete, at most 64."
            },
            "force": {"type": "boolean", "description": "Delete files that are still referenced, leaving those references broken. Default false."},
            "allow_empty_dir": {"type": "boolean", "description": "Allow deleting a directory that is empty. Default false."},
            "dry_run": {"type": "boolean", "description": "Only report what would be deleted, the references and the refusals. Default false."},
            "trash": {"type": "boolean", "description": "With the editor connected, move the files to the system trash (default true); false deletes them permanently. Without an editor files are always deleted permanently."},
            "project_path": {"type": "string", "description": "Project directory. Optional: defaults to the project open in the editor, else the one the server was started in."}
        }),
        &["paths"],
    )]
}

pub async fn call(tools: &ToolManager, args: Value) -> ToolResult {
    let paths = match requested_paths(&args) {
        Ok(paths) => paths,
        Err(problem) => {
            return ToolResult::error(
                problem,
                &[
                    "Pass paths: an array of 1 to 64 res:// paths, such as [\"res://old/player.gd\"]",
                ],
            );
        }
    };
    let choice = match tools.editor_choice(&args, true).await {
        Ok(choice) => choice,
        Err(result) => return result,
    };
    if matches!(choice, EditorChoice::Host) {
        return delete_on_host(tools, &args, paths).await;
    }
    let mut params = args.clone();
    if let Value::Object(map) = &mut params {
        map.remove("project_path");
    }
    let bridge = tools.bridge.clone().with_timeout(EDITOR_WAIT);
    match bridge.send_command("delete_files", params).await {
        Ok(report) => answer(choice.name_project(report)),
        Err(McpError::BridgeUnavailable(_)) => delete_on_host(tools, &args, paths).await,
        Err(error) => super::bridge_error_result(&error),
    }
}

fn requested_paths(args: &Value) -> Result<Vec<String>, String> {
    let Some(items) = args.get("paths").and_then(Value::as_array) else {
        return Err("paths must be an array of res:// paths".into());
    };
    if items.is_empty() {
        return Err("paths is empty: name at least one file to delete".into());
    }
    if items.len() > MAX_PATHS {
        return Err(format!("at most {MAX_PATHS} paths per call"));
    }
    items
        .iter()
        .map(|item| match item.as_str() {
            Some(path) if !path.trim().is_empty() => Ok(path.to_string()),
            _ => Err("every item of paths must be a non-empty string".into()),
        })
        .collect()
}

/// Turns a deletion report into the tool's answer: an error when something was refused.
fn answer(report: Value) -> ToolResult {
    let body = serde_json::to_string_pretty(&report).unwrap_or_default();
    let refused = report
        .get("refused")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    if refused == 0 {
        return ToolResult::text(body);
    }
    let deleted = report
        .get("deleted")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    let headline = if report.get("dry_run").and_then(Value::as_bool) == Some(true) {
        format!("Dry run, nothing was deleted: {refused} path(s) would be refused (see refused).")
    } else if deleted == 0 {
        format!("Nothing was deleted: {refused} path(s) were refused (see refused).")
    } else {
        format!(
            "Deleting stopped: {refused} path(s) failed after {deleted} were deleted (see deleted and refused)."
        )
    };
    ToolResult::error(
        format!("{headline}\n{body}"),
        &[
            "Read each refused entry's reason; references lists the files that still name a file",
            "Remove or repoint the references first, or pass force: true to delete referenced files anyway",
            "Leave the refused paths out to delete the others",
        ],
    )
}

async fn delete_on_host(tools: &ToolManager, args: &Value, paths: Vec<String>) -> ToolResult {
    let project = match tools.resolve_project(args).await {
        Ok(project) => project.dir,
        Err(result) => return result,
    };
    let options = Options {
        force: flag(args, "force"),
        allow_empty_dir: flag(args, "allow_empty_dir"),
        dry_run: flag(args, "dry_run"),
    };
    let dir = project.clone();
    match tokio::task::spawn_blocking(move || delete_on_disk(&dir, &paths, options)).await {
        Ok(mut report) => {
            report["project_dir"] = json!(project.display().to_string());
            answer(report)
        }
        Err(error) => ToolResult::error(
            format!("The deletion stopped unexpectedly: {error}"),
            &["Check the project with godot_resave_resources, then retry"],
        ),
    }
}

fn flag(args: &Value, key: &str) -> bool {
    args.get(key).and_then(Value::as_bool).unwrap_or(false)
}

#[derive(Clone, Copy)]
struct Options {
    force: bool,
    allow_empty_dir: bool,
    dry_run: bool,
}

struct Target {
    res_path: String,
    file: PathBuf,
    directory: bool,
    uid: Option<String>,
    /// `.uid` and `.import` next to the file, then the imported data the `.import` names.
    companions: Vec<PathBuf>,
    referenced_by: Vec<Value>,
}

/// The deletion without an editor, with the same checks and report as the addon.
fn delete_on_disk(project: &Path, requested: &[String], options: Options) -> Value {
    let mut refused: Vec<Value> = Vec::new();
    let mut warnings: Vec<String> = vec![
        "No editor was connected, so the files were handled on disk. An editor that has this project open needs a rescan (godot_resave_resources without paths), and it may still show tabs of the deleted files.".into(),
    ];
    let mut targets: Vec<Target> = Vec::new();
    for raw in requested {
        let file = match resolve_in_project(project, raw) {
            Ok(file) => file,
            Err(problem) => {
                refused.push(json!({"path": raw, "reason": problem}));
                continue;
            }
        };
        let res_path = res_display(project, &file)
            .trim_end_matches('/')
            .to_string();
        if targets.iter().any(|target| target.res_path == res_path) {
            continue;
        }
        if let Some(reason) = protected(&res_path)
            .or_else(|| protected_by_identity(project, &file))
            .or_else(|| companion_owner(&file))
        {
            refused.push(json!({"path": res_path, "reason": reason}));
            continue;
        }
        let Ok(metadata) = std::fs::symlink_metadata(&file) else {
            refused.push(json!({"path": res_path, "reason": "does not exist"}));
            continue;
        };
        if metadata.is_dir() {
            let empty = std::fs::read_dir(&file).is_ok_and(|mut entries| entries.next().is_none());
            if !options.allow_empty_dir {
                refused.push(json!({"path": res_path, "reason": "is a directory. Only an empty directory can be deleted, with allow_empty_dir: true; delete its files first"}));
            } else if !empty {
                refused.push(json!({"path": res_path, "reason": "is a directory that is not empty; delete its files first"}));
            } else {
                targets.push(Target {
                    res_path,
                    file,
                    directory: true,
                    uid: None,
                    companions: Vec::new(),
                    referenced_by: Vec::new(),
                });
            }
            continue;
        }
        targets.push(describe_file(project, res_path, file));
    }

    let unchecked = find_references(project, &mut targets);
    if unchecked.binary > 0 {
        warnings.push(format!(
            "{} binary resource(s) (.scn, .res) were not searched for references: only the editor can read them.",
            unchecked.binary
        ));
    }
    if unchecked.large > 0 {
        warnings.push(format!(
            "{} text file(s) larger than {MAX_SCAN_BYTES} bytes were not searched for references.",
            unchecked.large
        ));
    }
    let mut references: Vec<Value> = Vec::new();
    for target in &targets {
        if target.referenced_by.is_empty() {
            continue;
        }
        let count = target.referenced_by.len();
        references.push(json!({
            "path": target.res_path,
            "uid": target.uid.clone().unwrap_or_default(),
            "count": count,
            "referenced_by": target.referenced_by.iter().take(MAX_LISTED_REFERENCES).collect::<Vec<_>>(),
        }));
        if !options.force {
            let names: Vec<&str> = target
                .referenced_by
                .iter()
                .take(5)
                .filter_map(|user| user["file"].as_str())
                .collect();
            let more = if count > 5 {
                format!(" and {} more", count - 5)
            } else {
                String::new()
            };
            refused.push(json!({"path": target.res_path, "reason": format!(
                "is still referenced by {count} file(s): {}{more}. Remove the references first, or pass force: true to delete it anyway and leave them broken",
                names.join(", ")
            )}));
        }
    }

    let mut report = json!({
        "source": "disk",
        "dry_run": options.dry_run,
        "forced": options.force,
        "deleted": [],
        "refused": refused,
        "references": references,
        "closed_tabs": [],
        "rescan_needed": false,
    });
    if !refused.is_empty() || options.dry_run {
        report["would_delete"] = json!(
            targets
                .iter()
                .map(|target| listing(project, target))
                .collect::<Vec<_>>()
        );
        report["warnings"] = json!(warnings);
        return report;
    }

    let mut deleted: Vec<Value> = Vec::new();
    for target in &targets {
        let removed = if target.directory {
            std::fs::remove_dir(&target.file)
        } else {
            std::fs::remove_file(&target.file)
        };
        if let Err(error) = removed {
            refused.push(json!({"path": target.res_path, "reason": format!(
                "could not be deleted: {error}. The files listed under deleted were removed before this one"
            )}));
            break;
        }
        let mut companions: Vec<String> = Vec::new();
        for companion in &target.companions {
            match std::fs::remove_file(companion) {
                Ok(()) => companions.push(res_display(project, companion)),
                Err(error) => warnings.push(format!(
                    "{} could not be removed: {error}",
                    res_display(project, companion)
                )),
            }
        }
        deleted.push(json!({
            "path": target.res_path,
            "kind": if target.directory { "directory" } else { "file" },
            "removed_by": "delete",
            "companions": companions,
        }));
    }
    report["rescan_needed"] = json!(!deleted.is_empty());
    report["deleted"] = json!(deleted);
    report["refused"] = json!(refused);
    report["warnings"] = json!(warnings);
    report
}

fn listing(project: &Path, target: &Target) -> Value {
    json!({
        "path": target.res_path,
        "kind": if target.directory { "directory" } else { "file" },
        "companions": target.companions.iter().map(|path| res_display(project, path)).collect::<Vec<_>>(),
    })
}

/// Why a path may never be deleted through this tool, or `None`.
fn protected(res_path: &str) -> Option<String> {
    // Windows and macOS file systems ignore case, so res://Project.godot is project.godot there.
    let res_path = res_path.to_lowercase();
    let res_path = res_path.as_str();
    if is_alias(res_path) {
        return Some("is spelled in a way a file system may read as another file (a trailing dot or space, ':' or '~1'); use the plain name".into());
    }
    if res_path == "res://project.godot" {
        return Some("is the project file itself".into());
    }
    if res_path == "res://.godot" || res_path.starts_with("res://.godot/") {
        return Some("is in the editor's .godot cache, which Godot manages itself".into());
    }
    if res_path == "res://" {
        return Some("is the project directory".into());
    }
    if format!("{res_path}/") == BRIDGE_ADDON || res_path.starts_with(BRIDGE_ADDON) {
        return Some(
            "belongs to the Godot MCP Bridge addon; disable the plugin and remove it by hand"
                .into(),
        );
    }
    None
}

/// Whether a segment of the path is one some file systems read as another file: a trailing dot
/// or space, a stream (`:`) or a short name (`~1`). The comparisons in `protected` cannot see
/// through those.
fn is_alias(res_path: &str) -> bool {
    res_path
        .trim_start_matches("res://")
        .split('/')
        .any(|segment| {
            segment.ends_with(['.', ' '])
                || segment.contains(':')
                || segment
                    .as_bytes()
                    .windows(2)
                    .any(|pair| pair[0] == b'~' && pair[1].is_ascii_digit())
        })
}

/// `protected` for the file a path really names: its canonical place inside the project, which
/// sees through a short name, a link or a spelling in another case.
fn protected_by_identity(project: &Path, file: &Path) -> Option<String> {
    let real = file.canonicalize().ok()?;
    let root = project.canonicalize().ok()?;
    let relative = real.strip_prefix(&root).ok()?;
    protected(&format!(
        "res://{}",
        relative.to_string_lossy().replace('\\', "/")
    ))
}

/// Refuses a `.uid` or `.import` file whose own file still exists: it goes with that file.
fn companion_owner(file: &Path) -> Option<String> {
    let name = file.file_name()?.to_string_lossy().into_owned();
    for suffix in [".uid", ".import"] {
        if let Some(owner) = name.strip_suffix(suffix) {
            if !owner.is_empty() && file.with_file_name(owner).is_file() {
                return Some(format!(
                    "belongs to {owner} and is removed with it; delete {owner} instead"
                ));
            }
        }
    }
    None
}

/// A file target with its UID and companions.
fn describe_file(project: &Path, res_path: String, file: PathBuf) -> Target {
    let mut companions = Vec::new();
    let mut uid = None;
    let sidecar = |suffix: &str| {
        let mut name = file.as_os_str().to_owned();
        name.push(suffix);
        PathBuf::from(name)
    };
    let uid_file = sidecar(".uid");
    if uid_file.is_file() {
        uid = std::fs::read_to_string(&uid_file)
            .ok()
            .map(|text| text.trim().to_string())
            .filter(|text| text.starts_with("uid://"));
        companions.push(uid_file);
    }
    let import_file = sidecar(".import");
    if import_file.is_file() {
        let text = std::fs::read_to_string(&import_file).unwrap_or_default();
        uid = uid.or_else(|| header_uid(&text));
        companions.push(import_file);
        for part in text.split('"') {
            if !part.starts_with(IMPORTED_PREFIX) || part.contains("..") {
                continue;
            }
            // The importer also keeps <name>-<hash>.md5 beside the data it lists.
            let md5 = Path::new(part)
                .with_extension("md5")
                .to_string_lossy()
                .into_owned();
            for item in [part.to_string(), md5] {
                if let Ok(path) = resolve_in_project(project, &item) {
                    if path.is_file() && !companions.contains(&path) {
                        companions.push(path);
                    }
                }
            }
        }
    }
    if uid.is_none() && matches!(extension(&file).as_str(), "tscn" | "tres") {
        uid = std::fs::read_to_string(&file)
            .ok()
            .and_then(|text| text.lines().next().and_then(header_uid));
    }
    Target {
        res_path,
        file,
        directory: false,
        uid,
        companions,
        referenced_by: Vec::new(),
    }
}

/// The `uid="uid://..."` value of a scene or resource header, or of an `.import` file.
fn header_uid(text: &str) -> Option<String> {
    let start = text.find("uid=\"uid://")? + "uid=\"".len();
    let rest = &text[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

fn extension(path: &Path) -> String {
    path.extension()
        .map(|ext| ext.to_string_lossy().to_lowercase())
        .unwrap_or_default()
}

#[derive(Default)]
struct Unchecked {
    binary: usize,
    large: usize,
}

/// Fills `referenced_by` of every file target with the project files that name it by path or
/// UID. The targets and their companions do not count. What Godot does not scan (the `.godot`
/// cache, hidden entries, folders with a `.gdignore`, nested projects) is not searched.
fn find_references(project: &Path, targets: &mut [Target]) -> Unchecked {
    let mut unchecked = Unchecked::default();
    if targets.iter().all(|target| target.directory) {
        return unchecked;
    }
    let excluded: Vec<PathBuf> = targets
        .iter()
        .flat_map(|target| std::iter::once(target.file.clone()).chain(target.companions.clone()))
        .collect();
    let mut candidates = Vec::new();
    collect(project, &mut candidates);
    for candidate in candidates {
        if excluded.contains(&candidate) {
            continue;
        }
        let kind = extension(&candidate);
        if BINARY_EXTENSIONS.contains(&kind.as_str()) {
            unchecked.binary += 1;
            continue;
        }
        if !TEXT_EXTENSIONS.contains(&kind.as_str()) {
            continue;
        }
        if std::fs::metadata(&candidate).is_ok_and(|metadata| metadata.len() > MAX_SCAN_BYTES) {
            unchecked.large += 1;
            continue;
        }
        let Ok(bytes) = std::fs::read(&candidate) else {
            continue;
        };
        let text = String::from_utf8_lossy(&bytes);
        for target in targets.iter_mut().filter(|target| !target.directory) {
            let mut by = Vec::new();
            if mentions(&text, &target.res_path) {
                by.push("path");
            }
            if target
                .uid
                .as_deref()
                .is_some_and(|uid| mentions(&text, uid))
            {
                by.push("uid");
            }
            if !by.is_empty() {
                target
                    .referenced_by
                    .push(json!({"file": res_display(project, &candidate), "by": by}));
            }
        }
    }
    unchecked
}

fn collect(folder: &Path, into: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return;
    };
    let mut entries: Vec<_> = entries.filter_map(Result::ok).collect();
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let path = entry.path();
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if crate::paths::skipped_by_godot(&path, kind.is_dir()) {
            continue;
        }
        if kind.is_dir() {
            collect(&path, into);
        } else if kind.is_file() {
            into.push(path);
        }
    }
}

/// Whether `text` holds `needle` not followed by a character that would continue it, so
/// `res://a.gd` is not found inside `res://a.gdshader`. The addon matches the same way.
fn mentions(text: &str, needle: &str) -> bool {
    text.match_indices(needle).any(|(at, _)| {
        text[at + needle.len()..]
            .chars()
            .next()
            .is_none_or(|next| !(next.is_ascii_alphanumeric() || "_-./~".contains(next)))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_mention_is_a_whole_path_or_uid() {
        assert!(mentions("path=\"res://a.gd\"", "res://a.gd"));
        assert!(mentions("preload('res://a.gd')", "res://a.gd"));
        assert!(mentions("res://a.gd", "res://a.gd"));
        assert!(!mentions("path=\"res://a.gdshader\"", "res://a.gd"));
        assert!(!mentions("path=\"res://a.gd.uid\"", "res://a.gd"));
        assert!(mentions(
            "path=\"res://a.gdshader\" x=\"res://a.gd\"",
            "res://a.gd"
        ));
        assert!(mentions("uid=\"uid://abc\"", "uid://abc"));
        assert!(!mentions("uid=\"uid://abcd\"", "uid://abc"));
    }

    #[test]
    fn the_project_cache_and_the_bridge_are_protected() {
        for path in [
            "res://project.godot",
            "res://.godot",
            "res://.godot/uid_cache.bin",
            "res://addons/godot_mcp",
            "res://addons/godot_mcp/plugin.cfg",
            "res://Project.godot",
            "res://.GODOT/uid_cache.bin",
            "res://Addons/Godot_MCP/plugin.cfg",
            "res://project.godot.",
            "res://project.godot::$DATA",
            "res://PROJEC~1.GOD",
            "res://addons/godot_mcp /plugin.cfg",
        ] {
            assert!(protected(path).is_some(), "{path}");
        }
        for path in ["res://player.gd", "res://addons/other/plugin.cfg"] {
            assert!(protected(path).is_none(), "{path}");
        }
    }

    #[test]
    fn a_path_is_protected_by_the_file_it_names() {
        let dir = tempfile::tempdir().expect("temp dir");
        let root = dir.path();
        std::fs::create_dir_all(root.join(".godot/imported")).expect("cache folder");
        std::fs::create_dir_all(root.join("sub")).expect("folder");
        std::fs::write(root.join("project.godot"), "config_version=5\n").expect("project file");
        std::fs::write(root.join("player.gd"), "extends Node\n").expect("script");
        assert!(protected_by_identity(root, &root.join("sub/../project.godot")).is_some());
        assert!(protected_by_identity(root, &root.join("sub/../.godot/imported")).is_some());
        assert!(protected_by_identity(root, &root.join("sub/../player.gd")).is_none());
        assert!(protected_by_identity(root, &root.join("missing.gd")).is_none());
    }

    #[test]
    fn a_header_uid_is_read() {
        assert_eq!(
            header_uid("[gd_scene load_steps=2 format=3 uid=\"uid://b8x\"]").as_deref(),
            Some("uid://b8x")
        );
        assert_eq!(header_uid("[gd_scene format=3]"), None);
    }

    #[test]
    fn paths_must_be_a_short_list_of_strings() {
        assert!(requested_paths(&json!({})).is_err());
        assert!(requested_paths(&json!({"paths": []})).is_err());
        assert!(requested_paths(&json!({"paths": [""]})).is_err());
        assert!(requested_paths(&json!({"paths": vec!["res://a.gd"; 65]})).is_err());
        assert_eq!(
            requested_paths(&json!({"paths": ["res://a.gd"]})).ok(),
            Some(vec!["res://a.gd".to_string()])
        );
    }

    #[test]
    fn references_are_searched_only_where_godot_scans() {
        let dir = tempfile::tempdir().expect("project");
        let root = dir.path();
        for folder in [".hidden", "docs", "nested", "scenes"] {
            std::fs::create_dir(root.join(folder)).expect("mkdir");
            std::fs::write(root.join(folder).join("a.tscn"), "").expect("scene");
        }
        std::fs::write(root.join("project.godot"), "").expect("project");
        std::fs::write(root.join(".hidden.tscn"), "").expect("scene");
        std::fs::write(root.join("docs/.gdignore"), "").expect("gdignore");
        std::fs::write(root.join("nested/project.godot"), "").expect("nested");
        let mut found = Vec::new();
        collect(root, &mut found);
        assert_eq!(
            found,
            vec![root.join("project.godot"), root.join("scenes/a.tscn")]
        );
    }
}
