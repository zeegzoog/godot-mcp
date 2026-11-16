//! Moving and renaming project files and folders, keeping the project working.
//!
//! A move takes a file's companions with it (its `.uid` and `.import` files, and the imported data
//! in `.godot/imported`, renamed to the name Godot derives from the new path so nothing is
//! imported again), and rewrites every `res://` mention of an old path in the project's text files
//! to the new path. A `uid://` reference needs nothing: the uid moves with the file.
//!
//! The server plans the move and changes the files on disk. With the editor connected the addon
//! takes part twice (`editor/file_ops.gd`): before anything changes it refuses files that are open
//! with unsaved changes, and afterwards it does what the FileSystem dock does after its own move:
//! it points the editor's cached resources, uids and open scenes at the new paths, updates the
//! project settings it holds in memory, saves binary `.scn`/`.res` files again so their paths are
//! new too, reloads the open scenes and scripts whose text changed, and rescans. Without an editor
//! the server also updates the uid cache and the global class cache in `.godot`, and names the
//! binary files that still hold an old path.

use super::files::{
    BINARY_EXTENSIONS, MAX_SCAN_BYTES, TEXT_EXTENSIONS, collect, extension, header_uid, protected,
    protected_by_identity,
};
use super::{EditorChoice, ToolManager};
use crate::error::McpError;
use crate::godot::{md5_hex, record_uids};
use crate::paths::{res_display, resolve_in_project, skipped_by_godot};
use crate::protocol::{Tool, ToolResult};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const MOVE: &str = "godot_move_file";

const MAX_MOVES: usize = 64;
/// The editor saves binary resources again and rescans, which takes a while in a big project.
const EDITOR_WAIT: Duration = Duration::from_secs(120);
/// Rewritten files listed in the answer; the count is always complete.
const MAX_LISTED: usize = 200;
const IMPORTED_DIR: &str = "res://.godot/imported/";
/// The editor's cache of global classes, which a game run without the editor reads.
const CLASS_CACHE: &str = ".godot/global_script_class_cache.cfg";

pub fn handles(name: &str) -> bool {
    name == MOVE
}

pub fn catalog() -> Vec<Tool> {
    vec![super::tool(
        MOVE,
        "Moves or renames files and whole folders of a Godot project the way the editor's FileSystem dock does, keeping the project working. Each file moves with its .uid and .import files, and its imported data in .godot/imported is renamed to match the new path, so nothing is imported again. Every res:// mention of an old path in the project's text files (.tscn, .tres, .gd, shaders, project.godot, .cfg, .import, .cs, .json, the moved files included) is rewritten to the new path; a folder move rewrites every path inside it. uid:// references stay valid because the uid moves with the file. With the editor connected the move goes through it: a file that is open with unsaved changes (a scene, or a script in the script editor) is refused, and afterwards the editor's cached resources, uids, open scene tabs and in-memory project settings follow the new paths, binary .scn/.res files that named an old path are saved again by the editor so they name the new one (resaved), open scenes and scripts whose text changed are reloaded, and the editor rescans. Without an editor the files are changed on disk, the uid cache and global class cache in .godot are updated so a game run finds the files, binary_references lists the .scn/.res files that still hold an old path (they still load through their uids; open the project in the editor and move again or resave them), and rescan_needed is true. Pass moves: a list of {from, to} res:// paths (at most 64); a to ending in / moves the file or folder into that folder under its own name. Refuses a missing source, project.godot, anything in .godot/ or the Godot MCP addon, a lone .uid or .import file, a destination in .godot/, a destination that already exists (or whose .uid/.import does), a folder moved into itself, and moves that overlap each other. All or nothing: while any move is refused nothing changes, and the answer lists each refusal with its reason. dry_run lists the files that would move and every file whose references would be rewritten, without changing anything. Not rewritten: relative paths (preload(\"enemy.gd\")) and paths built at run time (\"res://levels/level_%d.tscn\" % n); check scripts that build paths.",
        json!({
            "moves": {
                "type": "array",
                "description": "Files or folders to move, at most 64.",
                "items": {
                    "type": "object",
                    "properties": {
                        "from": {"type": "string", "description": "res:// path of the file or folder to move."},
                        "to": {"type": "string", "description": "Its new res:// path; ending in / moves it into that folder under its own name. Missing folders are created."}
                    },
                    "required": ["from", "to"]
                }
            },
            "dry_run": {"type": "boolean", "description": "Only report what would move, the references that would be rewritten and the refusals. Default false."},
            "project_path": {"type": "string", "description": "Project directory. Optional: defaults to the project open in the editor, else the one the server was started in."}
        }),
        &["moves"],
    )]
}

pub async fn call(tools: &ToolManager, args: Value) -> ToolResult {
    let requests = match requested_moves(&args) {
        Ok(requests) => requests,
        Err(problem) => {
            return ToolResult::error(
                problem,
                &[
                    "Pass moves: an array of 1 to 64 objects such as {\"from\": \"res://old/player.gd\", \"to\": \"res://new/player.gd\"}",
                ],
            );
        }
    };
    let dry_run = args.get("dry_run").and_then(Value::as_bool) == Some(true);
    let choice = match tools.editor_choice(&args, true).await {
        Ok(choice) => choice,
        Err(result) => return result,
    };
    let (project, mut editor) = match &choice {
        EditorChoice::Editor {
            project_dir: Some(dir),
        } => (PathBuf::from(dir), true),
        _ => match tools.resolve_project(&args).await {
            Ok(project) => (project.dir, false),
            Err(result) => return result,
        },
    };

    let dir = project.clone();
    let mut plan = match tokio::task::spawn_blocking(move || plan(&dir, &requests)).await {
        Ok(plan) => plan,
        Err(error) => return stopped(&error.to_string()),
    };
    let bridge = tools.bridge.clone().with_timeout(EDITOR_WAIT);
    if editor && plan.refused.is_empty() {
        match bridge
            .send_command("move_files_check", plan.check_params())
            .await
        {
            Ok(check) => plan.absorb(&check),
            Err(McpError::BridgeUnavailable(_)) => editor = false,
            Err(error) => return super::bridge_error_result(&error),
        }
    }
    let source = if editor { "editor" } else { "disk" };
    if !plan.refused.is_empty() || dry_run {
        return answer(plan.report(source, dry_run));
    }

    let (plan, mut report) = match tokio::task::spawn_blocking(move || {
        let report = execute(&plan, editor);
        (plan, report)
    })
    .await
    {
        Ok(done) => done,
        Err(error) => return stopped(&error.to_string()),
    };
    if !report["refused"].as_array().is_some_and(Vec::is_empty) {
        return answer(report);
    }
    if editor {
        match bridge
            .send_command("move_files_apply", plan.apply_params())
            .await
        {
            Ok(applied) => {
                for key in [
                    "resaved",
                    "reloaded_scenes",
                    "reloaded_scripts",
                    "project_settings",
                    "rescanned",
                ] {
                    if let Some(value) = applied.get(key) {
                        report[key] = value.clone();
                    }
                }
                append_warnings(&mut report, &applied);
            }
            Err(error) => {
                report["rescan_needed"] = json!(true);
                push_warning(
                    &mut report,
                    format!(
                        "The files were moved on disk, but the editor could not catch up ({error}). Rescan it (godot_resave_resources without paths) and reopen the moved scenes; binary .scn/.res files listed under binary_references may still name the old paths."
                    ),
                );
            }
        }
    }
    answer(choice.name_project(report))
}

fn stopped(error: &str) -> ToolResult {
    ToolResult::error(
        format!("The move stopped unexpectedly: {error}"),
        &["Check the project with godot_resave_resources, then retry with dry_run: true"],
    )
}

struct Request {
    from: String,
    to: String,
}

fn requested_moves(args: &Value) -> Result<Vec<Request>, String> {
    let Some(items) = args.get("moves").and_then(Value::as_array) else {
        return Err("moves must be an array of {from, to} objects".into());
    };
    if items.is_empty() {
        return Err("moves is empty: name at least one file or folder to move".into());
    }
    if items.len() > MAX_MOVES {
        return Err(format!("at most {MAX_MOVES} moves per call"));
    }
    items
        .iter()
        .map(|item| {
            let field = |key: &str| {
                item.get(key)
                    .and_then(Value::as_str)
                    .filter(|value| !value.trim().is_empty())
                    .map(str::to_string)
            };
            match (field("from"), field("to")) {
                (Some(from), Some(to)) => Ok(Request { from, to }),
                _ => Err(
                    "every item of moves must be an object with non-empty from and to strings"
                        .into(),
                ),
            }
        })
        .collect()
}

/// A replacement of an old `res://` path by a new one in a text.
#[derive(Clone, Debug, PartialEq)]
enum Pair {
    /// The whole path: `res://a.gd` but not `res://a.gdshader`.
    Whole { old: String, new: String },
    /// Every path that starts with `old` (a folder with its `/`, or an imported data name).
    Prefix { old: String, new: String },
}

impl Pair {
    fn old(&self) -> &str {
        match self {
            Self::Whole { old, .. } | Self::Prefix { old, .. } => old,
        }
    }

    fn replacement(&self) -> &str {
        match self {
            Self::Whole { new, .. } | Self::Prefix { new, .. } => new,
        }
    }

    /// Whether `rest` (a text from a `res://` on) starts with this pair's old path.
    fn matches(&self, rest: &str) -> bool {
        match self {
            Self::Whole { old, .. } => {
                rest.starts_with(old.as_str())
                    && rest[old.len()..]
                        .chars()
                        .next()
                        .is_none_or(|next| !continues_path(next))
            }
            Self::Prefix { old, .. } => rest.starts_with(old.as_str()),
        }
    }

    fn to_json(&self) -> Value {
        json!({"old": self.old(), "new": self.replacement(), "prefix": matches!(self, Self::Prefix { .. })})
    }
}

/// A character that continues a path, so `res://a.gd` followed by it is another path. The addon
/// and `files::mentions` use the same set.
fn continues_path(next: char) -> bool {
    next.is_ascii_alphanumeric() || "_-./~".contains(next)
}

/// `text` with every old path of `pairs` replaced, and the number of replacements; `None` when
/// nothing matched. One pass from left to right: a replaced path is never matched again, so a
/// chain of moves (a to b, b to c) cannot rename a file twice. The longest old path wins.
fn rewrite(text: &str, pairs: &[Pair]) -> Option<(String, usize)> {
    let mut out = String::with_capacity(text.len());
    let mut copied = 0;
    let mut at = 0;
    let mut count = 0;
    while let Some(found) = text[at..].find("res://") {
        let start = at + found;
        let rest = &text[start..];
        match pairs.iter().find(|pair| pair.matches(rest)) {
            Some(pair) => {
                out.push_str(&text[copied..start]);
                out.push_str(pair.replacement());
                copied = start + pair.old().len();
                at = copied;
                count += 1;
            }
            None => at = start + "res://".len(),
        }
    }
    if count == 0 {
        return None;
    }
    out.push_str(&text[copied..]);
    Some((out, count))
}

/// One accepted move.
struct Move {
    from_res: String,
    to_res: String,
    from: PathBuf,
    to: PathBuf,
    directory: bool,
    /// For a file: its `.uid` and `.import` files that exist.
    companions: Vec<&'static str>,
}

/// A file whose path changes, directly or because its folder moves.
struct MovedFile {
    from_res: String,
    to_res: String,
    uid: Option<String>,
}

/// A text file whose references change: where it is now, where it will be, and its new text.
struct Rewrite {
    res: String,
    new_res: String,
    text: String,
    count: usize,
}

#[derive(Default)]
struct Plan {
    project: PathBuf,
    moves: Vec<Move>,
    files: Vec<MovedFile>,
    pairs: Vec<Pair>,
    rewrites: Vec<Rewrite>,
    /// Binary resources that mention an old path: where they are now and where they will be.
    binary: Vec<(String, String)>,
    /// Imported data renamed in `.godot/imported`.
    imported: Vec<(PathBuf, PathBuf)>,
    refused: Vec<Value>,
    warnings: Vec<String>,
}

impl Plan {
    fn refuse(&mut self, path: &str, reason: impl Into<String>) {
        self.refused
            .push(json!({"path": path, "reason": reason.into()}));
    }

    fn check_params(&self) -> Value {
        json!({
            "files": self.files.iter().map(|file| json!({"from": file.from_res, "to": file.to_res})).collect::<Vec<_>>(),
            "rewrite": self.rewrites.iter().map(|rewrite| rewrite.res.clone()).collect::<Vec<_>>(),
            "binary": self.binary.iter().map(|(now, _)| now.clone()).collect::<Vec<_>>(),
        })
    }

    fn apply_params(&self) -> Value {
        json!({
            "files": self.files.iter().map(|file| json!({
                "from": file.from_res,
                "to": file.to_res,
                "uid": file.uid.clone().unwrap_or_default(),
            })).collect::<Vec<_>>(),
            "folders": self.moves.iter().filter(|item| item.directory).map(|item| json!({"from": item.from_res, "to": item.to_res})).collect::<Vec<_>>(),
            "rewritten": self.rewrites.iter().map(|rewrite| rewrite.new_res.clone()).collect::<Vec<_>>(),
            "binary": self.binary.iter().map(|(_, later)| later.clone()).collect::<Vec<_>>(),
            "pairs": self.pairs.iter().map(Pair::to_json).collect::<Vec<_>>(),
        })
    }

    /// Adds the editor's refusals and warnings from `move_files_check`.
    fn absorb(&mut self, check: &Value) {
        for refusal in check
            .get("refused")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            self.refused.push(refusal.clone());
        }
        for warning in check
            .get("warnings")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            self.warnings.push(warning.to_string());
        }
    }

    fn listing(&self) -> Vec<Value> {
        self.moves
            .iter()
            .map(|item| {
                let mut entry = json!({
                    "from": item.from_res,
                    "to": item.to_res,
                    "kind": if item.directory { "directory" } else { "file" },
                });
                if item.directory {
                    entry["files"] = json!(
                        self.files
                            .iter()
                            .filter(|file| file
                                .from_res
                                .starts_with(&format!("{}/", item.from_res)))
                            .count()
                    );
                } else {
                    entry["companions"] = json!(
                        item.companions
                            .iter()
                            .map(|suffix| format!("{}{suffix}", item.to_res))
                            .collect::<Vec<_>>()
                    );
                }
                entry
            })
            .collect()
    }

    fn rewritten(&self) -> Value {
        json!({
            "files": self.rewrites.len(),
            "replacements": self.rewrites.iter().map(|rewrite| rewrite.count).sum::<usize>(),
            "listed": self.rewrites.iter().take(MAX_LISTED).map(|rewrite| json!({
                "file": rewrite.new_res,
                "was": rewrite.res,
                "replacements": rewrite.count,
            })).collect::<Vec<_>>(),
        })
    }

    fn imported_listing(&self) -> Vec<Value> {
        self.imported
            .iter()
            .map(|(old, new)| {
                json!({"from": res_display(&self.project, old), "to": res_display(&self.project, new)})
            })
            .collect()
    }

    /// The report before anything changed: a dry run, or a refused call.
    fn report(&self, source: &str, dry_run: bool) -> Value {
        json!({
            "source": source,
            "dry_run": dry_run,
            "moved": [],
            "would_move": self.listing(),
            "files_moved": self.files.len(),
            "would_rewrite": self.rewritten(),
            "imported": self.imported_listing(),
            "binary_references": self.binary.iter().map(|(now, _)| now.clone()).collect::<Vec<_>>(),
            "refused": self.refused,
            "warnings": self.warnings,
            "project_dir": self.project.display().to_string(),
        })
    }
}

/// Plans the moves: checks every request, lists the files that change path and the text files
/// whose references change, without changing anything.
fn plan(project: &Path, requests: &[Request]) -> Plan {
    let mut plan = Plan {
        project: project.to_path_buf(),
        ..Plan::default()
    };
    for request in requests {
        if let Some(item) = check_move(&mut plan, project, request) {
            if let Some(other) = plan
                .moves
                .iter()
                .find(|other| overlaps(&item, other))
                .map(|other| other.from_res.clone())
            {
                plan.refuse(
                    &item.from_res,
                    format!("overlaps the move of {other} in this call (the same file, a folder and a file inside it, or one moving into the other); move them in separate calls"),
                );
                continue;
            }
            plan.moves.push(item);
        }
    }
    if !plan.refused.is_empty() {
        return plan;
    }
    for index in 0..plan.moves.len() {
        expand(&mut plan, index);
    }
    plan.pairs
        .sort_by_key(|pair| std::cmp::Reverse(pair.old().len()));
    find_references(&mut plan);
    plan
}

fn overlaps(a: &Move, b: &Move) -> bool {
    let near = |x: &str, y: &str| {
        x == y || x.starts_with(&format!("{y}/")) || y.starts_with(&format!("{x}/"))
    };
    near(&a.from_res, &b.from_res)
        || near(&a.to_res, &b.to_res)
        || near(&a.from_res, &b.to_res)
        || near(&a.to_res, &b.from_res)
}

/// The move a request names, or `None` after refusing it.
fn check_move(plan: &mut Plan, project: &Path, request: &Request) -> Option<Move> {
    let from = match resolve_in_project(project, &request.from) {
        Ok(path) => path,
        Err(problem) => {
            plan.refuse(&request.from, problem);
            return None;
        }
    };
    let from_res = res_display(project, &from)
        .trim_end_matches('/')
        .to_string();
    if let Some(reason) = protected(&from_res).or_else(|| protected_by_identity(project, &from)) {
        plan.refuse(&from_res, reason);
        return None;
    }
    let Ok(metadata) = std::fs::symlink_metadata(&from) else {
        plan.refuse(&from_res, "does not exist");
        return None;
    };
    let directory = metadata.is_dir();
    if !directory {
        if let Some(owner) = companion_owner(&from) {
            plan.refuse(
                &from_res,
                format!("belongs to {owner} and moves with it; move {owner} instead"),
            );
            return None;
        }
    }

    let mut to_text = request.to.trim().replace('\\', "/");
    if to_text.ends_with('/') {
        let name = from_res.rsplit('/').next().unwrap_or_default();
        to_text.push_str(name);
    }
    let to = match resolve_in_project(project, &to_text) {
        Ok(path) => path,
        Err(problem) => {
            plan.refuse(
                &from_res,
                format!("cannot move to {}: {problem}", request.to),
            );
            return None;
        }
    };
    let to_res = res_display(project, &to).trim_end_matches('/').to_string();
    if let Some(reason) = protected(&to_res).or_else(|| protected_by_identity(project, &to)) {
        plan.refuse(&from_res, format!("cannot move to {to_res}: it {reason}"));
        return None;
    }
    if to_res == from_res {
        plan.refuse(&from_res, format!("is already at {to_res}"));
        return None;
    }
    if to_res.starts_with(&format!("{from_res}/")) {
        plan.refuse(
            &from_res,
            format!("cannot move a folder into itself ({to_res})"),
        );
        return None;
    }
    let same_file = |path: &Path| matches!((path.canonicalize(), from.canonicalize()), (Ok(a), Ok(b)) if a == b);
    if std::fs::symlink_metadata(&to).is_ok() && !same_file(&to) {
        plan.refuse(
            &from_res,
            format!("cannot move to {to_res}: it already exists. Move or delete it first, or pick another name"),
        );
        return None;
    }
    let mut companions = Vec::new();
    if !directory {
        for suffix in [".uid", ".import"] {
            if sidecar(&to, suffix).exists() && !same_file(&to) {
                plan.refuse(
                    &from_res,
                    format!("cannot move to {to_res}: {to_res}{suffix} already exists. Delete the leftover first"),
                );
                return None;
            }
            if sidecar(&from, suffix).is_file() {
                companions.push(suffix);
            }
        }
    }
    let mut ancestor = to.parent();
    while let Some(folder) = ancestor {
        if folder.is_file() {
            plan.refuse(
                &from_res,
                format!(
                    "cannot move to {to_res}: {} is a file",
                    res_display(project, folder)
                ),
            );
            return None;
        }
        if folder == project {
            break;
        }
        ancestor = folder.parent();
    }
    if !directory && extension(&from) != extension(&to) {
        plan.warnings.push(format!(
            "{from_res} gets another extension ({to_res}); Godot picks the loader by extension, so the file may no longer load."
        ));
    }
    if hidden_from_godot(project, &to) {
        plan.warnings.push(format!(
            "{to_res} is in a folder Godot does not scan (a hidden name, a .gdignore or a nested project): the editor will not list or import it, and references from there are not searched."
        ));
    }
    Some(Move {
        from_res,
        to_res,
        from,
        to,
        directory,
        companions,
    })
}

/// Whether `path` or a folder above it inside the project is one Godot leaves out.
fn hidden_from_godot(project: &Path, path: &Path) -> bool {
    let Ok(relative) = path.strip_prefix(project) else {
        return false;
    };
    let mut current = project.to_path_buf();
    let parts: Vec<_> = relative.components().collect();
    for (index, part) in parts.iter().enumerate() {
        current.push(part.as_os_str());
        let last = index + 1 == parts.len();
        if skipped_by_godot(&current, !last || current.is_dir()) {
            return true;
        }
    }
    false
}

/// The file a `.uid` or `.import` file belongs to, when that file exists.
fn companion_owner(file: &Path) -> Option<String> {
    let name = file.file_name()?.to_string_lossy().into_owned();
    [".uid", ".import"].iter().find_map(|suffix| {
        let owner = name.strip_suffix(suffix)?;
        (!owner.is_empty() && file.with_file_name(owner).is_file()).then(|| owner.to_string())
    })
}

fn sidecar(file: &Path, suffix: &str) -> PathBuf {
    let mut name = file.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

/// Adds the files a move changes the path of, their imported data and the rewrite pairs.
fn expand(plan: &mut Plan, index: usize) {
    let item = &plan.moves[index];
    let (from_res, to_res) = (item.from_res.clone(), item.to_res.clone());
    let mut files: Vec<(PathBuf, String, String)> = Vec::new();
    if item.directory {
        plan.pairs.push(Pair::Prefix {
            old: format!("{from_res}/"),
            new: format!("{to_res}/"),
        });
        plan.pairs.push(Pair::Whole {
            old: from_res.clone(),
            new: to_res.clone(),
        });
        let mut inside = Vec::new();
        every_file(&item.from, &mut inside);
        for file in inside {
            if companion_owner(&file).is_some() {
                continue;
            }
            let res = res_display(&plan.project, &file);
            let moved = format!("{to_res}{}", &res[from_res.len()..]);
            files.push((file, res, moved));
        }
    } else {
        plan.pairs.push(Pair::Whole {
            old: from_res.clone(),
            new: to_res.clone(),
        });
        files.push((item.from.clone(), from_res, to_res));
    }
    for (file, old, new) in files {
        if sidecar(&file, ".import").is_file() {
            imported_data(plan, &old, &new);
        }
        plan.files.push(MovedFile {
            uid: file_uid(&file),
            from_res: old,
            to_res: new,
        });
    }
}

/// Every file under `folder`, whatever Godot scans: a folder move takes all of them.
fn every_file(folder: &Path, into: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return;
    };
    let mut entries: Vec<_> = entries.filter_map(Result::ok).collect();
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            every_file(&entry.path(), into);
        } else {
            into.push(entry.path());
        }
    }
}

/// The uid of a file: its `.uid` file, its `.import` file, or the header of a scene or resource.
fn file_uid(file: &Path) -> Option<String> {
    if let Ok(text) = std::fs::read_to_string(sidecar(file, ".uid")) {
        let text = text.trim();
        if text.starts_with("uid://") {
            return Some(text.to_string());
        }
    }
    if let Ok(text) = std::fs::read_to_string(sidecar(file, ".import")) {
        if let Some(uid) = header_uid(&text) {
            return Some(uid);
        }
    }
    if matches!(extension(file).as_str(), "tscn" | "tres") {
        let text = std::fs::read_to_string(file).ok()?;
        return text.lines().next().and_then(header_uid);
    }
    None
}

/// The name Godot gives a source's imported data: `<file name>-<md5 of its res:// path>`.
fn imported_base(res: &str) -> String {
    let name = res.rsplit('/').next().unwrap_or_default();
    format!("{name}-{}", md5_hex(res.as_bytes()))
}

/// Plans renaming the imported data of a source moving from `old` to `new`, so the `.import`
/// file names data that exists under the name Godot looks for, and no reimport is needed.
fn imported_data(plan: &mut Plan, old: &str, new: &str) {
    let (old_base, new_base) = (imported_base(old), imported_base(new));
    let folder = plan.project.join(".godot").join("imported");
    let Ok(entries) = std::fs::read_dir(&folder) else {
        return;
    };
    let prefix = format!("{old_base}.");
    let mut found = false;
    for entry in entries.filter_map(Result::ok) {
        let name = entry.file_name().to_string_lossy().into_owned();
        if let Some(rest) = name.strip_prefix(&prefix) {
            plan.imported
                .push((entry.path(), folder.join(format!("{new_base}.{rest}"))));
            found = true;
        }
    }
    if found {
        plan.pairs.push(Pair::Prefix {
            old: format!("{IMPORTED_DIR}{prefix}"),
            new: format!("{IMPORTED_DIR}{new_base}."),
        });
    }
}

/// Where a `res://` path is after the moves; a file's `.uid` and `.import` go with it.
fn moved_path(plan: &Plan, res: &str) -> String {
    for item in &plan.moves {
        if res == item.from_res {
            return item.to_res.clone();
        }
        for suffix in &item.companions {
            if res.strip_suffix(suffix) == Some(item.from_res.as_str()) {
                return format!("{}{suffix}", item.to_res);
            }
        }
        if item.directory {
            if let Some(rest) = res.strip_prefix(&format!("{}/", item.from_res)) {
                return format!("{}/{rest}", item.to_res);
            }
        }
    }
    res.to_string()
}

/// Finds the text files whose references change and the binary files that name an old path.
fn find_references(plan: &mut Plan) {
    let mut candidates = Vec::new();
    collect(&plan.project, &mut candidates);
    let mut large = Vec::new();
    for candidate in candidates {
        let kind = extension(&candidate);
        let binary = BINARY_EXTENSIONS.contains(&kind.as_str());
        if !binary && !TEXT_EXTENSIONS.contains(&kind.as_str()) {
            continue;
        }
        let res = res_display(&plan.project, &candidate);
        if std::fs::metadata(&candidate).is_ok_and(|metadata| metadata.len() > MAX_SCAN_BYTES) {
            large.push(res);
            continue;
        }
        let Ok(bytes) = std::fs::read(&candidate) else {
            continue;
        };
        if binary {
            // A binary resource keeps its paths as UTF-8 strings, so they show in a lossy text.
            if rewrite(&String::from_utf8_lossy(&bytes), &plan.pairs).is_some() {
                let later = moved_path(plan, &res);
                plan.binary.push((res, later));
            }
            continue;
        }
        let text = match String::from_utf8(bytes) {
            Ok(text) => text,
            Err(error) => {
                if rewrite(&String::from_utf8_lossy(error.as_bytes()), &plan.pairs).is_some() {
                    plan.warnings.push(format!(
                        "{res} names a moved path but is not valid UTF-8, so it was left as it is; fix it by hand."
                    ));
                }
                continue;
            }
        };
        if let Some((text, count)) = rewrite(&text, &plan.pairs) {
            let new_res = moved_path(plan, &res);
            plan.rewrites.push(Rewrite {
                res,
                new_res,
                text,
                count,
            });
        }
    }
    if !large.is_empty() {
        plan.warnings.push(format!(
            "{} file(s) larger than {MAX_SCAN_BYTES} bytes were not searched for references: {}",
            large.len(),
            large.join(", ")
        ));
    }
}

fn res_to_disk(project: &Path, res: &str) -> PathBuf {
    project.join(res.trim_start_matches("res://"))
}

/// Moves the files on disk and writes the rewritten texts. A failed move puts back the moves made
/// before it, so the project is as it was; a failure after all files moved is a warning.
fn execute(plan: &Plan, editor: bool) -> Value {
    let mut warnings = plan.warnings.clone();
    let mut created: Vec<String> = Vec::new();
    let mut done: Vec<(PathBuf, PathBuf)> = Vec::new();
    let mut failure: Option<Value> = None;
    'moves: for item in &plan.moves {
        if let Some(parent) = item.to.parent() {
            let mut missing = Vec::new();
            let mut folder = Some(parent);
            while let Some(path) = folder.filter(|path| !path.exists()) {
                missing.push(path.to_path_buf());
                folder = path.parent();
            }
            if let Err(error) = std::fs::create_dir_all(parent) {
                failure = Some(
                    json!({"path": item.from_res, "reason": format!("could not create {}: {error}", res_display(&plan.project, parent))}),
                );
                break;
            }
            created.extend(
                missing
                    .iter()
                    .rev()
                    .map(|path| res_display(&plan.project, path)),
            );
        }
        let mut steps = vec![(item.from.clone(), item.to.clone())];
        for suffix in &item.companions {
            steps.push((sidecar(&item.from, suffix), sidecar(&item.to, suffix)));
        }
        for (from, to) in steps {
            if let Err(error) = std::fs::rename(&from, &to) {
                failure = Some(
                    json!({"path": res_display(&plan.project, &from), "reason": format!(
                        "could not be moved to {}: {error}. Nothing was changed: the moves before it were put back",
                        res_display(&plan.project, &to)
                    )}),
                );
                break 'moves;
            }
            done.push((from, to));
        }
    }
    if let Some(refusal) = failure {
        for (from, to) in done.iter().rev() {
            if let Err(error) = std::fs::rename(to, from) {
                warnings.push(format!(
                    "{} could not be put back at {}: {error}",
                    res_display(&plan.project, to),
                    res_display(&plan.project, from)
                ));
            }
        }
        for folder in created.iter().rev() {
            let _ = std::fs::remove_dir(res_to_disk(&plan.project, folder));
        }
        let mut report = plan.report(if editor { "editor" } else { "disk" }, false);
        report["refused"] = json!([refusal]);
        report["warnings"] = json!(warnings);
        return report;
    }

    let mut imported = Vec::new();
    for (from, to) in &plan.imported {
        match std::fs::rename(from, to) {
            Ok(()) => imported.push(json!({"from": res_display(&plan.project, from), "to": res_display(&plan.project, to)})),
            Err(error) => warnings.push(format!(
                "The imported data {} could not be renamed ({error}); Godot imports the file again on its next scan.",
                res_display(&plan.project, from)
            )),
        }
    }
    let mut unwritten = Vec::new();
    for rewrite in &plan.rewrites {
        if let Err(error) =
            std::fs::write(res_to_disk(&plan.project, &rewrite.new_res), &rewrite.text)
        {
            unwritten.push(rewrite.new_res.clone());
            warnings.push(format!(
                "{} could not be rewritten ({error}) and still names the old path(s).",
                rewrite.new_res
            ));
        }
    }

    let mut report = json!({
        "source": if editor { "editor" } else { "disk" },
        "dry_run": false,
        "moved": plan.listing(),
        "files_moved": plan.files.len(),
        "created_dirs": created,
        "rewritten": plan.rewritten(),
        "imported": imported,
        "binary_references": plan.binary.iter().map(|(_, later)| later.clone()).collect::<Vec<_>>(),
        "refused": [],
        "rescan_needed": !editor,
        "project_dir": plan.project.display().to_string(),
    });
    if !unwritten.is_empty() {
        report["unwritten"] = json!(unwritten);
    }
    if !editor {
        disk_caches(plan, &mut warnings);
        if !plan.binary.is_empty() {
            warnings.push(format!(
                "{} binary resource(s) (.scn, .res) still hold an old path (listed under binary_references): only the editor can rewrite them. They still load through the uids of the moved files; open the project in the editor and save them again (godot_resave_resources with their paths, or open and save each scene).",
                plan.binary.len()
            ));
        }
        warnings.push("No editor was connected, so the files were moved on disk. An editor that has this project open needs a rescan (godot_resave_resources without paths), and its open tabs of moved files still show the old paths.".into());
    }
    report["warnings"] = json!(warnings);
    report
}

/// Without an editor: points the uid cache and the global class cache at the new paths, so a game
/// or a headless script run before the editor rescans still finds the moved files.
fn disk_caches(plan: &Plan, warnings: &mut Vec<String>) {
    let entries: Vec<(String, String)> = plan
        .files
        .iter()
        .filter_map(|file| Some((file.to_res.clone(), file.uid.clone()?)))
        .collect();
    if !entries.is_empty() {
        if let Err(error) = record_uids(&plan.project, &entries) {
            warnings.push(format!(
                "The uid cache could not be updated ({error}); the editor rebuilds it on its next scan."
            ));
        }
    }
    let cache = plan.project.join(CLASS_CACHE);
    if let Ok(text) = std::fs::read_to_string(&cache) {
        if let Some((text, _)) = rewrite(&text, &plan.pairs) {
            if let Err(error) = std::fs::write(&cache, text) {
                warnings.push(format!(
                    "The global class cache could not be updated ({error}); the editor rebuilds it on its next scan."
                ));
            }
        }
    }
}

fn push_warning(report: &mut Value, warning: String) {
    if !report["warnings"].is_array() {
        report["warnings"] = json!([]);
    }
    if let Some(list) = report["warnings"].as_array_mut() {
        list.push(json!(warning));
    }
}

fn append_warnings(report: &mut Value, from: &Value) {
    for warning in from
        .get("warnings")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        push_warning(report, warning.to_string());
    }
}

/// Turns a move report into the tool's answer: an error when something was refused.
fn answer(report: Value) -> ToolResult {
    let body = serde_json::to_string_pretty(&report).unwrap_or_default();
    let refused = report
        .get("refused")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    if refused == 0 {
        return ToolResult::text(body);
    }
    let headline = if report.get("dry_run").and_then(Value::as_bool) == Some(true) {
        format!("Dry run, nothing was moved: {refused} move(s) would be refused (see refused).")
    } else {
        format!("Nothing was moved: {refused} move(s) were refused (see refused).")
    };
    ToolResult::error(
        format!("{headline}\n{body}"),
        &[
            "Read each refused entry's reason and fix that move: another destination, save or close the open file, or move it in a separate call",
            "Leave the refused moves out to move the others",
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn whole(old: &str, new: &str) -> Pair {
        Pair::Whole {
            old: old.into(),
            new: new.into(),
        }
    }

    fn prefix(old: &str, new: &str) -> Pair {
        Pair::Prefix {
            old: old.into(),
            new: new.into(),
        }
    }

    #[test]
    fn a_whole_path_is_replaced_but_not_a_longer_one() {
        let pairs = [whole("res://a.gd", "res://b/a.gd")];
        assert_eq!(
            rewrite(
                "x=\"res://a.gd\" y=\"res://a.gdshader\" z=preload('res://a.gd')",
                &pairs
            ),
            Some((
                "x=\"res://b/a.gd\" y=\"res://a.gdshader\" z=preload('res://b/a.gd')".into(),
                2
            ))
        );
        assert_eq!(rewrite("res://a.gd.uid res://xa.gd", &pairs), None);
        assert_eq!(
            rewrite("res://a.gd", &pairs),
            Some(("res://b/a.gd".into(), 1))
        );
    }

    #[test]
    fn a_folder_moves_every_path_inside_it_and_itself() {
        let mut pairs = vec![
            prefix("res://levels/old/", "res://legacy/old/"),
            whole("res://levels/old", "res://legacy/old"),
        ];
        pairs.sort_by_key(|pair| std::cmp::Reverse(pair.old().len()));
        let text = "[ext_resource path=\"res://levels/old/a/b.tscn\"]\nDirAccess.open(\"res://levels/old\")\n\"res://levels/older/c.tscn\" *res://levels/old/auto.gd";
        assert_eq!(
            rewrite(text, &pairs),
            Some((
                "[ext_resource path=\"res://legacy/old/a/b.tscn\"]\nDirAccess.open(\"res://legacy/old\")\n\"res://levels/older/c.tscn\" *res://legacy/old/auto.gd".into(),
                3
            ))
        );
    }

    #[test]
    fn a_chain_of_moves_renames_each_path_once() {
        let pairs = [
            whole("res://a.gd", "res://b.gd"),
            whole("res://b.gd", "res://c.gd"),
        ];
        assert_eq!(
            rewrite("res://a.gd res://b.gd", &pairs),
            Some(("res://b.gd res://c.gd".into(), 2))
        );
    }

    #[test]
    fn the_imported_name_is_the_file_name_and_the_md5_of_its_path() {
        assert_eq!(
            imported_base("res://assets/ui/frog_world_logo.png"),
            "frog_world_logo.png-d3bb592405f5f27b40400757a4149d6d"
        );
    }

    #[test]
    fn moves_must_be_a_short_list_of_from_and_to() {
        assert!(requested_moves(&json!({})).is_err());
        assert!(requested_moves(&json!({"moves": []})).is_err());
        assert!(requested_moves(&json!({"moves": [{"from": "res://a.gd"}]})).is_err());
        assert!(requested_moves(&json!({"moves": [{"from": "", "to": "res://b.gd"}]})).is_err());
        let many: Vec<Value> = (0..65)
            .map(|index| json!({"from": format!("res://{index}.gd"), "to": "res://x/"}))
            .collect();
        assert!(requested_moves(&json!({"moves": many})).is_err());
        assert_eq!(
            requested_moves(&json!({"moves": [{"from": "res://a.gd", "to": "res://b.gd"}]}))
                .map(|moves| moves.len())
                .ok(),
            Some(1)
        );
    }
}
