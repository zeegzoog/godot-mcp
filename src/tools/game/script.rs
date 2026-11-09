//! Running one function of a script file inside the running game, so a heavy scene stays loaded
//! between test scripts. The game compiles the file and runs it (`rt_script.gd`).

use super::project::run_label;
use super::{params_without_run, runtime_error};
use crate::error::McpError;
use crate::protocol::ToolResult;
use crate::tools::{ToolManager, str_arg};
use serde_json::{Value, json};

pub(super) const DEFAULT_TIMEOUT_SECONDS: u64 = 60;
pub(super) const MAX_TIMEOUT_SECONDS: u64 = 600;

pub(super) async fn run_script(tools: &ToolManager, args: &Value) -> ToolResult {
    if let Err(result) = check(args) {
        return result;
    }
    let run_id = str_arg(args, "run_id");
    match tools
        .runtime_call(run_id, "game.run_script", params_without_run(args))
        .await
    {
        Ok(mut value) => {
            if let Some(map) = value.as_object_mut() {
                map.insert("run_id".into(), json!(run_label(tools, run_id)));
            }
            mark_timeout(&mut value);
            answer(&value)
        }
        Err(error) => script_error(&error),
    }
}

/// The game itself answered this timeout, so what it ended is the function and not the run.
fn mark_timeout(value: &mut Value) {
    if value.get("failure").and_then(Value::as_str) != Some("timeout") {
        return;
    }
    let message = value.get("message").and_then(Value::as_str).unwrap_or("");
    let message = format!("{message} The run was not stopped, and the game is still running.");
    if let Some(map) = value.as_object_mut() {
        map.insert("message".into(), json!(message));
        map.insert("timeout_ended".into(), json!("function"));
        map.insert("game_running".into(), json!(true));
    }
}

/// A function that ran and failed still has its output and the game's time to report, so its
/// answer comes back whole, as an error with what to do next.
fn answer(value: &Value) -> ToolResult {
    let text = serde_json::to_string_pretty(value).unwrap_or_default();
    if value.get("ok").and_then(Value::as_bool) != Some(false) {
        return ToolResult::text(text);
    }
    let hints: &[&str] = match value.get("failure").and_then(Value::as_str) {
        Some("script_error") => &[
            "Fix the script and call again: the scene stays loaded, and the file is read again",
            "The game may have changed before the error: read it, or restart it with godot_run_project",
        ],
        Some("timeout") => &[
            "Raise timeout_seconds, up to 600, if the function needs longer",
            "Check what it awaits: a held game advances while the function waits, and a signal that never fires ends in this timeout",
        ],
        Some("value_too_large") => {
            &["Return less, or print what you need: the output comes back with the answer"]
        }
        _ => &["Do not free the script's own node inside the function; call again"],
    };
    ToolResult::error(text, hints)
}

/// The checks that need no game. The game checks the same parameters again.
fn check(args: &Value) -> Result<(), ToolResult> {
    let Some(script) = str_arg(args, "script") else {
        return Err(ToolResult::error(
            "script is required",
            &["Pass the res:// path of a .gd file, such as res://tests/check.gd"],
        ));
    };
    if !script.starts_with("res://") || !script.ends_with(".gd") || script.contains("..") {
        return Err(ToolResult::error(
            format!("script must be the res:// path of a .gd file of the project, not {script}"),
            &["Pass a path such as res://tests/check.gd"],
        ));
    }
    if let Some(function) = args.get("function") {
        let valid = function
            .as_str()
            .is_some_and(crate::paths::is_valid_identifier);
        if !valid {
            return Err(ToolResult::error(
                format!("function must be the name of a function, not {function}"),
                &["Pass the name alone, such as run; it defaults to run"],
            ));
        }
    }
    if let Some(timeout) = args.get("timeout_seconds") {
        let valid = timeout
            .as_u64()
            .is_some_and(|seconds| (1..=MAX_TIMEOUT_SECONDS).contains(&seconds));
        if !valid {
            return Err(ToolResult::error(
                format!(
                    "timeout_seconds must be a whole number from 1 to {MAX_TIMEOUT_SECONDS}, not {timeout}"
                ),
                &["Leave it out for 60 seconds"],
            ));
        }
    }
    Ok(())
}

/// A refusal by the game says that nothing ran; any other failure is told as the other
/// running-game tools tell it.
fn script_error(error: &McpError) -> ToolResult {
    let McpError::ToolExecution(message) = error else {
        return runtime_error(error);
    };
    let code = serde_json::from_str::<Value>(message)
        .ok()
        .and_then(|report| report.get("code").and_then(Value::as_str).map(String::from));
    match code.as_deref() {
        Some("NOT_FOUND") => ToolResult::error(
            message.clone(),
            &[
                "Check the script's path and the function's name: data.functions lists the functions it has",
                "A file written after the game started is found: the game reads it from disk on each call",
            ],
        ),
        Some("BAD_PARAMS") => ToolResult::error(
            message.clone(),
            &[
                "Fix what the message says and call again: nothing ran, and the scene stays loaded",
                "godot_validate_script checks a script without running it",
            ],
        ),
        _ => runtime_error(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::ToolContent;

    fn refused(args: Value) -> String {
        match check(&args) {
            Ok(()) => panic!("{args} was accepted"),
            Err(result) => serde_json::to_string(&result).unwrap_or_default(),
        }
    }

    fn text_of(result: &ToolResult) -> String {
        match &result.content[0] {
            ToolContent::Text { text } => text.clone(),
            _ => String::new(),
        }
    }

    #[test]
    fn a_call_names_a_script_of_the_project_and_a_function() {
        assert!(refused(json!({})).contains("script is required"));
        assert!(refused(json!({"script": "/tmp/check.gd"})).contains("res://"));
        assert!(refused(json!({"script": "res://check.txt"})).contains("res://"));
        assert!(refused(json!({"script": "res://../check.gd"})).contains("res://"));
        let bad_function = json!({"script": "res://check.gd", "function": "run()"});
        assert!(refused(bad_function).contains("function"));
        assert!(check(&json!({"script": "res://tests/check.gd"})).is_ok());
        assert!(check(&json!({"script": "res://check.gd", "function": "_setup"})).is_ok());
    }

    #[test]
    fn the_timeout_is_bounded_before_the_game_is_asked() {
        for bad in [json!(0), json!(601), json!(1.5), json!("60")] {
            let args = json!({"script": "res://check.gd", "timeout_seconds": bad});
            assert!(refused(args).contains("timeout_seconds"));
        }
        let args = json!({"script": "res://check.gd", "timeout_seconds": 600});
        assert!(check(&args).is_ok());
    }

    #[test]
    fn a_function_that_failed_comes_back_whole_with_what_to_do_next() {
        let failed = answer(&json!({"ok": false, "failure": "script_error", "output": ["before"]}));
        assert!(failed.is_error);
        let text = text_of(&failed);
        assert!(text.contains("before"), "{text}");
        assert!(text.contains("stays loaded"), "{text}");
        let mut timed_out = json!({"ok": false, "failure": "timeout", "message": "Ended."});
        mark_timeout(&mut timed_out);
        assert_eq!(timed_out["timeout_ended"], "function");
        assert_eq!(timed_out["game_running"], true);
        let said = timed_out["message"].as_str().unwrap_or("");
        assert!(said.contains("still running"), "{said}");
        let late = answer(&timed_out);
        assert!(text_of(&late).contains("timeout_seconds"));
        let mut other = json!({"ok": false, "failure": "script_error"});
        mark_timeout(&mut other);
        assert!(other.get("timeout_ended").is_none());
        let done = answer(&json!({"ok": true, "value": 3}));
        assert!(!done.is_error);
    }

    #[test]
    fn a_refusal_says_nothing_ran() {
        let refusal = |code: &str| {
            McpError::ToolExecution(json!({"code": code, "message": "something"}).to_string())
        };
        assert!(text_of(&script_error(&refusal("BAD_PARAMS"))).contains("nothing ran"));
        assert!(text_of(&script_error(&refusal("NOT_FOUND"))).contains("data.functions"));
        let gone = script_error(&McpError::Process("the game disconnected".into()));
        assert!(text_of(&gone).contains("godot_run_project"));
    }
}
