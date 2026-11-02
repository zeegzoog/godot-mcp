//! Import parameters: the `[params]` section of a file's `.import` sidecar, as the editor's
//! Import dock shows and stores it.
//!
//! The sidecar is read and written by Godot's own `ConfigFile`, through the addon's
//! `editor/import_ops.gd`. With the editor holding the project, the bridge edits it and reimports
//! the file through the editor's file system, so the Import dock and the loaded resource follow.
//! Without it, the same script runs in a throw-away project and `godot --headless --import`
//! reimports. The importer rewrites the sidecar with the options it has, so a name it does not
//! know is gone afterwards and is reported.
//!
//! The importer itself can change too, as the Import dock's "Import As" does: a PNG strip becomes a
//! Texture3D with the `3d_texture` importer. Godot replaces an importer name it does not have
//! with the file type's default one, so the name is checked against the built-in importers of
//! the file type first, and the importer the reimport used is checked after.

use super::export::{editor_has_project, exit_text, import_report, is_option_key, readable};
use super::{ToolManager, bridge_error_result, str_arg};
use crate::paths::{res_display, resolve_in_project};
use crate::protocol::{Tool, ToolResult};
use serde_json::{Map, Value, json};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

pub const GET: &str = "godot_get_import_params";
pub const SET: &str = "godot_set_import_params";

const MAX_PARAMS: usize = 128;
const MAX_VALUE_CHARS: usize = 4096;
const DEFAULT_TIMEOUT_SECONDS: u64 = 300;
const MAX_IMPORTER_CHARS: usize = 64;

/// Source extensions of the image importers.
const IMAGES: &[&str] = &[
    "bmp", "dds", "exr", "hdr", "jpeg", "jpg", "ktx", "ktx2", "png", "svg", "tga", "webp",
];
const SCENES: &[&str] = &["blend", "dae", "fbx", "glb", "gltf", "obj"];
const FONTS: &[&str] = &["otc", "otf", "pfb", "pfm", "ttc", "ttf", "woff", "woff2"];

/// Godot's built-in importers: name, the source extensions it imports, the resource it makes.
/// `keep` and `skip` (the dock's "Keep File" and "Skip") take any file.
const IMPORTERS: &[(&str, &[&str], &str)] = &[
    ("texture", IMAGES, "Texture2D"),
    ("2d_array_texture", IMAGES, "Texture2DArray"),
    ("cubemap_texture", IMAGES, "Cubemap"),
    ("cubemap_array_texture", IMAGES, "CubemapArray"),
    ("3d_texture", IMAGES, "Texture3D"),
    ("texture_atlas", IMAGES, "AtlasTexture"),
    ("streamed_texture_2d", IMAGES, "StreamedTexture2D"),
    ("bitmap", IMAGES, "BitMap"),
    ("image", IMAGES, "Image"),
    ("font_data_image", IMAGES, "FontFile"),
    ("svg", &["svg"], "DPITexture"),
    ("font_data_dynamic", FONTS, "FontFile"),
    ("font_data_bmfont", &["fnt", "font"], "FontFile"),
    ("wav", &["wav"], "AudioStreamWAV"),
    ("mp3", &["mp3"], "AudioStreamMP3"),
    ("oggvorbisstr", &["ogg"], "AudioStreamOggVorbis"),
    ("scene", SCENES, "PackedScene"),
    ("animation_library", SCENES, "AnimationLibrary"),
    ("wavefront_obj", &["obj"], "Mesh"),
    ("csv_translation", &["csv"], "Translation"),
    ("glsl", &["glsl"], "RDShaderFile"),
];

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
            "Changes a file's import parameters and, with importer, the importer itself (the Import dock's \"Import As\"), then reimports it: the given options in the [params] section of its .import sidecar are set, after copying every parameter of copy_from's .import when it is given (both files must use the same importer). An option keeps the type it holds: a whole number becomes an int unless the option holds a float, and a string such as \"Vector2(1, 1)\" becomes the option's type when it is a vector, color, rect or packed array. With the editor holding the project, the change goes through the editor's file system (the Import dock and the loaded resource follow); else Godot's ConfigFile edits the file and godot --headless --import reimports it. The importer keeps only the options it has, so check ignored_options (names it dropped: a typo or another importer's option) and new_options (names the file did not hold before). A new importer starts from its own default options, as in the dock (the old params are dropped, then copy_from and params apply); its name is checked against Godot's importers for the file type, and the answer gives importer, previous_importer and type (the resource class now imported). A file with no .import yet is refused: import it with godot_resave_resources first. Examples: path res://sky.png, params {\"compress/mode\": 2, \"compress/high_quality\": true} for VRAM-compressed textures; path res://lut.png, importer \"3d_texture\", params {\"slices/horizontal\": 33, \"slices/vertical\": 1} for a Texture3D from a strip of 33 slices.",
            json!({
                "path": path,
                "params": {
                    "type": "object",
                    "description": "Options to set, by the name the .import file uses, each a string, number or boolean. Examples: {\"compress/mode\": 2, \"mipmaps/generate\": true, \"process/size_limit\": 1024}.",
                    "additionalProperties": {"type": ["string", "number", "boolean"]}
                },
                "importer": {"type": "string", "description": "The importer to import the file with, as the Import dock's \"Import As\" lists them. For an image: texture (Texture2D), 2d_array_texture, cubemap_texture, cubemap_array_texture, 3d_texture (Texture3D), texture_atlas, bitmap, image, font_data_image; for a scene file: scene, animation_library; for any file: keep (exported as is) or skip (not imported). Omit to keep the current importer."},
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
    let importer = match str_arg(args, "importer").map(str::trim) {
        None | Some("") => None,
        Some(name) => match check_importer(name, &file.source) {
            Ok(()) => Some(name.to_string()),
            Err(problem) => {
                return ToolResult::error(
                    format!("{problem}; nothing was changed"),
                    &[
                        "Pass one of the importers the message lists, as the Import dock's \"Import As\" names them",
                        "Omit importer to keep the file's current one",
                    ],
                );
            }
        },
    };
    if params.is_empty() && copy_from.is_none() && importer.is_none() {
        return ToolResult::error(
            "Nothing to change: pass params, importer, copy_from or a mix",
            &[
                "Pass params, such as {\"compress/mode\": 2}",
                "Pass importer to import the file as another resource, such as 3d_texture",
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
        if let Some(name) = &importer {
            command["importer"] = json!(name);
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
        if answer.get("replaced_loaded") == Some(&json!(true)) {
            report["note"] = json!(
                "The editor had the file loaded as the old type; it now loads the new one, and an open scene that holds the old resource keeps it until the scene is reopened"
            );
        }
        report
    } else {
        let mut request =
            json!({"action": "set", "file": slashes(&file.sidecar), "params": params});
        if let Some(other) = &copy_from {
            request["copy_from"] = json!(slashes(&other.sidecar));
        }
        if let Some(name) = &importer {
            request["importer"] = json!(name);
        }
        let written_before = modified(&file.sidecar);
        let answer = match tools.cli.import_params(&request).await {
            Ok(answer) => answer,
            Err(error) => return godot_failed(&error),
        };
        if let Some(result) = refusal(&answer, &project) {
            return result;
        }
        let mut report = changes(&answer);
        report["source"] = json!("headless");
        let changed = report["importer_changed"] == json!(true)
            || report["changed"]
                .as_array()
                .is_some_and(|changed| !changed.is_empty());
        if wants_reimport && changed {
            if let Some(before) = written_before {
                newer_than(&file.sidecar, before);
            }
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
    report["type"] = report["after"].get("type").cloned().unwrap_or(Value::Null);
    // Godot imports with the file type's default importer when it has no importer by that name.
    if let Some(name) = &importer {
        let used = report["after"].get("importer").and_then(Value::as_str);
        if report["reimported"] == json!(true) && used.is_some_and(|used| used != name) {
            let used = used.unwrap_or_default();
            return ToolResult::error(
                format!(
                    "Godot has no {name} importer for {}, so the reimport imported it with {used}\n\nDetails:\n{}",
                    file.res,
                    serde_json::to_string_pretty(&report).unwrap_or_default()
                ),
                &[
                    &format!(
                        "Pass an importer Godot has for this file: {}",
                        offered(&file.source)
                    ),
                    "An importer from an editor plugin works only while that plugin is enabled in the project",
                ],
            );
        }
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
        "previous_importer": answer.get("previous_importer").cloned().unwrap_or(Value::Null),
        "importer_changed": answer.get("importer_changed").cloned().unwrap_or(json!(false)),
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

/// Checks an importer name for `source`: a name of Godot's built-in importers must import the
/// file's extension. Another name (an editor plugin's importer) is let through; the importer the
/// reimport used is checked afterwards.
fn check_importer(name: &str, source: &Path) -> Result<(), String> {
    if name.is_empty()
        || name.chars().count() > MAX_IMPORTER_CHARS
        || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        return Err(format!(
            "importer {name:?} is not an importer name: letters, digits and _ only, like 3d_texture"
        ));
    }
    if name == "keep" || name == "skip" {
        return Ok(());
    }
    let extension = extension_of(source);
    match IMPORTERS.iter().find(|(known, _, _)| *known == name) {
        Some((_, extensions, _)) if !extensions.contains(&extension.as_str()) => Err(format!(
            "The {name} importer does not import .{extension} files; Godot offers {} for them",
            offered(source)
        )),
        _ => Ok(()),
    }
}

/// The built-in importers Godot offers for `source`, with what each makes.
fn offered(source: &Path) -> String {
    let extension = extension_of(source);
    let mut names: Vec<String> = IMPORTERS
        .iter()
        .filter(|(_, extensions, _)| extensions.contains(&extension.as_str()))
        .map(|(name, _, makes)| format!("{name} ({makes})"))
        .collect();
    names.push("keep".into());
    names.push("skip".into());
    names.join(", ")
}

fn extension_of(source: &Path) -> String {
    source
        .extension()
        .map(|extension| extension.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default()
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

fn modified(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path)
        .and_then(|meta| meta.modified())
        .ok()
}

/// Godot's file system cache keeps a `.import` file's modification time in whole seconds and
/// reimports only when it differs, so an edit within the second of the last import would be
/// missed. Such a file is dated one second after `before`.
fn newer_than(path: &Path, before: SystemTime) {
    let seconds = |time: SystemTime| {
        time.duration_since(SystemTime::UNIX_EPOCH)
            .map(|since| since.as_secs())
            .unwrap_or_default()
    };
    let Some(now) = modified(path) else {
        return;
    };
    if seconds(now) > seconds(before) {
        return;
    }
    let later = before + Duration::from_secs(1);
    let dated = std::fs::File::options()
        .write(true)
        .open(path)
        .and_then(|file| file.set_modified(later));
    if let Err(error) = dated {
        tracing::warn!(
            "could not date {} for the reimport: {error}",
            path.display()
        );
    }
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
    fn importers_are_checked_against_the_file_type() {
        let png = Path::new("lut.png");
        assert!(check_importer("3d_texture", png).is_ok());
        assert!(check_importer("texture", Path::new("SKY.PNG")).is_ok());
        assert!(check_importer("keep", Path::new("notes.txt")).is_ok());
        assert!(check_importer("my_plugin_importer", png).is_ok());
        assert!(check_importer("scene", Path::new("frog.glb")).is_ok());
        let wrong = check_importer("wav", png).expect_err("wav on a png");
        assert!(wrong.contains("3d_texture (Texture3D)"), "{wrong}");
        assert!(check_importer("svg", png).is_err());
        for bad in [
            "",
            "3d texture",
            "a/b",
            "x\"y",
            &"a".repeat(MAX_IMPORTER_CHARS + 1),
        ] {
            assert!(check_importer(bad, png).is_err(), "{bad}");
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
