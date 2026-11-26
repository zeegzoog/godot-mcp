//! Scene, script, signal, resource and project-setting edits.
//!
//! The editor path goes through the bridge and is undoable. With no editor, creating
//! a scene, a resource or a project setting falls back to writing the file.

use super::shaders::{random_uid, uid_text};
use super::{EditorChoice, ToolManager, str_arg};
use crate::bridge_protocol::for_editor;
use crate::cli_runner::{ClassKind, SettingType};
use crate::error::McpError;
use crate::godot::{UidCacheUpdate, record_uids};
use crate::paths::{res_display, resolve_in_project};
use crate::protocol::{Tool, ToolResult};
use serde_json::{Value, json};
use std::io::{ErrorKind, Write};
use std::path::Path;

pub fn catalog() -> Vec<Tool> {
    vec![
        tool(
            "godot_create_project",
            "Creates a minimal Godot 4 project without an engine, in a new or existing directory (files kept). Never replaces project.godot; installs no bridge or main scene.",
            json!({"project_path": s("Absolute directory without .. segments."), "name": s("Non-empty project display name.")}),
            &["project_path", "name"],
        ).additive_idempotent(),
        tool(
            "godot_create_scene",
            "Creates a .tscn with one root node (opened in the editor when connected, else written directly). Refuses an existing scene.",
            json!({"scene_path": s("res:// path ending in .tscn"), "root_type": s("Default Node2D."), "root_name": s("Default Root."), "project_path": s("")}),
            &["scene_path"],
        ).additive_idempotent(),
        tool(
            "godot_attach_script",
            "Attaches an existing project script to a node in the edited scene (undoable); empty script_path detaches.",
            json!({"node_path": s(""), "script_path": s("res:// path, or empty to detach."), "make_editable": super::make_editable_param()}),
            &["node_path"],
        ).destructive_idempotent(),
        tool(
            "godot_connect_signal",
            "Connects a signal in the edited scene with CONNECT_PERSIST (undoable). The target must already have the method (attach its script first); an existing connection is refused.",
            json!({"node_path": s(""), "signal_name": s(""), "target_path": s("Default: the same node."), "method": s(""), "make_editable": super::make_editable_param()}),
            &["node_path", "signal_name", "method"],
        ).additive_idempotent(),
        tool(
            "godot_list_signals",
            "Lists signals of a node in the edited scene.",
            json!({"node_path": s("")}),
            &[],
        ).read_only(),
        tool(
            "godot_create_resource",
            "Creates a .tres of a built-in Resource class; never replaces a file. Uses the editor when connected; else writes it with a uid and removes it with an error if a headless Godot cannot load it.",
            json!({"resource_path": s(""), "class_name": s("For example StandardMaterial3D."), "project_path": s("")}),
            &["resource_path", "class_name"],
        ).additive_idempotent(),
        tool(
            "godot_get_project_settings",
            "Reads project settings: the live editor's when connected, else the keys in project.godot.",
            json!({"prefix": s("Optional key prefix."), "project_path": s("")}),
            &[],
        ).read_only(),
        tool(
            "godot_set_project_setting",
            "Sets and saves one project setting, or removes it (editor when connected, else project.godot). Pass exactly one of value, value_text, remove: true. value must have the setting's type (stored, else engine-defined, else value's): objects become Dictionaries, arrays Arrays, whole numbers int, nested values keep their JSON type (use value_text for a nested Vector4 or Color); other types take a string (an HTML color, or project.godot text). A feature override (name.mobile) has the type of name; importer_defaults/<importer> takes a Dictionary of import options. value_text is project.godot text (built-in types only, e.g. a shader_globals Dictionary with a Vector4). A type mismatch is refused unless replace_type is true for a setting the engine does not define. remove: true deletes it (engine settings return to default). input/... actions: godot_edit_input_action.",
            json!({
                "key": s("For example application/config/name."),
                "value": {"description": "String, number, boolean, array or object."},
                "value_text": s("Instead of value: project.godot text, e.g. {\"type\": \"vec4\", \"value\": Vector4(0, -1000, 0, 0)}."),
                "replace_type": {"type": "boolean", "description": "Allow another type for a setting the engine does not define. Default false."},
                "remove": {"type": "boolean", "description": "Remove the setting. Default false."},
                "project_path": s("")
            }),
            &["key"],
        ).destructive_idempotent(),
    ]
}

fn s(description: &str) -> Value {
    if description.is_empty() {
        json!({"type": "string"})
    } else {
        json!({"type": "string", "description": description})
    }
}

fn tool(name: &str, description: &str, properties: Value, required: &[&str]) -> super::ToolDraft {
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
        .send_command("create_scene", for_editor(args, &[]))
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
    let body = format!(
        "[gd_scene format=3]\n\n[node name={} type=\"{root_type}\"]\n",
        godot_string(&root_name)
    );
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
        "Created {scene_path} ({root_type}, root node named {}) in {} as text, because no editor has this project open.",
        serde_json::to_string(&root_name).unwrap_or_default(),
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
            .send_command("create_resource", for_editor(args, &[]))
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
            let uid = uid_text(random_uid());
            let body = empty_resource_text(class_name, &uid);
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
            let res_path = res_display(&project, &file);
            let verified = match tools.cli.check_resource_load(&project, &res_path).await {
                Ok(load) if load.loaded && load.class == class_name => {
                    "Godot loads it (checked in a headless run)".to_string()
                }
                Ok(load) => {
                    // The file is the one this call made, so removing it leaves the project as it was.
                    let _ = std::fs::remove_file(&file);
                    let seen = if load.loaded {
                        format!("Godot loaded it as {}", load.class)
                    } else {
                        "Godot could not load it".to_string()
                    };
                    return ToolResult::error(
                        format!(
                            "{res_path} was written but {seen}, so it was removed again: {}",
                            super::export::tail(&load.output)
                        ),
                        &[
                            "Open the project in the editor with the Godot MCP Bridge plugin and create the resource there",
                            "Report this with godot_note_feedback: the text the server writes should load in this Godot version",
                        ],
                    );
                }
                Err(error) => format!(
                    "the load check could not run ({error}), so it is not verified that Godot loads it"
                ),
            };
            let cache = match record_uids(&project, &[(res_path.clone(), uid.clone())]) {
                Ok(UidCacheUpdate::Recorded(_)) => " Its uid is recorded in .godot/uid_cache.bin.",
                Ok(UidCacheUpdate::Missing) | Err(_) => "",
            };
            ToolResult::text(format!(
                "Created {res_path} ({class_name}, {uid}) in {} as text; {verified}.{cache} Open the editor to let Godot import it.",
                project.display()
            ))
        }
        Err(error) => super::bridge_error_result(&error),
    }
}

/// The text Godot saves for a new resource of `class` whose values are all defaults: the header
/// with its uid and an empty `[resource]` section, without which the text loader stops at
/// "Unexpected end of file".
fn empty_resource_text(class: &str, uid: &str) -> String {
    format!("[gd_resource type=\"{class}\" format=3 uid=\"{uid}\"]\n\n[resource]\n")
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

/// A node name as Godot keeps it: the characters it forbids in a name (`. : @ / " %`) become
/// `_`, as `Node.name` does in the editor; every other character stays, and an empty name is
/// `Root`, as in the editor's `create_scene`. NUL, which a Godot string cannot hold, becomes `_`
/// too. The name is data: it goes into the scene file as an escaped, quoted value (TL-5).
fn node_name(requested: &str) -> String {
    if requested.is_empty() {
        return "Root".to_string();
    }
    requested
        .chars()
        .map(|c| {
            if matches!(c, '.' | ':' | '@' | '/' | '"' | '%' | '\0') {
                '_'
            } else {
                c
            }
        })
        .collect()
}

async fn editor_or_error(tools: &ToolManager, command: &str, args: &Value) -> ToolResult {
    match tools.bridge.send_command(command, args.clone()).await {
        Ok(value) => ToolResult::text(serde_json::to_string_pretty(&value).unwrap_or_default()),
        Err(error) => super::bridge_error_result(&error),
    }
}

/// What a `godot_set_project_setting` call asks for: exactly one of `value`, `value_text` and
/// `remove: true`.
enum SettingChange<'a> {
    Value(&'a Value),
    Text(&'a str),
    Remove,
}

/// The change a call asks for, or the error that names what is missing or doubled.
fn setting_change(args: &Value) -> Result<SettingChange<'_>, ToolResult> {
    let recovery = &[
        "Pass exactly one of: value (a string, number, boolean, array or object), value_text (a value as project.godot stores it, such as {\"type\": \"vec4\", \"value\": Vector4(0, -1000, 0, 0)}), or remove: true",
    ];
    for flag in ["remove", "replace_type"] {
        if args
            .get(flag)
            .is_some_and(|v| !v.is_null() && !v.is_boolean())
        {
            return Err(ToolResult::error(
                format!("{flag} must be true or false"),
                recovery,
            ));
        }
    }
    let remove = args.get("remove").and_then(Value::as_bool) == Some(true);
    let value = args.get("value").filter(|v| !v.is_null());
    let text = args.get("value_text").filter(|v| !v.is_null());
    if text.is_some_and(|text| !text.is_string()) {
        return Err(ToolResult::error(
            "value_text must be a string: a value as project.godot stores it",
            recovery,
        ));
    }
    match (value, text.and_then(Value::as_str), remove) {
        (Some(value), None, false) => Ok(SettingChange::Value(value)),
        (None, Some(text), false) => Ok(SettingChange::Text(text)),
        (None, None, true) => Ok(SettingChange::Remove),
        (None, None, false) if args.get("value").is_some() => Err(ToolResult::error(
            "value is null; a setting is removed with remove: true",
            recovery,
        )),
        (None, None, false) => Err(ToolResult::error("Missing value", recovery)),
        _ => Err(ToolResult::error(
            "Pass only one of value, value_text and remove: true",
            recovery,
        )),
    }
}

async fn set_setting(tools: &ToolManager, args: &Value) -> ToolResult {
    if str_arg(args, "key").is_some_and(|key| key.starts_with("input/")) {
        return ToolResult::error(
            "An input/ setting is an InputMap action (a deadzone and a list of InputEvents), not a plain value",
            &[
                "Read the actions with godot_get_input_map and change one with godot_edit_input_action",
            ],
        );
    }
    let change = match setting_change(args) {
        Ok(change) => change,
        Err(result) => return result,
    };
    let replace_type = args.get("replace_type").and_then(Value::as_bool) == Some(true);
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
            .send_command("set_project_setting", for_editor(args, &[]))
            .await
    };
    match sent {
        Ok(mut value) => {
            let as_text = matches!(change, SettingChange::Value(Value::String(text)) if looks_like_value_text(text));
            if as_text && value.get("type").and_then(Value::as_str) == Some("String") {
                value["note"] = json!(
                    "It is stored as a String; for the Godot value this text describes, set it again with value_text and replace_type: true."
                );
            }
            editor_answer(&choice, value)
        }
        Err(McpError::BridgeUnavailable(_)) => {
            let Some(key) = str_arg(args, "key") else {
                return ToolResult::error(
                    "Missing key",
                    &["Pass key: a setting such as application/config/name"],
                );
            };
            let project = match tools.host_project(args) {
                Ok(path) => path,
                Err(result) => return result,
            };
            let file = project.join("project.godot");
            match change {
                SettingChange::Value(value) => {
                    write_setting_on_disk(tools, &file, key, value, replace_type).await
                }
                SettingChange::Text(text) => {
                    write_setting_text_on_disk(tools, &file, key, text, replace_type).await
                }
                SettingChange::Remove => remove_setting_on_disk(&file, key),
            }
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
            .send_command("get_project_settings", for_editor(args, &[]))
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
        _ => match stored {
            Some(kind) => Err(mismatch(kind)),
            None => Err(
                "An array or object value is stored as the engine converts it, and the engine gave no text for it".to_string(),
            ),
        },
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
    replace_type: bool,
) -> ToolResult {
    let wrong_type = |problem: String| {
        ToolResult::error(
            problem,
            &[
                "Send a value of the type the setting already has, or change it in the editor's Project Settings",
                "For a Godot value such as a Vector4 or a Dictionary holding one, pass value_text: the value as project.godot stores it",
                "For a setting the engine does not define (such as shader_globals/<name>), pass replace_type: true to store a value of another type, or remove: true to remove it",
            ],
        )
    };
    if !is_setting_key(key) {
        return bad_key();
    }
    let original = std::fs::read_to_string(path).unwrap_or_default();
    // With replace_type the stored value does not count: only the engine's definition does.
    let stored = stored_setting(&original, key).filter(|_| !replace_type);
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
    let mut note = String::new();
    if value.as_str().is_some_and(looks_like_value_text) && rendered.starts_with('"') {
        note = " It is stored as a String; for the Godot value this text describes, set it again with value_text and replace_type: true.".to_string();
    }
    save_setting(
        path,
        &put_setting(&original, key, &rendered),
        key,
        &rendered,
        &note,
    )
}

fn bad_key() -> ToolResult {
    ToolResult::error(
        "key must start with a section and use only letters, digits and _ / . -, like application/config/name",
        &["Use a key like application/config/name"],
    )
}

/// Whether a string looks like a Godot value text (`{...}`, `[...]`, `Vector4(...)`), which a
/// caller may have meant as that value rather than as a String.
fn looks_like_value_text(text: &str) -> bool {
    let text = text.trim();
    if text.starts_with('{') || text.starts_with('[') {
        return true;
    }
    let name: String = text
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect();
    !name.is_empty()
        && name.starts_with(|c: char| c.is_ascii_uppercase())
        && text[name.len()..].starts_with('(')
        && text.ends_with(')')
}

fn save_setting(path: &Path, text: &str, key: &str, rendered: &str, note: &str) -> ToolResult {
    match std::fs::write(path, text) {
        Ok(()) => ToolResult::text(format!(
            "Set {key} = {rendered} in {}.{note} Reopen the project in the editor to pick it up.",
            path.display()
        )),
        Err(error) => ToolResult::error(
            format!("Failed to write {}: {error}", path.display()),
            &["Check that project.godot is writable"],
        ),
    }
}

/// Sets `key` to the value text `text` without an editor: the engine parses it as project.godot
/// is read, and the setting keeps its type (the stored one, else the engine's definition) unless
/// `replace_type` is given for a setting the engine does not define.
async fn write_setting_text_on_disk(
    tools: &ToolManager,
    path: &Path,
    key: &str,
    text: &str,
    replace_type: bool,
) -> ToolResult {
    if !is_setting_key(key) {
        return bad_key();
    }
    let original = std::fs::read_to_string(path).unwrap_or_default();
    let stored = stored_setting(&original, key);
    let found = match tools
        .cli
        .setting_from_text(key, text, stored.as_deref())
        .await
    {
        Ok(found) => found,
        Err(McpError::Validation(problem)) => {
            return ToolResult::error(
                format!("{key} was not changed: {problem}"),
                &[
                    "Give value_text as project.godot stores the value, such as {\"type\": \"vec4\", \"value\": Vector4(0, -1000, 0, 0)}",
                ],
            );
        }
        Err(error) => {
            return ToolResult::error(
                format!(
                    "value_text for {key} could not be read with the engine, so project.godot was not changed: {error}"
                ),
                &[
                    "Verify GODOT_PATH points to a working Godot executable",
                    "Or open the project in the editor with the Godot MCP Bridge plugin, which reads the text itself",
                ],
            );
        }
    };
    let rendered = match text_setting(key, &found, replace_type) {
        Ok(rendered) => rendered,
        Err(problem) => return problem,
    };
    save_setting(
        path,
        &put_setting(&original, key, &rendered),
        key,
        &rendered,
        "",
    )
}

/// The text to store for a parsed value text, or the refusal: a value of another type than the
/// setting's is stored only with `replace_type`, and never for a setting the engine defines.
fn text_setting(key: &str, found: &SettingType, replace_type: bool) -> Result<String, ToolResult> {
    let Some(text) = found
        .text
        .as_deref()
        .filter(|text| !text.is_empty() && !text.contains(['\n', '\r']))
    else {
        return Err(ToolResult::error(
            format!(
                "The engine gave no one-line text for value_text of {key}, so project.godot was not changed"
            ),
            &["Give a value of a built-in type, such as Vector4(0, 1, 0, 0) or {\"key\": 1}"],
        ));
    };
    let value_type = found.value_type.as_deref().unwrap_or("unknown");
    match found.type_name.as_deref() {
        None => Ok(text.to_string()),
        Some(type_name) if type_name == value_type => Ok(text.to_string()),
        Some(_) if replace_type && !found.defined => Ok(text.to_string()),
        Some(type_name) if found.defined => Err(ToolResult::error(
            format!(
                "{key} is a {type_name} setting of the engine; value_text gives a {value_type}, which is not stored"
            ),
            &[&format!(
                "Give value_text as a {type_name}, as project.godot stores one"
            )],
        )),
        Some(type_name) => Err(ToolResult::error(
            format!(
                "{key} holds a {type_name}; value_text gives a {value_type}, which is not stored without replace_type"
            ),
            &[
                "Pass replace_type: true to replace the stored value with this one (the engine does not define this setting)",
                "Or remove the setting with remove: true",
            ],
        )),
    }
}

/// Removes `key` from `project.godot` without an editor; a setting the engine defines then has
/// its default again. A key that is not stored changes nothing.
fn remove_setting_on_disk(path: &Path, key: &str) -> ToolResult {
    if !is_setting_key(key) {
        return bad_key();
    }
    let original = std::fs::read_to_string(path).unwrap_or_default();
    let Some(previous) = stored_setting(&original, key) else {
        return ToolResult::text(format!(
            "{key} is not stored in {}; nothing was removed.",
            path.display()
        ));
    };
    match std::fs::write(path, drop_setting(&original, key)) {
        Ok(()) => ToolResult::text(format!(
            "Removed {key} (it was {previous}) from {}. Reopen the project in the editor to pick it up.",
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
    let one_line = found
        .text
        .as_deref()
        .filter(|text| !text.is_empty() && !text.contains(['\n', '\r']));
    let Some(type_name) = found.type_name.as_deref() else {
        // A setting the engine does not define keeps the type of the value; an array or object
        // is written as the engine converted it.
        if (value.is_array() || value.is_object())
            && let Some(text) = one_line
        {
            return Ok(text.to_string());
        }
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
    match one_line {
        Some(text) => Ok(text.to_string()),
        _ if type_name == "Color" => Err(
            "This setting expects a Color: send an HTML color such as \"#102b38\" or \"#102b38ff\", or a color name such as \"white\"".to_string(),
        ),
        _ if matches!(type_name, "Dictionary" | "Array") => Err(format!(
            "This setting expects a {type_name}: send a JSON {}, or a string holding the text project.godot stores, such as {}. An Object is never created.",
            if type_name == "Array" { "array" } else { "object" },
            if type_name == "Array" { "[1, \"two\"]" } else { "{\"key\": 1}" },
        )),
        _ if type_name.starts_with("Packed") => Err(format!(
            "This setting expects a {type_name}: send a JSON array of its elements, or a string holding the text project.godot stores, such as \"{type_name}(...)\""
        )),
        _ => Err(format!(
            "This setting expects a {type_name}: send it as a string the way project.godot stores it, such as \"{type_name}(...)\". Change settings of other types in the editor's Project Settings."
        )),
    }
}

/// The section and the entry name of `key` in `project.godot`.
fn setting_parts(key: &str) -> Option<(&str, &str)> {
    key.split_once('/')
}

/// A section header of `project.godot` and the line it is on.
struct Header {
    name: String,
    line: usize,
}

/// One `name=value` entry of `project.godot`: its section, its name, its value text (every line
/// of it: Godot writes a Dictionary over several lines) and the lines `first..end` it spans.
struct Entry {
    section: String,
    name: String,
    value: String,
    first: usize,
    end: usize,
}

/// The headers and entries of `project.godot`. A value continues on the next lines while a
/// bracket, brace or parenthesis is open or a string is not closed, so the lines inside a
/// Dictionary are part of its entry and never read as entries or headers of their own.
fn layout(lines: &[&str]) -> (Vec<Header>, Vec<Entry>) {
    let mut headers = Vec::new();
    let mut entries = Vec::new();
    let mut section = String::new();
    let mut index = 0;
    while index < lines.len() {
        let trimmed = lines[index].trim();
        if let Some(name) = trimmed
            .strip_prefix('[')
            .and_then(|rest| rest.strip_suffix(']'))
        {
            section = name.to_string();
            headers.push(Header {
                name: section.clone(),
                line: index,
            });
            index += 1;
            continue;
        }
        let Some((name, rest)) = trimmed.split_once('=') else {
            index += 1;
            continue;
        };
        let end = value_end(lines, index, rest);
        let mut value = rest.trim().to_string();
        for line in &lines[index + 1..end] {
            value.push('\n');
            value.push_str(line);
        }
        entries.push(Entry {
            section: section.clone(),
            name: name.trim().to_string(),
            value: value.trim_end().to_string(),
            first: index,
            end,
        });
        index = end;
    }
    (headers, entries)
}

/// The line after the last line of the value that starts as `rest` on line `first`.
fn value_end(lines: &[&str], first: usize, rest: &str) -> usize {
    let mut depth: i64 = 0;
    let mut in_string = false;
    let mut escaped = false;
    let mut line = first;
    let mut text = rest;
    loop {
        for character in text.chars() {
            if in_string {
                if escaped {
                    escaped = false;
                } else if character == '\\' {
                    escaped = true;
                } else if character == '"' {
                    in_string = false;
                }
                continue;
            }
            match character {
                '"' => in_string = true,
                '(' | '[' | '{' => depth += 1,
                ')' | ']' | '}' => depth -= 1,
                _ => {}
            }
        }
        line += 1;
        if (depth <= 0 && !in_string) || line >= lines.len() {
            return line;
        }
        text = lines[line];
    }
}

/// The lines of `[section]`: the header's index and the index of the next header (or the end).
fn section_range(headers: &[Header], line_count: usize, section: &str) -> Option<(usize, usize)> {
    let at = headers.iter().position(|header| header.name == section)?;
    let end = headers.get(at + 1).map_or(line_count, |next| next.line);
    Some((headers[at].line, end))
}

/// The value text `project.godot` holds for `key`, if any, with every line of it.
fn stored_setting(text: &str, key: &str) -> Option<String> {
    let (section, name) = setting_parts(key)?;
    let lines: Vec<&str> = text.lines().collect();
    let (_, entries) = layout(&lines);
    entries
        .into_iter()
        .find(|entry| entry.section == section && entry.name == name)
        .map(|entry| entry.value)
}

/// `text` with `key` set to the stored text `rendered`: an existing entry is replaced in place
/// (all its lines), a new one goes after the last entry of its section, keeping the blank line
/// before the next section, and a new section is added at the end the way Godot writes one.
fn put_setting(text: &str, key: &str, rendered: &str) -> String {
    let Some((section, name)) = setting_parts(key) else {
        return text.to_string();
    };
    let assignment = format!("{name}={rendered}");
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    let borrowed: Vec<&str> = lines.iter().map(String::as_str).collect();
    let (headers, entries) = layout(&borrowed);
    let existing = entries
        .iter()
        .find(|entry| entry.section == section && entry.name == name)
        .map(|entry| (entry.first, entry.end));
    let range = section_range(&headers, borrowed.len(), section);
    match (existing, range) {
        (Some((first, end)), _) => {
            lines.splice(first..end, [assignment]);
        }
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
            lines.push(format!("[{section}]"));
            lines.push(String::new());
            lines.push(assignment);
        }
    }
    lines.join("\n") + "\n"
}

/// `text` without the entry of `key` (all its lines). A section left without entries goes too,
/// with the blank line before it, the way Godot writes a project without that section.
fn drop_setting(text: &str, key: &str) -> String {
    let Some((section, name)) = setting_parts(key) else {
        return text.to_string();
    };
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    let borrowed: Vec<&str> = lines.iter().map(String::as_str).collect();
    let (headers, entries) = layout(&borrowed);
    let Some((first, end)) = entries
        .iter()
        .find(|entry| entry.section == section && entry.name == name)
        .map(|entry| (entry.first, entry.end))
    else {
        return text.to_string();
    };
    let emptied = !entries
        .iter()
        .any(|entry| entry.section == section && entry.name != name);
    let range = section_range(&headers, borrowed.len(), section);
    let line_count = borrowed.len();
    match range {
        Some((start, section_end)) if emptied => {
            // The header, the entry and the blank lines of the section go, with the blank lines
            // before the header; one blank line is kept before the next section.
            let mut from = start;
            while from > 0 && lines[from - 1].trim().is_empty() {
                from -= 1;
            }
            lines.drain(from..section_end);
            if from > 0 && section_end < line_count {
                lines.insert(from, String::new());
            }
        }
        _ => {
            lines.drain(first..end);
        }
    }
    if lines.is_empty() {
        return String::new();
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
    let lines: Vec<&str> = text.lines().collect();
    let (_, entries) = layout(&lines);
    let mut settings = serde_json::Map::new();
    for entry in entries {
        if entry.section.is_empty() {
            continue;
        }
        let key = format!("{}/{}", entry.section, entry.name);
        if prefix.is_empty() || key.starts_with(prefix) {
            settings.insert(key, Value::String(plain_value(&entry.value)));
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
    fn a_removed_setting_takes_its_lines_and_an_emptied_section_with_it() {
        let text = "config_version=5\n\n[application]\n\nconfig/name=\"Frog\"\nrun/x=1\n\n[shader_globals]\n\nfrog_push=\"{\\\"type\\\": 1}\"\n\n[rendering]\n\nr=1\n";
        // Another entry stays in its section.
        assert_eq!(
            drop_setting(text, "application/run/x"),
            text.replace("run/x=1\n", "")
        );
        // The only entry of a section in the middle: the section goes, one blank line stays.
        assert_eq!(
            drop_setting(text, "shader_globals/frog_push"),
            "config_version=5\n\n[application]\n\nconfig/name=\"Frog\"\nrun/x=1\n\n[rendering]\n\nr=1\n"
        );
        // The last section goes without leaving blank lines at the end.
        assert_eq!(
            drop_setting(text, "rendering/r"),
            "config_version=5\n\n[application]\n\nconfig/name=\"Frog\"\nrun/x=1\n\n[shader_globals]\n\nfrog_push=\"{\\\"type\\\": 1}\"\n"
        );
        // A value over several lines goes whole.
        let multi = "[a]\n\nd={\n\"k\": 1\n}\ne=2\n";
        assert_eq!(drop_setting(multi, "a/d"), "[a]\n\ne=2\n");
        assert_eq!(drop_setting(multi, "a/missing"), multi);
        assert_eq!(drop_setting("[a]\n\nk=1\n", "a/k"), "");
    }

    #[test]
    fn a_string_that_looks_like_a_godot_value_is_noticed() {
        for text in [
            "{\"type\": \"vec4\"}",
            " [1, 2]",
            "Vector4(0, -1000, 0, 0)",
            "Color(1, 1, 1, 1)",
        ] {
            assert!(looks_like_value_text(text), "{text}");
        }
        for text in ["Frog", "res://main.tscn", "hello (world)", "f(x)", ""] {
            assert!(!looks_like_value_text(text), "{text}");
        }
    }

    #[test]
    fn a_value_text_keeps_the_setting_type_unless_replace_type_is_given() {
        let parsed = |type_name: Option<&str>, value_type: &str, defined: bool| SettingType {
            type_name: type_name.map(str::to_string),
            text: Some("{\"type\": \"vec4\", \"value\": Vector4(0, -1000, 0, 0)}".to_string()),
            value_type: Some(value_type.to_string()),
            defined,
        };
        let key = "shader_globals/frog_push";
        let frog = "{\"type\": \"vec4\", \"value\": Vector4(0, -1000, 0, 0)}";
        // A new setting, and one of the same type.
        for found in [
            parsed(None, "Dictionary", false),
            parsed(Some("Dictionary"), "Dictionary", false),
        ] {
            assert_eq!(text_setting(key, &found, false).ok().as_deref(), Some(frog));
        }
        // A String stored by an earlier call: refused, then replaced with replace_type.
        let wrong = parsed(Some("String"), "Dictionary", false);
        let refused = text_setting(key, &wrong, false).expect_err("refused");
        let message = format!("{refused:?}");
        assert!(
            message.contains("replace_type") && message.contains("remove"),
            "{message}"
        );
        assert_eq!(text_setting(key, &wrong, true).ok().as_deref(), Some(frog));
        // A setting the engine defines keeps its type even with replace_type.
        let defined = parsed(Some("Vector2"), "Dictionary", true);
        assert!(text_setting("physics/2d/default_gravity_vector", &defined, true).is_err());
        // A text that would add lines is never written.
        let mut multi = parsed(None, "Dictionary", false);
        multi.text = Some("{\n}".to_string());
        assert!(text_setting(key, &multi, false).is_err());
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
    fn a_value_written_over_several_lines_is_one_entry() {
        // How Godot writes a Dictionary: the lines inside it are neither entries nor headers.
        let text = "config_version=5\n\n[importer_defaults]\n\ntexture={\n\"detect_3d/compress_to\": 0,\n\"a=b\": \"x\\\"}[y]\",\n\"list\": [\n1]\n}\nscene={}\n\n[input]\n\nui_accept={\n\"deadzone\": 0.5\n}\n";
        assert_eq!(
            stored_setting(text, "importer_defaults/texture").as_deref(),
            Some("{\n\"detect_3d/compress_to\": 0,\n\"a=b\": \"x\\\"}[y]\",\n\"list\": [\n1]\n}")
        );
        assert_eq!(
            stored_setting(text, "importer_defaults/scene").as_deref(),
            Some("{}")
        );
        let settings = read_settings(text, "importer_defaults/");
        assert_eq!(
            settings.as_object().map(|map| map.len()),
            Some(2),
            "{settings}"
        );
        assert_eq!(
            put_setting(
                text,
                "importer_defaults/texture",
                "{\"detect_3d/compress_to\": 1}"
            ),
            "config_version=5\n\n[importer_defaults]\n\ntexture={\"detect_3d/compress_to\": 1}\nscene={}\n\n[input]\n\nui_accept={\n\"deadzone\": 0.5\n}\n"
        );
        // A new entry goes after the whole last value of its section.
        assert_eq!(
            put_setting(text, "input/ui_cancel", "{}"),
            format!("{text}ui_cancel={{}}\n")
        );
        assert_eq!(
            put_setting("[a]\n\nx={\n\"k\": 1\n}\n\n[b]\n", "a/y", "2"),
            "[a]\n\nx={\n\"k\": 1\n}\ny=2\n\n[b]\n"
        );
    }

    #[test]
    fn an_array_or_object_is_stored_as_the_engine_converts_it() {
        let typed = |type_name: Option<&str>, text: Option<&str>| SettingType {
            type_name: type_name.map(str::to_string),
            text: text.map(str::to_string),
            ..SettingType::default()
        };
        let value = json!({"detect_3d/compress_to": 0});
        assert_eq!(
            render_typed(
                &value,
                &typed(Some("Dictionary"), Some("{\"detect_3d/compress_to\": 0}"))
            ),
            Ok("{\"detect_3d/compress_to\": 0}".to_string())
        );
        // A setting nobody defines keeps the value's type: the engine's text of the array.
        assert_eq!(
            render_typed(&json!([1, "two"]), &typed(None, Some("[1, \"two\"]"))),
            Ok("[1, \"two\"]".to_string())
        );
        // No text, or one that would add lines, is never written.
        for found in [
            typed(Some("Dictionary"), None),
            typed(Some("Dictionary"), Some("{\n\"a\": 1\n}")),
            typed(None, None),
            typed(None, Some("[\n]")),
        ] {
            assert!(render_typed(&value, &found).is_err(), "{found:?}");
        }
        let refused = render_typed(&json!(5), &typed(Some("Dictionary"), None)).unwrap_err();
        assert!(
            refused.contains("expects a Dictionary") && refused.contains("JSON object"),
            "{refused}"
        );
        let refused = render_typed(&json!(5), &typed(Some("PackedStringArray"), None)).unwrap_err();
        assert!(refused.contains("JSON array"), "{refused}");
        // An object for a string setting names the type it expects.
        let refused = render_setting(&value, Some(StoredKind::Text)).unwrap_err();
        assert!(refused.contains("expects a string"), "{refused}");
    }

    #[test]
    fn a_setting_of_another_type_takes_the_engine_text_or_is_refused() {
        let typed = |type_name: &str, text: Option<&str>| SettingType {
            type_name: Some(type_name.to_string()),
            text: text.map(str::to_string),
            ..SettingType::default()
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
