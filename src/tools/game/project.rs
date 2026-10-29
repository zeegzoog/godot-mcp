//! The project's own states and commands: the two tools over the adapter the game loaded.

use super::{params_without_run, runtime_error};
use crate::error::McpError;
use crate::protocol::ToolResult;
use crate::tools::{ToolManager, str_arg};
use serde_json::{Value, json};

/// The most state names one call may ask for.
const MAX_NAMES: usize = 64;
/// The longest state or command name.
const MAX_NAME_LENGTH: usize = 256;

pub(super) async fn state(tools: &ToolManager, args: &Value) -> ToolResult {
    if let Err(message) = check_names(args.get("names")) {
        return invalid(message, "names");
    }
    forward(tools, args, "project.state").await
}

pub(super) async fn command(tools: &ToolManager, args: &Value) -> ToolResult {
    let Some(name) = str_arg(args, "name") else {
        return invalid("name is required".into(), "name");
    };
    if let Err(message) = check_name(name, "name") {
        return invalid(message, "name");
    }
    forward(tools, args, "project.command").await
}

fn check_names(names: Option<&Value>) -> Result<(), String> {
    let Some(names) = names.and_then(Value::as_array) else {
        return Ok(());
    };
    if names.len() > MAX_NAMES {
        return Err(format!(
            "names holds {} entries; at most {MAX_NAMES} are read in one call",
            names.len()
        ));
    }
    names
        .iter()
        .filter_map(Value::as_str)
        .try_for_each(|name| check_name(name, "names"))
}

fn check_name(name: &str, parameter: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err(format!("{parameter} must not hold an empty name"));
    }
    if name.chars().count() > MAX_NAME_LENGTH {
        return Err(format!(
            "{parameter} holds a name longer than {MAX_NAME_LENGTH} characters"
        ));
    }
    Ok(())
}

fn invalid(message: String, parameter: &str) -> ToolResult {
    ToolResult::error(
        message,
        &[&format!(
            "Fix the parameter '{parameter}'; its shape is in the tool's inputSchema from tools/list"
        )],
    )
}

async fn forward(tools: &ToolManager, args: &Value, method: &str) -> ToolResult {
    let run_id = str_arg(args, "run_id");
    match tools
        .runtime_call(run_id, method, params_without_run(args))
        .await
    {
        Ok(mut value) => {
            if let Some(map) = value.as_object_mut() {
                map.insert("run_id".into(), json!(run_label(tools, run_id)));
            }
            ToolResult::text(serde_json::to_string_pretty(&value).unwrap_or_default())
        }
        Err(error) => project_error(&error),
    }
}

/// The run an answer came from: the managed run that was chosen, else the editor's Play session.
fn run_label(tools: &ToolManager, run_id: Option<&str>) -> String {
    if run_id == Some("editor") {
        return "editor".into();
    }
    tools
        .games
        .select(run_id)
        .ok()
        .flatten()
        .map_or_else(|| "editor".into(), |run| run.id().to_string())
}

/// A failure the game reported about the adapter gets hints for the adapter; any other failure
/// is told as the other running-game tools tell it.
fn project_error(error: &McpError) -> ToolResult {
    let McpError::ToolExecution(message) = error else {
        return runtime_error(error);
    };
    let Ok(report) = serde_json::from_str::<Value>(message) else {
        return runtime_error(error);
    };
    let Some(code) = report.get("code").and_then(Value::as_str) else {
        return runtime_error(error);
    };
    let data = report.get("data").cloned().unwrap_or(Value::Null);
    match code {
        "UNSUPPORTED" => {
            let named = data.get("configured").and_then(Value::as_bool) == Some(true);
            if named {
                ToolResult::error(
                    message.clone(),
                    &[
                        "Fix what the message says about the adapter script, then start the game again: the adapter is loaded once, when the game starts",
                        "godot_get_run_status shows the adapter's status and error for each run",
                    ],
                )
            } else {
                ToolResult::error(
                    message.clone(),
                    &[
                        "Name the adapter script in the project setting godot_mcp/adapter (godot_set_project_setting), then start the game again: the adapter is loaded once, when the game starts",
                        "The script's register(registry) adds states with registry.add_state(name, provider) and commands with registry.add_command(name, handler, help)",
                    ],
                )
            }
        }
        "NOT_FOUND" => {
            let known = data
                .get("commands")
                .and_then(Value::as_array)
                .map(|names| {
                    names
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .unwrap_or_default();
            ToolResult::error(
                message.clone(),
                &[
                    &format!("Registered commands: {known}"),
                    "godot_game_state lists each command with its help in adapter.commands",
                ],
            )
        }
        "BAD_PARAMS" if data.get("sizes").is_some() => ToolResult::error(
            message.clone(),
            &[
                "Ask for fewer names; data.sizes holds the bytes of each state",
                "Have the provider return less: an answer is limited so that one call cannot fill an agent's context",
            ],
        ),
        "BAD_PARAMS" => ToolResult::error(
            message.clone(),
            &[
                "Fix the argument the message names; the command's help is in adapter.commands of godot_game_state",
            ],
        ),
        "GODOT_ERROR" => ToolResult::error(
            message.clone(),
            &[
                "godot_run_output with level error has the stack of the script error",
                "A command or state must not await: the engine ends the function that called it",
                "A command may have changed the game before it failed: read godot_game_state before running it again",
            ],
        ),
        _ => runtime_error(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn failure(code: &str, data: Value) -> McpError {
        McpError::ToolExecution(
            json!({"code": code, "message": "something", "data": data}).to_string(),
        )
    }

    fn hints_of(error: &McpError) -> String {
        let result = project_error(error);
        assert!(result.is_error);
        match &result.content[0] {
            crate::protocol::ToolContent::Text { text } => text.clone(),
            _ => String::new(),
        }
    }

    #[test]
    fn every_failure_class_of_the_adapter_has_its_own_recovery() {
        let none = hints_of(&failure("UNSUPPORTED", json!({"configured": false})));
        assert!(
            none.contains("godot_mcp/adapter") && none.contains("godot_set_project_setting"),
            "{none}"
        );
        let broken = hints_of(&failure(
            "UNSUPPORTED",
            json!({"configured": true, "loaded": false}),
        ));
        assert!(
            broken.contains("godot_get_run_status")
                && !broken.contains("godot_set_project_setting"),
            "{broken}"
        );
        let unknown = hints_of(&failure(
            "NOT_FOUND",
            json!({"commands": ["clear", "grant"]}),
        ));
        assert!(unknown.contains("clear, grant"), "{unknown}");
        let big = hints_of(&failure("BAD_PARAMS", json!({"sizes": {"big": 300000}})));
        assert!(big.contains("fewer names"), "{big}");
        let refused = hints_of(&failure("BAD_PARAMS", Value::Null));
        assert!(refused.contains("adapter.commands"), "{refused}");
        let script = hints_of(&failure("GODOT_ERROR", Value::Null));
        assert!(
            script.contains("godot_run_output") && script.contains("must not await"),
            "{script}"
        );
    }

    #[test]
    fn a_failure_that_is_not_the_adapters_is_told_as_other_game_tools_tell_it() {
        let gone = hints_of(&McpError::Process("the game disconnected".into()));
        assert!(gone.contains("godot_run_project"), "{gone}");
    }

    #[test]
    fn names_are_bounded_before_the_game_is_asked() {
        assert!(check_names(None).is_ok());
        assert!(check_names(Some(&json!([]))).is_ok());
        assert!(check_names(Some(&json!(["level", "balls"]))).is_ok());
        assert!(check_names(Some(&json!([""]))).is_err());
        let many: Vec<String> = (0..=MAX_NAMES).map(|index| index.to_string()).collect();
        assert!(check_names(Some(&json!(many))).is_err());
        assert!(check_name(&"x".repeat(MAX_NAME_LENGTH), "name").is_ok());
        assert!(check_name(&"x".repeat(MAX_NAME_LENGTH + 1), "name").is_err());
    }
}
