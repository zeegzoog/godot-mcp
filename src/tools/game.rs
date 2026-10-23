//! Running-game inspection, input and screenshots.
//!
//! Process runs speak Godot's remote debugger. Editor Play is reached through the
//! plugin's debugger relay. Method calls require an explicit `permit` list.

use super::{ToolManager, str_arg, string_list};
use crate::error::McpError;
use crate::paths::{resolve_in_project, same_directory};
use crate::protocol::{Tool, ToolResult};
use serde_json::{Value, json};
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

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
            | "godot_game_input"
            | "godot_game_input_release"
            | "godot_game_wait"
            | "godot_game_physics"
            | "godot_game_screenshot"
            | "godot_game_record"
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
            "Reads a live node's properties, groups and, optionally, signals and methods. A property uses '/' for a Dictionary key or an Object property, the same way godot_game_wait does: get only, no method calls, and a private name or script is refused. A missing segment anywhere in that path is an entry in property_errors that names the segment; other requested properties are still returned. null is only a value that really is null. A property whose own name contains '/' (metadata/note, theme_override_colors/font_color) is read whole. ':' steps through a Resource (material_override:shader_parameter/caustics), including a segment that starts with _. A requested script name is omitted, not rejected. A ':' step through something that is not a Resource is an error and names '/'.",
            json!({"run_id": s(""), "path": s("Node path relative to the current scene, or /root/..."), "properties": {"type": "array", "items": {"type": "string"}}, "signals": b(), "methods": b()}),
            &[],
        ),
        tool(
            "godot_game_set",
            "Sets properties on a live node. '/' writes a Dictionary key or an Object property, the same way godot_game_get reads one: get only, no method calls. A missing property or key is an error that names the segment; a new Dictionary key is not created. The value is read back, and a write that did not take (a read-only property, or a setter that rejects the value) is an error, never success with null. A property whose own name contains '/' (shader_parameter/caustics, metadata/note) is written whole. ':' steps through a Resource (material_override:shader_parameter/caustics). A '/' step stays inside the object it starts from: a Node along the way is an error, and that node is named with the path parameter. Refuses script, a private segment, and a ':' step that is not a Resource. Nothing is saved to disk.",
            json!({"run_id": s(""), "path": s(""), "properties": {"type": "object"}}),
            &["properties"],
        ),
        tool(
            "godot_game_call",
            "Calls one method on a live node. The method must be listed in permit and is refused if it can quit, evaluate code, call another method by name (call, callv, call_group, propagate_call, rpc, emit_signal, notification), set a property by name (set, set_deferred, set_indexed), or free or replace a node (free, queue_free, replace_by). A JSON string is passed as a StringName or NodePath where the method asks for one.",
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
            "Connects to signals for a few frames and returns the ones that fired, then disconnects. Refused while the game is held: step with godot_game_physics, then read with godot_game_signals.",
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
            "Injects input into the running game. Steps: key, text, mouse_move, mouse_button, touch, joy_button, joy_axis, wait, click, click_node, swipe, path, circle, time_scale. phase is tap (default), press, or release. click is a mouse tap at x, y. click_node taps the center of a Control, Node2D, or Node3D. swipe presses at x, y and releases at to_x, to_y across frames (default 8, pointer mouse or touch). path presses once at the first of points ([[x, y], ...], 2 to 256), moves through them, and releases at the last; frames_per_point is the frames spent on each step between points (default 1); a path takes at most 600 frames in all (points - 1 times frames_per_point). circle is that same drag around center [x, y]: radius, turns (default 1, max 8) and points_per_turn (default 16, 4 to 64). pointer is mouse or touch, and touch may set index. Every pointer position (mouse_move, mouse_button, touch, click, swipe, path, circle) is a pixel of the root viewport's visible rectangle, the same space _input and _unhandled_input see after content scale. It is not an OS screen pixel and not a world coordinate. A touch step, and a swipe, path or circle with pointer touch, also emits the engine's emulated mouse event when Input.emulate_mouse_from_touch is on (project setting input_devices/pointing/emulate_mouse_from_touch, on by default). The engine delivers that InputEventMouseButton or InputEventMouseMotion first, with device InputEvent.DEVICE_ID_EMULATION, then the touch, both at the same viewport position. emulate_mouse false turns that emulation off for this call and restores the previous value afterwards, including when a step fails. Mouse steps do not move the OS cursor unless warp_mouse is true. time_scale sets Engine.time_scale (0 through 8) until another call changes it. A failed step releases every button and touch this server is still holding. More than 20 steps return ok_count and failed instead of one object per step, unless verbose is true. In a held game the steps are applied at once: key, mouse_button, touch and joy_button taps stay down for hold_ticks ticks (default 2) and are released at the start of the tick they fall due on, so how the ticks are split into steps does not change what the game sees; wait, text, swipe, path and circle steps are refused while held. Leaving the hold releases everything still held.",
            json!({"run_id": s(""), "steps": {"type": "array", "items": step_schema()}, "warp_mouse": {"type": "boolean", "description": "Move the real OS cursor to each mouse event. Default false."}, "emulate_mouse": {"type": "boolean", "description": "When false, touch steps do not also emit the engine's emulated mouse events. The previous value is restored when the call finishes. Default: leave the project setting (on unless the project turned it off)."}, "verbose": {"type": "boolean", "description": "Return every step when there are more than 20. Default false, which answers with ok_count and failed."}, "hold_ticks": ir("Held game only: how many ticks a tap stays down before it is released. 1 to 600, default 2.", 1, Some(600))}),
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
            "Waits in the running game. frames waits process frames (default 1). node waits until that node exists. signal waits until that signal fires on node (timeout frames, default 180; not combined with a property or scene_generation). property reads a public node property, including nested Dictionary keys or Object properties separated by /, using get only: no expressions and no method calls. A missing segment anywhere in that path is an error that names the segment; null is returned only when the property exists and its value is null. op is eq, ne, lt, lte, gt, gte, or exists. eq and ne require the same type (a JSON number that is a whole number may match an int; a fraction against an int is a type error; strings are never coerced). Ordering is int-with-int or float-with-float, and float equality is exact. Private names, script, and anything inside an Array are refused. scene_generation waits until the running generation is greater than the given one, and can be combined with node and property. The result and a timeout include the observed value and the scene context. time_scale sets Engine.time_scale from 0 through 8 and keeps it until changed again; the runtime restores the previous scale when the game exits. Refused while the game is held, because no frame would pass: step with godot_game_physics instead.",
            json!({"run_id": s(""), "frames": i("Default 1. Ignored when node, signal, property, or scene_generation is set, except as the default timeout."), "node": s("Wait until this node exists. Required when property is set."), "signal": s("Wait until this signal fires on node."), "property": s("Public property path. Use / for a Dictionary key or an Object property."), "op": s("eq, ne, lt, lte, gt, gte, or exists."), "value": {"description": "Value to compare. Same type as the property. No string coercion. A whole JSON number may match an int."}, "scene_generation": i("Wait until the running scene generation is greater than this."), "timeout": i("Frame limit for node, signal, property, or scene_generation. Default 180, max 600."), "time_scale": n("Engine.time_scale, from 0 through 8.")}),
            &[],
        ),
        tool(
            "godot_game_physics",
            "Controls game time. action hold stops the game at the start of a physics tick: no tick, frame, timer or signal advances, even in nodes that ignore pause, while reads (tree, get, set, call with permit, signals, snapshot, output, one-frame screenshot) are still answered. While held, step ticks runs exactly that many physics ticks (1 to 600) and holds again; step with until (node, property, op, value, checked before every tick, as in godot_game_wait) and max (1 to 3600) runs until the condition holds and says until_met; resume lets the game run. Held steps are exact only when the game was started with godot_run_project held true: the answer carries deterministic and, when false, deterministic_reason (not_started_held, fixed_rate_differs or frames_did_not_follow_ticks). A hold entered later, or in the editor, still stands still but is not exact. A held game refuses what needs time to pass (godot_game_wait, godot_game_observe, a screenshot sheet or camera, godot_game_record, input wait, text, swipe, path and circle steps) and says to step instead. Without a hold, step advances 1 to 120 real engine physics iterations, then leaves the tree paused (resume clears that pause); it sets max_physics_steps_per_frame to 1 while it runs, does not call node _physics_process, does not step idle _process or nodes that ignore pause, and does not make the game deterministic. status reports held, deterministic and the timing settings",
            json!({"run_id": s(""), "action": s("step (default), hold, resume, or status. hold stops the game at the start of a physics tick until it is stepped or resumed."), "ticks": ir("Physics iterations for step: 1 to 120, or 1 to 600 while the game is held.", 1, None), "until": until_schema(), "max": ir("With until: the most ticks to run. 1 to 3600, default 600.", 1, Some(3600))}),
            &[],
        ),
        tool(
            "godot_game_screenshot",
            "Captures the running game window, a viewport, or a Camera2D/Camera3D as one PNG. frames (1 to 8) stitches shots taken every process frames into a single sheet. For a longer series saved as PNG files, use godot_game_record. The text names the godot_game_input coordinate space (the root viewport's visible width and height, after content scale) and the scale from returned image pixels to that space: multiply an image pixel by the scale. A sheet uses that scale per frame, after subtracting the cell origin. When a baseline is compared, roi is the comparison region in returned-image pixels (roi_space returned_image) and the same scale applies inside a frame; roi does not crop the image and is refused without a baseline. A camera capture uses the camera's viewport; when that viewport is not the root, the text also names the captured viewport and the scale to it. The text says how many pixels changed since the previous capture of this run, and where (a sheet compares its last frame, and only against a capture of the same size). save_baseline stores the returned image under a name (at most 8, each at most 512×512 pixels). baseline compares the returned image with a saved one. roi is the comparison region (x, y, w, h in pixels of that returned image, including a sheet) and requires baseline; it does not crop the returned image. tolerance (0 to 255, only with baseline) is the allowed per-channel difference; a larger difference counts as changed. The text reports changed, sampled, ratio, bounds, tolerance, the baseline name, and whether the pixel count is exact or sampled. Fails instead of returning an empty image. Headless games have nothing to capture. While the game is held a single capture shows the last frame it drew (a sheet with frames above 1, or a camera switch, is refused: take one capture, step, take the next). A missing baseline, a bad name, or an roi outside the image is an error and does not replace a saved image.",
            json!({"run_id": s(""), "node": s("Viewport or node whose viewport to capture."), "camera": s("Camera2D or Camera3D to make current for the shot."), "max_dimension": i("Longest side of the returned image in pixels. Default 768."), "frames": ir("How many frames to stitch. Default 1, max 8.", 1, Some(8)), "every": ir("Process frames between shots. Default 1, max 60.", 1, Some(60)), "baseline": s("Name of a saved image to compare."), "save_baseline": s("Store the returned image under this name. Letters, digits, underscore, hyphen. At most 8 names."), "roi": roi_schema(), "tolerance": ir("Per-channel difference that still counts as unchanged, only when comparing a baseline. 0 to 255. Default 0.", 0, Some(255))}),
            &[],
        ),
        tool(
            "godot_game_record",
            "Records the game window as PNG files on disk, for motion that a sheet of 8 frames cannot cover. frames is 1 to 240 (default 60). every is the process frames between shots, 1 to 60 (default 1). frames * every must be at most 480. max_dimension is the longest side of each file (default 768, 16 to 4096). Each frame only grabs an image. The PNG files are written after the last frame, and the timings are those capture instants. frames × width × height × 4 must stay within 512 MB after max_dimension. directory is an absolute path inside the system temporary directory; omit it for a new one. The reply lists each file's path, size and timing, and does not embed image bytes. format png (default) writes only those files. gif or mp4 also builds recording.gif or recording.mp4 when ffmpeg is on PATH; without ffmpeg the PNG files are still returned. Refused while the game is held, and a headless game has nothing to record.",
            json!({
                "run_id": s(""),
                "frames": ir("How many PNG files to save. 1 to 240, default 60.", 1, Some(240)),
                "every": ir("Process frames between shots. 1 to 60, default 1. frames * every must be at most 480.", 1, Some(60)),
                "max_dimension": ir("Longest side of each PNG in pixels. 16 to 4096, default 768.", 16, Some(4096)),
                "directory": s("Absolute directory to write into, inside the system temporary directory. Created if missing. Must not already contain frame PNGs or a recording. Omit for a new one."),
                "format": {"type": "string", "enum": ["png", "gif", "mp4"], "description": "png (default) keeps the frames. gif or mp4 also runs ffmpeg when it is on PATH."}
            }),
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
            "Pause, resume, or step a process run that this server is debugging. Pause is refused on a game held with godot_game_physics: it is already still.",
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

/// The fields of one `godot_game_input` step, so a caller does not have to guess them. The
/// runtime checks each step; this describes it.
fn step_schema() -> Value {
    json!({
        "type": "object",
        "description": "One input step. type is required; the other fields depend on it (key: key, phase; text: text; mouse_move, mouse_button, touch: x, y, button, index, phase; click: x, y, button; click_node: node, pointer, button; swipe: x, y, to_x, to_y, frames, pointer, button; path: points, frames_per_point, pointer, button, index; circle: center, radius, turns, points_per_turn, frames_per_point, pointer, button, index; joy_button: button, device, phase; joy_axis: axis, value, device; wait: frames, node, signal, timeout; time_scale: scale).",
        "properties": {
            "type": {"type": "string", "enum": ["key", "text", "mouse_move", "mouse_button", "touch", "click", "click_node", "swipe", "path", "circle", "joy_button", "joy_axis", "wait", "time_scale"]},
            "phase": {"type": "string", "enum": ["tap", "press", "release"], "description": "Default tap."},
            "key": {"type": "string", "description": "An engine key name such as Right, Space or A."},
            "text": {"type": "string"},
            "x": {"type": "number"}, "y": {"type": "number"},
            "to_x": {"type": "number"}, "to_y": {"type": "number"},
            "button": {"type": "string", "description": "A mouse button (left, right, middle, ...) or a joypad button name."},
            "index": {"type": "integer"},
            "pointer": {"type": "string", "enum": ["mouse", "touch"]},
            "node": {"type": "string", "description": "A node path in the running game."},
            "signal": {"type": "string"},
            "frames": {"type": "integer", "description": "wait: frames to wait (0 to 600); swipe: frames the swipe takes (1 to 60, default 8)."},
            "points": {"type": "array", "description": "path: 2 to 256 points, each [x, y].", "items": {"type": "array", "items": {"type": "number"}}},
            "frames_per_point": {"type": "integer", "description": "path and circle: frames spent on each step between points. 1 to 60, default 1; the whole gesture takes at most 600 frames."},
            "center": {"type": "array", "description": "circle: [x, y].", "items": {"type": "number"}},
            "radius": {"type": "number", "description": "circle: greater than 0."},
            "turns": {"type": "integer", "description": "circle: 1 to 8, default 1."},
            "points_per_turn": {"type": "integer", "description": "circle: 4 to 64, default 16."},
            "timeout": {"type": "integer", "description": "wait for a node or signal: frames before giving up (1 to 600)."},
            "axis": {"type": "string"}, "value": {"type": "number", "description": "joy_axis: -1 to 1."},
            "device": {"type": "integer"},
            "scale": {"type": "number", "description": "time_scale: 0 to 8."}
        },
        "required": ["type"]
    })
}

/// The condition of a held step, as in `godot_game_wait`.
fn until_schema() -> Value {
    json!({
        "type": "object",
        "description": "Held game only. Step until a public node property satisfies a condition, checked before every tick. Use max instead of ticks.",
        "properties": {
            "node": {"type": "string", "description": "Node path; '.' is the current scene."},
            "property": {"type": "string", "description": "A public property, or a path such as stats/hp."},
            "op": {"type": "string", "enum": ["eq", "ne", "lt", "lte", "gt", "gte", "exists"]},
            "value": {"description": "The value to compare with."}
        }
    })
}

/// Comparison region of a baseline screenshot, in the returned image's pixels. Not a crop.
fn roi_schema() -> Value {
    json!({
        "type": "object",
        "description": "Comparison region in pixels of the returned image (integer x, y, w, h). Requires baseline; it does not crop the image. w and h are at least 1.",
        "properties": {
            "x": {"type": "integer"}, "y": {"type": "integer"},
            "w": {"type": "integer", "minimum": 1}, "h": {"type": "integer", "minimum": 1}
        }
    })
}

/// An integer parameter with the range the tool documents.
fn ir(description: &str, minimum: i64, maximum: Option<i64>) -> Value {
    let mut schema = json!({"type": "integer", "description": description, "minimum": minimum});
    if let Some(maximum) = maximum {
        schema["maximum"] = json!(maximum);
    }
    schema
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
        "godot_game_input" => game_input(tools, &args).await,
        "godot_game_input_release" => runtime(tools, &args, "input.release_all").await,
        "godot_game_wait" => runtime(tools, &args, "input.wait").await,
        "godot_game_physics" => runtime(tools, &args, "game.physics").await,
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

async fn game_input(tools: &ToolManager, args: &Value) -> ToolResult {
    let verbose = args
        .get("verbose")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let mut params = params_without_run(args);
    if let Some(map) = params.as_object_mut() {
        map.remove("verbose");
    }
    if let Err(message) = prepare_input_steps(&mut params) {
        return ToolResult::error(
            message,
            &["Give path 2 to 256 points of [x, y], or a circle with a positive radius"],
        );
    }
    match tools
        .runtime_call(str_arg(args, "run_id"), "input.run", params)
        .await
    {
        Ok(mut value) => {
            compact_input_reply(&mut value, verbose);
            ToolResult::text(serde_json::to_string_pretty(&value).unwrap_or_default())
        }
        Err(error) => runtime_error(&error),
    }
}

const MAX_PATH_POINTS: i64 = 256;
const COMPACT_AFTER: usize = 20;

/// Turns each `circle` step into a `path`. The game plays that one gesture.
fn prepare_input_steps(params: &mut Value) -> Result<(), String> {
    let Some(steps) = params.get_mut("steps").and_then(Value::as_array_mut) else {
        return Ok(());
    };
    for (index, step) in steps.iter_mut().enumerate() {
        if step.get("type").and_then(Value::as_str) != Some("circle") {
            continue;
        }
        let points = circle_from_step(step, index)?;
        let Some(map) = step.as_object_mut() else {
            return Err(format!("steps[{index}] must be an object"));
        };
        map.insert("type".into(), json!("path"));
        map.insert("points".into(), json!(points));
        map.remove("center");
        map.remove("radius");
        map.remove("turns");
        map.remove("points_per_turn");
    }
    Ok(())
}

fn circle_from_step(step: &Value, index: usize) -> Result<Vec<[f64; 2]>, String> {
    let Some(pair) = step
        .get("center")
        .and_then(Value::as_array)
        .filter(|pair| pair.len() >= 2)
    else {
        return Err(format!("steps[{index}]: circle needs center [x, y]"));
    };
    let Some(x) = json_f64(&pair[0]) else {
        return Err(format!("steps[{index}]: circle center x must be a number"));
    };
    let Some(y) = json_f64(&pair[1]) else {
        return Err(format!("steps[{index}]: circle center y must be a number"));
    };
    let Some(radius) = step.get("radius").and_then(json_f64) else {
        return Err(format!("steps[{index}]: circle needs a radius"));
    };
    if !radius.is_finite() || radius <= 0.0 {
        return Err(format!(
            "steps[{index}]: circle radius must be greater than 0"
        ));
    }
    let turns = match step.get("turns") {
        Some(value) => json_whole(value)
            .ok_or_else(|| format!("steps[{index}]: turns must be a whole number from 1 to 8"))?,
        None => 1,
    };
    if !(1..=8).contains(&turns) {
        return Err(format!(
            "steps[{index}]: turns must be a whole number from 1 to 8"
        ));
    }
    let per = match step.get("points_per_turn") {
        Some(value) => json_whole(value).ok_or_else(|| {
            format!("steps[{index}]: points_per_turn must be a whole number from 4 to 64")
        })?,
        None => 16,
    };
    if !(4..=64).contains(&per) {
        return Err(format!(
            "steps[{index}]: points_per_turn must be a whole number from 4 to 64"
        ));
    }
    let count = turns.saturating_mul(per).saturating_add(1);
    if count > MAX_PATH_POINTS {
        return Err(format!(
            "steps[{index}]: a circle may have at most {MAX_PATH_POINTS} points"
        ));
    }
    Ok(circle_points(x, y, radius, turns, per))
}

fn circle_points(x: f64, y: f64, radius: f64, turns: i64, points_per_turn: i64) -> Vec<[f64; 2]> {
    let total = turns * points_per_turn;
    (0..=total)
        .map(|index| {
            let angle = std::f64::consts::TAU * (index as f64) / (points_per_turn as f64);
            [x + radius * angle.cos(), y + radius * angle.sin()]
        })
        .collect()
}

fn json_f64(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_i64().map(|number| number as f64))
}

fn json_whole(value: &Value) -> Option<i64> {
    if let Some(number) = value.as_i64() {
        return Some(number);
    }
    value
        .as_f64()
        .filter(|number| number.fract() == 0.0 && number.is_finite())
        .map(|number| number as i64)
}

/// A long step list answers with a count. `verbose` keeps every step.
fn compact_input_reply(value: &mut Value, verbose: bool) {
    let Some(steps) = value.get("steps").and_then(Value::as_array).cloned() else {
        return;
    };
    if verbose || steps.len() <= COMPACT_AFTER {
        return;
    }
    let mut ok_count = 0_u64;
    let mut failed = Vec::new();
    for (index, step) in steps.into_iter().enumerate() {
        if step.get("ok").and_then(Value::as_bool) == Some(true) {
            ok_count += 1;
            continue;
        }
        let mut item = step;
        if let Some(map) = item.as_object_mut() {
            map.entry("index").or_insert_with(|| json!(index));
        }
        failed.push(item);
    }
    if let Some(map) = value.as_object_mut() {
        map.remove("steps");
        map.insert("ok_count".into(), json!(ok_count));
        map.insert("failed".into(), Value::Array(failed));
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
    if let Some(mapping) = input_mapping(value, width, height, frames) {
        note.push_str(", ");
        note.push_str(&mapping);
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

/// How returned image pixels map onto the coordinate space `godot_game_input` uses.
///
/// Absent when the capture did not report an input size. A sheet's scale is per frame.
/// A capture of some other viewport names that viewport and scales to it.
fn input_mapping(value: &Value, image_w: u32, image_h: u32, frames: u64) -> Option<String> {
    let input_w = json_u32(value, "input_width")?;
    let input_h = json_u32(value, "input_height")?;
    let frame_w = json_u32(value, "frame_width").unwrap_or(image_w);
    let frame_h = json_u32(value, "frame_height").unwrap_or(image_h);
    if frame_w == 0 || frame_h == 0 {
        return None;
    }
    let captured_w = json_u32(value, "captured_width").unwrap_or(input_w);
    let captured_h = json_u32(value, "captured_height").unwrap_or(input_h);
    let same_viewport = captured_w == input_w && captured_h == input_h;
    let (scale_x, scale_y) = if same_viewport {
        (
            f64::from(input_w) / f64::from(frame_w),
            f64::from(input_h) / f64::from(frame_h),
        )
    } else {
        (
            f64::from(captured_w) / f64::from(frame_w),
            f64::from(captured_h) / f64::from(frame_h),
        )
    };
    let per_frame = if frames > 1 {
        format!(" per frame (frame {frame_w}x{frame_h})")
    } else {
        String::new()
    };
    let scale = scale_phrase(scale_x, scale_y);
    if same_viewport {
        Some(format!(
            "input space {input_w}x{input_h}, image scale {scale}{per_frame}"
        ))
    } else {
        Some(format!(
            "input space {input_w}x{input_h}, captured viewport {captured_w}x{captured_h}, image scale {scale} to the captured viewport{per_frame}"
        ))
    }
}

fn scale_phrase(scale_x: f64, scale_y: f64) -> String {
    if !scale_x.is_finite() || !scale_y.is_finite() {
        return "?".into();
    }
    if (scale_x - scale_y).abs() < 0.005 {
        format_scale(scale_x)
    } else {
        format!("{} by {}", format_scale(scale_x), format_scale(scale_y))
    }
}

fn format_scale(value: f64) -> String {
    if !value.is_finite() || value <= 0.0 {
        return "?".into();
    }
    let rounded = (value * 1000.0).round() / 1000.0;
    if (rounded - rounded.round()).abs() < 1e-9 {
        return format!("{}", rounded.round() as i64);
    }
    format!("{rounded:.3}")
        .trim_end_matches('0')
        .trim_end_matches('.')
        .to_string()
}

fn json_u32(value: &Value, key: &str) -> Option<u32> {
    let item = value.get(key)?;
    if let Some(number) = item.as_u64() {
        return u32::try_from(number).ok();
    }
    if let Some(number) = item.as_i64() {
        return u32::try_from(number).ok();
    }
    let number = item.as_f64()?;
    if number.is_finite() && number >= 0.0 && number <= f64::from(u32::MAX) {
        Some(number.round() as u32)
    } else {
        None
    }
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

/// A breakpoint location is `res://path/to/script.gd:LINE`: a project file, no parent segments,
/// a line from 1 (TL-5).
pub(super) fn check_breakpoint(spec: &str) -> Result<(), ToolResult> {
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

async fn breakpoints(tools: &ToolManager, args: &Value) -> ToolResult {
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

async fn debug_control(tools: &ToolManager, args: &Value) -> ToolResult {
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
                Ok(Some(run)) => return held_aware_call(&run, method, params).await,
                Ok(None) => {}
                Err(error) => return Err(error),
            }
        }
        if method == "game.record" {
            return editor_runtime(self, method, params, RECORD_WAIT)
                .await
                .map_err(record_timeout);
        }
        editor_runtime(self, method, params, Duration::from_secs(20)).await
    }
}

/// How long a held step may take before the server stops waiting. The game still finishes it.
const HELD_STEP_WAIT: Duration = Duration::from_secs(60);

/// How long a recording may take. Capturing is at most 480 process frames. The PNG files are
/// written after the last frame and before the runtime answers, so this wait covers that disk
/// time. The 512 MB pixel cap keeps the write inside it.
const RECORD_WAIT: Duration = Duration::from_secs(120);

/// A runtime call that knows whether the game is held: a held step gets the long wait, a timeout
/// of one says the game is still running it, and the answer to a physics call keeps the run's
/// idea of "held" current.
async fn held_aware_call(
    run: &crate::godot::Run,
    method: &str,
    params: Value,
) -> Result<Value, McpError> {
    let physics = method == "game.physics";
    let recording = method == "game.record";
    let was_held = physics && run.is_held();
    let wait = if recording {
        RECORD_WAIT
    } else if was_held {
        HELD_STEP_WAIT
    } else {
        Duration::from_secs(20)
    };
    let outcome = run.runtime(method, params, wait).await;
    if physics {
        if let Some(held) = outcome
            .as_ref()
            .ok()
            .and_then(|value| value.get("held"))
            .and_then(Value::as_bool)
        {
            run.set_held(held);
        }
    }
    outcome.map_err(|error| {
        if recording {
            record_timeout(error)
        } else {
            held_step_timeout(was_held, error)
        }
    })
}

/// A timeout while a held step runs is not a failure of the game: it goes on and holds again.
fn held_step_timeout(was_held: bool, error: McpError) -> McpError {
    match error {
        McpError::Process(message) if was_held && message.starts_with("timed out") => {
            McpError::Process(format!(
                "the held step did not finish within {} s ({message}). The game finishes it and holds again. Read godot_game_physics with action status before stepping again, or lower ticks or max",
                HELD_STEP_WAIT.as_secs()
            ))
        }
        other => other,
    }
}

/// A recording that outlives its wait is still writing the PNG files. That is not a held step.
fn record_timeout(error: McpError) -> McpError {
    match error {
        McpError::Process(message) if message.starts_with("timed out") => {
            McpError::Process(format!(
                "timed out while the game was writing the recorded PNG files ({message}). The frames were already captured. Wait and look in the directory, or record fewer or smaller frames"
            ))
        }
        other => other,
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
    wait: Duration,
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
    let deadline = tokio::time::Instant::now() + wait;
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

const MAX_RECORD_FRAMES: i64 = 240;
const MAX_RECORD_EVERY: i64 = 60;
const MAX_RECORD_SPAN: i64 = 480;

async fn record(tools: &ToolManager, args: &Value) -> ToolResult {
    let frames = match bounded_int(args, "frames", 60, 1, MAX_RECORD_FRAMES) {
        Ok(value) => value,
        Err(result) => return result,
    };
    let every = match bounded_int(args, "every", 1, 1, MAX_RECORD_EVERY) {
        Ok(value) => value,
        Err(result) => return result,
    };
    if frames.saturating_mul(every) > MAX_RECORD_SPAN {
        return ToolResult::error(
            format!("frames * every must be at most {MAX_RECORD_SPAN} process frames"),
            &["Lower frames or every. 180 frames with every 1 is about three seconds at 60 fps"],
        );
    }
    let max_dimension = match bounded_int(args, "max_dimension", 768, 16, 4096) {
        Ok(value) => value,
        Err(result) => return result,
    };
    let format = str_arg(args, "format").unwrap_or("png");
    if !matches!(format, "png" | "gif" | "mp4") {
        return ToolResult::error(
            "format must be png, gif, or mp4",
            &["Omit format to keep the PNG files only"],
        );
    }
    let directory = match prepare_record_dir(str_arg(args, "directory")) {
        Ok(directory) => directory,
        Err(result) => return result,
    };
    let mut params = params_without_run(args);
    if let Some(map) = params.as_object_mut() {
        map.insert("directory".into(), json!(directory.display().to_string()));
        map.insert("frames".into(), json!(frames));
        map.insert("every".into(), json!(every));
        map.insert("max_dimension".into(), json!(max_dimension));
        map.remove("format");
    }
    let value = match tools
        .runtime_call(str_arg(args, "run_id"), "game.record", params)
        .await
    {
        Ok(value) => value,
        Err(error) => return runtime_error(&error),
    };
    let Some(reported) = value.get("frames").and_then(Value::as_array) else {
        return ToolResult::error(
            "The game did not return a frame list",
            &["Update the Godot MCP addon runtime"],
        );
    };
    let expected = usize::try_from(frames).unwrap_or(usize::MAX);
    if reported.len() != expected {
        return ToolResult::error(
            format!(
                "The recording saved {} frames, not {frames}",
                reported.len()
            ),
            &["Retry godot_game_record. A closed window stops the recording"],
        );
    }
    let mut frames_out = Vec::with_capacity(reported.len());
    for (offset, frame) in reported.iter().enumerate() {
        let index = offset + 1;
        let path = directory.join(format!("frame-{index:04}.png"));
        if !is_png(&path) {
            return ToolResult::error(
                format!("{} is not a PNG", path.display()),
                &["The game could not finish writing the frame"],
            );
        }
        let Some(object) = frame.as_object() else {
            return ToolResult::error(
                "A recorded frame was not an object",
                &["Update the Godot MCP addon runtime"],
            );
        };
        let mut item = object.clone();
        item.insert("index".into(), json!(index));
        item.insert("file".into(), json!(format!("frame-{index:04}.png")));
        item.insert("path".into(), json!(path.display().to_string()));
        frames_out.push(Value::Object(item));
    }
    let rate = recording_rate(&frames_out);
    let video = encode_video(&directory, format, frames, &rate).await;
    let mut body = value;
    if let Some(object) = body.as_object_mut() {
        object.insert("directory".into(), json!(directory.display().to_string()));
        object.insert("count".into(), json!(frames));
        object.insert("frames".into(), Value::Array(frames_out));
        object.insert("video".into(), video);
    }
    ToolResult::text(serde_json::to_string_pretty(&body).unwrap_or_default())
}

fn bounded_int(
    args: &Value,
    key: &str,
    default: i64,
    min: i64,
    max: i64,
) -> Result<i64, ToolResult> {
    let Some(value) = args.get(key).filter(|value| !value.is_null()) else {
        return Ok(default);
    };
    let Some(number) = value.as_i64() else {
        return Err(ToolResult::error(
            format!("{key} must be an integer from {min} to {max}"),
            &["Pass a whole number"],
        ));
    };
    if !(min..=max).contains(&number) {
        return Err(ToolResult::error(
            format!("{key} must be from {min} to {max}"),
            &["Use a value inside the documented range"],
        ));
    }
    Ok(number)
}

fn prepare_record_dir(requested: Option<&str>) -> Result<PathBuf, ToolResult> {
    let temp_root = std::env::temp_dir();
    let directory = if let Some(text) = requested {
        let hint = format!(
            "Pass an absolute directory below {}, or omit directory",
            temp_root.display()
        );
        if text.is_empty() || text.chars().any(char::is_control) {
            return Err(ToolResult::error(
                "directory must be an absolute path",
                &[hint.as_str()],
            ));
        }
        let path = PathBuf::from(text);
        if !path.is_absolute()
            || path
                .components()
                .any(|component| matches!(component, Component::ParentDir))
        {
            return Err(ToolResult::error(
                "directory must be an absolute path and must not contain ..",
                &[hint.as_str()],
            ));
        }
        // A tool call must not write files anywhere on the machine: the recording goes below the
        // system temporary directory, links in the part that exists included.
        let inside = resolve_in_project(&temp_root, text)
            .ok()
            .filter(|resolved| !same_directory(resolved, &temp_root));
        inside.ok_or_else(|| {
            ToolResult::error(
                format!(
                    "directory must be a folder inside the system temporary directory {}",
                    temp_root.display()
                ),
                &[hint.as_str()],
            )
        })?
    } else {
        // Straight into the temporary directory, with an unguessable name: a shared parent folder
        // could be made by another user beforehand and then swapped under the recording.
        temp_root.join(format!(
            "godot-mcp-record-{}",
            uuid::Uuid::new_v4().simple()
        ))
    };
    if directory.is_file() {
        return Err(ToolResult::error(
            format!("directory is a file: {}", directory.display()),
            &["Pass a directory, or omit directory for a temporary one"],
        ));
    }
    if let Err(error) = create_private_dir(&directory) {
        return Err(ToolResult::error(
            format!("Could not create the recording directory: {error}"),
            &["Choose a directory you can write to"],
        ));
    }
    let entries = match std::fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) => {
            return Err(ToolResult::error(
                format!("Could not read the recording directory: {error}"),
                &["Choose a directory you can write to"],
            ));
        }
    };
    let occupied = entries.flatten().any(|entry| {
        let name = entry.file_name();
        // Compared without case: a Windows or macOS volume treats Recording.GIF as recording.gif.
        let name = name.to_string_lossy().to_lowercase();
        (name.starts_with("frame-") && name.ends_with(".png")) || name.starts_with("recording.")
    });
    if occupied {
        return Err(ToolResult::error(
            "directory already contains frame PNG files or a recording",
            &["Pass an empty directory, or omit directory for a new one"],
        ));
    }
    Ok(directory)
}

/// Creates the folder and its missing parents readable by this user only (`0700` on Unix), so the
/// frames of a game are not listed to the other accounts of a shared machine.
fn create_private_dir(path: &Path) -> std::io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(path)
}

fn is_png(path: &Path) -> bool {
    let mut header = [0_u8; 8];
    let Ok(mut file) = std::fs::File::open(path) else {
        return false;
    };
    std::io::Read::read_exact(&mut file, &mut header).is_ok()
        && header == [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n']
}

fn recording_rate(frames: &[Value]) -> String {
    let msec = |frame: &Value| frame.get("msec").and_then(Value::as_f64);
    let first = frames.first().and_then(msec).unwrap_or(0.0);
    let last = frames.last().and_then(msec).unwrap_or(first);
    let span = last - first;
    let rate = if frames.len() > 1 && span > 0.0 {
        (frames.len() - 1) as f64 * 1000.0 / span
    } else {
        10.0
    };
    format!("{:.2}", rate.clamp(1.0, 60.0))
}

async fn encode_video(directory: &Path, format: &str, count: i64, rate: &str) -> Value {
    encode_video_with(directory, format, count, rate, "ffmpeg").await
}

async fn encode_video_with(
    directory: &Path,
    format: &str,
    count: i64,
    rate: &str,
    program: &str,
) -> Value {
    if format == "png" {
        return json!({"format": "png", "ffmpeg": "not_requested"});
    }
    let video_path = directory.join(format!("recording.{format}"));
    let mut command = tokio::process::Command::new(program);
    command
        .current_dir(directory)
        .kill_on_drop(true)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .arg("-y")
        .arg("-hide_banner")
        .arg("-loglevel")
        .arg("error")
        .arg("-framerate")
        .arg(rate)
        .arg("-start_number")
        .arg("1")
        .arg("-i")
        .arg("frame-%04d.png")
        .arg("-frames:v")
        .arg(count.to_string());
    if format == "mp4" {
        command.args([
            "-vf",
            "scale=trunc(iw/2)*2:trunc(ih/2)*2",
            "-pix_fmt",
            "yuv420p",
        ]);
    }
    command.arg(&video_path);
    let outcome = tokio::time::timeout(Duration::from_secs(60), command.output()).await;
    match outcome {
        Ok(Ok(output)) if output.status.success() && video_path.is_file() => json!({
            "format": format,
            "path": video_path.display().to_string(),
            "ffmpeg": "used",
        }),
        Ok(Err(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            json!({"format": format, "ffmpeg": "not_found"})
        }
        Ok(Ok(output)) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let detail = stderr
                .lines()
                .find(|line| !line.trim().is_empty())
                .unwrap_or("ffmpeg failed")
                .trim();
            json!({"format": format, "ffmpeg": "failed", "error": detail})
        }
        Ok(Err(error)) => json!({"format": format, "ffmpeg": "failed", "error": error.to_string()}),
        Err(_) => json!({"format": format, "ffmpeg": "failed", "error": "ffmpeg timed out"}),
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

    #[test]
    fn a_circle_becomes_a_closed_path_and_a_long_reply_shrinks() {
        let mut params = json!({
            "steps": [{
                "type": "circle",
                "center": [0, 0],
                "radius": 10,
                "turns": 1,
                "points_per_turn": 4,
                "pointer": "touch",
                "index": 2
            }]
        });
        prepare_input_steps(&mut params).expect("circle");
        let step = &params["steps"][0];
        assert_eq!(step["type"], "path");
        assert_eq!(step["pointer"], "touch");
        assert_eq!(step["index"], 2);
        assert!(step.get("radius").is_none());
        let points = step["points"].as_array().expect("points");
        assert_eq!(points.len(), 5);
        let first = &points[0];
        let last = &points[4];
        assert!((first[0].as_f64().unwrap() - 10.0).abs() < 1e-9);
        assert!(first[1].as_f64().unwrap().abs() < 1e-9);
        assert!(points[1][0].as_f64().unwrap().abs() < 1e-9);
        assert!((points[1][1].as_f64().unwrap() - 10.0).abs() < 1e-9);
        assert!((last[0].as_f64().unwrap() - first[0].as_f64().unwrap()).abs() < 1e-9);
        assert!((last[1].as_f64().unwrap() - first[1].as_f64().unwrap()).abs() < 1e-9);

        let mut too_big = json!({"steps": [{"type": "circle", "center": [0, 0], "radius": 1, "turns": 8, "points_per_turn": 64}]});
        let error = prepare_input_steps(&mut too_big).expect_err("too many points");
        assert!(error.contains("256"), "{error}");

        let mut reply = json!({
            "steps": (0..21).map(|index| json!({"type": "key", "ok": index != 3})).collect::<Vec<_>>(),
            "warp_mouse": false
        });
        compact_input_reply(&mut reply, false);
        assert!(reply.get("steps").is_none(), "{reply}");
        assert_eq!(reply["ok_count"], 20);
        assert_eq!(reply["failed"].as_array().map(Vec::len), Some(1));
        assert_eq!(reply["failed"][0]["index"], 3);
        assert_eq!(reply["warp_mouse"], false);

        let mut short = json!({"steps": [{"type": "key", "ok": true}]});
        compact_input_reply(&mut short, false);
        assert_eq!(short["steps"].as_array().map(Vec::len), Some(1));

        let mut listed = json!({
            "steps": (0..21).map(|_| json!({"type": "key", "ok": true})).collect::<Vec<_>>()
        });
        compact_input_reply(&mut listed, true);
        assert_eq!(listed["steps"].as_array().map(Vec::len), Some(21));
    }

    #[test]
    fn a_held_step_that_times_out_is_still_running_in_the_game() {
        let timeout = || {
            McpError::Process(
                "timed out waiting for the game runtime to answer 'game.physics'".into(),
            )
        };
        let held = held_step_timeout(true, timeout()).to_string();
        assert!(held.contains("finishes it and holds again"), "{held}");
        assert!(held.contains("action status"), "{held}");
        // A timeout of anything else, or of a game that was not held, is reported as it is.
        assert_eq!(
            held_step_timeout(false, timeout()).to_string(),
            timeout().to_string()
        );
        let other = || McpError::Process("the game disconnected".into());
        assert_eq!(
            held_step_timeout(true, other()).to_string(),
            other().to_string()
        );
    }

    #[test]
    fn a_recording_timeout_says_the_png_files_are_still_being_written() {
        let timeout = || {
            McpError::Process(
                "timed out waiting for the game runtime to answer 'game.record'".into(),
            )
        };
        let text = record_timeout(timeout()).to_string();
        assert!(text.contains("writing the recorded PNG files"), "{text}");
        assert!(text.contains("already captured"), "{text}");
        assert!(!text.to_lowercase().contains("held step"), "{text}");
        let other = || McpError::Process("the game disconnected".into());
        assert_eq!(record_timeout(other()).to_string(), other().to_string());
    }

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
            &json!({
                "frames": 1,
                "input_width": 32,
                "input_height": 16,
                "frame_width": 32,
                "frame_height": 16,
                "diff": {"compared": false}
            }),
            32,
            16,
            "main window",
        );
        assert_eq!(
            first,
            "Game screenshot 32x16 (main window), input space 32x16, image scale 1, no earlier capture to compare"
        );
        let sheet = screenshot_note(
            &json!({
                "frames": 4,
                "columns": 2,
                "input_width": 64,
                "input_height": 64,
                "frame_width": 32,
                "frame_height": 32,
                "diff": {"compared": true, "changed": 12, "rect": {"x": 1, "y": 2, "w": 3, "h": 4}}
            }),
            64,
            64,
            "main window",
        );
        assert_eq!(
            sheet,
            "Game screenshot 64x64 (main window), 4 frames, 2 columns, input space 64x64, image scale 2 per frame (frame 32x32), changed 12 px in 1,2,3x4"
        );
    }

    #[test]
    fn screenshot_note_maps_image_pixels_onto_the_input_space() {
        let scaled = screenshot_note(
            &json!({
                "frames": 1,
                "input_width": 540,
                "input_height": 900,
                "captured_width": 540,
                "captured_height": 900,
                "frame_width": 360,
                "frame_height": 600,
                "diff": {"compared": false}
            }),
            360,
            600,
            "main window",
        );
        assert_eq!(
            scaled,
            "Game screenshot 360x600 (main window), input space 540x900, image scale 1.5, no earlier capture to compare"
        );
        let other = screenshot_note(
            &json!({
                "frames": 1,
                "input_width": 540,
                "input_height": 900,
                "captured_width": 200,
                "captured_height": 100,
                "frame_width": 100,
                "frame_height": 50
            }),
            100,
            50,
            "SubViewport",
        );
        assert!(
            other.contains(
                "input space 540x900, captured viewport 200x100, image scale 2 to the captured viewport"
            ),
            "{other}"
        );
        let uneven = screenshot_note(
            &json!({
                "frames": 1,
                "input_width": 100,
                "input_height": 40,
                "frame_width": 50,
                "frame_height": 10
            }),
            50,
            10,
            "main window",
        );
        assert!(
            uneven.contains("input space 100x40, image scale 2 by 4"),
            "{uneven}"
        );
    }

    #[test]
    fn screenshot_note_reports_a_named_baseline() {
        let note = screenshot_note(
            &json!({
                "frames": 1,
                "input_width": 32,
                "input_height": 16,
                "frame_width": 32,
                "frame_height": 16,
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
            "Game screenshot 32x16 (main window), input space 32x16, image scale 1, no earlier capture to compare, saved baseline menu, baseline menu changed 4 px, sampled 10, ratio 0.4, tolerance 8, exact, roi_space returned_image, roi 1,2,3x4, bounds 1,2,3x4"
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
                "input_width": 64,
                "input_height": 64,
                "diff": {"compared": false, "reason": "the previous capture had another size"}
            }),
            64,
            64,
            "main window",
        );
        assert_eq!(
            note,
            "Game screenshot 64x64 (main window), input space 64x64, image scale 1, not compared: the previous capture had another size"
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

    #[tokio::test]
    async fn a_missing_ffmpeg_leaves_the_request_as_png_only() {
        let dir = tempfile::tempdir().expect("record dir");
        let video = encode_video_with(dir.path(), "gif", 1, "10", "godot-mcp-ffmpeg-missing").await;
        assert_eq!(video["ffmpeg"], "not_found");
        assert!(video.get("path").is_none());
    }

    #[test]
    fn the_recording_rate_follows_the_capture_times_and_stays_in_range() {
        let frames = |times: &[f64]| {
            times
                .iter()
                .map(|msec| json!({"msec": msec}))
                .collect::<Vec<_>>()
        };
        assert_eq!(recording_rate(&frames(&[0.0, 100.0, 200.0])), "10.00");
        assert_eq!(recording_rate(&frames(&[0.0, 1.0, 2.0])), "60.00");
        assert_eq!(recording_rate(&frames(&[0.0, 5000.0])), "1.00");
        assert_eq!(recording_rate(&frames(&[3.0, 3.0, 3.0])), "10.00");
        assert_eq!(recording_rate(&frames(&[7.0])), "10.00");
        assert_eq!(recording_rate(&[]), "10.00");
    }

    #[test]
    fn a_directory_with_frames_or_a_recording_is_refused_whatever_its_case() {
        for name in [
            "frame-0001.png",
            "FRAME-0001.PNG",
            "recording.mp4",
            "Recording.GIF",
        ] {
            let dir = tempfile::tempdir().expect("record dir");
            std::fs::write(dir.path().join(name), b"earlier").expect("write");
            assert!(prepare_record_dir(dir.path().to_str()).is_err(), "{name}");
            assert_eq!(
                std::fs::read(dir.path().join(name)).expect("read"),
                b"earlier"
            );
        }
    }

    #[test]
    fn the_temporary_directory_itself_is_not_a_recording_directory() {
        let root = std::env::temp_dir();
        assert!(prepare_record_dir(root.to_str()).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn a_new_recording_directory_is_private_and_directly_in_the_temporary_directory() {
        use std::os::unix::fs::PermissionsExt;
        let Ok(directory) = prepare_record_dir(None) else {
            panic!("no default recording directory");
        };
        let mode = std::fs::metadata(&directory)
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777;
        let parent = directory.parent().map(Path::to_path_buf);
        let name = directory
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_default();
        std::fs::remove_dir(&directory).expect("remove");
        assert_eq!(mode, 0o700);
        assert_eq!(parent, Some(std::env::temp_dir()));
        assert!(name.starts_with("godot-mcp-record-"), "{name}");
    }

    /// Runs `encode_video_with` against a shell script that stands in for ffmpeg. A script written
    /// a moment ago can still be "busy" on Linux while another test forks, so that is retried.
    #[cfg(unix)]
    async fn encode_with_stub(dir: &Path, format: &str, count: i64, script: &str) -> Value {
        use std::os::unix::fs::PermissionsExt;
        let stub = dir.join("stub-ffmpeg.sh");
        std::fs::write(&stub, script).expect("write stub");
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        let program = stub.to_string_lossy().to_string();
        for _ in 0..40 {
            let video = encode_video_with(dir, format, count, "12.50", &program).await;
            let busy = video["error"]
                .as_str()
                .is_some_and(|text| text.to_lowercase().contains("busy"));
            if !busy {
                return video;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("the stub stayed busy");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn ffmpeg_is_given_the_frames_and_the_movie_is_reported() {
        let dir = tempfile::tempdir().expect("record dir");
        let script = "#!/bin/sh\necho \"$@\" > args.txt\nfor last; do :; done\n: > \"$last\"\n";
        let video = encode_with_stub(dir.path(), "mp4", 3, script).await;
        assert_eq!(video["ffmpeg"], "used", "{video}");
        assert_eq!(
            video["path"],
            dir.path().join("recording.mp4").display().to_string()
        );
        let arguments = std::fs::read_to_string(dir.path().join("args.txt")).expect("args");
        for expected in [
            "-framerate 12.50",
            "-start_number 1",
            "-i frame-%04d.png",
            "-frames:v 3",
            "yuv420p",
        ] {
            assert!(arguments.contains(expected), "{expected}: {arguments}");
        }
        assert!(
            arguments.trim_end().ends_with("recording.mp4"),
            "{arguments}"
        );

        let gif = encode_with_stub(dir.path(), "gif", 3, script).await;
        assert_eq!(gif["ffmpeg"], "used", "{gif}");
        let arguments = std::fs::read_to_string(dir.path().join("args.txt")).expect("args");
        assert!(!arguments.contains("yuv420p"), "{arguments}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_failing_ffmpeg_is_reported_with_its_first_line_and_leaves_no_movie() {
        let dir = tempfile::tempdir().expect("record dir");
        let script = "#!/bin/sh\necho 'Unable to find a suitable output format' >&2\necho second >&2\nexit 1\n";
        let video = encode_with_stub(dir.path(), "gif", 2, script).await;
        assert_eq!(video["ffmpeg"], "failed", "{video}");
        assert_eq!(video["error"], "Unable to find a suitable output format");
        assert!(video.get("path").is_none());

        let silent = encode_with_stub(dir.path(), "gif", 2, "#!/bin/sh\nexit 0\n").await;
        assert_eq!(silent["ffmpeg"], "failed", "{silent}");
        assert!(silent.get("path").is_none());
    }
}
