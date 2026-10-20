//! Running-game inspection, input and screenshots.
//!
//! Process runs speak Godot's remote debugger. Editor Play is reached through the
//! plugin's debugger relay. Method calls require an explicit `permit` list.

use super::{ToolManager, str_arg, string_list};
use crate::error::McpError;
use crate::protocol::{Tool, ToolResult};
use serde_json::{Value, json};
use std::time::Duration;

const DENY_METHODS: &[&str] = &[
    "quit",
    "set_script",
    "call",
    "callv",
    "call_deferred",
    "rpc",
    "rpc_id",
    "free",
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
            | "godot_game_input"
            | "godot_game_input_release"
            | "godot_game_wait"
            | "godot_game_screenshot"
            | "godot_debug_breakpoints"
            | "godot_debug_control"
            | "godot_debug_state"
    )
}

pub fn catalog() -> Vec<Tool> {
    vec![
        tool(
            "godot_run_output",
            "Reads logs, warnings and errors (with stack traces) from a managed process run or the editor's running game.",
            json!({"run_id": s("Optional run id. Default: newest process run, else the editor game."), "since": i("Return entries with seq >= since."), "level": s("all (default), warning, or error.")}),
            &[],
        ),
        tool(
            "godot_game_tree",
            "Lists live nodes in the running game (path, class, position, visible text).",
            json!({"run_id": s(""), "root": s("Default: current scene. \"/root\" includes autoloads."), "depth": i(""), "max_nodes": i("Default 150."), "name": s(""), "class": s(""), "group": s(""), "text": s("")}),
            &[],
        ),
        tool(
            "godot_game_get",
            "Reads a live node's properties, groups and, optionally, signals and methods.",
            json!({"run_id": s(""), "path": s("Node path relative to the current scene, or /root/..."), "properties": {"type": "array", "items": {"type": "string"}}, "signals": b(), "methods": b()}),
            &[],
        ),
        tool(
            "godot_game_set",
            "Sets properties on a live node. Refuses `script` and private names. Nothing is saved to disk.",
            json!({"run_id": s(""), "path": s(""), "properties": {"type": "object"}}),
            &["properties"],
        ),
        tool(
            "godot_game_call",
            "Calls one method on a live node. The method must be listed in permit and is rejected if it can quit, eval, or trampoline (call/rpc/free).",
            json!({"run_id": s(""), "path": s(""), "method": s(""), "args": {"type": "array"}, "permit": {"type": "array", "items": {"type": "string"}, "description": "Methods this call is allowed to invoke. Must include method."}}),
            &["method", "permit"],
        ),
        tool(
            "godot_game_signals",
            "Lists a live node's signals and current connections.",
            json!({"run_id": s(""), "path": s("")}),
            &[],
        ),
        tool(
            "godot_game_observe",
            "Connects to signals for a few frames and returns the ones that fired, then disconnects.",
            json!({"run_id": s(""), "path": s(""), "signals": {"type": "array", "items": {"type": "string"}}, "frames": i("How many frames to watch (default 1, max 600).")}),
            &["signals"],
        ),
        tool(
            "godot_game_snapshot",
            "Compact snapshot of the running game: scene, frame, pause flag, and the current scene's children.",
            json!({"run_id": s("")}),
            &[],
        ),
        tool(
            "godot_game_input",
            "Injects input into the running game: key, text, mouse_move, mouse_button, touch, joy_button, joy_axis, wait. phase is tap (default), press, or release.",
            json!({"run_id": s(""), "steps": {"type": "array", "items": {"type": "object"}}}),
            &["steps"],
        ),
        tool(
            "godot_game_input_release",
            "Releases every key, mouse button, joypad button, axis and touch this server is still holding.",
            json!({"run_id": s("")}),
            &[],
        ),
        tool(
            "godot_game_wait",
            "Waits for process frames in the running game so input and screenshots can observe the result.",
            json!({"run_id": s(""), "frames": i("Default 1.")}),
            &[],
        ),
        tool(
            "godot_game_screenshot",
            "Captures the running game window, a viewport, or a Camera2D/Camera3D as a PNG. Fails instead of returning an empty image. Headless games have nothing to capture.",
            json!({"run_id": s(""), "node": s("Viewport or node whose viewport to capture."), "camera": s("Camera2D or Camera3D to make current for the shot."), "max_dimension": i("Longest side in pixels. Default 768.")}),
            &[],
        ),
        tool(
            "godot_debug_breakpoints",
            "Lists, adds or removes breakpoints on a process run (Godot's core:breakpoint). Editor Play uses the editor's own debugger.",
            json!({"run_id": s(""), "action": {"type": "string", "enum": ["list", "add", "remove", "clear"]}, "breakpoint": s("res://script.gd:LINE")}),
            &[],
        ),
        tool(
            "godot_debug_control",
            "Pause, resume, or step a process run that this server is debugging.",
            json!({"run_id": s(""), "action": {"type": "string", "enum": ["pause", "resume", "step_into", "step_over", "step_out"]}, "wait_seconds": i("How long to wait for the next stop. Default 10.")}),
            &["action"],
        ),
        tool(
            "godot_debug_state",
            "Stack frames and variables of a stopped process run.",
            json!({"run_id": s(""), "frame": i("0 is the innermost frame.")}),
            &[],
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

fn i(description: &str) -> Value {
    if description.is_empty() {
        json!({"type": "integer"})
    } else {
        json!({"type": "integer", "description": description})
    }
}

fn b() -> Value {
    json!({"type": "boolean"})
}

fn tool(name: &str, description: &str, properties: Value, required: &[&str]) -> Tool {
    super::tool(name, description, properties, required)
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
        "godot_game_input" => runtime(tools, &args, "input.run").await,
        "godot_game_input_release" => runtime(tools, &args, "input.release_all").await,
        "godot_game_wait" => runtime(tools, &args, "input.wait").await,
        "godot_game_screenshot" => screenshot(tools, &args).await,
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
    let since = args.get("since").and_then(Value::as_u64).unwrap_or(0);
    let level = str_arg(args, "level").unwrap_or("all");
    match tools.games.logs(str_arg(args, "run_id"), since, level) {
        Ok(value) => ToolResult::text(serde_json::to_string_pretty(&value).unwrap_or_default()),
        Err(_) => runtime(tools, args, "rt.output").await,
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

async fn screenshot(tools: &ToolManager, args: &Value) -> ToolResult {
    let value = match tools
        .runtime_call(
            str_arg(args, "run_id"),
            "game.screenshot",
            params_without_run(args),
        )
        .await
    {
        Ok(value) => value,
        Err(error) => return runtime_error(&error),
    };
    let Some(data) = value.get("data").and_then(Value::as_str) else {
        return ToolResult::error(
            "The game did not return image data",
            &["Start a windowed game and retry godot_game_screenshot"],
        );
    };
    let bytes = match base64_decode(data) {
        Some(bytes) => bytes,
        None => {
            return ToolResult::error(
                "Screenshot payload was not valid base64",
                &["Update the Godot MCP addon runtime"],
            );
        }
    };
    let Some((width, height)) = png_dimensions(&bytes) else {
        return ToolResult::error(
            "Screenshot payload was not a PNG",
            &["The capture failed before a real image was produced"],
        );
    };
    let source = value
        .get("source")
        .and_then(Value::as_str)
        .unwrap_or("viewport");
    ToolResult::image_with_text(
        format!("Game screenshot {width}x{height} ({source})"),
        data,
        "image/png",
    )
}

async fn breakpoints(tools: &ToolManager, args: &Value) -> ToolResult {
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

async fn debug_control(tools: &ToolManager, args: &Value) -> ToolResult {
    let run = match tools.games.debug(str_arg(args, "run_id")) {
        Ok(run) => run,
        Err(error) => return runtime_error(&error),
    };
    let wait = debug_wait(args);
    let debug = run.debug_ctl();
    let action = str_arg(args, "action").unwrap_or("");
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

async fn debug_state(tools: &ToolManager, args: &Value) -> ToolResult {
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

impl ToolManager {
    pub(super) async fn runtime_call(
        &self,
        run_id: Option<&str>,
        method: &str,
        params: Value,
    ) -> Result<Value, McpError> {
        if run_id != Some("editor") {
            match self.games.select(run_id) {
                Ok(Some(run)) => {
                    return run.runtime(method, params, Duration::from_secs(20)).await;
                }
                Ok(None) => {}
                Err(error) => return Err(error),
            }
        }
        editor_runtime(self, method, params).await
    }
}

/// The debugger session can connect before the runtime registers its capture. Never
/// send input or a mutating request until hello confirms that the destination is ready.
async fn ready_editor_session(tools: &ToolManager) -> Result<i64, McpError> {
    let wait = async {
        loop {
            let status = tools.bridge.send_command("game_status", json!({})).await?;
            // The relay orders active sessions newest first. Do not fall back to an
            // older game while a newer game is still starting.
            if let Some(session) = status
                .get("sessions")
                .and_then(Value::as_array)
                .and_then(|sessions| sessions.first())
            {
                if session
                    .get("hello")
                    .and_then(Value::as_str)
                    .is_some_and(|hello| !hello.is_empty())
                {
                    if let Some(id) = session
                        .get("session")
                        .and_then(Value::as_i64)
                        .filter(|id| *id >= 0)
                    {
                        return Ok(id);
                    }
                }
            }
            let state = tools
                .bridge
                .send_command("get_run_status", json!({}))
                .await?;
            if state.get("running").and_then(Value::as_bool) != Some(true) {
                return Err(McpError::Process(
                    "No game started from the editor is running.".into(),
                ));
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    };
    tokio::time::timeout(Duration::from_secs(20), wait).await.map_err(|_| {
        McpError::Process("timed out waiting for the editor game's runtime hello; check the game for startup errors and ensure the matching Godot MCP addon is enabled".into())
    })?
}

async fn editor_runtime(
    tools: &ToolManager,
    method: &str,
    params: Value,
) -> Result<Value, McpError> {
    let mut events = tools.bridge.subscribe_events();
    let session = ready_editor_session(tools).await?;
    let id = NEXT_EDITOR.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let payload = serde_json::to_string(&json!({"id": id, "method": method, "params": params}))?;
    tools
        .bridge
        .send_command(
            "game_send",
            json!({"command": "mcp:request", "data": [payload], "session": session}),
        )
        .await?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        if left.is_zero() {
            return Err(McpError::Process(
                "timed out waiting for the editor's running game".to_string(),
            ));
        }
        let event = match tokio::time::timeout(left, events.recv()).await {
            Ok(Some(event)) => event,
            Ok(None) => {
                return Err(McpError::Process(
                    "the editor bridge closed while waiting for the game".to_string(),
                ));
            }
            Err(_) => {
                return Err(McpError::Process(
                    "timed out waiting for the editor's running game".to_string(),
                ));
            }
        };
        if event.get("event").and_then(Value::as_str) != Some("game.message") {
            continue;
        }
        if event.pointer("/data/session").and_then(Value::as_i64) != Some(session) {
            continue;
        }
        if event.pointer("/data/message").and_then(Value::as_str) != Some("mcp:reply") {
            continue;
        }
        let Some(text) = event.pointer("/data/data/0").and_then(Value::as_str) else {
            continue;
        };
        let parsed: Value = serde_json::from_str(text)?;
        if !crate::godot::json_id_eq(parsed.get("id"), id) {
            continue;
        }
        if let Some(err) = parsed.get("error") {
            return Err(McpError::ToolExecution(err.to_string()));
        }
        return Ok(parsed.get("result").cloned().unwrap_or(Value::Null));
    }
}

static NEXT_EDITOR: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(1);

fn png_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() < 24 || &bytes[..8] != b"\x89PNG\r\n\x1a\n" {
        return None;
    }
    let width = u32::from_be_bytes(bytes[16..20].try_into().ok()?);
    let height = u32::from_be_bytes(bytes[20..24].try_into().ok()?);
    if width == 0 || height == 0 {
        None
    } else {
        Some((width, height))
    }
}

fn base64_decode(text: &str) -> Option<Vec<u8>> {
    const TABLE: &[u8; 128] = &{
        let mut table = [255_u8; 128];
        let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut index = 0;
        while index < alphabet.len() {
            table[alphabet[index] as usize] = index as u8;
            index += 1;
        }
        table
    };
    let mut out = Vec::new();
    let mut buffer = 0_u32;
    let mut bits = 0;
    for byte in text.bytes() {
        if byte == b'=' || byte.is_ascii_whitespace() {
            continue;
        }
        if byte > 127 || TABLE[byte as usize] == 255 {
            return None;
        }
        buffer = (buffer << 6) | u32::from(TABLE[byte as usize]);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buffer >> bits) as u8);
            buffer &= (1 << bits) - 1;
        }
    }
    Some(out)
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

    #[test]
    fn png_magic_is_rejected_when_absent() {
        assert!(base64_decode("aaaa").is_some());
        assert!(base64_decode("@@@").is_none());
        assert!(png_dimensions(b"not a png").is_none());
        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        png.extend_from_slice(&[0; 8]);
        png.extend_from_slice(&32u32.to_be_bytes());
        png.extend_from_slice(&16u32.to_be_bytes());
        assert_eq!(png_dimensions(&png), Some((32, 16)));
        png[16..20].copy_from_slice(&0u32.to_be_bytes());
        assert!(png_dimensions(&png).is_none());
    }
}
