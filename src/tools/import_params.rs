//! Import parameters: the `[params]` section of a file's `.import` sidecar, as the editor's
//! Import dock shows and stores it.
//!
//! The sidecar is read and written by Godot's own `ConfigFile`, through the addon's
//! `editor/import_ops.gd`. With the editor holding the project, the bridge edits it and reimports
//! the file through the editor's file system, so the Import dock and the loaded resource follow.
//! Without it, the same script runs in a throw-away project and `godot --headless --import`
//! reimports. The importer rewrites the sidecar with the options it has, so a name it does not
//! know is gone afterwards and is reported.

use super::export::{editor_has_project, exit_text, import_report, is_option_key, readable};
use super::{ToolManager, bridge_error_result, str_arg};
use crate::paths::{res_display, resolve_in_project};
use crate::protocol::{Tool, ToolResult};
use serde_json::{Map, Value, json};
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const GET: &str = "godot_get_import_params";
pub const SET: &str = "godot_set_import_params";

const MAX_PARAMS: usize = 128;
const MAX_VALUE_CHARS: usize = 4096;
const DEFAULT_TIMEOUT_SECONDS: u64 = 300;

pub fn handles(name: &str) -> bool {
    name == GET || name == SET
}

pub fn catalog() -> Vec<Tool> {
    let project_path = json!({
        "type": "string",
        "description": "Project directory containing project.godot. Optional: defaults to the project open in the connected editor, else the project the server was started in."
    });
    let path = json!({
        "type": "string",
        "description": "The imported file (not its .import sidecar), as a res:// path or relative to the project, such as res://assets/sky.png."
    });
    vec![
        super::tool(
            GET,
            "Reads a file's import parameters, the [params] section of its .import sidecar that the Import dock shows: importer (such as texture), type (the imported resource class) and params (each option's value, such as compress/mode or mipmaps/generate), with each option's Godot type under types. A value JSON cannot carry (a Vector2, a Dictionary) is given in Godot's text form. Read from the editor when it has the project open, else with the Godot binary. A file with no .import yet is refused: import it with godot_resave_resources first.",
            json!({ "path": path, "project_path": project_path }),
            &["path"],
        ),
        super::tool(
            SET,
            "Changes a file's import parameters the way the Import dock does, then reimports it: the given options in the [params] section of its .import sidecar are set, after copying every parameter of copy_from's .import when it is given (both files must use the same importer). An option keeps the type it holds: a whole number becomes an int unless the option holds a float, and a string such as \"Vector2(1, 1)\" becomes the option's type when it is a vector, color, rect or packed array. With the editor holding the project, the change goes through the editor's file system (the Import dock and the loaded resource follow); else Godot's ConfigFile edits the file and godot --headless --import reimports it. The importer keeps only the options it has, so check ignored_options (names it dropped: a typo or another importer's option) and new_options (names the file did not hold before). A file with no .import yet is refused: import it with godot_resave_resources first. Example: path res://sky.png, params {\"compress/mode\": 2, \"compress/high_quality\": true} for VRAM-compressed textures.",
            json!({
                "path": path,
                "params": {
                    "type": "object",
                    "description": "Options to set, by the name the .import file uses, each a string, number or boolean. Examples: {\"compress/mode\": 2, \"mipmaps/generate\": true, \"process/size_limit\": 1024}.",
                    "additionalProperties": {"type": ["string", "number", "boolean"]}
                },
                "copy_from": {"type": "string", "description": "Another imported file of the project whose parameters are copied first, such as res://assets/sky.png. params are applied after the copy."},
                "reimport": {"type": "boolean", "description": "Reimport the file after a change. Default true. With false only the .import file changes; the editor reimports it on its next scan, and godot --headless --import does too."},
                "timeout_seconds": {"type": "integer", "minimum": 1, "maximum": 3600, "description": "Limit for the headless import. Default 300."},
                "project_path": project_path
            }),
            &["path"],
        ),
    ]
}

pub async fn call(tools: &ToolManager, name: &str, args: Value) -> ToolResult {
    if name == GET {
        get(tools, &args).await
    } else {
        set(tools, &args).await
    }
}

/// The imported file and its sidecar, both inside the project.
struct Imported {
    source: PathBuf,
    sidecar: PathBuf,
    res: String,
}

async fn get(tools: &ToolManager, args: &Value) -> ToolResult {
    let project = match tools.resolve_project(args).await {
        Ok(project) => project.dir,
        Err(result) => return result,
    };
    let file = match imported(&project, str_arg(args, "path").unwrap_or_default(), "path") {
        Ok(file) => file,
        Err(result) => return result,
    };
    let (answer, source) = if editor_has_project(tools, &project).await {
        match tools
            .bridge
            .send_command("import_params", json!({"action": "get", "path": file.res}))
            .await
        {
            Ok(answer) => (answer, "editor"),
            Err(error) => return bridge_error_result(&error),
        }
    } else {
        let request = json!({"action": "get", "file": slashes(&file.sidecar)});
        match tools.cli.import_params(&request).await {
            Ok(answer) => (answer, "headless"),
            Err(error) => return godot_failed(&error),
        }
    };
    if let Some(result) = refusal(&answer, &project) {
        return result;
    }
    let mut report = json!({"path": file.res, "source": source});
    for key in ["importer", "type", "params", "types"] {
        report[key] = answer.get(key).cloned().unwrap_or(Value::Null);
    }
    ToolResult::text(serde_json::to_string_pretty(&report).unwrap_or_default())
}

async fn set(tools: &ToolManager, args: &Value) -> ToolResult {
    let params = match requested_params(args) {
        Ok(params) => params,
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
    let file = match imported(&project, str_arg(args, "path").unwrap_or_default(), "path") {
        Ok(file) => file,
        Err(result) => return result,
    };
    let copy_from = match str_arg(args, "copy_from").filter(|path| !path.trim().is_empty()) {
        None => None,
        Some(path) => match imported(&project, path, "copy_from") {
            Ok(other) if other.source == file.source => {
                return ToolResult::error(
                    format!("copy_from names {} itself", file.res),
                    &["Pass another imported file as copy_from, or drop it"],
                );
            }
            Ok(other) => Some(other),
            Err(result) => return result,
        },
    };
    if params.is_empty() && copy_from.is_none() {
        return ToolResult::error(
            "Nothing to change: pass params, copy_from or both",
            &[
                "Pass params, such as {\"compress/mode\": 2}",
                "Or pass copy_from: another imported file whose parameters to copy",
            ],
        );
    }
    let wants_reimport = args
        .get("reimport")
        .and_then(Value::as_bool)
        .unwrap_or(true);

    let mut report = if editor_has_project(tools, &project).await {
        let mut command = json!({
            "action": "set", "path": file.res, "params": params, "reimport": wants_reimport
        });
        if let Some(other) = &copy_from {
            command["copy_from"] = json!(other.res);
        }
        let bridge = tools.bridge.clone().with_timeout(Duration::from_secs(90));
        let answer = match bridge.send_command("import_params", command).await {
            Ok(answer) => answer,
            Err(error) => return bridge_error_result(&error),
        };
        if let Some(result) = refusal(&answer, &project) {
            return result;
        }
        let mut report = changes(&answer);
        report["source"] = json!("editor");
        report["reimported"] = answer.get("reimported").cloned().unwrap_or(json!(false));
        report["after"] = answer.get("after").cloned().unwrap_or(Value::Null);
        report
    } else {
        let mut request =
            json!({"action": "set", "file": slashes(&file.sidecar), "params": params});
        if let Some(other) = &copy_from {
            request["copy_from"] = json!(slashes(&other.sidecar));
        }
        let answer = match tools.cli.import_params(&request).await {
            Ok(answer) => answer,
            Err(error) => return godot_failed(&error),
        };
        if let Some(result) = refusal(&answer, &project) {
            return result;
        }
        let mut report = changes(&answer);
        report["source"] = json!("headless");
        let changed = report["changed"]
            .as_array()
            .is_some_and(|changed| !changed.is_empty());
        if wants_reimport && changed {
            let limit = Duration::from_secs(
                args.get("timeout_seconds")
                    .and_then(Value::as_u64)
                    .unwrap_or(DEFAULT_TIMEOUT_SECONDS),
            );
            let run = match tools.cli.import_project_within(&project, limit).await {
                Ok(run) => readable(run),
                Err(error) => return godot_failed(&error),
            };
            report["import"] = import_report(&run);
            report["reimported"] = json!(!run.timed_out && run.exit_code == Some(0));
            if run.timed_out || run.exit_code != Some(0) {
                let what = if run.timed_out {
                    format!("did not finish within {}s", limit.as_secs())
                } else {
                    format!("failed (exit code {})", exit_text(run.exit_code))
                };
                report["path"] = json!(file.res);
                return ToolResult::error(
                    format!(
                        "The parameters of {} were stored, but the import {what}\n\nDetails:\n{}",
                        file.res,
                        serde_json::to_string_pretty(&report).unwrap_or_default()
                    ),
                    &[
                        "Read the import errors and log in the details and fix what they name; godot_resave_resources imports again",
                        "Raise timeout_seconds for a large project",
                    ],
                );
            }
        } else {
            report["reimported"] = json!(false);
        }
        let after = json!({"action": "get", "file": slashes(&file.sidecar)});
        report["after"] = match tools.cli.import_params(&after).await {
            Ok(answer) if answer.get("error").is_none() => answer,
            Ok(_) | Err(_) => Value::Null,
        };
        report
    };

    report["path"] = json!(file.res);
    if let Some(other) = &copy_from {
        report["copy_from"] = json!(other.res);
    }
    // A reimport rewrites the sidecar with the importer's own options only.
    let ignored: Vec<String> = match report["after"].get("params") {
        Some(Value::Object(stored)) if report["reimported"] == json!(true) => params
            .keys()
            .filter(|key| !stored.contains_key(*key))
            .cloned()
            .collect(),
        _ => Vec::new(),
    };
    if !ignored.is_empty() {
        report["warning"] = json!(format!(
            "The {} importer has no option named {}, so the reimport dropped it; godot_get_import_params lists the options it has",
            report["importer"].as_str().unwrap_or("file's"),
            ignored.join(", ")
        ));
    }
    report["ignored_options"] = json!(ignored);
    if report["reimported"] != json!(true) && report.get("import").is_none() {
        report["note"] = json!(if wants_reimport {
            "Nothing changed, so the file was not reimported"
        } else {
            "reimport false: the editor reimports the file on its next scan; godot_resave_resources does it now"
        });
    }
    ToolResult::text(serde_json::to_string_pretty(&report).unwrap_or_default())
}

/// The report fields every set answer carries.
fn changes(answer: &Value) -> Value {
    json!({
        "importer": answer.get("importer").cloned().unwrap_or(Value::Null),
        "changed": answer.get("changed").cloned().unwrap_or(json!([])),
        "new_options": answer.get("new_options").cloned().unwrap_or(json!([])),
        "copied": answer.get("copied").cloned().unwrap_or(json!(0)),
    })
}

/// The options of a set call, checked: names a `.import` file can hold, plain values.
fn requested_params(args: &Value) -> Result<Map<String, Value>, String> {
    let Some(params) = args.get("params").filter(|value| !value.is_null()) else {
        return Ok(Map::new());
    };
    let Some(params) = params.as_object() else {
        return Err("params must be an object of option names and values".into());
    };
    if params.len() > MAX_PARAMS {
        return Err(format!("at most {MAX_PARAMS} params per call"));
    }
    for (key, value) in params {
        if !is_option_key(key) {
            return Err(format!(
                "option name {key:?} is not one an importer uses: letters, digits and _ / . - only, like compress/mode"
            ));
        }
        if !(value.is_string() || value.is_number() || value.is_boolean()) {
            return Err(format!(
                "option {key} must be a string, number or boolean; copy a Dictionary or Array option with copy_from"
            ));
        }
        if value
            .as_str()
            .is_some_and(|text| text.chars().count() > MAX_VALUE_CHARS)
        {
            return Err(format!(
                "option {key} is longer than {MAX_VALUE_CHARS} characters"
            ));
        }
    }
    Ok(params.clone())
}

/// Resolves an imported file of the project and checks that its `.import` sidecar exists.
fn imported(project: &Path, requested: &str, field: &str) -> Result<Imported, ToolResult> {
    let fix = [format!(
        "Pass {field}: an imported file inside the project, such as res://assets/sky.png"
    )];
    let fix: Vec<&str> = fix.iter().map(String::as_str).collect();
    let source = resolve_in_project(project, requested)
        .map_err(|problem| ToolResult::error(problem, &fix))?;
    let res = res_display(project, &source);
    if source
        .extension()
        .is_some_and(|extension| extension == "import")
    {
        return Err(ToolResult::error(
            format!("{field} {res} is a .import sidecar; name the file it imports"),
            &[&format!(
                "Pass {}",
                res.strip_suffix(".import").unwrap_or(&res)
            )],
        ));
    }
    if !source.is_file() {
        return Err(ToolResult::error(
            format!("{field} {res} is not a file of the project"),
            &fix,
        ));
    }
    let mut name = source.file_name().unwrap_or_default().to_os_string();
    name.push(".import");
    let sidecar = source.with_file_name(name);
    if !sidecar.is_file() {
        return Err(ToolResult::error(
            format!(
                "{res} has no .import file yet, so it has no import parameters: Godot has not imported it"
            ),
            &[
                &format!(
                    "Import it first with godot_resave_resources (paths: [\"{res}\"]), then call again"
                ),
                "A file Godot does not import (a script, a scene, a .tres) has no import parameters",
            ],
        ));
    }
    Ok(Imported {
        source,
        sidecar,
        res,
    })
}

/// The tool error for an answer that carries an error code; `None` for an answer to use.
fn refusal(answer: &Value, project: &Path) -> Option<ToolResult> {
    let code = str_arg(answer, "error")?;
    let file = str_arg(answer, "file")
        .map(|file| res_display(project, Path::new(file)))
        .unwrap_or_default();
    Some(match code {
        "TYPE" => ToolResult::error(
            format!(
                "Option {} holds a {} (now {}); a value of another type is not stored, and nothing was changed",
                str_arg(answer, "option").unwrap_or("?"),
                str_arg(answer, "expected").unwrap_or("value of another type"),
                answer
                    .get("current")
                    .map(Value::to_string)
                    .unwrap_or_default()
            ),
            &["Send a value of the type the option has (godot_get_import_params lists the types)"],
        ),
        "IMPORTER" => ToolResult::error(
            format!(
                "copy_from is imported by the {} importer and the file by the {} importer, so their parameters do not match; nothing was changed",
                str_arg(answer, "source_importer").unwrap_or("?"),
                str_arg(answer, "importer").unwrap_or("?")
            ),
            &["Copy from a file of the same kind (a texture from a texture)"],
        ),
        "NO_IMPORT" => ToolResult::error(
            format!(
                "{} has no .import file yet",
                str_arg(answer, "path").unwrap_or("The file")
            ),
            &["Import it first with godot_resave_resources, then call again"],
        ),
        _ => ToolResult::error(
            format!(
                "Godot could not use {file}: {}",
                str_arg(answer, "message").unwrap_or(code)
            ),
            &[
                "Check that the .import file is readable and writable; godot_resave_resources rewrites a damaged one",
            ],
        ),
    })
}

fn godot_failed(error: &crate::error::McpError) -> ToolResult {
    ToolResult::error(
        error.to_string(),
        &["Verify GODOT_PATH points to a working Godot editor executable"],
    )
}

/// The path as Godot reads it on every platform.
fn slashes(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn params_are_plain_values_under_option_names() {
        let ok = requested_params(&json!({"params": {
            "compress/mode": 2, "compress/high_quality": true, "svg/scale": 1.5,
            "process/size_limit": 0, "editor/scale_with_editor_scale": false,
            "nodes/root_type": "Node3D"
        }}))
        .expect("params");
        assert_eq!(ok.len(), 6);
        assert!(requested_params(&json!({})).expect("none").is_empty());
        for bad in [
            json!({"params": {"a=b": 1}}),
            json!({"params": {"compress/mode": [1]}}),
            json!({"params": {"_subresources": {}}}),
            json!({"params": "compress/mode=2"}),
            json!({"params": {"x": "y".repeat(MAX_VALUE_CHARS + 1)}}),
        ] {
            assert!(requested_params(&bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_file_without_a_sidecar_names_the_resave_tool() {
        let project = tempfile::tempdir().expect("project");
        std::fs::write(project.path().join("project.godot"), "").expect("project.godot");
        std::fs::write(project.path().join("sky.png"), b"png").expect("sky.png");
        let refused = imported(project.path(), "res://sky.png", "path")
            .err()
            .expect("refused");
        let text = serde_json::to_string(&refused).expect("json");
        assert!(text.contains("godot_resave_resources"), "{text}");

        std::fs::write(project.path().join("sky.png.import"), "[remap]\n").expect("sidecar");
        let file = imported(project.path(), "sky.png", "path").expect("imported");
        assert_eq!(file.res, "res://sky.png");
        assert!(file.sidecar.ends_with("sky.png.import"));
        assert!(imported(project.path(), "res://sky.png.import", "path").is_err());
        assert!(imported(project.path(), "../sky.png", "path").is_err());
        assert!(imported(project.path(), "res://missing.png", "path").is_err());
    }
}
