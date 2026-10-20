//! Scene, script, signal, resource and project-setting edits.
//!
//! The editor path goes through the bridge and is undoable. With no editor, creating
//! a scene, a resource or a project setting falls back to writing the file.

use super::{ToolManager, str_arg};
use crate::error::McpError;
use crate::paths::resolve_in_project;
use crate::protocol::{Tool, ToolResult};
use serde_json::{Value, json};
use std::io::{ErrorKind, Write};
use std::path::Path;

pub fn handles(name: &str) -> bool {
    matches!(
        name,
        "godot_create_scene"
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
            "godot_create_scene",
            "Creates a .tscn with one root node and opens it in the editor when the bridge is connected. Refuses to replace an existing scene. Without the editor, writes the file directly.",
            json!({"scene_path": s("res:// path ending in .tscn"), "root_type": s("Default Node2D."), "root_name": s("Default Root."), "project_path": s("")}),
            &["scene_path"],
        ),
        tool(
            "godot_attach_script",
            "Attaches a script to a node in the edited scene (undoable). The script must exist in the project. Pass an empty script_path to detach.",
            json!({"node_path": s(""), "script_path": s("res:// path, or empty to detach.")}),
            &["node_path"],
        ),
        tool(
            "godot_connect_signal",
            "Connects a signal inside the edited scene with CONNECT_PERSIST (undoable). The target must already have the method (attach its script first); a connection that exists is refused.",
            json!({"node_path": s(""), "signal_name": s(""), "target_path": s("Default: the same node."), "method": s("")}),
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
            "Sets one project setting and saves it. Uses the live editor when connected. The value must be a string, number or boolean, and of the type the setting already has.",
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

async fn create_scene(tools: &ToolManager, args: &Value) -> ToolResult {
    match tools
        .bridge
        .send_command("create_scene", args.clone())
        .await
    {
        Ok(value) => ToolResult::text(serde_json::to_string_pretty(&value).unwrap_or_default()),
        Err(McpError::BridgeUnavailable(_)) => write_scene(tools, args).await,
        Err(error) => super::bridge_error_result(&error),
    }
}

async fn write_scene(tools: &ToolManager, args: &Value) -> ToolResult {
    let Some(scene_path) = str_arg(args, "scene_path") else {
        return ToolResult::error("Missing scene_path", &[]);
    };
    if !scene_path.ends_with(".tscn") {
        return ToolResult::error("scene_path must end in .tscn", &[]);
    }
    let root_type = str_arg(args, "root_type").unwrap_or("Node2D");
    let root_name = str_arg(args, "root_name").unwrap_or("Root");
    if !crate::paths::is_valid_identifier(root_type)
        || !crate::paths::is_valid_identifier(root_name)
    {
        return ToolResult::error("root_type and root_name must be identifiers", &[]);
    }
    let project = match tools.host_project(args) {
        Ok(path) => path,
        Err(result) => return result,
    };
    let file = match resolve_in_project(&project, scene_path) {
        Ok(file) => file,
        Err(problem) => return ToolResult::error(problem, &[]),
    };
    let body = format!("[gd_scene format=3]\n\n[node name=\"{root_name}\" type=\"{root_type}\"]\n");
    match create_new_file(&file, &body) {
        Ok(()) => {}
        Err(error) if error.kind() == ErrorKind::AlreadyExists => {
            return ToolResult::error(
                format!("Scene already exists: {}", file.display()),
                &["Pick another scene_path, or edit the existing scene with the node tools"],
            );
        }
        Err(_) => return ToolResult::error(format!("Failed to write {}", file.display()), &[]),
    }
    ToolResult::text(format!(
        "Created {scene_path} ({root_type}). The editor was closed, so this was written as text."
    ))
}

async fn create_resource(tools: &ToolManager, args: &Value) -> ToolResult {
    match tools
        .bridge
        .send_command("create_resource", args.clone())
        .await
    {
        Ok(value) => ToolResult::text(serde_json::to_string_pretty(&value).unwrap_or_default()),
        Err(McpError::BridgeUnavailable(_)) => {
            let Some(path) = str_arg(args, "resource_path") else {
                return ToolResult::error("Missing resource_path", &[]);
            };
            let Some(class_name) = str_arg(args, "class_name") else {
                return ToolResult::error("Missing class_name", &[]);
            };
            if !crate::paths::is_valid_identifier(class_name) || !path.ends_with(".tres") {
                return ToolResult::error(
                    "class_name must be an identifier and resource_path must end in .tres",
                    &[],
                );
            }
            let project = match tools.host_project(args) {
                Ok(path) => path,
                Err(result) => return result,
            };
            let file = match resolve_in_project(&project, path) {
                Ok(file) => file,
                Err(problem) => return ToolResult::error(problem, &[]),
            };
            let body = format!("[gd_resource type=\"{class_name}\" format=3]\n");
            match create_new_file(&file, &body) {
                Ok(()) => {}
                Err(error) if error.kind() == ErrorKind::AlreadyExists => {
                    return ToolResult::error(
                        format!("Resource already exists: {}", file.display()),
                        &["Pick another resource_path; an existing resource is never replaced"],
                    );
                }
                Err(_) => {
                    return ToolResult::error(format!("Failed to write {}", file.display()), &[]);
                }
            }
            ToolResult::text(format!(
                "Created {path} ({class_name}) as text. Open the editor to let Godot import it."
            ))
        }
        Err(error) => super::bridge_error_result(&error),
    }
}

async fn editor_or_error(tools: &ToolManager, command: &str, args: &Value) -> ToolResult {
    match tools.bridge.send_command(command, args.clone()).await {
        Ok(value) => ToolResult::text(serde_json::to_string_pretty(&value).unwrap_or_default()),
        Err(error) => super::bridge_error_result(&error),
    }
}

async fn set_setting(tools: &ToolManager, args: &Value) -> ToolResult {
    match tools
        .bridge
        .send_command("set_project_setting", args.clone())
        .await
    {
        Ok(value) => ToolResult::text(serde_json::to_string_pretty(&value).unwrap_or_default()),
        Err(McpError::BridgeUnavailable(_)) => {
            let Some(key) = str_arg(args, "key") else {
                return ToolResult::error("Missing key", &[]);
            };
            let Some(value) = args.get("value") else {
                return ToolResult::error("Missing value", &[]);
            };
            let project = match tools.host_project(args) {
                Ok(path) => path,
                Err(result) => return result,
            };
            match write_setting(&project.join("project.godot"), key, value) {
                Ok(()) => ToolResult::text(format!(
                    "Set {key} in project.godot. Reopen the project in the editor to pick it up."
                )),
                Err(error) => ToolResult::error(error, &["Use a key like application/config/name"]),
            }
        }
        Err(error) => super::bridge_error_result(&error),
    }
}

async fn get_settings(tools: &ToolManager, args: &Value) -> ToolResult {
    match tools
        .bridge
        .send_command("get_project_settings", args.clone())
        .await
    {
        Ok(value) => ToolResult::text(serde_json::to_string_pretty(&value).unwrap_or_default()),
        Err(McpError::BridgeUnavailable(_)) => {
            let project = match tools.host_project(args) {
                Ok(path) => path,
                Err(result) => return result,
            };
            let text = std::fs::read_to_string(project.join("project.godot")).unwrap_or_default();
            let prefix = str_arg(args, "prefix").unwrap_or("");
            ToolResult::text(
                json!({"source": "project.godot", "settings": read_settings(&text, prefix)})
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
        && !key.starts_with('/')
        && !key.ends_with('/')
        && !key.contains("//")
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

fn write_setting(path: &Path, key: &str, value: &Value) -> Result<(), String> {
    if !is_setting_key(key) {
        return Err(
            "key must start with a section and use only letters, digits and _ / . -, like application/config/name"
                .to_string(),
        );
    }
    let Some((section, name)) = key.split_once('/') else {
        return Err(format!(
            "'{key}' must contain a section, like application/config/name"
        ));
    };
    let original = std::fs::read_to_string(path).unwrap_or_default();
    let mut lines: Vec<String> = original.lines().map(str::to_string).collect();
    let header = format!("[{section}]");
    let prefix = format!("{name}=");
    let range = lines
        .iter()
        .position(|line| line.trim() == header)
        .map(|start| {
            let end = lines
                .iter()
                .enumerate()
                .skip(start + 1)
                .find(|(_, line)| line.trim().starts_with('['))
                .map_or(lines.len(), |(index, _)| index);
            (start, end)
        });
    let existing = range.and_then(|(start, end)| {
        lines[start..end]
            .iter()
            .position(|line| line.trim_start().starts_with(&prefix))
            .map(|offset| start + offset)
    });
    let stored =
        existing.map(|index| StoredKind::of(lines[index].trim_start().trim_start_matches(&prefix)));
    let assignment = format!("{prefix}{}", render_setting(value, stored)?);
    match (existing, range) {
        (Some(index), _) => lines[index] = assignment,
        (None, Some((_, end))) => lines.insert(end, assignment),
        (None, None) => {
            if !lines.is_empty() && !lines.last().is_some_and(|line| line.is_empty()) {
                lines.push(String::new());
            }
            lines.push(header);
            lines.push(String::new());
            lines.push(assignment);
        }
    }
    std::fs::write(path, lines.join("\n") + "\n").map_err(|err| err.to_string())
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
            settings.insert(key, Value::String(value.to_string()));
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
            "", "name", "/a", "a/", "a//b", "a/b c", "a/b]", "a/b\n[x", "a/b=c",
        ] {
            assert!(!is_setting_key(bad), "{bad:?}");
        }
    }

    #[test]
    fn project_setting_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("project.godot");
        std::fs::write(
            &path,
            "config_version=5\n\n[application]\n\nconfig/name=\"Old\"\n",
        )
        .unwrap();
        write_setting(&path, "application/config/name", &json!("New")).unwrap();
        write_setting(&path, "display/window/size/viewport_width", &json!(640)).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("config/name=\"New\""), "{text}");
        assert!(text.contains("[display]"), "{text}");
        assert!(text.contains("window/size/viewport_width=640"), "{text}");
        let settings = read_settings(&text, "application/");
        assert_eq!(settings["application/config/name"], "\"New\"");
    }
}
