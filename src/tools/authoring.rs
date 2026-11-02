//! Scene, script, signal, resource and project-setting edits.
//!
//! The editor path goes through the bridge and is undoable. With no editor, creating
//! a scene, a resource or a project setting falls back to writing the file.

use super::{EditorChoice, ToolManager, str_arg};
use crate::cli_runner::{ClassKind, SettingType};
use crate::error::McpError;
use crate::paths::{res_display, resolve_in_project};
use crate::protocol::{Tool, ToolResult};
use serde_json::{Value, json};
use std::io::{ErrorKind, Write};
use std::path::Path;

pub fn handles(name: &str) -> bool {
    matches!(
        name,
        "godot_create_project"
            | "godot_create_scene"
            | "godot_attach_script"
            | "godot_connect_signal"
            | "godot_list_signals"
            | "godot_create_resource"
            | "godot_set_project_setting"
            | "godot_get_project_settings"
    )
}

pub fn catalog() -> Vec<Tool> {
    vec![
        tool(
            "godot_create_project",
            "Creates a minimal Godot 4 project without an engine or editor. Accepts a new or existing directory (documentation and assets are preserved). Never replaces project.godot. Does not install the editor bridge or create a main scene.",
            json!({"project_path": s("Absolute directory path, without parent traversal."), "name": s("Non-empty project display name.")}),
            &["project_path", "name"],
        ),
        tool(
            "godot_create_scene",
            "Creates a .tscn with one root node and opens it in the editor when the bridge is connected. Refuses to replace an existing scene. Without the editor, writes the file directly.",
            json!({"scene_path": s("res:// path ending in .tscn"), "root_type": s("Default Node2D."), "root_name": s("Default Root."), "project_path": s("")}),
            &["scene_path"],
        ),
        tool(
            "godot_attach_script",
            "Attaches a script to a node in the edited scene (undoable). The script must exist in the project. Pass an empty script_path to detach.",
            json!({"node_path": s(""), "script_path": s("res:// path, or empty to detach."), "make_editable": super::make_editable_param()}),
            &["node_path"],
        ),
        tool(
            "godot_connect_signal",
            "Connects a signal inside the edited scene with CONNECT_PERSIST (undoable). The target must already have the method (attach its script first); a connection that exists is refused.",
            json!({"node_path": s(""), "signal_name": s(""), "target_path": s("Default: the same node."), "method": s(""), "make_editable": super::make_editable_param()}),
            &["node_path", "signal_name", "method"],
        ),
        tool(
            "godot_list_signals",
            "Lists signals of a node in the edited scene.",
            json!({"node_path": s("")}),
            &[],
        ),
        tool(
            "godot_create_resource",
            "Creates a Resource .tres of a built-in class. Never replaces an existing file. Uses the editor when connected; without it, writes an empty resource as text.",
            json!({"resource_path": s(""), "class_name": s("For example StandardMaterial3D."), "project_path": s("")}),
            &["resource_path", "class_name"],
        ),
        tool(
            "godot_get_project_settings",
            "Reads project settings. Uses the live editor when connected, otherwise the keys stored in project.godot.",
            json!({"prefix": s("Optional key prefix."), "project_path": s("")}),
            &[],
        ),
        tool(
            "godot_set_project_setting",
            "Sets one project setting and saves it. Uses the live editor when connected. The value must be a string, number or boolean, and of the type the setting has: the stored one, else the engine's definition of the setting (a feature override such as name.mobile has the type of name). A setting of another type takes a string, with and without the editor: an HTML color or color name (\"#102b38\", \"white\") for a Color, otherwise the text project.godot stores (\"Vector2i(1152, 648)\"); an Object is never created. Without the editor, project.godot is written and the engine is asked for the type of a setting that is not stored yet. InputMap actions (input/...) are changed with godot_edit_input_action.",
            json!({"key": s("For example application/config/name."), "value": {"description": "String, number, or boolean."}, "project_path": s("")}),
            &["key", "value"],
        ),
    ]
}

fn s(description: &str) -> Value {
    if description.is_empty() {
        json!({"type": "string"})
    } else {
        json!({"type": "string", "description": description})
    }
}

fn tool(name: &str, description: &str, properties: Value, required: &[&str]) -> Tool {
    super::tool(name, description, properties, required)
}

pub async fn call(tools: &ToolManager, name: &str, args: Value) -> ToolResult {
    match name {
        "godot_create_project" => create_project(&args),
        "godot_create_scene" => create_scene(tools, &args).await,
        "godot_attach_script" => editor_or_error(tools, "attach_script", &args).await,
        "godot_connect_signal" => editor_or_error(tools, "connect_signal", &args).await,
        "godot_list_signals" => editor_or_error(tools, "list_signals", &args).await,
        "godot_create_resource" => create_resource(tools, &args).await,
        "godot_set_project_setting" => set_setting(tools, &args).await,
        "godot_get_project_settings" => get_settings(tools, &args).await,
        _ => ToolResult::error(format!("Tool '{name}' has no handler"), &[]),
    }
}

/// Bootstrap is host-side because there is no project for an editor to open yet.
fn create_project(args: &Value) -> ToolResult {
    let recovery = &[
        "Pass an absolute project_path without .. and a non-empty string name; existing projects are never replaced",
    ];
    let (Some(raw_path), Some(name)) = (str_arg(args, "project_path"), str_arg(args, "name"))
    else {
        return ToolResult::error("Missing or invalid project_path or name", recovery);
    };
    let normalized = raw_path.replace('\\', "/");
    let directory = Path::new(&normalized);
    if !directory.is_absolute()
        || directory
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
        || name.trim().is_empty()
        || name.contains('\0')
    {
        return ToolResult::error("Invalid project_path or name", recovery);
    }
    if let Err(error) = std::fs::create_dir_all(directory) {
        return ToolResult::error(
            format!("Cannot create project directory: {error}"),
            recovery,
        );
    }
    let directory = match directory.canonicalize() {
        Ok(path) => path,
        Err(error) => {
            return ToolResult::error(
                format!("Cannot resolve project directory: {error}"),
                recovery,
            );
        }
    };
    let target = match resolve_in_project(&directory, "res://project.godot") {
        Ok(path) => path,
        Err(error) => return ToolResult::error(error, recovery),
    };
    let body = format!(
        "config_version=5\n\n[application]\nconfig/name={}\n\n[rendering]\nrenderer/rendering_method=\"gl_compatibility\"\nrenderer/rendering_method.mobile=\"gl_compatibility\"\n",
        godot_string(name)
    );
    match create_new_file(&target, &body) {
        Ok(()) => ToolResult::text(
            json!({"project_path": directory, "created": "res://project.godot", "name": name})
                .to_string(),
        ),
        Err(error) => ToolResult::error(
            format!("Cannot create project.godot (existing projects are never replaced): {error}"),
            recovery,
        ),
    }
}

/// The editor's answer as text, with the project it acted on named when it was asked.
fn editor_answer(choice: &EditorChoice, value: Value) -> ToolResult {
    ToolResult::text(serde_json::to_string_pretty(&choice.name_project(value)).unwrap_or_default())
}

async fn create_scene(tools: &ToolManager, args: &Value) -> ToolResult {
    let choice = match tools.editor_choice(args, true).await {
        Ok(choice) => choice,
        Err(result) => return result,
    };
    if matches!(choice, EditorChoice::Host) {
        return write_scene(tools, args).await;
    }
    match tools
        .bridge
        .send_command("create_scene", args.clone())
        .await
    {
        Ok(value) => editor_answer(&choice, value),
        Err(McpError::BridgeUnavailable(_)) => write_scene(tools, args).await,
        Err(error) => super::bridge_error_result(&error),
    }
}

async fn write_scene(tools: &ToolManager, args: &Value) -> ToolResult {
    let Some(scene_path) = str_arg(args, "scene_path") else {
        return ToolResult::error(
            "Missing scene_path",
            &["Pass scene_path: a res:// path ending in .tscn"],
        );
    };
    if !scene_path.ends_with(".tscn") {
        return ToolResult::error(
            "scene_path must end in .tscn",
            &["Use a path such as res://scenes/main.tscn"],
        );
    }
    let root_type = str_arg(args, "root_type").unwrap_or("Node2D");
    let root_name = node_name(str_arg(args, "root_name").unwrap_or("Root"));
    if !crate::paths::is_valid_identifier(root_type) {
        return ToolResult::error(
            format!("Invalid root_type {root_type:?}: use letters, digits and underscores only"),
            &["Example: Node2D, CharacterBody3D, Control"],
        );
    }
    match tools.cli.class_kind(root_type).await {
        Ok(ClassKind::Scene) => {}
        Ok(_) => {
            return ToolResult::error(
                format!("Class '{root_type}' is not a Node type"),
                &["Use a Node class the engine can create, such as Node2D, Node3D or Control"],
            );
        }
        Err(error) => return class_check_failed(&error),
    }
    let project = match tools.host_project(args) {
        Ok(path) => path,
        Err(result) => return result,
    };
    let file = match resolve_in_project(&project, scene_path) {
        Ok(file) => file,
        Err(problem) => {
            return ToolResult::error(
                problem,
                &["Use a res:// path or a path relative to the project directory"],
            );
        }
    };
    let body = format!("[gd_scene format=3]\n\n[node name=\"{root_name}\" type=\"{root_type}\"]\n");
    match create_new_file(&file, &body) {
        Ok(()) => {}
        Err(error) if error.kind() == ErrorKind::AlreadyExists => {
            return ToolResult::error(
                format!("Scene already exists: {}", res_display(&project, &file)),
                &["Pick another scene_path, or edit the existing scene with the node tools"],
            );
        }
        Err(_) => {
            return ToolResult::error(
                format!("Failed to write {}", res_display(&project, &file)),
                &["Check that the folder can be created inside the project and is writable"],
            );
        }
    }
    ToolResult::text(format!(
        "Created {scene_path} ({root_type}) in {} as text, because no editor has this project open.",
        project.display()
    ))
}

async fn create_resource(tools: &ToolManager, args: &Value) -> ToolResult {
    let choice = match tools.editor_choice(args, true).await {
        Ok(choice) => choice,
        Err(result) => return result,
    };
    let sent = if matches!(choice, EditorChoice::Host) {
        Err(McpError::BridgeUnavailable(
            "another project than the editor's was named".into(),
        ))
    } else {
        tools
            .bridge
            .send_command("create_resource", args.clone())
            .await
    };
    match sent {
        Ok(value) => editor_answer(&choice, value),
        Err(McpError::BridgeUnavailable(_)) => {
            let Some(path) = str_arg(args, "resource_path") else {
                return ToolResult::error(
                    "Missing resource_path",
                    &["Pass resource_path: a res:// path ending in .tres"],
                );
            };
            let Some(class_name) = str_arg(args, "class_name") else {
                return ToolResult::error(
                    "Missing class_name",
                    &["Pass class_name: a built-in Resource class such as Curve"],
                );
            };
            if !path.ends_with(".tres") {
                return ToolResult::error(
                    "resource_path must end in .tres",
                    &["Use a path such as res://materials/wood.tres"],
                );
            }
            match tools.cli.class_kind(class_name).await {
                Ok(ClassKind::Resource) => {}
                Ok(_) => {
                    return ToolResult::error(
                        format!("Unknown resource class: {class_name}"),
                        &[
                            "Use a built-in Resource class the engine can create, such as Curve or Gradient",
                        ],
                    );
                }
                Err(McpError::Validation(problem)) => {
                    return ToolResult::error(
                        format!("Unknown resource class: {problem}"),
                        &[
                            "Use a built-in Resource class the engine can create, such as Curve or Gradient",
                        ],
                    );
                }
                Err(error) => return class_check_failed(&error),
            }
            let project = match tools.host_project(args) {
                Ok(path) => path,
                Err(result) => return result,
            };
            let file = match resolve_in_project(&project, path) {
                Ok(file) => file,
                Err(problem) => {
                    return ToolResult::error(
                        problem,
                        &["Use a res:// path or a path relative to the project directory"],
                    );
                }
            };
            let body = format!("[gd_resource type=\"{class_name}\" format=3]\n");
            match create_new_file(&file, &body) {
                Ok(()) => {}
                Err(error) if error.kind() == ErrorKind::AlreadyExists => {
                    return ToolResult::error(
                        format!("Resource already exists: {}", res_display(&project, &file)),
                        &["Pick another resource_path; an existing resource is never replaced"],
                    );
                }
                Err(_) => {
                    return ToolResult::error(
                        format!("Failed to write {}", res_display(&project, &file)),
                        &[
                            "Check that the folder can be created inside the project and is writable",
                        ],
                    );
                }
            }
            ToolResult::text(format!(
                "Created {path} ({class_name}) in {} as text. Open the editor to let Godot import it.",
                project.display()
            ))
        }
        Err(error) => super::bridge_error_result(&error),
    }
}

/// The class could not be checked because the engine could not be run; nothing was written.
fn class_check_failed(error: &McpError) -> ToolResult {
    ToolResult::error(
        format!("The class could not be checked with the engine, so no file was written: {error}"),
        &[
            "Verify GODOT_PATH points to a working Godot executable",
            "Or open the project in the editor with the Godot MCP Bridge plugin, which checks the class itself",
        ],
    )
}

/// A node name as Godot keeps it: the characters it forbids in a name become `_`. The name is
/// data and goes into the scene file as a quoted value (TL-5).
fn node_name(requested: &str) -> String {
    let cleaned: String = requested
        .chars()
        .map(|c| {
            if c.is_control() || matches!(c, '.' | ':' | '@' | '/' | '"' | '%') {
                '_'
            } else {
                c
            }
        })
        .collect();
    if cleaned.trim().is_empty() {
        "Root".to_string()
    } else {
        cleaned
    }
}

async fn editor_or_error(tools: &ToolManager, command: &str, args: &Value) -> ToolResult {
    match tools.bridge.send_command(command, args.clone()).await {
        Ok(value) => ToolResult::text(serde_json::to_string_pretty(&value).unwrap_or_default()),
        Err(error) => super::bridge_error_result(&error),
    }
}

async fn set_setting(tools: &ToolManager, args: &Value) -> ToolResult {
    if str_arg(args, "key").is_some_and(|key| key.starts_with("input/")) {
        return ToolResult::error(
            "An input/ setting is an InputMap action (a deadzone and a list of InputEvents), not a string, number or boolean",
            &[
                "Read the actions with godot_get_input_map and change one with godot_edit_input_action",
            ],
        );
    }
    let choice = match tools.editor_choice(args, true).await {
        Ok(choice) => choice,
        Err(result) => return result,
    };
    let sent = if matches!(choice, EditorChoice::Host) {
        Err(McpError::BridgeUnavailable(
            "another project than the editor's was named".into(),
        ))
    } else {
        tools
            .bridge
            .send_command("set_project_setting", args.clone())
            .await
    };
    match sent {
        Ok(value) => editor_answer(&choice, value),
        Err(McpError::BridgeUnavailable(_)) => {
            let Some(key) = str_arg(args, "key") else {
                return ToolResult::error(
                    "Missing key",
                    &["Pass key: a setting such as application/config/name"],
                );
            };
            let Some(value) = args.get("value") else {
                return ToolResult::error(
                    "Missing value",
                    &["Pass value: a string, number or boolean"],
                );
            };
            let project = match tools.host_project(args) {
                Ok(path) => path,
                Err(result) => return result,
            };
            write_setting_on_disk(tools, &project.join("project.godot"), key, value).await
        }
        Err(error) => super::bridge_error_result(&error),
    }
}

async fn get_settings(tools: &ToolManager, args: &Value) -> ToolResult {
    let choice = match tools.editor_choice(args, false).await {
        Ok(choice) => choice,
        Err(result) => return result,
    };
    let sent = if matches!(choice, EditorChoice::Host) {
        Err(McpError::BridgeUnavailable(
            "another project than the editor's was named".into(),
        ))
    } else {
        tools
            .bridge
            .send_command("get_project_settings", args.clone())
            .await
    };
    match sent {
        Ok(value) => ToolResult::text(serde_json::to_string_pretty(&value).unwrap_or_default()),
        Err(McpError::BridgeUnavailable(_)) => {
            let project = match tools.host_project(args) {
                Ok(path) => path,
                Err(result) => return result,
            };
            let text = std::fs::read_to_string(project.join("project.godot")).unwrap_or_default();
            let prefix = str_arg(args, "prefix").unwrap_or("");
            ToolResult::text(
                json!({
                    "source": "project.godot",
                    "project_dir": project.display().to_string(),
                    "settings": read_settings(&text, prefix),
                    "truncated": false,
                })
                .to_string(),
            )
        }
        Err(error) => super::bridge_error_result(&error),
    }
}

/// Writes a new file and fails with `AlreadyExists` instead of replacing one, also when the file
/// appears between a check and the write.
fn create_new_file(file: &Path, body: &str) -> std::io::Result<()> {
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut created = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(file)?;
    created.write_all(body.as_bytes())
}

/// A setting key is a path of letters, digits and `_ . -` that starts with a section, like
/// `application/config/name`. Nothing else may reach `project.godot`, where a `]` or a line
/// break in a key would start a new section or a new entry.
fn is_setting_key(key: &str) -> bool {
    key.contains('/')
        && key
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '/' | '.' | '-'))
}

/// A string as `project.godot` stores it: quoted, with every character that would end the
/// value or the line escaped.
fn godot_string(text: &str) -> String {
    let mut quoted = String::with_capacity(text.len() + 2);
    quoted.push('"');
    for character in text.chars() {
        match character {
            '\\' => quoted.push_str("\\\\"),
            '"' => quoted.push_str("\\\""),
            '\n' => quoted.push_str("\\n"),
            '\r' => quoted.push_str("\\r"),
            '\t' => quoted.push_str("\\t"),
            other if other.is_control() => {
                quoted.push_str(&format!("\\u{:04x}", u32::from(other)));
            }
            other => quoted.push(other),
        }
    }
    quoted.push('"');
    quoted
}

/// What an entry of `project.godot` holds, as far as a plain value can replace it.
#[derive(Clone, Copy, PartialEq)]
enum StoredKind {
    Text,
    Flag,
    Whole,
    Fraction,
    Structured,
}

impl StoredKind {
    fn of(stored: &str) -> Self {
        let stored = stored.trim();
        if stored.len() >= 2 && stored.starts_with('"') && stored.ends_with('"') {
            Self::Text
        } else if stored == "true" || stored == "false" {
            Self::Flag
        } else if stored.parse::<i64>().is_ok() {
            Self::Whole
        } else if stored.parse::<f64>().is_ok() {
            Self::Fraction
        } else {
            Self::Structured
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Text => "string",
            Self::Flag => "bool",
            Self::Whole => "int",
            Self::Fraction => "float",
            Self::Structured => "structured value",
        }
    }
}

/// The text to store for `value`. A setting that already exists keeps its type: a value of
/// another type would be written to `project.godot` and fail at the next load.
fn render_setting(value: &Value, stored: Option<StoredKind>) -> Result<String, String> {
    let mismatch = |kind: StoredKind| {
        format!(
            "This setting expects a {}; a value of another type is not stored. Change settings of other types in the editor's Project Settings.",
            kind.name()
        )
    };
    match value {
        Value::String(text) => match stored {
            None | Some(StoredKind::Text) => Ok(godot_string(text)),
            Some(kind) => Err(mismatch(kind)),
        },
        Value::Bool(flag) => match stored {
            None | Some(StoredKind::Flag) => Ok(flag.to_string()),
            Some(kind) => Err(mismatch(kind)),
        },
        Value::Number(number) => match stored {
            None if number.is_f64() => Ok(format!("{:?}", number.as_f64().unwrap_or_default())),
            None => Ok(number.to_string()),
            Some(StoredKind::Whole) => number
                .as_i64()
                .or_else(|| {
                    number
                        .as_f64()
                        .filter(|float| float.fract() == 0.0 && float.abs() < 9.0e15)
                        .map(|float| float as i64)
                })
                .map(|whole| whole.to_string())
                .ok_or_else(|| mismatch(StoredKind::Whole)),
            Some(StoredKind::Fraction) => Ok(format!("{:?}", number.as_f64().unwrap_or_default())),
            Some(kind) => Err(mismatch(kind)),
        },
        _ => Err("value must be a string, number or boolean".to_string()),
    }
}

/// Sets `key` in the `project.godot` at `path` without an editor. The value keeps the type the
/// setting has: the one stored in the file, else the engine's own definition of the setting, which
/// the Godot executable is asked for. A setting neither knows keeps the type of the value.
async fn write_setting_on_disk(
    tools: &ToolManager,
    path: &Path,
    key: &str,
    value: &Value,
) -> ToolResult {
    let wrong_type = |problem: String| {
        ToolResult::error(
            problem,
            &[
                "Send a value of the type the setting already has, or change it in the editor's Project Settings",
            ],
        )
    };
    if !is_setting_key(key) {
        return ToolResult::error(
            "key must start with a section and use only letters, digits and _ / . -, like application/config/name",
            &["Use a key like application/config/name"],
        );
    }
    if !matches!(value, Value::String(_) | Value::Bool(_) | Value::Number(_)) {
        return wrong_type("value must be a string, number or boolean".to_string());
    }
    let original = std::fs::read_to_string(path).unwrap_or_default();
    let stored = stored_setting(&original, key);
    let rendered = match stored.as_deref().map(StoredKind::of) {
        Some(kind) if kind != StoredKind::Structured => render_setting(value, Some(kind)),
        _ => match tools.cli.setting_type(key, value, stored.as_deref()).await {
            Ok(found) => render_typed(value, &found),
            Err(McpError::Validation(problem)) => {
                Err(format!("The type of {key} could not be checked: {problem}"))
            }
            Err(error) => {
                return ToolResult::error(
                    format!(
                        "The type of {key} could not be checked with the engine, so project.godot was not changed: {error}"
                    ),
                    &[
                        "Verify GODOT_PATH points to a working Godot executable",
                        "Or open the project in the editor with the Godot MCP Bridge plugin, which checks the type itself",
                    ],
                );
            }
        },
    };
    let rendered = match rendered {
        Ok(rendered) => rendered,
        Err(problem) => return wrong_type(problem),
    };
    match std::fs::write(path, put_setting(&original, key, &rendered)) {
        Ok(()) => ToolResult::text(format!(
            "Set {key} = {rendered} in {}. Reopen the project in the editor to pick it up.",
            path.display()
        )),
        Err(error) => ToolResult::error(
            format!("Failed to write {}: {error}", path.display()),
            &["Check that project.godot is writable"],
        ),
    }
}

/// The text to store for `value`, given what the engine answered about the setting.
fn render_typed(value: &Value, found: &SettingType) -> Result<String, String> {
    let Some(type_name) = found.type_name.as_deref() else {
        return render_setting(value, None);
    };
    let plain = match type_name {
        "String" => Some(StoredKind::Text),
        "bool" => Some(StoredKind::Flag),
        "int" => Some(StoredKind::Whole),
        "float" => Some(StoredKind::Fraction),
        _ => None,
    };
    if let Some(kind) = plain {
        return render_setting(value, Some(kind));
    }
    match found.text.as_deref() {
        Some(text) if !text.is_empty() && !text.contains(['\n', '\r']) => Ok(text.to_string()),
        _ if type_name == "Color" => Err(
            "This setting expects a Color: send an HTML color such as \"#102b38\" or \"#102b38ff\", or a color name such as \"white\"".to_string(),
        ),
        _ => Err(format!(
            "This setting expects a {type_name}: send it as a string the way project.godot stores it, such as \"{type_name}(...)\". Change settings of other types in the editor's Project Settings."
        )),
    }
}

/// The section header and the line prefix of `key` in `project.godot`.
fn setting_parts(key: &str) -> Option<(String, String)> {
    key.split_once('/')
        .map(|(section, name)| (format!("[{section}]"), format!("{name}=")))
}

/// The lines of `[section]` in `lines`: the header's index and the index of the next header (or
/// the end).
fn section_range(lines: &[&str], header: &str) -> Option<(usize, usize)> {
    let start = lines.iter().position(|line| line.trim() == header)?;
    let end = lines
        .iter()
        .enumerate()
        .skip(start + 1)
        .find(|(_, line)| line.trim().starts_with('['))
        .map_or(lines.len(), |(index, _)| index);
    Some((start, end))
}

/// The value text `project.godot` holds for `key`, if any.
fn stored_setting(text: &str, key: &str) -> Option<String> {
    let (header, prefix) = setting_parts(key)?;
    let lines: Vec<&str> = text.lines().collect();
    let (start, end) = section_range(&lines, &header)?;
    lines[start + 1..end].iter().find_map(|line| {
        line.trim_start()
            .strip_prefix(&prefix)
            .map(|stored| stored.trim().to_string())
    })
}

/// `text` with `key` set to the stored text `rendered`: an existing entry is replaced in place, a
/// new one goes after the last entry of its section, keeping the blank line before the next
/// section, and a new section is added at the end the way Godot writes one.
fn put_setting(text: &str, key: &str, rendered: &str) -> String {
    let Some((header, prefix)) = setting_parts(key) else {
        return text.to_string();
    };
    let assignment = format!("{prefix}{rendered}");
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    let borrowed: Vec<&str> = lines.iter().map(String::as_str).collect();
    let range = section_range(&borrowed, &header);
    let existing = range.and_then(|(start, end)| {
        (start + 1..end).find(|&index| borrowed[index].trim_start().starts_with(&prefix))
    });
    match (existing, range) {
        (Some(index), _) => lines[index] = assignment,
        (None, Some((start, end))) => {
            let last = (start..end)
                .rev()
                .find(|&index| !lines[index].trim().is_empty())
                .unwrap_or(start);
            let mut at = last + 1;
            if last == start {
                // Only the header so far: Godot puts a blank line between it and the entries.
                if lines.get(at).is_some_and(|line| line.trim().is_empty()) {
                    at += 1;
                } else {
                    lines.insert(at, String::new());
                    at += 1;
                }
            }
            lines.insert(at, assignment);
            if lines
                .get(at + 1)
                .is_some_and(|line| line.trim().starts_with('['))
            {
                lines.insert(at + 1, String::new());
            }
        }
        (None, None) => {
            if !lines.is_empty() && !lines.last().is_some_and(|line| line.trim().is_empty()) {
                lines.push(String::new());
            }
            lines.push(header);
            lines.push(String::new());
            lines.push(assignment);
        }
    }
    lines.join("\n") + "\n"
}

/// A stored value as the editor reports it: a quoted string without its quotes and escapes, any
/// other value as its text.
fn plain_value(stored: &str) -> String {
    let stored = stored.trim();
    if stored.len() >= 2 && stored.starts_with('"') && stored.ends_with('"') {
        if let Ok(text) = serde_json::from_str::<String>(stored) {
            return text;
        }
        return stored[1..stored.len() - 1].to_string();
    }
    stored.to_string()
}

fn read_settings(text: &str, prefix: &str) -> Value {
    let mut section = String::new();
    let mut settings = serde_json::Map::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(name) = trimmed
            .strip_prefix('[')
            .and_then(|rest| rest.strip_suffix(']'))
        {
            section = name.to_string();
            continue;
        }
        let Some((name, value)) = trimmed.split_once('=') else {
            continue;
        };
        if section.is_empty() {
            continue;
        }
        let key = format!("{section}/{name}");
        if prefix.is_empty() || key.starts_with(prefix) {
            settings.insert(key, Value::String(plain_value(value)));
        }
    }
    Value::Object(settings)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_string_setting_is_one_escaped_line() {
        assert_eq!(godot_string("plain"), r#""plain""#);
        assert_eq!(godot_string("a\"b\\c"), r#""a\"b\\c""#);
        assert_eq!(godot_string("one\ntwo\r\t"), r#""one\ntwo\r\t""#);
        assert_eq!(godot_string("bell\u{7}"), r#""bell\u0007""#);
        assert_eq!(godot_string("мир"), "\"мир\"");
    }

    #[test]
    fn a_setting_key_is_a_clean_path_with_a_section() {
        for good in ["application/config/name", "custom/a-b.c_d/e1"] {
            assert!(is_setting_key(good), "{good}");
        }
        for bad in [
            "", "name", "/a", "a/", "a//b", "a/b c", "a/b]", "a/b\n[x", "a/b=c", "a/../b", "a/./b",
            "../a/b",
        ] {
            assert!(!is_setting_key(bad), "{bad:?}");
        }
    }

    #[test]
    fn project_setting_roundtrip() {
        let text = "config_version=5\n\n[application]\n\nconfig/name=\"Old\"\n";
        let text = put_setting(text, "application/config/name", "\"New\"");
        let text = put_setting(&text, "display/window/size/viewport_width", "640");
        assert_eq!(
            text,
            "config_version=5\n\n[application]\n\nconfig/name=\"New\"\n\n[display]\n\nwindow/size/viewport_width=640\n"
        );
        assert_eq!(
            stored_setting(&text, "application/config/name").as_deref(),
            Some("\"New\"")
        );
        assert_eq!(stored_setting(&text, "application/config/icon"), None);
        let settings = read_settings(&text, "application/");
        assert_eq!(settings["application/config/name"], "New");
    }

    #[test]
    fn a_new_setting_keeps_the_blank_line_before_the_next_section() {
        let text = "config_version=5\n\n[application]\n\nconfig/name=\"Frog\"\nrun/main_scene=\"res://main.tscn\"\n\n[display]\n\nwindow/stretch/mode=\"viewport\"\n";
        assert_eq!(
            put_setting(
                text,
                "application/boot_splash/bg_color",
                "Color(0.0627451, 0.16862746, 0.21960784, 1)"
            ),
            "config_version=5\n\n[application]\n\nconfig/name=\"Frog\"\nrun/main_scene=\"res://main.tscn\"\nboot_splash/bg_color=Color(0.0627451, 0.16862746, 0.21960784, 1)\n\n[display]\n\nwindow/stretch/mode=\"viewport\"\n"
        );
        // A section with only its header gets the blank line Godot writes after a header.
        assert_eq!(
            put_setting("[a]\n\n[b]\n\nx=1\n", "a/k", "2"),
            "[a]\n\nk=2\n\n[b]\n\nx=1\n"
        );
        assert_eq!(put_setting("[a]\n[b]\n", "a/k", "2"), "[a]\n\nk=2\n\n[b]\n");
        assert_eq!(put_setting("[a]\n", "a/k", "2"), "[a]\n\nk=2\n");
        // A section written without a blank line before the next one gets one.
        assert_eq!(
            put_setting("[a]\n\nx=1\n[b]\n", "a/k", "2"),
            "[a]\n\nx=1\nk=2\n\n[b]\n"
        );
    }

    #[test]
    fn a_setting_of_another_type_takes_the_engine_text_or_is_refused() {
        let typed = |type_name: &str, text: Option<&str>| SettingType {
            type_name: Some(type_name.to_string()),
            text: text.map(str::to_string),
        };
        assert_eq!(
            render_typed(
                &json!("#102b38"),
                &typed("Color", Some("Color(0.1, 0.2, 0.3, 1)"))
            ),
            Ok("Color(0.1, 0.2, 0.3, 1)".to_string())
        );
        let refused = render_typed(&json!("teal-ish"), &typed("Color", None)).unwrap_err();
        assert!(refused.contains("expects a Color"), "{refused}");
        assert!(refused.contains("#102b38"), "{refused}");
        let refused = render_typed(&json!(3), &typed("Vector2i", None)).unwrap_err();
        assert!(refused.contains("Vector2i(...)"), "{refused}");
        // An engine answer that would break the line is never written.
        assert!(render_typed(&json!("x"), &typed("StringName", Some("&\"a\nb\""))).is_err());

        // Plain types follow the same rules as a value already in project.godot.
        assert_eq!(
            render_typed(&json!(648), &typed("int", None)),
            Ok("648".to_string())
        );
        assert_eq!(
            render_typed(&json!(648.0), &typed("int", None)),
            Ok("648".to_string())
        );
        assert!(render_typed(&json!(2.5), &typed("int", None)).is_err());
        assert!(render_typed(&json!("60"), &typed("int", None)).is_err());
        assert_eq!(
            render_typed(&json!(1), &typed("float", None)),
            Ok("1.0".to_string())
        );
        assert_eq!(
            render_typed(&json!("Frog"), &typed("String", None)),
            Ok("\"Frog\"".to_string())
        );
        assert!(render_typed(&json!("yes"), &typed("bool", None)).is_err());

        // A setting the engine does not define keeps the value's own type.
        let unknown = SettingType::default();
        assert_eq!(render_typed(&json!(7), &unknown), Ok("7".to_string()));
        assert_eq!(
            render_typed(&json!("#fff"), &unknown),
            Ok("\"#fff\"".to_string())
        );
    }
}
