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
            | "godot_game_physics"
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
            "Compact snapshot of the running game: scene path, monotonic scene_generation, scene_instance_id, frame, pause flag, children, and the loaded root script's path and SHA-256. The hash is the source Godot had loaded when this scene instance became current, not a later read of the file on disk. Reloading the same scene path increments the generation and changes the instance id.",
            json!({"run_id": s("")}),
            &[],
        ),
        tool(
            "godot_game_input",
            "Injects input into the running game. Steps: key, text, mouse_move, mouse_button, touch, joy_button, joy_axis, wait, click, click_node, swipe, time_scale. phase is tap (default), press, or release. click is a mouse tap at x, y. click_node taps the center of a Control, Node2D, or Node3D. swipe presses at x, y and releases at to_x, to_y across frames (default 8, pointer mouse or touch). Mouse steps are Godot input events and do not move the OS cursor unless warp_mouse is true. time_scale sets Engine.time_scale (0 through 8) until another call changes it. A failed step releases every button and touch this server is still holding.",
            json!({"run_id": s(""), "steps": {"type": "array", "items": {"type": "object"}}, "warp_mouse": {"type": "boolean", "description": "Move the real OS cursor to each mouse event. Default false."}}),
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
            "Waits in the running game. frames waits process frames (default 1). node waits until that node exists. signal waits until that signal fires on node (timeout frames, default 180; not combined with a property or scene_generation). property reads a public node property, including nested Dictionary keys or Object properties separated by /, using get only: no expressions and no method calls. op is eq, ne, lt, lte, gt, gte, or exists. eq and ne require the same type (a JSON number that is a whole number may match an int; a fraction against an int is a type error; strings are never coerced). Ordering is int-with-int or float-with-float, and float equality is exact. Private names, script, and anything inside an Array are refused. scene_generation waits until the running generation is greater than the given one, and can be combined with node and property. The result and a timeout include the observed value and the scene context. time_scale sets Engine.time_scale from 0 through 8 and keeps it until changed again; the runtime restores the previous scale when the game exits.",
            json!({"run_id": s(""), "frames": i("Default 1. Ignored when node, signal, property, or scene_generation is set, except as the default timeout."), "node": s("Wait until this node exists. Required when property is set."), "signal": s("Wait until this signal fires on node."), "property": s("Public property path. Use / for a Dictionary key or an Object property."), "op": s("eq, ne, lt, lte, gt, gte, or exists."), "value": {"description": "Value to compare. Same type as the property. No string coercion. A whole JSON number may match an int."}, "scene_generation": i("Wait until the running scene generation is greater than this."), "timeout": i("Frame limit for node, signal, property, or scene_generation. Default 180, max 600."), "time_scale": n("Engine.time_scale, from 0 through 8.")}),
            &[],
        ),
        tool(
            "godot_game_physics",
            "Steps the running game by N real engine physics iterations (1 to 120), then leaves the tree paused. resume clears that pause and continues the game. Each tick has the normal physics delta, so N ticks are N / physics_ticks_per_second seconds of game time (scaled by time_scale). The step sets max_physics_steps_per_frame to 1 while it runs, then restores it; the tick rate and the time scale are not changed. It does not call node _physics_process. Idle _process is not stepped. Nodes with process_mode ALWAYS still run while paused, and the engine physics-frame counter keeps increasing while paused, so it is not a count of pausable game physics. Custom threads and GPU compute are not flushed. This does not make the game deterministic. On failure or when the runtime exits mid-step, the saved timing settings are restored and the tree is paused.",
            json!({"run_id": s(""), "action": s("step (default), resume, or status."), "ticks": i("Physics iterations for step. Integer from 1 to 120.")}),
            &[],
        ),
        tool(
            "godot_game_screenshot",
            "Captures the running game window, a viewport, or a Camera2D/Camera3D as one PNG. frames (1 to 8) stitches shots taken every process frames into a single sheet. The text says how many pixels changed since the previous capture of this run, and where (a sheet compares its last frame, and only against a capture of the same size). save_baseline stores the returned image under a name (at most 8, each at most 512×512 pixels). baseline compares the returned image with a saved one. roi is x, y, w, h in pixels of that returned image (roi_space returned_image), including a sheet. tolerance (0 to 255) is the allowed per-channel difference; a larger difference counts as changed. The text reports changed, sampled, ratio, bounds, tolerance, the baseline name, and whether the pixel count is exact or sampled. Fails instead of returning an empty image. Headless games have nothing to capture. A missing baseline, a bad name, or an roi outside the image is an error and does not replace a saved image.",
            json!({"run_id": s(""), "node": s("Viewport or node whose viewport to capture."), "camera": s("Camera2D or Camera3D to make current for the shot."), "max_dimension": i("Longest side of the returned image in pixels. Default 768."), "frames": i("How many frames to stitch. Default 1, max 8."), "every": i("Process frames between shots. Default 1, max 60."), "baseline": s("Name of a saved image to compare."), "save_baseline": s("Store the returned image under this name. Letters, digits, underscore, hyphen. At most 8 names."), "roi": {"type": "object", "description": "Region of the returned image: integer x, y, w, h. w and h are at least 1."}, "tolerance": i("Per-channel difference that still counts as unchanged. 0 to 255. Default 0.")}),
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

fn n(description: &str) -> Value {
    json!({"type": "number", "description": description})
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
        "godot_game_physics" => runtime(tools, &args, "game.physics").await,
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
                                "script_sha256_note": "This line was kept by the debugger buffer without a scene stamp from the moment it was written. It is not the script currently on disk.",
                            }),
                        );
                    }
                }
            }
            ToolResult::text(serde_json::to_string_pretty(&value).unwrap_or_default())
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
        screenshot_note(&value, width, height, source),
        data,
        "image/png",
    )
}

fn screenshot_note(value: &Value, width: u32, height: u32, source: &str) -> String {
    let frames = value.get("frames").and_then(Value::as_u64).unwrap_or(1);
    let mut note = format!("Game screenshot {width}x{height} ({source})");
    if frames > 1 {
        let columns = value.get("columns").and_then(Value::as_u64).unwrap_or(1);
        note.push_str(&format!(", {frames} frames, {columns} columns"));
    }
    match value
        .get("diff")
        .and_then(|diff| diff.get("compared"))
        .and_then(Value::as_bool)
    {
        Some(true) => {
            let changed = value["diff"]
                .get("changed")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            note.push_str(&format!(", changed {changed} px"));
            if let Some(rect) = value["diff"].get("rect").filter(|rect| rect.is_object()) {
                note.push_str(&format!(
                    " in {},{},{}x{}",
                    rect["x"].as_i64().unwrap_or(0),
                    rect["y"].as_i64().unwrap_or(0),
                    rect["w"].as_i64().unwrap_or(0),
                    rect["h"].as_i64().unwrap_or(0),
                ));
            }
            match value["diff"].get("exact").and_then(Value::as_bool) {
                Some(true) => note.push_str(", pixel count exact"),
                Some(false) => note.push_str(", pixel count sampled"),
                None => {}
            }
        }
        Some(false) => match value["diff"].get("reason").and_then(Value::as_str) {
            Some(reason) if reason != "no previous capture" => {
                note.push_str(&format!(", not compared: {reason}"));
            }
            _ => note.push_str(", no earlier capture to compare"),
        },
        None => {}
    }
    append_baseline(&mut note, value);
    note
}

fn append_baseline(note: &mut String, value: &Value) {
    let Some(baseline) = value.get("baseline").filter(|item| item.is_object()) else {
        return;
    };
    if let Some(saved) = baseline.get("saved").and_then(Value::as_str) {
        note.push_str(&format!(", saved baseline {saved}"));
    }
    if baseline.get("compared").and_then(Value::as_bool) != Some(true) {
        return;
    }
    let name = baseline
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("baseline");
    let changed = json_number(baseline.get("changed"));
    let sampled = json_number(baseline.get("sampled"));
    let ratio = json_number(baseline.get("ratio"));
    let tolerance = json_number(baseline.get("tolerance"));
    let kind = if baseline.get("exact").and_then(Value::as_bool) == Some(true) {
        "exact"
    } else {
        "sampled"
    };
    note.push_str(&format!(
        ", baseline {name} changed {changed} px, sampled {sampled}, ratio {ratio}, tolerance {tolerance}, {kind}, roi_space returned_image"
    ));
    if let Some(roi) = baseline.get("roi").filter(|roi| roi.is_object()) {
        note.push_str(&format!(
            ", roi {},{},{}x{}",
            roi["x"].as_i64().unwrap_or(0),
            roi["y"].as_i64().unwrap_or(0),
            roi["w"].as_i64().unwrap_or(0),
            roi["h"].as_i64().unwrap_or(0),
        ));
    }
    if let Some(bounds) = baseline.get("bounds").filter(|bounds| bounds.is_object()) {
        note.push_str(&format!(
            ", bounds {},{},{}x{}",
            bounds["x"].as_i64().unwrap_or(0),
            bounds["y"].as_i64().unwrap_or(0),
            bounds["w"].as_i64().unwrap_or(0),
            bounds["h"].as_i64().unwrap_or(0),
        ));
    }
}

fn since_of(args: &Value) -> u64 {
    args.get("since").and_then(Value::as_u64).unwrap_or(0)
}

fn level_of(args: &Value) -> &str {
    str_arg(args, "level").unwrap_or("all")
}

fn json_number(value: Option<&Value>) -> String {
    value
        .map(|item| serde_json::to_string(item).unwrap_or_else(|_| "0".into()))
        .unwrap_or_else(|| "0".into())
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
    fn screenshot_note_names_a_sheet_and_the_changed_rect() {
        let first = screenshot_note(
            &json!({"frames": 1, "diff": {"compared": false}}),
            32,
            16,
            "main window",
        );
        assert_eq!(
            first,
            "Game screenshot 32x16 (main window), no earlier capture to compare"
        );
        let sheet = screenshot_note(
            &json!({
                "frames": 4,
                "columns": 2,
                "diff": {"compared": true, "changed": 12, "rect": {"x": 1, "y": 2, "w": 3, "h": 4}}
            }),
            64,
            64,
            "main window",
        );
        assert_eq!(
            sheet,
            "Game screenshot 64x64 (main window), 4 frames, 2 columns, changed 12 px in 1,2,3x4"
        );
    }

    #[test]
    fn screenshot_note_reports_a_named_baseline() {
        let note = screenshot_note(
            &json!({
                "frames": 1,
                "diff": {"compared": false},
                "baseline": {
                    "compared": true,
                    "name": "menu",
                    "changed": 4,
                    "sampled": 10,
                    "ratio": 0.4,
                    "tolerance": 8,
                    "exact": true,
                    "roi_space": "returned_image",
                    "roi": {"x": 1, "y": 2, "w": 3, "h": 4},
                    "bounds": {"x": 1, "y": 2, "w": 3, "h": 4},
                    "saved": "menu"
                }
            }),
            32,
            16,
            "main window",
        );
        assert_eq!(
            note,
            "Game screenshot 32x16 (main window), no earlier capture to compare, saved baseline menu, baseline menu changed 4 px, sampled 10, ratio 0.4, tolerance 8, exact, roi_space returned_image, roi 1,2,3x4, bounds 1,2,3x4"
        );
        let sampled = screenshot_note(
            &json!({
                "frames": 1,
                "baseline": {
                    "compared": true,
                    "name": "wide",
                    "changed": 1,
                    "sampled": 2,
                    "ratio": 0.5,
                    "tolerance": 0,
                    "exact": false,
                    "roi": {"x": 0, "y": 0, "w": 2, "h": 2}
                }
            }),
            8,
            8,
            "main window",
        );
        assert!(
            sampled.contains("sampled") && !sampled.contains("exact"),
            "{sampled}"
        );
    }

    #[test]
    fn screenshot_note_says_why_two_captures_were_not_compared() {
        let note = screenshot_note(
            &json!({
                "frames": 1,
                "diff": {"compared": false, "reason": "the previous capture had another size"}
            }),
            64,
            64,
            "main window",
        );
        assert_eq!(
            note,
            "Game screenshot 64x64 (main window), not compared: the previous capture had another size"
        );
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
