//! Running-game inspection, input and screenshots.
//!
//! Process runs speak Godot's remote debugger. Editor Play is reached through the
//! plugin's debugger relay. Method calls require an explicit `permit` list.
//!
//! This file routes the tools and holds the log and method-call tools. The child modules hold the
//! catalog, input steps, screenshots, recordings, the debugger tools and the way a call reaches the
//! game (a managed run or the editor's Play session).

use super::{ToolManager, str_arg, string_list};

mod catalog;
mod debug;
mod input;
mod project;
mod record;
mod screenshot;
mod session;

use crate::error::McpError;
use crate::protocol::ToolResult;
pub use catalog::catalog;
pub(in crate::tools) use debug::check_breakpoint;
use debug::{breakpoints, debug_control, debug_state};
use input::game_input;
use record::record;
use screenshot::screenshot;
use serde_json::{Value, json};

const DENY_METHODS: &[&str] = &[
    "quit",
    "set_script",
    "call",
    "callv",
    "call_deferred",
    "call_group",
    "call_group_flags",
    "propagate_call",
    "rpc",
    "rpc_id",
    "set",
    "set_deferred",
    "set_indexed",
    "emit_signal",
    "notification",
    "propagate_notification",
    "free",
    "queue_free",
    "replace_by",
];

pub fn handles(name: &str) -> bool {
    matches!(
        name,
        "godot_run_output"
            | "godot_game_tree"
            | "godot_game_get"
            | "godot_game_set"
            | "godot_game_call"
            | "godot_game_signals"
            | "godot_game_observe"
            | "godot_game_snapshot"
            | "godot_game_state"
            | "godot_game_command"
            | "godot_game_input"
            | "godot_game_input_release"
            | "godot_game_wait"
            | "godot_game_physics"
            | "godot_game_scenario"
            | "godot_game_screenshot"
            | "godot_game_record"
            | "godot_debug_breakpoints"
            | "godot_debug_control"
            | "godot_debug_state"
    )
}

pub async fn call(tools: &ToolManager, name: &str, args: Value) -> ToolResult {
    match name {
        "godot_run_output" => output(tools, &args).await,
        "godot_game_tree" => runtime(tools, &args, "game.tree").await,
        "godot_game_get" => runtime(tools, &args, "game.get").await,
        "godot_game_set" => runtime(tools, &args, "game.set").await,
        "godot_game_call" => game_call(tools, &args).await,
        "godot_game_signals" => runtime(tools, &args, "game.signals").await,
        "godot_game_observe" => runtime(tools, &args, "game.observe").await,
        "godot_game_snapshot" => runtime(tools, &args, "game.snapshot").await,
        "godot_game_state" => project::state(tools, &args).await,
        "godot_game_command" => project::command(tools, &args).await,
        "godot_game_input" => game_input(tools, &args).await,
        "godot_game_input_release" => runtime(tools, &args, "input.release_all").await,
        "godot_game_wait" => runtime(tools, &args, "input.wait").await,
        "godot_game_physics" => runtime(tools, &args, "game.physics").await,
        "godot_game_scenario" => crate::tools::scenario::call(tools, &args).await,
        "godot_game_screenshot" => screenshot(tools, &args).await,
        "godot_game_record" => record(tools, &args).await,
        "godot_debug_breakpoints" => breakpoints(tools, &args).await,
        "godot_debug_control" => debug_control(tools, &args).await,
        "godot_debug_state" => debug_state(tools, &args).await,
        _ => ToolResult::error(format!("Tool '{name}' has no handler"), &[]),
    }
}

fn params_without_run(args: &Value) -> Value {
    let mut copy = args.clone();
    if let Value::Object(map) = &mut copy {
        map.remove("run_id");
        map.remove("project_path");
    }
    copy
}

async fn runtime(tools: &ToolManager, args: &Value, method: &str) -> ToolResult {
    match tools
        .runtime_call(str_arg(args, "run_id"), method, params_without_run(args))
        .await
    {
        Ok(value) => ToolResult::text(serde_json::to_string_pretty(&value).unwrap_or_default()),
        Err(error) => runtime_error(&error),
    }
}

async fn output(tools: &ToolManager, args: &Value) -> ToolResult {
    // The in-game logger copies the scene stamp when the line is written. Prefer it so an
    // old error keeps the script hash from that moment, not the file now on disk.
    let runtime_failure = match tools
        .runtime_call(
            str_arg(args, "run_id"),
            "rt.output",
            params_without_run(args),
        )
        .await
    {
        Ok(mut value) => {
            // Keep the process identity the debugger buffer already reported. The lines
            // themselves stay the logger's, each stamped when it was written.
            if let Ok(buffered) =
                tools
                    .games
                    .logs(str_arg(args, "run_id"), since_of(args), level_of(args))
            {
                if let Some(run_id) = buffered.get("run_id") {
                    value["run_id"] = run_id.clone();
                }
                if let Some(running) = buffered.get("running") {
                    value["running"] = running.clone();
                }
            }
            return ToolResult::text(serde_json::to_string_pretty(&value).unwrap_or_default());
        }
        Err(error) => error,
    };
    match tools
        .games
        .logs(str_arg(args, "run_id"), since_of(args), level_of(args))
    {
        Ok(mut value) => {
            if let Some(lines) = value.get_mut("lines").and_then(Value::as_array_mut) {
                for line in lines.iter_mut() {
                    if let Some(entry) = line.as_object_mut() {
                        entry.insert(
                            "scene".into(),
                            json!({
                                "script_sha256": null,
                                "script_sha256_basis": "unavailable",
                            }),
                        );
                    }
                }
                if !lines.is_empty() {
                    value["script_sha256_note"] = json!(
                        "These lines were kept by the debugger buffer without a scene stamp from the moment they were written. script_sha256 is not the script currently on disk."
                    );
                }
            }
            ToolResult::text(serde_json::to_string_pretty(&value).unwrap_or_default())
        }
        // Asking what a run printed when none runs and no editor is connected is not a failure
        // of the question (TL-11).
        Err(_) if matches!(runtime_failure, McpError::BridgeUnavailable(_)) => {
            if str_arg(args, "run_id").is_none() {
                return ToolResult::text(
                    json!({
                        "running": false,
                        "lines": [],
                        "message": "Nothing is running, so there is no output. Start a game with godot_run_project.",
                    })
                    .to_string(),
                );
            }
            runtime_error(&runtime_failure)
        }
        Err(_) => runtime_error(&runtime_failure),
    }
}

async fn game_call(tools: &ToolManager, args: &Value) -> ToolResult {
    let Some(method) = str_arg(args, "method") else {
        return ToolResult::error("Missing method", &["Pass method and permit"]);
    };
    if !crate::paths::is_valid_identifier(method)
        || DENY_METHODS.contains(&method)
        || method.starts_with('_')
    {
        return ToolResult::error(
            format!("Method '{method}' is not allowed"),
            &["Call a normal gameplay method and list it in permit"],
        );
    }
    let permit = string_list(args.get("permit"));
    if !permit.iter().any(|item| item == method) {
        return ToolResult::error(
            format!("Method '{method}' is not in permit"),
            &["Pass permit: [\"the_method\"] for this call only"],
        );
    }
    runtime(tools, args, "game.call").await
}

fn since_of(args: &Value) -> u64 {
    args.get("since").and_then(Value::as_u64).unwrap_or(0)
}

fn level_of(args: &Value) -> &str {
    str_arg(args, "level").unwrap_or("all")
}

fn text_result(result: Result<Value, McpError>) -> ToolResult {
    match result {
        Ok(value) => ToolResult::text(
            serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string()),
        ),
        Err(error) => runtime_error(&error),
    }
}

fn runtime_error(error: &McpError) -> ToolResult {
    match error {
        McpError::BridgeUnavailable(_) | McpError::Bridge(_) | McpError::ToolExecution(_) => {
            super::bridge_error_result(error)
        }
        other => ToolResult::error(
            other.to_string(),
            &[
                "Start the game with godot_run_project",
                "For breakpoints, the server must launch the process (editor Play uses Godot's own debugger)",
            ],
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The names `rt_live.gd` refuses, read from its source.
    fn runtime_deny_list() -> Vec<String> {
        let source = include_str!("../../addons/godot_mcp/runtime/ops/rt_live.gd");
        let start = source
            .find("const DENY: Array[String] = [")
            .expect("DENY list");
        let list = &source[start..];
        let list = &list[list.find("= [").expect("list start") + 3..];
        let list = &list[..list.find(']').expect("list end")];
        let mut names: Vec<String> = list
            .split(',')
            .map(|item| item.trim().trim_matches('"').to_string())
            .filter(|item| !item.is_empty())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn the_server_and_the_runtime_refuse_the_same_methods_and_the_ones_the_concept_names() {
        let mut server: Vec<String> = DENY_METHODS.iter().map(|name| name.to_string()).collect();
        server.sort();
        assert_eq!(server, runtime_deny_list(), "the two refusal lists differ");
        // RO-6: quit, evaluate, call by name (directly, on a group, through a signal or a
        // notification), set by name, free or replace a node.
        for name in [
            "quit",
            "set_script",
            "call",
            "callv",
            "call_deferred",
            "call_group",
            "call_group_flags",
            "propagate_call",
            "rpc",
            "rpc_id",
            "set",
            "set_deferred",
            "set_indexed",
            "emit_signal",
            "notification",
            "propagate_notification",
            "free",
            "queue_free",
            "replace_by",
        ] {
            assert!(
                server.iter().any(|item| item == name),
                "{name} is not refused"
            );
        }
        // Changing the tree around a node stays allowed (RO-1: only the running game changes).
        for name in ["add_child", "remove_child", "reparent"] {
            assert!(!server.iter().any(|item| item == name), "{name} is refused");
        }
    }
}
