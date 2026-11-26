//! Smoke-running an exported pack: `godot --main-pack`, stopped after a given time.
//!
//! A trimmed export (`export_filter` `resources` or `scenes`) holds the listed files and their
//! dependencies only, so a file the game loads by a path no scene names is missing from it. The
//! run reads what Godot logs and names the `res://` paths it failed to load.

use super::export::{exit_text, failure, godot_failed, messages, readable, tail};
use super::{ToolManager, str_arg, user_args_of};
use crate::paths::resolve_in_project;
use crate::protocol::{Tool, ToolResult};
use serde_json::{Value, json};
use std::time::Duration;

pub const RUN_PACK: &str = "godot_run_pack";

const DEFAULT_SECONDS: u64 = 10;
const MAX_SECONDS: u64 = 600;
/// Phrases of the Godot messages that mean a file was asked for and not found or not loaded.
const MISSING_PHRASES: &[&str] = &[
    "Resource file not found",
    "Can't open file from path",
    "Cannot open file",
    "Unable to open file",
    "Failed loading resource",
    "No loader found for resource",
    "Can't load dependency",
    "Failed to load script",
    "File not found",
    "Unrecognized UID",
];
/// How many missing files an answer names.
const MAX_LISTED_MISSING: usize = 200;

pub fn catalog() -> Vec<Tool> {
    vec![super::tool(
        RUN_PACK,
        "Smoke-runs an exported .pck/.zip with godot --main-pack (headless by default) from an empty temp folder for seconds, to find files a trimmed export lacks. Returns ok, ran_full_time, exit code, duration, missing_resources (failed res:// or uid:// loads), ERROR/WARNING lines and the output tail. ok is false (an error answer) on a missing file, a logged error, or an early non-zero exit or crash. An early exit 0 is ok. Uses the game's real user:// folder. Not a managed run: godot_get_run_status does not list it. Waits for an export running in the same project.",
        json!({
            "pack_path": {"type": "string", "description": "res:// or project-relative .pck or .zip inside the project."},
            "scene": {"type": "string", "description": "Scene to start instead of the main one."},
            "headless": {"type": "boolean", "description": "Default true; false opens a window (compiles shaders)."},
            "seconds": {"type": "integer", "minimum": 1, "maximum": MAX_SECONDS, "description": "Default 10."},
            "user_args": {"type": "array", "items": {"type": "string"}, "description": "Game arguments, after --."},
            "project_path": {"type": "string", "description": "Default: editor's, else server's project."}
        }),
        &["pack_path"],
    ).additive()]
}

pub async fn call(tools: &ToolManager, args: Value) -> ToolResult {
    let Some(requested) = str_arg(&args, "pack_path").filter(|path| !path.trim().is_empty()) else {
        return ToolResult::error(
            "pack_path is required: the .pck or .zip to run",
            &["Pass pack_path, such as build/game.pck (godot_export_project writes one)"],
        );
    };
    let scene = str_arg(&args, "scene");
    if let Some(scene) = scene
        && (!scene.starts_with("res://") || scene.split(['/', '\\']).any(|part| part == ".."))
    {
        return ToolResult::error(
            format!("Invalid scene '{scene}': scene must be a res:// path inside the pack"),
            &[
                "Use a path such as res://scenes/main.tscn, or leave scene out to start the main scene",
            ],
        );
    }
    let seconds = match args.get("seconds").filter(|value| !value.is_null()) {
        None => DEFAULT_SECONDS,
        Some(value) => match value.as_u64().filter(|n| (1..=MAX_SECONDS).contains(n)) {
            Some(seconds) => seconds,
            None => {
                return ToolResult::error(
                    format!("seconds must be a whole number from 1 to {MAX_SECONDS}, not {value}"),
                    &["Pass seconds: 10, or leave it out for the default"],
                );
            }
        },
    };
    let user_args = match user_args_of(&args) {
        Ok(user_args) => user_args.unwrap_or_default(),
        Err(result) => return result,
    };
    let headless = args
        .get("headless")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let project = match tools.resolve_project(&args).await {
        Ok(project) => project.dir,
        Err(result) => return result,
    };
    let pack = match resolve_in_project(&project, requested) {
        Ok(pack) => pack,
        Err(problem) => {
            return ToolResult::error(
                format!("pack_path {requested:?} is not usable: {problem}"),
                &["Pass a pack inside the project, such as build/game.pck"],
            );
        }
    };
    let extension = pack
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    if extension != "pck" && extension != "zip" {
        return ToolResult::error(
            format!("pack_path {requested:?} must end in .pck or .zip"),
            &["Pass the pack godot_export_project wrote (mode pack), such as build/game.pck"],
        );
    }
    if !pack.is_file() {
        return ToolResult::error(
            format!(
                "There is no file at pack_path {requested:?} ({})",
                pack.display()
            ),
            &["Export the pack first with godot_export_project (mode pack), then run it"],
        );
    }
    // Godot gets an absolute path, which never starts with '-'.
    let pack = std::path::absolute(&pack).unwrap_or(pack);
    // An export writing this pack finishes first; the run itself holds nothing.
    tools.gates.settle(&project).await;

    let run = match tools
        .cli
        .run_pack(
            &pack,
            headless,
            scene,
            &user_args,
            Duration::from_secs(seconds),
        )
        .await
    {
        Ok(run) => readable(run),
        Err(error) => return godot_failed(&error),
    };

    let mut errors = messages(&run, "ERROR:");
    errors.extend(messages(&run, "SCRIPT ERROR:"));
    errors.extend(messages(&run, "USER ERROR:"));
    let mut missing = missing_resources(&format!("{}\n{}", run.stdout, run.stderr));
    let missing_count = missing.len();
    missing.truncate(MAX_LISTED_MISSING);
    let quit_badly = !run.timed_out && run.exit_code != Some(0);
    let ok = !quit_badly && missing_count == 0 && errors.is_empty();
    let duration = (run.duration.as_secs_f64() * 10.0).round() / 10.0;
    let mut report = json!({
        "ok": ok,
        "project_dir": project.display().to_string(),
        "pack_path": pack.display().to_string(),
        "scene": scene,
        "headless": headless,
        "seconds": seconds,
        "ran_full_time": run.timed_out,
        "exit_code": run.exit_code,
        "duration_seconds": duration,
        "missing_resources": missing,
        "errors": errors,
        "warnings": messages(&run, "WARNING:"),
        "log": tail(&run.stdout),
        "stderr": tail(&run.stderr),
    });
    if missing_count > MAX_LISTED_MISSING {
        report["missing_resources_count"] = json!(missing_count);
    }
    if ok {
        return ToolResult::text(serde_json::to_string_pretty(&report).unwrap_or_default());
    }

    let mut summary = Vec::new();
    if quit_badly {
        summary.push(match run.exit_code {
            Some(code) => format!("The pack's game quit after {duration}s with exit code {code}"),
            None => format!(
                "The pack's game crashed after {duration}s (exit code {})",
                exit_text(None)
            ),
        });
    }
    if missing_count > 0 {
        summary.push(format!(
            "it failed to load {missing_count} file(s) the pack does not hold: {}",
            report["missing_resources"]
                .as_array()
                .map(|paths| {
                    paths
                        .iter()
                        .take(10)
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .unwrap_or_default()
        ));
    }
    if !report["errors"].as_array().is_none_or(Vec::is_empty) {
        summary.push(format!(
            "Godot logged {} error(s)",
            report["errors"].as_array().map_or(0, Vec::len)
        ));
    }
    let mut summary = summary.join("; ");
    if let Some(first) = summary.get_mut(..1) {
        first.make_ascii_uppercase();
    }
    let hints: &[&str] = if missing_count > 0 {
        &[
            "Add each missing resource to the preset's export_files with godot_set_export_preset (the resources filter takes the listed files and their dependencies), and a non-resource file such as *.json to include_filter; then export again with godot_export_project and run the pack again",
            "A path the game builds at run time (load(\"res://levels/%s.tscn\" % name)) is never named by a scene: list every file it can build",
        ]
    } else if quit_badly {
        &[
            "Read stderr and log in the details: Godot names what stopped the game",
            "Check that the pack was exported by this Godot version (godot_get_version), with godot_export_project",
        ]
    } else {
        &["Read errors in the details, fix what they name, export again and run the pack again"]
    };
    failure(summary, &report, hints)
}

/// The `res://` and `uid://` paths of the lines in `text` that say a file was not found or not
/// loaded, each once, in the order Godot named them.
fn missing_resources(text: &str) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    for line in text.lines() {
        if !MISSING_PHRASES.iter().any(|phrase| line.contains(phrase)) {
            continue;
        }
        if let Some(path) = first_path(line)
            && !found.contains(&path)
        {
            found.push(path);
        }
    }
    found
}

/// The first `res://` or `uid://` path in `line`, without the quotes, the `:<line>` and the
/// final period Godot puts around it.
fn first_path(line: &str) -> Option<String> {
    let start = ["res://", "uid://"]
        .iter()
        .filter_map(|scheme| line.find(scheme))
        .min()?;
    let rest = &line[start..];
    // A path may hold spaces, so it ends at a quote, a bracket, Godot's " (expected type" or
    // " - " that follows it, or the end of the line.
    let mut end = rest
        .find(['\'', '"', '`', ')', ',', ';', '\t'])
        .unwrap_or(rest.len());
    for stop in [" (", " - "] {
        if let Some(at) = rest[..end].find(stop) {
            end = at;
        }
    }
    let mut path = rest[..end].trim_end().trim_end_matches(['.', ':']);
    if let Some((before, number)) = path.rsplit_once(':')
        && !before.ends_with(':')
        && number.chars().all(|c| c.is_ascii_digit())
    {
        path = before;
    }
    let scheme_only = path == "res://" || path == "uid://";
    (!scheme_only).then(|| path.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_loads_name_their_files_once() {
        let log = "\
ERROR: Resource file not found: res://levels/two.tscn (expected type: PackedScene)
   at: _load (core/io/resource_loader.cpp:345)
ERROR: Can't open file from path 'res://data/items.json'.
ERROR: Failed loading resource: res://art/frog.png.
ERROR: Cannot open file 'res://shaders/water.gdshader'.
ERROR: Failed to load script \"res://scripts/boss.gd\" with error \"File not found\".
ERROR: Unrecognized UID: \"uid://b3k2x\".
ERROR: res://levels/three.tscn:12 - Parse Error: something else
ERROR: Resource file not found: res://levels/two.tscn (expected type: PackedScene)
ERROR: Can't load dependency: res://music/theme.ogg.
WARNING: Unable to open file: res://.
print: found res://not/missing.tres
";
        assert_eq!(
            missing_resources(log),
            vec![
                "res://levels/two.tscn",
                "res://data/items.json",
                "res://art/frog.png",
                "res://shaders/water.gdshader",
                "res://scripts/boss.gd",
                "uid://b3k2x",
                "res://music/theme.ogg",
            ]
        );
    }

    #[test]
    fn a_path_loses_its_line_number_and_punctuation() {
        assert_eq!(
            first_path("File not found: res://a/b.tres:42.").as_deref(),
            Some("res://a/b.tres")
        );
        assert_eq!(
            first_path("Failed loading resource: res://a b/c.png.").as_deref(),
            Some("res://a b/c.png")
        );
        assert_eq!(
            first_path("ERROR: res://levels/three.tscn:12 - Parse Error: x").as_deref(),
            Some("res://levels/three.tscn")
        );
        assert_eq!(first_path("no path here"), None);
        assert_eq!(first_path("Unable to open file: res://."), None);
    }
}
