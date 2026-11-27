//! Export presets and headless exports.
//!
//! `export_presets.cfg` is read and written by Godot itself (its `ConfigFile`, in a throw-away
//! project), the way the Export dialog stores it. The editor keeps the presets in memory and
//! writes them back whenever its Export dialog changes one, so a preset is not changed while the
//! editor has the project open. An export is `godot --headless --export-*`, after
//! `godot --headless --import` so every resource is imported first.

use super::{ToolManager, str_arg};
use crate::cli_runner::{ExportMode, HeadlessRun};
use crate::paths::{res_display, resolve_in_project, same_directory};
use crate::protocol::{Tool, ToolResult};
use serde_json::{Map, Value, json};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

pub const LIST: &str = "godot_list_export_presets";
pub const SET: &str = "godot_set_export_preset";
pub const EXPORT: &str = "godot_export_project";

const MAX_NAME_CHARS: usize = 128;
const MAX_FIELD_CHARS: usize = 4096;
const MAX_OPTIONS: usize = 128;
/// A preset's `export_files` list; a large game names a few thousand resources.
const MAX_EXPORT_FILES: usize = 100_000;
/// The values of a preset's `export_filter`, as Godot stores them.
const EXPORT_FILTERS: &[&str] = &[
    "all_resources",
    "scenes",
    "resources",
    "exclude",
    "customized",
];
/// The fields a set call stores as text, as `export_presets.cfg` holds them.
const TEXT_FIELDS: &[&str] = &[
    "export_path",
    "export_filter",
    "include_filter",
    "exclude_filter",
    "custom_features",
];
/// How many missing `export_files` an answer names.
const MAX_LISTED_MISSING: usize = 20;
const DEFAULT_TIMEOUT_SECONDS: u64 = 600;
/// How much of each output stream an answer quotes.
const LOG_TAIL_BYTES: usize = 12 * 1024;
const MAX_LISTED_LINES: usize = 40;
/// The platforms the Godot editor itself provides, by the name a preset stores.
const BUILT_IN_PLATFORMS: &[&str] = &[
    "Linux",
    "Windows Desktop",
    "macOS",
    "Android",
    "iOS",
    "Web",
    "visionOS",
];
/// Files that belong to the project: an export must never be written over one.
const SOURCE_EXTENSIONS: &[&str] = &[
    "gd", "tscn", "scn", "tres", "res", "godot", "cfg", "import", "uid", "gdshader", "cs",
];

pub fn catalog() -> Vec<Tool> {
    let project_path = json!({
        "type": "string",
        "description": "Default: editor's, else server's project."
    });
    vec![
        super::tool(
            LIST,
            "Lists export presets: index, name, platform, export_path, export_filter, export_files, include/exclude_filter, custom_features and stored options (an unstored option takes the platform default). Needs the Godot binary only.",
            json!({ "project_path": project_path }),
            &[],
        ).read_only(),
        super::tool(
            SET,
            "Creates (create true, with platform) or changes an export preset in export_presets.cfg. Only given fields and options change; options keep their stored type. Godot ignores an option its platform lacks: check new_options in the answer for typos. Refused while the editor has the project open: godot_close_editor first. Platforms: Linux, Windows Desktop, macOS, Android, iOS, Web, visionOS, or one an export plugin adds. A new preset exports all_resources.",
            json!({
                "name": {"type": "string", "description": "Preset name."},
                "create": {"type": "boolean", "description": "Create if missing. Default false: an unknown name is refused."},
                "platform": {"type": "string", "description": "Required to create; else must match the stored one."},
                "export_path": {"type": "string", "description": "Project-relative output path."},
                "export_filter": {"type": "string", "enum": EXPORT_FILTERS, "description": "all_resources, scenes / resources (listed + dependencies), exclude (all but listed), customized (not settable). include/exclude_filter apply on top."},
                "export_files": {"type": "array", "items": {"type": "string"}, "description": "res:// paths for the filter; replaces the list. missing_files names absent ones."},
                "include_filter": {"type": "string", "description": "Comma-separated globs of non-resource files to add (*.json)."},
                "exclude_filter": {"type": "string", "description": "Comma-separated globs to leave out."},
                "custom_features": {"type": "string", "description": "Comma-separated feature tags."},
                "options": {
                    "type": "object",
                    "description": "Options by name, each a string, number or boolean, e.g. {\"binary_format/architecture\": \"arm32\"}.",
                    "additionalProperties": {"type": ["string", "number", "boolean"]}
                },
                "project_path": project_path
            }),
            &["name"],
        ).destructive_idempotent(),
        super::tool(
            EXPORT,
            "Exports headless with a preset: pack (.pck/.zip data, no template), release or debug (need export templates or custom_template options). Imports first unless import is false or the editor has the project open. Returns ok, path, size_bytes, exit code, duration, ERROR/WARNING lines, log tail; a missing preset or template, failed import, no file or timeout is an error. output_path must be in the project (folder created, file replaced). Runs alone in the project: waits for godot_run_script calls there.",
            json!({
                "preset": {"type": "string", "description": "Preset name (godot_list_export_presets)."},
                "mode": {"type": "string", "enum": ["pack", "release", "debug"], "description": "pack (default): data only. release/debug: the game, with an export template."},
                "output_path": {"type": "string", "description": "In the project; .pck/.zip for pack. Default: preset export_path."},
                "import": {"type": "boolean", "description": "Import before exporting. Default true."},
                "timeout_seconds": {"type": "integer", "minimum": 1, "maximum": 3600, "description": "Per step. Default 600; the process tree is killed on timeout."},
                "project_path": project_path
            }),
            &["preset"],
        ).destructive(),
    ]
}

pub async fn call(tools: &ToolManager, name: &str, args: Value) -> ToolResult {
    match name {
        LIST => list(tools, &args).await,
        SET => set(tools, &args).await,
        _ => export(tools, &args).await,
    }
}

async fn list(tools: &ToolManager, args: &Value) -> ToolResult {
    let project = match tools.resolve_project(args).await {
        Ok(project) => project.dir,
        Err(result) => return result,
    };
    let answer = match read_presets(tools, &project).await {
        Ok(answer) => answer,
        Err(result) => return result,
    };
    let mut report = json!({
        "project_dir": project.display().to_string(),
        "file_exists": answer.get("file_exists").cloned().unwrap_or(json!(false)),
        "presets": answer.get("presets").cloned().unwrap_or(json!([])),
    });
    if report["file_exists"] != json!(true) {
        report["note"] = json!(
            "The project has no export_presets.cfg. Create a preset with godot_set_export_preset (create true)."
        );
    }
    ToolResult::text(serde_json::to_string_pretty(&report).unwrap_or_default())
}

async fn set(tools: &ToolManager, args: &Value) -> ToolResult {
    let request = match set_request(args) {
        Ok(request) => request,
        Err(problem) => {
            return ToolResult::error(
                problem,
                &["Fix the parameter the message names; the shapes are in the tool's inputSchema"],
            );
        }
    };
    let project = match tools.resolve_project(args).await {
        Ok(project) => project.dir,
        Err(result) => return result,
    };
    if editor_has_project(tools, &project).await {
        return ToolResult::error(
            format!(
                "The editor has {} open, and it writes its own copy of the export presets back to export_presets.cfg, so the preset was not changed",
                project.display()
            ),
            &[
                "Close the editor with godot_close_editor, then call godot_set_export_preset again",
                "Or ask the person to change the preset in Project > Export",
            ],
        );
    }
    let mut missing_files: Vec<String> = Vec::new();
    if let Some(files) = request.get("export_files").and_then(Value::as_array) {
        for path in files.iter().filter_map(Value::as_str) {
            match resolve_in_project(&project, path) {
                Ok(file) if file.is_file() => {}
                Ok(_) => missing_files.push(path.to_string()),
                Err(problem) => {
                    return ToolResult::error(
                        format!("export_files entry {path:?} is not usable: {problem}"),
                        &["List files inside the project only, as res:// paths"],
                    );
                }
            }
        }
    }
    let mut request = request;
    request["action"] = json!("set");
    request["file"] = json!(presets_file(&project));
    let answer = match tools.cli.export_presets(&request).await {
        Ok(answer) => answer,
        Err(error) => return godot_failed(&error),
    };
    let preset = str_arg(args, "name").unwrap_or_default();
    match str_arg(&answer, "error") {
        None => {}
        Some("NOT_FOUND") => {
            return ToolResult::error(
                format!(
                    "The project has no export preset named {preset:?}; presets: {}",
                    names_of(&answer)
                ),
                &[
                    "Pass create: true and platform to create it",
                    "Or pass one of the listed names",
                ],
            );
        }
        Some("PLATFORM") => {
            return ToolResult::error(
                format!(
                    "Preset {preset:?} exports for {}, and its platform is not changed",
                    str_arg(&answer, "platform").unwrap_or("another platform")
                ),
                &[
                    "Drop platform to change the preset",
                    "Or create a preset with another name for the other platform",
                ],
            );
        }
        Some("TYPE") => {
            return ToolResult::error(
                format!(
                    "Option {} holds a {}; a value of another type is not stored",
                    str_arg(&answer, "option").unwrap_or("?"),
                    str_arg(&answer, "expected").unwrap_or("value of another type")
                ),
                &["Send a value of the type the option has (godot_list_export_presets shows it)"],
            );
        }
        Some(code) => return presets_unusable(code, &answer, &project),
    }
    let mut report = json!({
        "project_dir": project.display().to_string(),
        "created": answer.get("created").cloned().unwrap_or(json!(false)),
        "changed": answer.get("changed").cloned().unwrap_or(json!([])),
        "new_options": answer.get("new_options").cloned().unwrap_or(json!([])),
        "preset": answer.get("preset").cloned().unwrap_or(Value::Null),
    });
    if !missing_files.is_empty() {
        report["missing_files_count"] = json!(missing_files.len());
        missing_files.truncate(MAX_LISTED_MISSING);
        report["missing_files"] = json!(missing_files);
    }
    let preset_value = &report["preset"];
    let filter = preset_value["export_filter"].as_str().unwrap_or("");
    let listed = preset_value["export_files"]
        .as_array()
        .is_some_and(|files| !files.is_empty());
    if matches!(filter, "scenes" | "resources") && !listed {
        report["note"] = json!(format!(
            "export_filter is {filter} and export_files is empty, so the export takes no {}; set export_files",
            if filter == "scenes" {
                "scenes"
            } else {
                "resources"
            }
        ));
    }
    if let Some(platform) = str_arg(args, "platform")
        && !BUILT_IN_PLATFORMS.contains(&platform)
    {
        report["warning"] = json!(format!(
            "{platform:?} is not a platform the Godot editor provides ({}); Godot skips a preset whose platform no export plugin adds",
            BUILT_IN_PLATFORMS.join(", ")
        ));
    }
    ToolResult::text(serde_json::to_string_pretty(&report).unwrap_or_default())
}

async fn export(tools: &ToolManager, args: &Value) -> ToolResult {
    let preset_name = str_arg(args, "preset").unwrap_or_default();
    if let Err(problem) = check_name(preset_name) {
        return ToolResult::error(
            problem,
            &["Pass preset: a name godot_list_export_presets lists"],
        );
    }
    let mode = match str_arg(args, "mode").unwrap_or("pack") {
        "release" => ExportMode::Release,
        "debug" => ExportMode::Debug,
        _ => ExportMode::Pack,
    };
    let limit = Duration::from_secs(
        args.get("timeout_seconds")
            .and_then(Value::as_u64)
            .unwrap_or(DEFAULT_TIMEOUT_SECONDS),
    );
    let project = match tools.resolve_project(args).await {
        Ok(project) => project.dir,
        Err(result) => return result,
    };

    let answer = match read_presets(tools, &project).await {
        Ok(answer) => answer,
        Err(result) => return result,
    };
    if answer.get("file_exists") != Some(&json!(true)) {
        return ToolResult::error(
            format!(
                "{} has no export_presets.cfg, so there is no preset {preset_name:?} to export with",
                project.display()
            ),
            &[
                "Create the preset with godot_set_export_preset (create: true, platform)",
                "Or ask the person to add one in Project > Export",
            ],
        );
    }
    let presets = answer
        .get("presets")
        .and_then(Value::as_array)
        .map_or(&[][..], Vec::as_slice);
    let Some(preset) = presets
        .iter()
        .find(|preset| str_arg(preset, "name") == Some(preset_name))
    else {
        return ToolResult::error(
            format!(
                "The project has no export preset named {preset_name:?}; presets: {}",
                list_names(presets)
            ),
            &[
                "Pass one of the listed names (godot_list_export_presets shows them all)",
                "Or create the preset with godot_set_export_preset",
            ],
        );
    };
    let platform = str_arg(preset, "platform").unwrap_or_default().to_string();

    let requested = str_arg(args, "output_path")
        .filter(|path| !path.trim().is_empty())
        .or_else(|| str_arg(preset, "export_path").filter(|path| !path.trim().is_empty()));
    let Some(requested) = requested else {
        return ToolResult::error(
            format!("Preset {preset_name:?} has no export_path, and no output_path was given"),
            &[
                "Pass output_path, such as build/game.pck",
                "Or set the preset's export_path with godot_set_export_preset",
            ],
        );
    };
    let output = match output_file(&project, requested, mode) {
        Ok(output) => output,
        Err(problem) => {
            return ToolResult::error(
                problem,
                &[
                    "Pass output_path inside the project, such as build/game.pck (a .pck or .zip in pack mode)",
                    "Copy the file elsewhere after the export if it must live outside the project",
                ],
            );
        }
    };
    if let Some(parent) = output.parent()
        && let Err(err) = std::fs::create_dir_all(parent)
    {
        return ToolResult::error(
            format!("Cannot create the folder {}: {err}", parent.display()),
            &["Check that the project directory is writable"],
        );
    }

    let mut report = json!({
        "project_dir": project.display().to_string(),
        "preset": preset_name,
        "platform": platform,
        "mode": mode_name(mode),
        "output_path": output.display().to_string(),
    });

    let wants_import = args.get("import").and_then(Value::as_bool).unwrap_or(true);
    let editor_imports = wants_import && editor_has_project(tools, &project).await;
    // The import rewrites .godot and the export packs every file, so both wait for the script
    // runs working in the project and keep new ones from starting until the pack is written.
    let _alone = tools.gates.exclusive(&project).await;
    if !wants_import {
        report["import"] = json!({"ran": false, "reason": "import false was passed"});
    } else if editor_imports {
        report["import"] = json!({
            "ran": false,
            "reason": "the editor has the project open and keeps its imports current"
        });
    } else {
        let run = match tools.cli.import_project_within(&project, limit).await {
            Ok(run) => readable(run),
            Err(error) => return godot_failed(&error),
        };
        report["import"] = import_report(&run);
        if run.timed_out || run.exit_code != Some(0) {
            let what = if run.timed_out {
                format!("did not finish within {}s", limit.as_secs())
            } else {
                format!("failed (exit code {})", exit_text(run.exit_code))
            };
            return failure(
                format!("The import before the export {what}, so nothing was exported"),
                &report,
                &[
                    "Read the import errors and log in the details; fix the asset they name and export again",
                    "Raise timeout_seconds for a large project",
                ],
            );
        }
    }

    let written_before = modified(&output);
    let run = match tools
        .cli
        .export_project(&project, mode, preset_name, &output, limit)
        .await
    {
        Ok(run) => readable(run),
        Err(error) => return godot_failed(&error),
    };
    let written_after = modified(&output);
    let size = std::fs::metadata(&output)
        .ok()
        .filter(|meta| meta.is_file())
        .map(|meta| meta.len());
    let fresh = written_after.is_some() && written_after != written_before;
    let errors = messages(&run, "ERROR:");
    report["exit_code"] = json!(run.exit_code);
    report["timed_out"] = json!(run.timed_out);
    report["duration_seconds"] = json!((run.duration.as_secs_f64() * 10.0).round() / 10.0);
    report["size_bytes"] = json!(if fresh { size } else { None });
    report["errors"] = json!(errors);
    report["warnings"] = json!(messages(&run, "WARNING:"));
    report["log"] = json!(tail(&run.stdout));
    report["stderr"] = json!(tail(&run.stderr));

    let ok = !run.timed_out && run.exit_code == Some(0) && fresh && size.unwrap_or(0) > 0;
    report["ok"] = json!(ok);
    if ok {
        return ToolResult::text(serde_json::to_string_pretty(&report).unwrap_or_default());
    }

    let log = format!("{}\n{}", run.stdout, run.stderr);
    let summary = if run.timed_out {
        format!(
            "The export with preset {preset_name:?} did not finish within {}s and was stopped",
            limit.as_secs()
        )
    } else if run.exit_code != Some(0) {
        format!(
            "The export with preset {preset_name:?} failed (exit code {})",
            exit_text(run.exit_code)
        )
    } else {
        format!(
            "Godot reported no error, but the export with preset {preset_name:?} wrote no file at {}",
            res_display(&project, &output)
        )
    };
    let hints: &[&str] = if log.contains("No export template found") {
        &[
            "Install the export templates of this exact Godot version (Editor > Manage Export Templates), or set the preset's custom_template/release or custom_template/debug option to a template binary with godot_set_export_preset",
            "Or export with mode pack, which needs no template",
        ]
    } else if log.contains("doesn't have a matching platform") {
        &[
            "The preset's platform is not available in this Godot build; check platform with godot_list_export_presets",
        ]
    } else if log.contains("configuration errors") {
        &[
            "Fix the configuration errors in the log with godot_set_export_preset (the option names are in the message)",
        ]
    } else if log.contains("doesn't end with a supported extension") {
        &["Use an output_path ending in .pck or .zip for mode pack"]
    } else if run.timed_out {
        &["Raise timeout_seconds for a large project, then export again"]
    } else {
        &["Read errors and the log in the details, fix what they name, and export again"]
    };
    failure(summary, &report, hints)
}

/// The JSON request a set call sends to the preset script, without `action` and `file`.
fn set_request(args: &Value) -> Result<Value, String> {
    let name = str_arg(args, "name").unwrap_or_default();
    check_name(name)?;
    let create = args.get("create").and_then(Value::as_bool).unwrap_or(false);
    let mut request = Map::new();
    request.insert("name".into(), json!(name));
    request.insert("create".into(), json!(create));
    match str_arg(args, "platform") {
        Some(platform) => {
            if platform.trim().is_empty()
                || platform.chars().count() > MAX_NAME_CHARS
                || platform.chars().any(char::is_control)
            {
                return Err(format!(
                    "platform {platform:?} is not a platform name, such as Linux or Android"
                ));
            }
            request.insert("platform".into(), json!(platform));
        }
        None if create => {
            return Err(
                "platform is required to create a preset: Linux, Windows Desktop, macOS, Android, iOS, Web or visionOS"
                    .into(),
            );
        }
        None => {}
    }
    for &field in TEXT_FIELDS {
        if let Some(text) = str_arg(args, field) {
            if text.chars().count() > MAX_FIELD_CHARS || text.chars().any(char::is_control) {
                return Err(format!(
                    "{field} must be at most {MAX_FIELD_CHARS} characters, without line breaks or control characters"
                ));
            }
            request.insert(field.into(), json!(text));
        }
    }
    if let Some(filter) = str_arg(args, "export_filter")
        && !EXPORT_FILTERS.contains(&filter)
    {
        return Err(format!(
            "export_filter {filter:?} is not one of Godot's filters: {}",
            EXPORT_FILTERS.join(", ")
        ));
    }
    if let Some(files) = args.get("export_files").filter(|value| !value.is_null()) {
        request.insert("export_files".into(), json!(export_files(files)?));
    }
    if let Some(options) = args.get("options").filter(|value| !value.is_null()) {
        let Some(options) = options.as_object() else {
            return Err("options must be an object of option names and values".into());
        };
        if options.len() > MAX_OPTIONS {
            return Err(format!("at most {MAX_OPTIONS} options per call"));
        }
        for (key, value) in options {
            if !is_option_key(key) {
                return Err(format!(
                    "option name {key:?} is not one Godot uses: letters, digits and _ / . - only, like binary_format/architecture"
                ));
            }
            if !(value.is_string() || value.is_number() || value.is_boolean()) {
                return Err(format!("option {key} must be a string, number or boolean"));
            }
            if value
                .as_str()
                .is_some_and(|text| text.chars().count() > MAX_FIELD_CHARS)
            {
                return Err(format!(
                    "option {key} is longer than {MAX_FIELD_CHARS} characters"
                ));
            }
        }
        request.insert("options".into(), Value::Object(options.clone()));
    }
    let changes_something = create
        || request.contains_key("options")
        || request.contains_key("export_files")
        || TEXT_FIELDS.iter().any(|field| request.contains_key(*field));
    if !changes_something {
        return Err(format!(
            "nothing to change for preset {name:?}: pass create, options, export_files or one of {}",
            TEXT_FIELDS.join(", ")
        ));
    }
    Ok(Value::Object(request))
}

/// The `export_files` of a set call as the `res://` paths Godot stores, in the given order
/// without repeats: a project-relative path gets `res://`, and `\\` becomes `/`.
fn export_files(files: &Value) -> Result<Vec<String>, String> {
    let Some(files) = files.as_array() else {
        return Err("export_files must be an array of res:// paths".into());
    };
    if files.len() > MAX_EXPORT_FILES {
        return Err(format!(
            "export_files holds {} paths; at most {MAX_EXPORT_FILES}",
            files.len()
        ));
    }
    let mut paths: Vec<String> = Vec::with_capacity(files.len());
    let mut seen = std::collections::HashSet::new();
    for file in files {
        let Some(text) = file.as_str() else {
            return Err(format!("export_files entry {file} is not a string"));
        };
        let relative = text
            .trim()
            .strip_prefix("res://")
            .unwrap_or(text.trim())
            .replace('\\', "/");
        let bad = relative.contains(':')
            || relative.chars().count() > MAX_FIELD_CHARS
            || relative.chars().any(char::is_control)
            || relative
                .split('/')
                .any(|part| part.is_empty() || part == "." || part == "..");
        if bad {
            return Err(format!(
                "export_files entry {text:?} is not a file path inside the project, such as res://levels/one.tscn (no '..', no absolute path)"
            ));
        }
        let path = format!("res://{relative}");
        if seen.insert(path.clone()) {
            paths.push(path);
        }
    }
    Ok(paths)
}

/// A preset name travels to Godot as the argument after `--export-*`, so one that starts with
/// `-` could be read as an engine option; the Export dialog never makes such a name.
fn check_name(name: &str) -> Result<(), String> {
    if name.trim().is_empty() {
        return Err("The preset name is empty".into());
    }
    if name.starts_with('-') {
        return Err(format!(
            "Preset name {name:?} starts with '-', which Godot would read as a command-line option"
        ));
    }
    if name.chars().count() > MAX_NAME_CHARS || name.chars().any(char::is_control) {
        return Err(format!(
            "Preset name {name:?} must be at most {MAX_NAME_CHARS} characters, without control characters"
        ));
    }
    Ok(())
}

pub(super) fn is_option_key(key: &str) -> bool {
    !key.is_empty()
        && !key.starts_with(['/', '.', '-'])
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '/' | '.' | '-'))
}

/// The absolute file an export writes: inside the project, not in `.godot/`, not a project
/// file, and with the extension `--export-pack` accepts in pack mode.
fn output_file(project: &Path, requested: &str, mode: ExportMode) -> Result<PathBuf, String> {
    let output = resolve_in_project(project, requested)?;
    let relative = output.strip_prefix(project).unwrap_or(&output);
    if relative.as_os_str().is_empty() || output.is_dir() {
        return Err(format!(
            "output_path {requested:?} is a folder; name the file to write, such as build/game.pck"
        ));
    }
    if relative
        .components()
        .next()
        .is_some_and(|first| first.as_os_str() == ".godot")
    {
        return Err(format!(
            "output_path {requested:?} is inside .godot/, Godot's own cache"
        ));
    }
    let extension = output
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    if SOURCE_EXTENSIONS.contains(&extension.as_str()) {
        return Err(format!(
            "output_path {requested:?} would replace a project file (.{extension})"
        ));
    }
    if mode == ExportMode::Pack && extension != "pck" && extension != "zip" {
        return Err(format!(
            "output_path {requested:?} must end in .pck or .zip in pack mode"
        ));
    }
    // Godot gets an absolute path, which never starts with '-'.
    Ok(std::path::absolute(&output).unwrap_or(output))
}

async fn read_presets(tools: &ToolManager, project: &Path) -> Result<Value, ToolResult> {
    let request = json!({"action": "list", "file": presets_file(project)});
    let answer = tools
        .cli
        .export_presets(&request)
        .await
        .map_err(|error| godot_failed(&error))?;
    match str_arg(&answer, "error") {
        None => Ok(answer),
        Some(code) => Err(presets_unusable(code, &answer, project)),
    }
}

fn presets_file(project: &Path) -> String {
    project
        .join("export_presets.cfg")
        .to_string_lossy()
        .replace('\\', "/")
}

/// True when the connected editor reports `project` as the project it has open.
pub(super) async fn editor_has_project(tools: &ToolManager, project: &Path) -> bool {
    match tools
        .bridge
        .send_command("get_project_info", json!({}))
        .await
    {
        Ok(info) => info
            .get("project_dir")
            .and_then(Value::as_str)
            .is_some_and(|dir| same_directory(project, Path::new(dir))),
        Err(_) => false,
    }
}

fn presets_unusable(code: &str, answer: &Value, project: &Path) -> ToolResult {
    let detail = str_arg(answer, "message").unwrap_or(code);
    ToolResult::error(
        format!(
            "Godot could not use {}: {detail}",
            project.join("export_presets.cfg").display()
        ),
        &[
            "Check that the file is a valid export_presets.cfg (open Project > Export in the editor once) and that it is writable",
        ],
    )
}

pub(super) fn godot_failed(error: &crate::error::McpError) -> ToolResult {
    ToolResult::error(
        error.to_string(),
        &[
            "Verify GODOT_PATH points to a working Godot editor executable (exports need the editor build)",
        ],
    )
}

pub(super) fn failure(summary: String, report: &Value, hints: &[&str]) -> ToolResult {
    ToolResult::error(
        format!(
            "{summary}\n\nDetails:\n{}",
            serde_json::to_string_pretty(report).unwrap_or_default()
        ),
        hints,
    )
}

pub(super) fn import_report(run: &HeadlessRun) -> Value {
    json!({
        "ran": true,
        "exit_code": run.exit_code,
        "timed_out": run.timed_out,
        "duration_seconds": (run.duration.as_secs_f64() * 10.0).round() / 10.0,
        "errors": messages(run, "ERROR:"),
        "log": tail(&format!("{}\n{}", run.stdout, run.stderr)),
    })
}

/// The output without terminal colors and without the editor's progress lines
/// (`[  16% ] ...`, `[ DONE ] ...`), which would fill the quoted log of a large project.
pub(super) fn readable(mut run: HeadlessRun) -> HeadlessRun {
    let clean = |text: &str| {
        strip_ansi(text)
            .lines()
            .filter(|line| !is_progress(line))
            .collect::<Vec<_>>()
            .join("\n")
    };
    run.stdout = clean(&run.stdout);
    run.stderr = clean(&run.stderr);
    run
}

fn strip_ansi(text: &str) -> String {
    let mut plain = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            plain.push(c);
            continue;
        }
        // CSI: ESC [ parameters, ended by a letter.
        if chars.next() == Some('[') {
            for end in chars.by_ref() {
                if end.is_ascii_alphabetic() {
                    break;
                }
            }
        }
    }
    plain
}

fn is_progress(line: &str) -> bool {
    let Some(rest) = line.trim_start().strip_prefix('[') else {
        return false;
    };
    let Some((inside, _)) = rest.split_once(']') else {
        return false;
    };
    let inside = inside.trim();
    inside == "DONE"
        || inside
            .strip_suffix('%')
            .is_some_and(|number| number.trim().parse::<u8>().is_ok())
}

/// Each message of both streams that starts with `prefix` (such as `ERROR:`), with the lines
/// that continue it up to Godot's `at:` source line, at most [`MAX_LISTED_LINES`] of them.
/// Godot prints the reason an export failed on the lines after its `ERROR:` line.
pub(super) fn messages(run: &HeadlessRun, prefix: &str) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    for stream in [&run.stdout, &run.stderr] {
        let mut current: Option<String> = None;
        for line in stream.lines() {
            let trimmed = line.trim();
            let starts_message = ["ERROR:", "WARNING:", "SCRIPT ERROR:", "USER ERROR:"]
                .iter()
                .any(|start| trimmed.starts_with(start));
            if starts_message || trimmed.starts_with("at:") {
                found.extend(current.take());
                if trimmed.starts_with(prefix) {
                    current = Some(trimmed.to_string());
                }
            } else if let Some(message) = current.as_mut()
                && !trimmed.is_empty()
            {
                message.push('\n');
                message.push_str(trimmed);
            }
        }
        found.extend(current);
    }
    found.truncate(MAX_LISTED_LINES);
    found
}

/// The end of `text`, at most [`LOG_TAIL_BYTES`] bytes, cut at a line start.
pub(super) fn tail(text: &str) -> String {
    let text = text.trim();
    if text.len() <= LOG_TAIL_BYTES {
        return text.to_string();
    }
    let mut start = text.len() - LOG_TAIL_BYTES;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    let rest = &text[start..];
    let rest = rest.split_once('\n').map_or(rest, |(_, after)| after);
    format!("[...]\n{rest}")
}

fn modified(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).ok()?.modified().ok()
}

pub(super) fn exit_text(code: Option<i32>) -> String {
    code.map_or("none: killed by a signal".into(), |code| code.to_string())
}

fn mode_name(mode: ExportMode) -> &'static str {
    match mode {
        ExportMode::Pack => "pack",
        ExportMode::Release => "release",
        ExportMode::Debug => "debug",
    }
}

fn names_of(answer: &Value) -> String {
    let names: Vec<String> = answer
        .get("names")
        .and_then(Value::as_array)
        .map(|names| {
            names
                .iter()
                .filter_map(Value::as_str)
                .map(|name| format!("{name:?}"))
                .collect()
        })
        .unwrap_or_default();
    if names.is_empty() {
        "none".into()
    } else {
        names.join(", ")
    }
}

fn list_names(presets: &[Value]) -> String {
    names_of(&json!({
        "names": presets.iter().filter_map(|preset| preset.get("name")).collect::<Vec<_>>()
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_preset_name_cannot_become_an_engine_option() {
        for good in ["Linux", "webOS TV", "Web (threads)", "Προεπιλογή"] {
            assert!(check_name(good).is_ok(), "{good}");
        }
        for bad in ["", "  ", "-s", "--script", "a\nb"] {
            assert!(check_name(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn option_names_are_config_keys() {
        for good in [
            "binary_format/architecture",
            "texture_format/etc2_astc",
            "custom_template/release",
            "application/icon-path.x",
        ] {
            assert!(is_option_key(good), "{good}");
        }
        for bad in ["", "/x", "a=b", "a b", "a]b", "a\nb", "-a", "a\"b"] {
            assert!(!is_option_key(bad), "{bad:?}");
        }
    }

    #[test]
    fn the_output_stays_inside_the_project_and_off_its_files() {
        let project = tempfile::tempdir().expect("project");
        std::fs::write(project.path().join("project.godot"), "").expect("project.godot");
        let pack = output_file(project.path(), "build/game.pck", ExportMode::Pack).expect("pck");
        assert!(pack.is_absolute() && pack.ends_with("build/game.pck"));
        assert!(output_file(project.path(), "res://out/data.zip", ExportMode::Pack).is_ok());
        assert!(output_file(project.path(), "build/game.arm32", ExportMode::Release).is_ok());
        for (bad, mode) in [
            ("../game.pck", ExportMode::Pack),
            ("/elsewhere/game.pck", ExportMode::Pack),
            ("build/game.exe", ExportMode::Pack),
            ("project.godot", ExportMode::Release),
            ("scenes/main.tscn", ExportMode::Debug),
            (".godot/game.pck", ExportMode::Pack),
            ("res://", ExportMode::Release),
        ] {
            assert!(output_file(project.path(), bad, mode).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_set_request_needs_a_change_and_a_platform_to_create() {
        assert!(set_request(&json!({"name": "Linux"})).is_err());
        assert!(set_request(&json!({"name": "Linux", "create": true})).is_err());
        let request = set_request(&json!({
            "name": "webOS", "create": true, "platform": "Linux",
            "options": {"binary_format/architecture": "arm32", "texture_format/etc2_astc": true}
        }))
        .expect("request");
        assert_eq!(request["options"]["binary_format/architecture"], "arm32");
        assert!(
            set_request(&json!({"name": "webOS", "options": {"a=b": 1}})).is_err(),
            "a key that would break the file"
        );
        assert!(
            set_request(&json!({"name": "webOS", "export_path": "a\nb"})).is_err(),
            "a line break"
        );
    }

    #[test]
    fn export_files_become_res_paths_without_repeats() {
        let request = set_request(&json!({
            "name": "Data", "export_filter": "resources",
            "export_files": ["res://main.tscn", "levels\\one.tscn", " res://main.tscn "]
        }))
        .expect("request");
        assert_eq!(request["export_filter"], "resources");
        assert_eq!(
            request["export_files"],
            json!(["res://main.tscn", "res://levels/one.tscn"])
        );
        assert_eq!(
            set_request(&json!({"name": "Data", "export_files": []})).expect("empty")["export_files"],
            json!([]),
            "an empty list is a change: it clears the stored list"
        );
        for bad in [
            "",
            "res://",
            "res://levels/",
            "res://a//b.tscn",
            "res://./a.tscn",
            "res://a/../../b.tscn",
            "C:/game/a.tscn",
            "user://save.tres",
            "/abs/a.tscn",
            "a\nb.tscn",
        ] {
            assert!(
                set_request(&json!({"name": "Data", "export_files": [bad]})).is_err(),
                "{bad:?}"
            );
        }
        for filter in EXPORT_FILTERS {
            assert!(set_request(&json!({"name": "Data", "export_filter": filter})).is_ok());
        }
        assert!(set_request(&json!({"name": "Data", "export_filter": "Resources"})).is_err());
    }

    #[test]
    fn an_error_message_keeps_its_reason_and_drops_the_source_line() {
        let run = readable(HeadlessRun {
            exit_code: Some(1),
            timed_out: false,
            stdout: "\u{1b}[90m[  16% ] savepack | Storing File\u{1b}[0m\n\u{1b}[92m[ DONE ]\u{1b}[39m savepack\nkept".into(),
            stderr: "ERROR: Cannot export project with preset \"webOS\" due to configuration errors:\nNo export template found at the expected path:\n/templates/linux_release.arm32\n\n   at: _fs_changed (editor/editor_node.cpp:1471)\nWARNING: slow\n   at: x\nERROR: Project export for preset \"webOS\" failed.\n   at: _fs_changed (editor/editor_node.cpp:1487)".into(),
            duration: Duration::ZERO,
        });
        assert_eq!(run.stdout, "kept");
        assert_eq!(
            messages(&run, "ERROR:"),
            vec![
                "ERROR: Cannot export project with preset \"webOS\" due to configuration errors:\nNo export template found at the expected path:\n/templates/linux_release.arm32".to_string(),
                "ERROR: Project export for preset \"webOS\" failed.".to_string(),
            ]
        );
        assert_eq!(
            messages(&run, "WARNING:"),
            vec!["WARNING: slow".to_string()]
        );
    }

    #[test]
    fn the_log_tail_keeps_the_end() {
        let long = "line\n".repeat(LOG_TAIL_BYTES);
        let cut = tail(&long);
        assert!(cut.starts_with("[...]\nline") && cut.len() <= LOG_TAIL_BYTES + 8);
        assert_eq!(tail("short"), "short");
    }
}
