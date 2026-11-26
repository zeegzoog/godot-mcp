//! The versions of the two wire protocols between this server and the addon: the editor bridge
//! (`addons/godot_mcp/bridge_server.gd`) and the game runtime (`runtime/mcp_runtime.gd`).
//!
//! The editor's addon answers `get_version` with its `protocol` and every command it has with
//! the names of its parameters (`commands`). The bridge client reads that once per connection
//! and refuses, before sending, a parameter of a command that the addon does not list, so an
//! older addon cannot report success for a parameter it never read. An addon that answers no
//! `protocol` is protocol 1 and is sent everything, as before.

use serde_json::{Map, Value};
use std::collections::{BTreeSet, HashMap};

/// The bridge protocol of the addon this server embeds (`PROTOCOL` in `bridge_server.gd`).
/// 1 is an addon that answers no `protocol` field and ignores parameters it does not know.
pub const BRIDGE_PROTOCOL: u64 = 2;

/// The runtime protocol of the game runtime this server embeds (`PROTOCOL_VERSION` in
/// `mcp_runtime.gd`), which the game sends in its `mcp:hello`.
pub const RUNTIME_PROTOCOL: u64 = 1;

/// What an addon answers for a command it does not have.
pub const UNKNOWN_COMMAND: &str = "Unknown command type: ";

/// What an addon answers for a parameter of a command it does not have.
pub const UNKNOWN_PARAMETER: &str = "Unknown parameter '";

/// Tool arguments only the server reads; they are never sent to the editor.
pub const HOST_ONLY: &[&str] = &["project_path"];

/// `args` without the keys only the server reads (`HOST_ONLY` and `also`), for a command that
/// forwards a tool's arguments to the editor.
pub fn for_editor(args: &Value, also: &[&str]) -> Value {
    let mut params = args.clone();
    if let Value::Object(map) = &mut params {
        for key in HOST_ONLY.iter().chain(also) {
            map.remove(*key);
        }
    }
    params
}

/// The commands and parameters an editor's addon said it has.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AddonSurface {
    pub protocol: u64,
    commands: HashMap<String, BTreeSet<String>>,
}

impl AddonSurface {
    /// Reads a `get_version` answer; `None` for an addon too old to list its commands.
    pub fn from_version(answer: &Value) -> Option<Self> {
        let protocol = answer.get("protocol").and_then(as_whole)?;
        let listed = answer.get("commands").and_then(Value::as_object)?;
        let commands = listed
            .iter()
            .map(|(command, names)| {
                let names = names
                    .as_array()
                    .map(|names| {
                        names
                            .iter()
                            .filter_map(Value::as_str)
                            .map(str::to_string)
                            .collect()
                    })
                    .unwrap_or_default();
                (command.clone(), names)
            })
            .collect();
        Some(Self { protocol, commands })
    }

    /// Refuses a parameter of a listed command that the addon does not have, with the text the
    /// addon itself would answer. A null value counts as not given. A command the addon does not
    /// list is sent: the addon answers "Unknown command type" for it just as well, and commands
    /// added to a copy of the addon (the editor tests do) are not in its list.
    pub fn check(&self, command: &str, params: &Value) -> Result<(), String> {
        let Some(names) = self.commands.get(command) else {
            return Ok(());
        };
        let empty = Map::new();
        let given = params.as_object().unwrap_or(&empty);
        if let Some((key, _)) = given
            .iter()
            .find(|(key, value)| !value.is_null() && !names.contains(key.as_str()))
        {
            let accepted: Vec<&str> = names.iter().map(String::as_str).collect();
            return Err(format!(
                "{UNKNOWN_PARAMETER}{key}' for command '{command}' (the editor's addon, bridge protocol {}, accepts: {})",
                self.protocol,
                if accepted.is_empty() {
                    "none".to_string()
                } else {
                    accepted.join(", ")
                }
            ));
        }
        Ok(())
    }
}

/// The parameter and the command of an "Unknown parameter" refusal in `text`.
pub fn unknown_parameter_in(text: &str) -> Option<(String, String)> {
    let rest = &text[text.find(UNKNOWN_PARAMETER)? + UNKNOWN_PARAMETER.len()..];
    let (key, rest) = rest.split_once("' for command '")?;
    let (command, _) = rest.split_once('\'')?;
    (!key.is_empty() && !command.is_empty()).then(|| (key.to_string(), command.to_string()))
}

/// Refuses a game whose runtime speaks another runtime protocol than this server's. A hello
/// without a protocol number is accepted: every runtime has sent one, so it is not a runtime of
/// another protocol.
pub fn check_runtime_hello(hello: &Value) -> Result<(), String> {
    match hello.get("protocol").and_then(as_whole) {
        Some(protocol) if protocol != RUNTIME_PROTOCOL => Err(format!(
            "the game's MCP runtime speaks runtime protocol {protocol}, and this server speaks {RUNTIME_PROTOCOL}, so its answers cannot be trusted. A game started with the editor's Play button runs the runtime of the addon in the project, which is of another version than this server"
        )),
        _ => Ok(()),
    }
}

/// A whole number, also when Godot's JSON wrote it as a float (`2.0`).
fn as_whole(value: &Value) -> Option<u64> {
    value.as_u64().or_else(|| {
        value
            .as_f64()
            .filter(|number| *number >= 0.0 && number.fract() == 0.0)
            .map(|number| number as u64)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::ToolManager;
    use serde_json::json;
    use std::collections::BTreeMap;
    use std::path::Path;

    const BRIDGE_SERVER: &str = include_str!("../addons/godot_mcp/bridge_server.gd");
    const RUNTIME: &str = include_str!("../addons/godot_mcp/runtime/mcp_runtime.gd");

    /// `PARAM_TYPES` of `bridge_server.gd`: every command with the names of its parameters.
    fn param_types() -> BTreeMap<String, BTreeSet<String>> {
        let start = BRIDGE_SERVER
            .find("const PARAM_TYPES: Dictionary = {\n")
            .expect("PARAM_TYPES in bridge_server.gd");
        let body = &BRIDGE_SERVER[start..];
        let body = &body[body.find('\n').expect("first line") + 1..];
        let body = &body[..body.find("\n}\n").expect("end of PARAM_TYPES")];
        body.lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| {
                let line = line.trim().trim_end_matches(',');
                let (command, spec) = line.split_once(": {").expect("\"command\": {...}");
                let spec = spec.strip_suffix('}').expect("closing brace");
                let names = spec
                    .split(',')
                    .map(str::trim)
                    .filter(|item| !item.is_empty())
                    .map(|item| {
                        let (name, kind) = item.split_once(':').expect("\"name\": TYPE_X");
                        assert!(kind.trim().starts_with("TYPE_"), "{line}");
                        name.trim().trim_matches('"').to_string()
                    })
                    .collect();
                (command.trim_matches('"').to_string(), names)
            })
            .collect()
    }

    /// The commands `dispatch_command` of `bridge_server.gd` answers.
    fn dispatched() -> BTreeSet<String> {
        let start = BRIDGE_SERVER
            .find("func dispatch_command(")
            .expect("dispatch_command");
        let body = &BRIDGE_SERVER[start..];
        let body = &body[..body.find("\n# ---").expect("end of dispatch_command")];
        body.lines()
            .filter_map(|line| {
                line.strip_prefix("        \"")
                    .and_then(|rest| rest.strip_suffix("\":"))
            })
            .map(str::to_string)
            .collect()
    }

    fn rust_sources(dir: &Path, found: &mut Vec<(String, String)>) {
        for entry in std::fs::read_dir(dir).expect("read src") {
            let path = entry.expect("entry").path();
            if path.is_dir() {
                rust_sources(&path, found);
            } else if path.extension().is_some_and(|ext| ext == "rs")
                && !path.ends_with("bridge_protocol.rs")
            {
                let text = std::fs::read_to_string(&path).expect("read source");
                found.push((path.display().to_string(), text));
            }
        }
    }

    /// The top-level keys of the object literal that `text` starts with (`{ "a": 1, ... }`).
    fn literal_keys(text: &str) -> BTreeSet<String> {
        let mut keys = BTreeSet::new();
        let mut depth = 0usize;
        let mut chars = text.char_indices().peekable();
        while let Some((at, c)) = chars.next() {
            match c {
                '{' | '[' | '(' => depth += 1,
                '}' | ']' | ')' => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                '"' => {
                    let mut end = at + 1;
                    for (index, next) in chars.by_ref() {
                        if next == '"' && !text[..index].ends_with('\\') {
                            end = index;
                            break;
                        }
                    }
                    if depth == 1 && text[end + 1..].trim_start().starts_with(':') {
                        keys.insert(text[at + 1..end].to_string());
                    }
                }
                _ => {}
            }
        }
        keys
    }

    /// Every `send_command("command", json!({...}))` in the server's sources: the command and,
    /// when the parameters are written out there, their keys.
    fn literal_sends() -> Vec<(String, String, Option<BTreeSet<String>>)> {
        let mut sources = Vec::new();
        rust_sources(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
            &mut sources,
        );
        let mut sends = Vec::new();
        for (file, text) in &sources {
            for (at, _) in text.match_indices("send_command(") {
                let rest = text[at + "send_command(".len()..].trim_start();
                let Some(rest) = rest.strip_prefix('"') else {
                    continue;
                };
                let Some((command, rest)) = rest.split_once('"') else {
                    continue;
                };
                let rest = rest.trim_start().trim_start_matches(',').trim_start();
                let keys = rest.strip_prefix("json!(").map(literal_keys);
                sends.push((file.clone(), command.to_string(), keys));
            }
        }
        sends
    }

    /// The tools whose arguments go to the editor as they are, less `HOST_ONLY` and the keys
    /// named here, and the command they go to.
    const FORWARDED: &[(&str, &str, &[&str])] = &[
        ("godot_get_project_info", "get_project_info", &[]),
        ("godot_get_scene_tree", "get_scene_tree", &[]),
        ("godot_get_open_scenes", "get_open_scenes", &[]),
        ("godot_add_node", "add_node", &[]),
        ("godot_remove_node", "remove_node", &[]),
        ("godot_get_node_properties", "get_node_properties", &[]),
        ("godot_set_node_properties", "set_node_properties", &[]),
        ("godot_capture_viewport", "capture_viewport", &[]),
        ("godot_open_scene", "open_scene", &[]),
        ("godot_save_scene", "save_scene", &[]),
        ("godot_resave_resources", "resave_resources", &["force"]),
        ("godot_create_scene", "create_scene", &[]),
        ("godot_attach_script", "attach_script", &[]),
        ("godot_connect_signal", "connect_signal", &[]),
        ("godot_list_signals", "list_signals", &[]),
        ("godot_create_resource", "create_resource", &[]),
        ("godot_set_project_setting", "set_project_setting", &[]),
        ("godot_get_project_settings", "get_project_settings", &[]),
        ("godot_get_input_map", "get_input_map", &[]),
        ("godot_edit_input_action", "edit_input_action", &[]),
        ("godot_delete_file", "delete_files", &[]),
        ("godot_read_script", "read_script", &[]),
        ("godot_edit_script", "edit_script", &[]),
        ("godot_create_shader", "create_shader", &[]),
        ("godot_read_shader", "read_shader", &[]),
        ("godot_edit_shader", "edit_shader", &[]),
    ];

    #[test]
    fn every_command_the_addon_answers_has_its_parameters_listed() {
        let listed: BTreeSet<String> = param_types().into_keys().collect();
        assert_eq!(
            dispatched(),
            listed,
            "dispatch_command and PARAM_TYPES of bridge_server.gd name other commands"
        );
    }

    #[test]
    fn the_server_sends_only_commands_and_parameters_the_addon_lists() {
        let types = param_types();
        let sends = literal_sends();
        assert!(sends.len() > 30, "too few sends found: {}", sends.len());
        for (file, command, keys) in sends {
            let accepted = types
                .get(&command)
                .unwrap_or_else(|| panic!("{file} sends '{command}', which PARAM_TYPES lacks"));
            for key in keys.unwrap_or_default() {
                assert!(
                    accepted.contains(&key),
                    "{file} sends '{key}' with '{command}', which PARAM_TYPES lacks"
                );
            }
        }
    }

    #[test]
    fn forwarded_tool_arguments_are_parameters_of_their_command() {
        let types = param_types();
        let tools = ToolManager::list_tools();
        for (tool, command, also) in FORWARDED {
            let schema = tools
                .iter()
                .find(|listed| listed.name == *tool)
                .unwrap_or_else(|| panic!("no tool {tool}"));
            let accepted = types
                .get(*command)
                .unwrap_or_else(|| panic!("PARAM_TYPES lacks {command}"));
            let properties = schema.input_schema["properties"]
                .as_object()
                .expect("properties");
            for key in properties.keys() {
                if HOST_ONLY.contains(&key.as_str()) || also.contains(&key.as_str()) {
                    continue;
                }
                assert!(
                    accepted.contains(key),
                    "{tool} forwards '{key}' to '{command}', which PARAM_TYPES lacks"
                );
            }
        }
    }

    #[test]
    fn the_protocol_numbers_are_the_addons() {
        assert!(
            BRIDGE_SERVER.contains(&format!("const PROTOCOL: int = {BRIDGE_PROTOCOL}\n")),
            "PROTOCOL in bridge_server.gd differs from BRIDGE_PROTOCOL"
        );
        assert!(
            RUNTIME.contains(&format!("const PROTOCOL_VERSION := {RUNTIME_PROTOCOL}\n")),
            "PROTOCOL_VERSION in mcp_runtime.gd differs from RUNTIME_PROTOCOL"
        );
    }

    #[test]
    fn host_only_arguments_are_not_forwarded() {
        let args = json!({"script_path": "res://a.gd", "project_path": "/p", "force": true});
        assert_eq!(
            for_editor(&args, &["force"]),
            json!({"script_path": "res://a.gd"})
        );
    }

    fn surface() -> AddonSurface {
        AddonSurface::from_version(&json!({
            "version_string": "4.5",
            "protocol": 2.0,
            "commands": {"read_script": ["script_path"], "save_scene": []}
        }))
        .expect("a surface")
    }

    #[test]
    fn an_addon_without_a_protocol_has_no_surface() {
        assert_eq!(
            AddonSurface::from_version(&json!({"version_string": "4.5"})),
            None
        );
        assert_eq!(
            AddonSurface::from_version(&json!({"protocol": 2, "commands": "x"})),
            None
        );
    }

    #[test]
    fn a_listed_call_passes_and_a_null_parameter_counts_as_not_given() {
        let surface = surface();
        assert_eq!(surface.protocol, 2);
        assert_eq!(
            surface.check(
                "read_script",
                &json!({"script_path": "res://a.gd", "other": null})
            ),
            Ok(())
        );
        assert_eq!(surface.check("save_scene", &json!({})), Ok(()));
    }

    #[test]
    fn a_parameter_the_addon_lacks_is_refused_with_the_addons_words() {
        let surface = surface();
        assert_eq!(
            surface.check("__test_undo", &json!({"x": 1})),
            Ok(()),
            "an unlisted command is left to the addon"
        );
        let extra = surface
            .check(
                "read_script",
                &json!({"script_path": "res://a.gd", "line": 3}),
            )
            .expect_err("unknown parameter");
        assert_eq!(
            unknown_parameter_in(&extra),
            Some(("line".to_string(), "read_script".to_string())),
            "{extra}"
        );
        assert!(extra.contains("accepts: script_path"), "{extra}");
    }

    #[test]
    fn the_addons_own_refusal_is_recognised() {
        // The text bridge_server.gd answers (validate_params).
        let text = "Error: Unknown parameter 'reload' for command 'open_scene' (this addon accepts: scene_path). The server is newer";
        assert_eq!(
            unknown_parameter_in(text),
            Some(("reload".to_string(), "open_scene".to_string()))
        );
        assert_eq!(unknown_parameter_in("Node not found"), None);
        assert!(
            BRIDGE_SERVER
                .contains("\"Unknown parameter '%s' for command '%s' (this addon accepts: %s).")
        );
    }

    #[test]
    fn a_runtime_of_another_protocol_is_refused() {
        assert_eq!(check_runtime_hello(&json!({"protocol": 1})), Ok(()));
        assert_eq!(check_runtime_hello(&json!({"protocol": 1.0})), Ok(()));
        assert_eq!(check_runtime_hello(&json!({})), Ok(()));
        let error = check_runtime_hello(&json!({"protocol": 7})).expect_err("mismatch");
        assert!(error.contains("runtime protocol 7"), "{error}");
    }
}
