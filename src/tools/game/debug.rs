//! The debugger tools: breakpoints, control and state of a managed run.

use super::{runtime_error, text_result};
use crate::error::McpError;
use crate::protocol::ToolResult;
use crate::tools::{ToolManager, str_arg};
use serde_json::{Value, json};
use std::time::Duration;

pub(super) async fn breakpoints(tools: &ToolManager, args: &Value) -> ToolResult {
    if matches!(str_arg(args, "action"), Some("add" | "remove")) {
        if let Some(spec) = str_arg(args, "breakpoint") {
            if let Err(result) = check_breakpoint(spec) {
                return result;
            }
        }
    }
    let run = match tools.games.debug(str_arg(args, "run_id")) {
        Ok(run) => run,
        Err(error) => return runtime_error(&error),
    };
    let action = str_arg(args, "action").unwrap_or("list");
    let debug = run.debug_ctl();
    let result = match action {
        "list" => Ok(json!({"breakpoints": debug.list_breakpoints()})),
        "clear" => {
            for spec in debug.list_breakpoints() {
                if let Some((file, line)) = crate::godot::split_breakpoint(&spec) {
                    if let Err(error) = debug.set_breakpoint(&file, line, false) {
                        return runtime_error(&error);
                    }
                }
            }
            Ok(json!({"breakpoints": []}))
        }
        "add" | "remove" => {
            let Some(spec) = str_arg(args, "breakpoint") else {
                return ToolResult::error("breakpoint is required", &["Use res://script.gd:LINE"]);
            };
            let Some((file, line)) = crate::godot::split_breakpoint(spec) else {
                return ToolResult::error(
                    format!("'{spec}' is not a breakpoint"),
                    &["Use res://script.gd:LINE"],
                );
            };
            if let Err(error) = debug.set_breakpoint(&file, line, action == "add") {
                return runtime_error(&error);
            }
            Ok(json!({"breakpoints": debug.list_breakpoints()}))
        }
        other => {
            return ToolResult::error(
                format!("Unknown breakpoint action '{other}'"),
                &["Use list, add, remove, or clear"],
            );
        }
    };
    text_result(result)
}

/// A breakpoint location is `res://path/to/script.gd:LINE`: a project file, no parent segments,
/// a line from 1 (TL-5).
pub(in crate::tools) fn check_breakpoint(spec: &str) -> Result<(), ToolResult> {
    let refused = |problem: String| {
        Err(ToolResult::error(
            problem,
            &["Use a location such as res://scripts/player.gd:12"],
        ))
    };
    let Some((file, _line)) = crate::godot::split_breakpoint(spec) else {
        return refused(format!(
            "'{spec}' is not a breakpoint: expected res://script.gd:LINE with a line of at least 1"
        ));
    };
    if !file.starts_with("res://") || file.split(['/', '\\']).any(|part| part == "..") {
        return refused(format!(
            "'{spec}' is not inside the project: a breakpoint file is a res:// path without parent segments"
        ));
    }
    Ok(())
}

pub(super) async fn debug_control(tools: &ToolManager, args: &Value) -> ToolResult {
    let run = match tools.games.debug(str_arg(args, "run_id")) {
        Ok(run) => run,
        Err(error) => return runtime_error(&error),
    };
    let wait = debug_wait(args);
    let debug = run.debug_ctl();
    let action = str_arg(args, "action").unwrap_or("");
    if action == "pause" && run.is_held() {
        return ToolResult::error(
            "The game is held, so it is already still",
            &[
                "Step it with godot_game_physics (action step)",
                "Resume it with godot_game_physics (action resume)",
            ],
        );
    }
    let result = match action {
        "pause" => debug.pause(wait).await.map(stop_json),
        "resume" => debug.resume().await.map(|()| json!({"resumed": true})),
        "step_into" => debug.step("into", wait).await.map(stop_json),
        "step_over" => debug.step("over", wait).await.map(stop_json),
        "step_out" => debug.step("out", wait).await.map(stop_json),
        other => Err(McpError::Validation(format!(
            "Unknown debug action '{other}'"
        ))),
    };
    text_result(result)
}

pub(super) async fn debug_state(tools: &ToolManager, args: &Value) -> ToolResult {
    let run = match tools.games.debug(str_arg(args, "run_id")) {
        Ok(run) => run,
        Err(error) => return runtime_error(&error),
    };
    let debug = run.debug_ctl();
    let Some(stop) = debug.stop() else {
        return ToolResult::text(
            json!({"stopped": false, "breakpoints": debug.list_breakpoints()}).to_string(),
        );
    };
    let frame = args.get("frame").and_then(Value::as_i64).unwrap_or(0);
    // A stack or variables that could not be read are reported, not shown as empty.
    let (stack, stack_error) = match debug.stack().await {
        Ok(stack) => (stack, None),
        Err(error) => (Vec::new(), Some(error.to_string())),
    };
    let (variables, variables_error) = match debug.variables(frame).await {
        Ok(variables) => (variables, None),
        Err(error) => (Vec::new(), Some(error.to_string())),
    };
    let mut state = json!({
        "stopped": true,
        "stop": stop_json(stop),
        "stack": stack,
        "frame": frame,
        "variables": variables,
        "breakpoints": debug.list_breakpoints(),
    });
    if let Some(error) = stack_error {
        state["stack_error"] = json!(error);
    }
    if let Some(error) = variables_error {
        state["variables_error"] = json!(error);
    }
    ToolResult::text(serde_json::to_string_pretty(&state).unwrap_or_default())
}

/// Longest a debug call waits for the game to stop.
const MAX_WAIT_SECONDS: u64 = 300;

/// The wait a caller asked for in `wait_seconds`, ten seconds by default. It is bounded:
/// a deadline built from an unbounded duration overflows and panics inside the tool call.
fn debug_wait(args: &Value) -> Duration {
    let seconds = args
        .get("wait_seconds")
        .and_then(Value::as_u64)
        .unwrap_or(10);
    Duration::from_secs(seconds.min(MAX_WAIT_SECONDS))
}

fn stop_json(stop: crate::godot::StopInfo) -> Value {
    json!({
        "reason": stop.reason,
        "error": stop.error,
        "can_step": stop.can_step,
        "has_stack": stop.has_stack,
        "thread": stop.thread,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wait_the_caller_names_is_bounded() {
        assert_eq!(debug_wait(&json!({})), Duration::from_secs(10));
        assert_eq!(
            debug_wait(&json!({"wait_seconds": 3})),
            Duration::from_secs(3)
        );
        // Adding an unbounded duration to an Instant panics inside the tool call.
        let longest = debug_wait(&json!({"wait_seconds": u64::MAX}));
        assert_eq!(longest, Duration::from_secs(MAX_WAIT_SECONDS));
        let _ = tokio::time::Instant::now() + longest;
    }
}
