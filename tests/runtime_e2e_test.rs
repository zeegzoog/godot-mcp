//! Behavior tests for a real headless Godot process: live inspection, injected input,
//! logs, and the remote debugger. Ignored unless `GODOT_PATH` points at Godot 4.5+.
//!
//! ```bash
//! GODOT_PATH=/path/to/godot cargo test --test runtime_e2e_test -- --ignored --test-threads=1 --nocapture
//! ```

mod common;

use godot_mcp::bridge::BridgeClient;
use godot_mcp::cli_runner::CliRunner;
use godot_mcp::godot::{BinaryUid, binary_uid};
use godot_mcp::protocol::ToolResult;
use godot_mcp::tools::ToolManager;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

const IGNORE: &str = "needs GODOT_PATH pointing at a Godot 4.5+ executable";

/// A windowed Godot process needs a display server. The Linux CI runner has neither
/// `DISPLAY` nor `WAYLAND_DISPLAY`, and the game exits before its runtime says hello.
fn windowed_game_can_open() -> bool {
    if cfg!(target_os = "linux") {
        std::env::var_os("DISPLAY").is_some() || std::env::var_os("WAYLAND_DISPLAY").is_some()
    } else {
        true
    }
}

fn godot_path() -> PathBuf {
    let _ = dotenvy::dotenv();
    std::env::var("GODOT_PATH")
        .ok()
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .filter(|path| path.exists())
        .expect("set GODOT_PATH to run the runtime end-to-end tests")
}

fn run_id_of(started: &ToolResult) -> String {
    text_of(started)
        .split("run_id: ")
        .nth(1)
        .and_then(|rest| rest.split(|c: char| !c.is_ascii_alphanumeric()).next())
        .expect("run id")
        .to_string()
}

fn text_of(result: &ToolResult) -> String {
    result
        .content
        .iter()
        .filter_map(|item| match item {
            godot_mcp::protocol::ToolContent::Text { text } => Some(text.as_str()),
            godot_mcp::protocol::ToolContent::Image { .. } => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn json_of(result: &ToolResult) -> Value {
    let text = text_of(result);
    serde_json::from_str(&text).unwrap_or_else(|_| panic!("not json: {text}"))
}

fn json_number(value: &Value) -> f64 {
    value
        .as_f64()
        .or_else(|| value.as_i64().map(|number| number as f64))
        .unwrap_or(f64::NAN)
}

fn png_size(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() < 24 || &bytes[..8] != b"\x89PNG\r\n\x1a\n" || &bytes[12..16] != b"IHDR" {
        return None;
    }
    let width = u32::from_be_bytes(bytes[16..20].try_into().ok()?);
    let height = u32::from_be_bytes(bytes[20..24].try_into().ok()?);
    Some((width, height))
}

fn project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("project");
    let root = dir.path();
    std::fs::create_dir_all(root.join("scenes")).unwrap();
    std::fs::create_dir_all(root.join("scripts")).unwrap();
    std::fs::write(
        root.join("project.godot"),
        "config_version=5\n\n[application]\n\nconfig/name=\"RuntimeProbe\"\nrun/main_scene=\"res://scenes/main.tscn\"\nconfig/features=PackedStringArray(\"4.5\")\n",
    )
    .unwrap();
    std::fs::write(
        root.join("scripts/probe.gd"),
        r#"extends Node2D
var last_key := ""
var mouse_presses := 0
var mouse_releases := 0
var mouse_down := false
var swipe_moves := 0
var touch_presses := 0
var touch_drags := 0
var joy_button := ""
var joy_axis := 0.0
var hits := 0
var last_mouse := Vector2.ZERO
var scale_seen := 1.0
var pad_presses := 0
var tiny_presses := 0
var firefly_law: RefCounted
var river: RefCounted
var round_state: RefCounted
var mark_ref: Node
var bag := {"count": 2}
var quiet = null
var swatch: Resource
var locked := 1:
    set(value):
        pass
signal pinged(value)

func _ready() -> void:
    firefly_law = Law.new()
    river = River.new()
    round_state = Round.new()
    mark_ref = $Mark
    swatch = Tone.new()
    set_meta("note", "hi")
    print("mcp-probe-ready")
    push_error("mcp-probe-error")

class Law:
    extends RefCounted
    var births := 3
    var _hidden := 1

class River:
    extends RefCounted
    var camera_x := 1.5

class Round:
    extends RefCounted
    var letter_index := 0

class Tone:
    extends Resource
    var level := 4

func _process(_delta: float) -> void:
    hits += 1
    scale_seen = Engine.time_scale
    pinged.emit(hits)
    queue_redraw()

func _draw() -> void:
    draw_rect(Rect2(float(hits % 50), 8, 24, 24), Color(1, 0, 0))

func _input(event: InputEvent) -> void:
    if event is InputEventKey and event.pressed and not event.echo:
        last_key = OS.get_keycode_string(event.keycode)
    elif event is InputEventMouseButton:
        last_mouse = event.position
        if event.pressed:
            mouse_presses += 1
            mouse_down = true
        else:
            mouse_releases += 1
            mouse_down = false
    elif event is InputEventMouseMotion and mouse_down:
        swipe_moves += 1
        last_mouse = event.position
    elif event is InputEventScreenTouch and event.pressed:
        touch_presses += 1
    elif event is InputEventScreenDrag:
        touch_drags += 1
    elif event is InputEventJoypadButton and event.pressed:
        joy_button = str(event.button_index)
    elif event is InputEventJoypadMotion:
        joy_axis = event.axis_value

func _on_pad_pressed() -> void:
    pad_presses += 1

func _on_tiny_pressed() -> void:
    tiny_presses += 1

func poke(value: int) -> int:
    pinged.emit(value)
    return value + 1

func stop_drawing(stopped: bool) -> bool:
    RenderingServer.render_loop_enabled = not stopped
    return stopped

func minimize(minimized: bool) -> bool:
    DisplayServer.window_set_mode(DisplayServer.WINDOW_MODE_MINIMIZED if minimized else DisplayServer.WINDOW_MODE_WINDOWED)
    return minimized
"#,
    )
    .unwrap();
    std::fs::write(
        root.join("scenes/main.tscn"),
        "[gd_scene load_steps=2 format=3]\n\n[ext_resource type=\"Script\" path=\"res://scripts/probe.gd\" id=\"1\"]\n\n[node name=\"Probe\" type=\"Node2D\"]\nscript = ExtResource(\"1\")\n\n[node name=\"Mark\" type=\"Node2D\" parent=\".\"]\nposition = Vector2(40, 70)\n\n[node name=\"Pad\" type=\"Button\" parent=\".\"]\noffset_left = 100.0\noffset_top = 20.0\noffset_right = 140.0\noffset_bottom = 60.0\n\n[node name=\"Tiny\" type=\"Button\" parent=\".\"]\noffset_left = 4.0\noffset_top = 4.0\noffset_right = 24.0\noffset_bottom = 24.0\n\n[connection signal=\"pressed\" from=\"Pad\" to=\".\" method=\"_on_pad_pressed\"]\n[connection signal=\"pressed\" from=\"Tiny\" to=\".\" method=\"_on_tiny_pressed\"]\n",
    )
    .unwrap();
    dir
}

fn manager(project: &Path) -> Arc<ToolManager> {
    Arc::new(
        ToolManager::new(BridgeClient::new(9_001), CliRunner::new(godot_path()))
            .with_working_dir(project.to_path_buf()),
    )
}

async fn call(tools: &Arc<ToolManager>, name: &str, args: Value) -> ToolResult {
    Arc::clone(tools).call_tool(name, args).await
}

#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn running_game_accepts_input_and_can_be_inspected() {
    let _ = IGNORE;
    let project = project();
    let tools = manager(project.path());
    let started = call(
        &tools,
        "godot_run_project",
        json!({
            "project_path": project.path(),
            "scene": "res://scenes/main.tscn",
            "mode": "process",
            "headless": true
        }),
    )
    .await;
    assert!(!started.is_error, "{}", text_of(&started));
    let run_id = run_id_of(&started);
    assert!(
        !project.path().join("addons").exists(),
        "a run copied the runtime into the project's addons folder"
    );
    assert!(
        project
            .path()
            .join(format!(".godot/godot_mcp/run-{run_id}/mcp_runtime.gd"))
            .is_file(),
        "the run has no private copy of the runtime"
    );

    let tree = call(&tools, "godot_game_tree", json!({"run_id": run_id})).await;
    assert!(!tree.is_error, "{}", text_of(&tree));
    let tree_json = json_of(&tree);
    let nodes = tree_json["nodes"].as_array().expect("nodes");
    assert!(
        nodes.iter().any(|node| node["name"] == "Probe"),
        "{tree_json}"
    );

    let input = call(
        &tools,
        "godot_game_input",
        json!({
            "run_id": run_id,
            "steps": [
                {"type": "key", "key": "A", "phase": "tap"},
                {"type": "mouse_button", "button": "left", "x": 12, "y": 34, "phase": "tap"},
                {"type": "touch", "x": 8, "y": 9, "phase": "tap"},
                {"type": "joy_button", "button": "a", "phase": "press"},
                {"type": "joy_axis", "axis": "left_x", "value": 0.5},
                {"type": "wait", "frames": 1}
            ]
        }),
    )
    .await;
    assert!(!input.is_error, "{}", text_of(&input));
    let input_json = json_of(&input);
    assert_eq!(
        input_json["steps"].as_array().map(Vec::len),
        Some(6),
        "{input_json}"
    );
    assert_eq!(input_json["warp_mouse"], false, "{input_json}");

    let props = call(
        &tools,
        "godot_game_get",
        json!({
            "run_id": run_id,
            "path": ".",
            "properties": ["last_key", "mouse_presses", "touch_presses", "joy_button", "joy_axis", "hits"]
        }),
    )
    .await;
    assert!(!props.is_error, "{}", text_of(&props));
    let props_json = json_of(&props);
    let values = &props_json["properties"];
    assert_eq!(values["last_key"], "A", "{props_json}");
    assert!(
        values["mouse_presses"].as_i64().unwrap_or(0) >= 1,
        "{props_json}"
    );
    assert!(
        values["touch_presses"].as_i64().unwrap_or(0) >= 1,
        "{props_json}"
    );
    assert_eq!(values["joy_button"], "0", "{props_json}");
    assert!(
        values["joy_axis"].as_f64().unwrap_or(0.0) > 0.4,
        "{props_json}"
    );
    assert!(values["hits"].as_i64().unwrap_or(0) > 0, "{props_json}");

    let nested = call(
        &tools,
        "godot_game_get",
        json!({
            "run_id": run_id,
            "path": ".",
            "properties": ["firefly_law/births", "bag/count", "swatch:level"]
        }),
    )
    .await;
    assert!(!nested.is_error, "{}", text_of(&nested));
    let nested_values = &json_of(&nested)["properties"];
    assert_eq!(
        nested_values["firefly_law/births"],
        3,
        "{}",
        text_of(&nested)
    );
    assert_eq!(nested_values["bag/count"], 2, "{}", text_of(&nested));
    assert_eq!(nested_values["swatch:level"], 4, "{}", text_of(&nested));
    let missing = call(
        &tools,
        "godot_game_get",
        json!({
            "run_id": run_id,
            "path": ".",
            "properties": ["river/berry", "river/camera_x", "quiet", "bag/nope"]
        }),
    )
    .await;
    assert!(!missing.is_error, "{}", text_of(&missing));
    let missing_json = json_of(&missing);
    assert!(
        missing_json["properties"].get("river/berry").is_none(),
        "a missing segment must not be null: {missing_json}"
    );
    assert_eq!(
        missing_json["properties"]["river/camera_x"], 1.5,
        "{missing_json}"
    );
    assert!(
        missing_json["properties"]["quiet"].is_null(),
        "a real null stays null: {missing_json}"
    );
    let property_errors = &missing_json["property_errors"];
    assert!(
        property_errors["river/berry"]
            .as_str()
            .unwrap_or("")
            .contains("berry"),
        "{missing_json}"
    );
    assert!(
        property_errors["bag/nope"]
            .as_str()
            .unwrap_or("")
            .contains("nope"),
        "{missing_json}"
    );
    let missing_wait = call(
        &tools,
        "godot_game_wait",
        json!({
            "run_id": run_id,
            "node": ".",
            "property": "river/berry",
            "op": "exists",
            "timeout": 30
        }),
    )
    .await;
    assert!(missing_wait.is_error, "{}", text_of(&missing_wait));
    let missing_wait_text = text_of(&missing_wait);
    assert!(
        missing_wait_text.contains("berry") && !missing_wait_text.contains("Timed out"),
        "{missing_wait_text}"
    );
    // A property whose own name has a slash is read whole, not walked as a path.
    let slashed = call(
        &tools,
        "godot_game_get",
        json!({"run_id": run_id, "path": ".", "properties": ["metadata/note"]}),
    )
    .await;
    assert!(!slashed.is_error, "{}", text_of(&slashed));
    assert_eq!(
        json_of(&slashed)["properties"]["metadata/note"],
        "hi",
        "{}",
        text_of(&slashed)
    );
    let waited = call(
        &tools,
        "godot_game_wait",
        json!({"run_id": run_id, "node": ".", "property": "metadata/note", "op": "eq", "value": "hi", "timeout": 30}),
    )
    .await;
    assert!(!waited.is_error, "{}", text_of(&waited));
    let through_colon = call(
        &tools,
        "godot_game_get",
        json!({"run_id": run_id, "path": ".", "properties": ["firefly_law:births"]}),
    )
    .await;
    assert!(through_colon.is_error, "{}", text_of(&through_colon));
    let colon_text = text_of(&through_colon);
    assert!(
        colon_text.contains("not a resource") && colon_text.contains("firefly_law/births"),
        "{colon_text}"
    );
    let hidden = call(
        &tools,
        "godot_game_get",
        json!({"run_id": run_id, "path": ".", "properties": ["firefly_law/_hidden"]}),
    )
    .await;
    assert!(hidden.is_error, "{}", text_of(&hidden));
    assert!(
        text_of(&hidden).to_lowercase().contains("refus"),
        "{}",
        text_of(&hidden)
    );

    let called = call(
        &tools,
        "godot_game_call",
        json!({"run_id": run_id, "path": ".", "method": "poke", "permit": ["poke"], "args": [3]}),
    )
    .await;
    assert!(!called.is_error, "{}", text_of(&called));
    assert_eq!(json_of(&called)["value"], 4, "{}", text_of(&called));

    let observed = call(
        &tools,
        "godot_game_observe",
        json!({"run_id": run_id, "path": ".", "signals": ["pinged"], "frames": 1}),
    )
    .await;
    assert!(!observed.is_error, "{}", text_of(&observed));

    let shot = call(&tools, "godot_game_screenshot", json!({"run_id": run_id})).await;
    assert!(shot.is_error, "{}", text_of(&shot));
    assert!(
        shot.content
            .iter()
            .all(|item| !matches!(item, godot_mcp::protocol::ToolContent::Image { .. })),
        "a headless game must not return image bytes"
    );
    assert!(text_of(&shot).contains("headless"), "{}", text_of(&shot));

    let too_many = call(
        &tools,
        "godot_game_screenshot",
        json!({"run_id": run_id, "frames": 9}),
    )
    .await;
    assert!(too_many.is_error, "{}", text_of(&too_many));
    assert!(
        text_of(&too_many).contains("'frames' must be at most 8"),
        "{}",
        text_of(&too_many)
    );

    let released = call(
        &tools,
        "godot_game_input_release",
        json!({"run_id": run_id}),
    )
    .await;
    assert!(!released.is_error, "{}", text_of(&released));
    let held = &json_of(&released)["held"];
    for key in ["keys", "mouse", "joy_buttons", "joy_axes", "touches"] {
        assert_eq!(
            held[key].as_i64(),
            Some(0),
            "held.{key} should be empty: {held}"
        );
    }

    let gestures = call(
        &tools,
        "godot_game_input",
        json!({
            "run_id": run_id,
            "steps": [
                {"type": "click", "x": 3, "y": 4},
                {"type": "swipe", "x": 10, "y": 10, "to_x": 90, "to_y": 12, "frames": 4},
                {"type": "swipe", "pointer": "touch", "x": 1, "y": 1, "to_x": 20, "to_y": 5, "frames": 3},
                {"type": "click_node", "node": "Pad"},
                {"type": "click_node", "node": "Mark"},
                {"type": "time_scale", "scale": 0.5},
                {"type": "wait", "frames": 1}
            ]
        }),
    )
    .await;
    assert!(!gestures.is_error, "{}", text_of(&gestures));
    let gesture_json = json_of(&gestures);
    assert_eq!(
        gesture_json["steps"].as_array().map(Vec::len),
        Some(7),
        "{gesture_json}"
    );
    assert_eq!(gesture_json["held"]["mouse"], 0, "{gesture_json}");
    assert_eq!(gesture_json["held"]["touches"], 0, "{gesture_json}");
    let played = call(
        &tools,
        "godot_game_get",
        json!({
            "run_id": run_id,
            "path": ".",
            "properties": ["mouse_presses", "mouse_releases", "swipe_moves", "touch_drags", "last_mouse", "scale_seen"]
        }),
    )
    .await;
    assert!(!played.is_error, "{}", text_of(&played));
    let played_json = json_of(&played);
    let played_values = &played_json["properties"];
    assert!(
        played_values["mouse_presses"].as_i64().unwrap_or(0) >= 3,
        "{played_json}"
    );
    assert!(
        played_values["mouse_releases"].as_i64().unwrap_or(0) >= 2,
        "{played_json}"
    );
    assert!(
        played_values["swipe_moves"].as_i64().unwrap_or(0) >= 1,
        "{played_json}"
    );
    assert!(
        played_values["touch_drags"].as_i64().unwrap_or(0) >= 1,
        "{played_json}"
    );
    let mouse = played_values["last_mouse"].as_array().expect("last_mouse");
    let mouse_x = mouse[0].as_f64().unwrap_or(999.0);
    let mouse_y = mouse[1].as_f64().unwrap_or(999.0);
    assert!(
        (mouse_x - 40.0).abs() <= 2.0 && (mouse_y - 70.0).abs() <= 2.0,
        "click_node missed Mark: {played_json}"
    );
    assert!(
        (played_values["scale_seen"].as_f64().unwrap_or(0.0) - 0.5).abs() < 0.01,
        "{played_json}"
    );

    let drags = call(
        &tools,
        "godot_game_input",
        json!({
            "run_id": run_id,
            "steps": [
                {"type": "path", "points": [[10, 12], [40, 12], [40, 30]], "frames_per_point": 1},
                {"type": "circle", "center": [80, 80], "radius": 20, "turns": 1, "points_per_turn": 8}
            ]
        }),
    )
    .await;
    assert!(!drags.is_error, "{}", text_of(&drags));
    assert_eq!(
        json_of(&drags)["steps"].as_array().map(Vec::len),
        Some(2),
        "{}",
        text_of(&drags)
    );
    let after_drag = call(
        &tools,
        "godot_game_get",
        json!({"run_id": run_id, "path": ".", "properties": ["mouse_presses", "mouse_releases", "swipe_moves", "mouse_down", "last_mouse"]}),
    )
    .await;
    let drag_values = &json_of(&after_drag)["properties"];
    assert!(
        drag_values["mouse_presses"].as_i64().unwrap_or(0) >= 5,
        "{}",
        text_of(&after_drag)
    );
    assert!(
        drag_values["mouse_releases"].as_i64().unwrap_or(0)
            >= drag_values["mouse_presses"].as_i64().unwrap_or(0) - 1,
        "a path must release: {}",
        text_of(&after_drag)
    );
    assert_eq!(drag_values["mouse_down"], false, "{}", text_of(&after_drag));
    assert!(
        drag_values["swipe_moves"].as_i64().unwrap_or(0) >= 4,
        "{}",
        text_of(&after_drag)
    );
    let end = drag_values["last_mouse"].as_array().expect("last_mouse");
    let end_x = end[0].as_f64().unwrap_or(0.0);
    let end_y = end[1].as_f64().unwrap_or(0.0);
    assert!(
        (end_x - 100.0).abs() <= 1.0 && (end_y - 80.0).abs() <= 1.0,
        "circle did not finish where it started: {}",
        text_of(&after_drag)
    );

    let mut many = Vec::new();
    for _ in 0..21 {
        many.push(json!({"type": "key", "key": "B", "phase": "tap"}));
    }
    let compact = call(
        &tools,
        "godot_game_input",
        json!({"run_id": run_id, "steps": many}),
    )
    .await;
    assert!(!compact.is_error, "{}", text_of(&compact));
    let compact_json = json_of(&compact);
    assert_eq!(compact_json["ok_count"], 21, "{compact_json}");
    assert_eq!(
        compact_json["failed"].as_array().map(Vec::len),
        Some(0),
        "{compact_json}"
    );
    assert!(compact_json.get("steps").is_none(), "{compact_json}");

    // A drag lasts at most as long as the longest wait, so the call returns while the game still
    // answers; a longer one is refused before anything is pressed.
    let long_points: Vec<Value> = (0..12).map(|index| json!([10 + index, 12])).collect();
    let too_long = call(
        &tools,
        "godot_game_input",
        json!({
            "run_id": run_id,
            "steps": [{"type": "path", "points": long_points, "frames_per_point": 60}]
        }),
    )
    .await;
    let too_long_text = text_of(&too_long);
    assert!(
        too_long.is_error,
        "a path of 660 frames must be refused: {too_long_text}"
    );
    assert!(too_long_text.contains("600"), "{too_long_text}");

    let slowed = call(
        &tools,
        "godot_game_wait",
        json!({"run_id": run_id, "time_scale": 1, "frames": 0}),
    )
    .await;
    assert!(!slowed.is_error, "{}", text_of(&slowed));
    assert!(
        (json_of(&slowed)["time_scale"].as_f64().unwrap_or(0.0) - 1.0).abs() < 0.01,
        "{}",
        text_of(&slowed)
    );

    let waited_signal = call(
        &tools,
        "godot_game_wait",
        json!({"run_id": run_id, "node": ".", "signal": "pinged", "timeout": 30}),
    )
    .await;
    assert!(!waited_signal.is_error, "{}", text_of(&waited_signal));
    assert_eq!(
        json_of(&waited_signal)["signal"],
        "pinged",
        "{}",
        text_of(&waited_signal)
    );

    let missing = call(
        &tools,
        "godot_game_wait",
        json!({"run_id": run_id, "node": "Nope", "timeout": 2}),
    )
    .await;
    assert!(missing.is_error, "{}", text_of(&missing));
    assert!(
        text_of(&missing).contains("Timed out waiting for 'Nope'"),
        "{}",
        text_of(&missing)
    );

    let stuck = call(
        &tools,
        "godot_game_input",
        json!({
            "run_id": run_id,
            "steps": [
                {"type": "key", "key": "B", "phase": "press"},
                {"type": "click_node", "node": "Nope"}
            ]
        }),
    )
    .await;
    assert!(stuck.is_error, "{}", text_of(&stuck));
    let cleared = call(
        &tools,
        "godot_game_input_release",
        json!({"run_id": run_id}),
    )
    .await;
    assert!(!cleared.is_error, "{}", text_of(&cleared));
    let cleared_held = &json_of(&cleared)["held"];
    assert_eq!(cleared_held["keys"].as_i64(), Some(0), "{cleared_held}");

    let logs = call(
        &tools,
        "godot_run_output",
        json!({"run_id": run_id, "level": "error"}),
    )
    .await;
    assert!(!logs.is_error, "{}", text_of(&logs));
    assert!(
        text_of(&logs).contains("mcp-probe-error"),
        "{}",
        text_of(&logs)
    );

    let paused = call(
        &tools,
        "godot_debug_control",
        json!({"run_id": run_id, "action": "pause", "wait_seconds": 8}),
    )
    .await;
    assert!(!paused.is_error, "{}", text_of(&paused));
    let state = call(&tools, "godot_debug_state", json!({"run_id": run_id})).await;
    assert!(!state.is_error, "{}", text_of(&state));
    let state_json = json_of(&state);
    assert_eq!(state_json["stopped"], true, "{state_json}");
    // core:break stops between frames and has no script stack. A line breakpoint does.
    assert_eq!(state_json["stop"]["reason"], "pause", "{state_json}");
    // A stopped game cannot answer the runtime, so its output has to come from what the
    // server kept, at once.
    let asked = std::time::Instant::now();
    let while_stopped = call(
        &tools,
        "godot_run_output",
        json!({"run_id": run_id, "level": "error"}),
    )
    .await;
    assert!(!while_stopped.is_error, "{}", text_of(&while_stopped));
    assert!(
        asked.elapsed() < std::time::Duration::from_secs(5),
        "the output of a stopped game took {:?}",
        asked.elapsed()
    );
    assert!(
        text_of(&while_stopped).contains("mcp-probe-error"),
        "{}",
        text_of(&while_stopped)
    );
    let resumed = call(
        &tools,
        "godot_debug_control",
        json!({"run_id": run_id, "action": "resume"}),
    )
    .await;
    assert!(!resumed.is_error, "{}", text_of(&resumed));

    // The line of `hits += 1` in `_process`, wherever the fixture script puts it.
    let process_line = std::fs::read_to_string(project.path().join("scripts/probe.gd"))
        .expect("probe.gd")
        .lines()
        .position(|line| line.trim() == "hits += 1")
        .expect("the hit counter line")
        + 1;
    let armed = call(
        &tools,
        "godot_debug_breakpoints",
        json!({"run_id": run_id, "action": "add", "breakpoint": format!("res://scripts/probe.gd:{process_line}")}),
    )
    .await;
    assert!(!armed.is_error, "{}", text_of(&armed));
    let mut hit = Value::Null;
    for _ in 0..40 {
        let state = call(&tools, "godot_debug_state", json!({"run_id": run_id})).await;
        assert!(!state.is_error, "{}", text_of(&state));
        let state_json = json_of(&state);
        if state_json["stopped"] == true && state_json["stop"]["reason"] == "breakpoint" {
            hit = state_json;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    let stack = hit["stack"].as_array().cloned().unwrap_or_default();
    assert!(
        stack.iter().any(|frame| frame["function"]
            .as_str()
            .unwrap_or("")
            .contains("_process")),
        "breakpoint did not stop in _process: {hit}"
    );
    let resumed = call(
        &tools,
        "godot_debug_control",
        json!({"run_id": run_id, "action": "resume"}),
    )
    .await;
    assert!(!resumed.is_error, "{}", text_of(&resumed));

    let stopped = call(&tools, "godot_stop_project", json!({"run_id": run_id})).await;
    assert!(!stopped.is_error, "{}", text_of(&stopped));
    let override_cfg =
        std::fs::read_to_string(project.path().join("override.cfg")).unwrap_or_default();
    assert!(
        !override_cfg.contains("godot_mcp: temporary runtime autoload"),
        "override.cfg was left injected: {override_cfg}"
    );
    assert!(
        !project.path().join("addons").exists(),
        "the run left runtime files in a project that had no addon"
    );
    assert!(
        !project.path().join(".godot/godot_mcp").exists(),
        "the run left its private copy of the runtime behind"
    );
}

fn is_sha256(value: &Value) -> bool {
    value
        .as_str()
        .is_some_and(|text| text.len() == 64 && text.chars().all(|c| c.is_ascii_hexdigit()))
}

fn observe_project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("project");
    let root = dir.path();
    std::fs::create_dir_all(root.join("scenes")).unwrap();
    std::fs::create_dir_all(root.join("scripts")).unwrap();
    std::fs::write(
        root.join("project.godot"),
        "config_version=5\n\n[application]\n\nconfig/name=\"ObserveProbe\"\nrun/main_scene=\"res://scenes/observe.tscn\"\nconfig/features=PackedStringArray(\"4.5\")\n",
    )
    .unwrap();
    std::fs::write(
        root.join("scripts/observe.gd"),
        r#"extends Node2D
var hp := 1
var stats := {"hp": 1}
var samples := [1, 2]
var mark: Node2D
var goal: Variant = null
var physics_ticks := 0
var physics_time := 0.0
var slide := 0.0
var _secret := 1

func _ready() -> void:
    mark = get_node("Mark")
    push_error("observe-ready")

func _process(_delta: float) -> void:
    queue_redraw()

func _physics_process(delta: float) -> void:
    physics_ticks += 1
    physics_time += delta

func _draw() -> void:
    draw_rect(Rect2(slide, 0, 80, 80), Color(1, 0, 0))

func nudge() -> void:
    slide = 80.0
    queue_redraw()

func reload_scene() -> void:
    get_tree().reload_current_scene()

func open_next() -> void:
    get_tree().change_scene_to_file("res://scenes/next.tscn")

func timing() -> Dictionary:
    var always := get_node_or_null("Always")
    return {
        "time_scale": Engine.time_scale,
        "physics_ticks_per_second": Engine.physics_ticks_per_second,
        "max_physics_steps_per_frame": Engine.max_physics_steps_per_frame,
        "paused": get_tree().paused,
        "physics_ticks": physics_ticks,
        "physics_time": physics_time,
        "always_ticks": always.ticks if always != null else -1,
    }

func complain() -> void:
    push_error("observe-later")
"#,
    )
    .unwrap();
    std::fs::write(
        root.join("scripts/always.gd"),
        "extends Node\nvar ticks := 0\nfunc _ready() -> void:\n\tprocess_mode = Node.PROCESS_MODE_ALWAYS\nfunc _physics_process(_delta: float) -> void:\n\tticks += 1\n",
    )
    .unwrap();
    std::fs::write(
        root.join("scenes/observe.tscn"),
        "[gd_scene load_steps=3 format=3]\n\n[ext_resource type=\"Script\" path=\"res://scripts/observe.gd\" id=\"1\"]\n[ext_resource type=\"Script\" path=\"res://scripts/always.gd\" id=\"2\"]\n\n[node name=\"Observe\" type=\"Node2D\"]\nscript = ExtResource(\"1\")\n\n[node name=\"Mark\" type=\"Node2D\" parent=\".\"]\nposition = Vector2(40, 70)\n\n[node name=\"Always\" type=\"Node\" parent=\".\"]\nscript = ExtResource(\"2\")\n",
    )
    .unwrap();
    dir
}

async fn start_observe(headless: bool) -> (tempfile::TempDir, Arc<ToolManager>, String) {
    let project = observe_project();
    let tools = manager(project.path());
    let started = call(
        &tools,
        "godot_run_project",
        json!({
            "project_path": project.path(),
            "scene": "res://scenes/observe.tscn",
            "mode": "process",
            "headless": headless
        }),
    )
    .await;
    assert!(!started.is_error, "{}", text_of(&started));
    let run_id = run_id_of(&started);
    (project, tools, run_id)
}

fn call_value(body: &Value) -> Value {
    body.get("value").cloned().unwrap_or(Value::Null)
}

fn assert_error(result: &ToolResult, needle: &str) {
    let text = text_of(result);
    assert!(
        result.is_error,
        "expected an error mentioning {needle}: {text}"
    );
    assert!(
        text.to_lowercase().contains(&needle.to_lowercase()),
        "error should mention {needle}: {text}"
    );
}

#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn headless_game_waits_for_a_value_a_new_scene_and_real_physics_ticks() {
    let _ = IGNORE;
    let (project, tools, run_id) = start_observe(true).await;

    let snap = call(&tools, "godot_game_snapshot", json!({"run_id": run_id})).await;
    assert!(!snap.is_error, "{}", text_of(&snap));
    let snap_json = json_of(&snap);
    let generation = snap_json["scene_generation"].as_i64().expect("generation");
    let instance = snap_json["scene_instance_id"].as_i64().expect("instance");
    assert!(generation >= 1, "{snap_json}");
    assert!(instance > 0, "{snap_json}");
    assert_eq!(
        snap_json["script_sha256_basis"], "loaded_source_at_scene_enter",
        "{snap_json}"
    );
    assert!(is_sha256(&snap_json["script_sha256"]), "{snap_json}");
    assert!(
        snap_json["script_path"]
            .as_str()
            .unwrap_or("")
            .ends_with("observe.gd"),
        "{snap_json}"
    );
    assert!(
        snap_json["script_sha256_note"]
            .as_str()
            .unwrap_or("")
            .contains("not a later read"),
        "{snap_json}"
    );
    let loaded_hash = snap_json["script_sha256"].as_str().unwrap().to_string();
    let scene_path = snap_json["scene"].as_str().unwrap_or("").to_string();

    let logs = call(
        &tools,
        "godot_run_output",
        json!({"run_id": run_id, "level": "error"}),
    )
    .await;
    assert!(!logs.is_error, "{}", text_of(&logs));
    let logs_json = json_of(&logs);
    let ready = logs_json["lines"]
        .as_array()
        .and_then(|lines| {
            lines.iter().find(|line| {
                line["text"]
                    .as_str()
                    .unwrap_or("")
                    .contains("observe-ready")
            })
        })
        .expect("observe-ready was not logged");
    assert_eq!(ready["scene"]["script_sha256"], loaded_hash, "{ready}");
    assert_eq!(
        ready["scene"]["script_sha256_basis"], "loaded_source_at_scene_enter",
        "{ready}"
    );

    let eq = call(
        &tools,
        "godot_game_wait",
        json!({"run_id": run_id, "node": ".", "property": "hp", "op": "eq", "value": 1}),
    )
    .await;
    assert!(!eq.is_error, "{}", text_of(&eq));
    let eq_json = json_of(&eq);
    assert_eq!(eq_json["observed"], 1, "{eq_json}");
    assert_eq!(
        eq_json["scene"]["scene_generation"], generation,
        "{eq_json}"
    );

    let nested = call(
        &tools,
        "godot_game_wait",
        json!({"run_id": run_id, "node": ".", "property": "stats/hp", "op": "gte", "value": 1}),
    )
    .await;
    assert!(!nested.is_error, "{}", text_of(&nested));
    assert_eq!(json_of(&nested)["observed"], 1, "{}", text_of(&nested));

    let exists = call(
        &tools,
        "godot_game_wait",
        json!({"run_id": run_id, "node": ".", "property": "mark/position", "op": "exists"}),
    )
    .await;
    assert!(!exists.is_error, "{}", text_of(&exists));
    assert_eq!(json_of(&exists)["exists"], true, "{}", text_of(&exists));

    // null compares with any type: eq null holds only for a null value, ne null for any
    // other, so a wait can see a Variant property being cleared.
    let is_null = call(
        &tools,
        "godot_game_wait",
        json!({"run_id": run_id, "node": ".", "property": "goal", "op": "eq", "value": null}),
    )
    .await;
    assert!(!is_null.is_error, "{}", text_of(&is_null));
    assert!(
        json_of(&is_null)["observed"].is_null(),
        "{}",
        text_of(&is_null)
    );
    for property in ["hp", "mark", "stats"] {
        let not_null = call(
            &tools,
            "godot_game_wait",
            json!({"run_id": run_id, "node": ".", "property": property, "op": "ne", "value": null}),
        )
        .await;
        assert!(!not_null.is_error, "{property}: {}", text_of(&not_null));
    }
    let never_null = call(
        &tools,
        "godot_game_wait",
        json!({"run_id": run_id, "node": ".", "property": "hp", "op": "eq", "value": null, "timeout": 2}),
    )
    .await;
    let never_text = text_of(&never_null);
    assert!(never_null.is_error, "{never_text}");
    assert!(!never_text.contains("Type mismatch"), "{never_text}");
    assert!(never_text.contains("\"observed\""), "{never_text}");
    assert_error(
        &call(
            &tools,
            "godot_game_wait",
            json!({"run_id": run_id, "node": ".", "property": "goal", "op": "lt", "value": null, "timeout": 1}),
        )
        .await,
        "ordering",
    );

    assert_error(
        &call(
            &tools,
            "godot_game_wait",
            json!({"run_id": run_id, "node": ".", "property": "samples/0", "op": "exists", "timeout": 1}),
        )
        .await,
        "array",
    );
    assert_error(
        &call(
            &tools,
            "godot_game_wait",
            json!({"run_id": run_id, "node": ".", "property": "_secret", "op": "exists", "timeout": 1}),
        )
        .await,
        "private",
    );
    assert_error(
        &call(
            &tools,
            "godot_game_wait",
            json!({"run_id": run_id, "node": ".", "property": "script", "op": "exists", "timeout": 1}),
        )
        .await,
        "script",
    );
    assert_error(
        &call(
            &tools,
            "godot_game_wait",
            json!({"run_id": run_id, "node": ".", "property": "hp", "op": "eq", "value": "1", "timeout": 1}),
        )
        .await,
        "type",
    );
    assert_error(
        &call(
            &tools,
            "godot_game_wait",
            json!({"run_id": run_id, "node": ".", "property": "hp", "op": "lt", "value": 1.5, "timeout": 1}),
        )
        .await,
        "type",
    );
    assert_error(
        &call(
            &tools,
            "godot_game_wait",
            json!({"run_id": run_id, "property": "hp", "op": "eq", "value": 1}),
        )
        .await,
        "node",
    );
    assert_error(
        &call(
            &tools,
            "godot_game_wait",
            json!({"run_id": run_id, "node": ".", "property": "hp", "op": "regex", "value": 1}),
        )
        .await,
        "op",
    );
    assert_error(
        &call(
            &tools,
            "godot_game_wait",
            json!({
                "run_id": run_id,
                "node": ".",
                "signal": "ready",
                "property": "hp",
                "op": "eq",
                "value": 1
            }),
        )
        .await,
        "signal",
    );
    let timed = call(
        &tools,
        "godot_game_wait",
        json!({"run_id": run_id, "node": ".", "property": "hp", "op": "eq", "value": 99, "timeout": 2}),
    )
    .await;
    let timed_text = text_of(&timed);
    assert!(timed.is_error, "{timed_text}");
    assert!(
        timed_text.contains("99") || timed_text.contains("Timed out"),
        "{timed_text}"
    );
    assert!(timed_text.contains("\"observed\""), "{timed_text}");
    assert!(timed_text.contains("scene_generation"), "{timed_text}");

    let script = project.path().join("scripts/observe.gd");
    let original = std::fs::read_to_string(&script).unwrap();
    std::fs::write(&script, format!("{original}\n# disk only\n")).unwrap();
    let after_edit = json_of(&call(&tools, "godot_game_snapshot", json!({"run_id": run_id})).await);
    assert_eq!(after_edit["script_sha256"], loaded_hash, "{after_edit}");
    assert_eq!(after_edit["scene_generation"], generation, "{after_edit}");
    assert_eq!(after_edit["scene_instance_id"], instance, "{after_edit}");

    let complained = call(
        &tools,
        "godot_game_call",
        json!({"run_id": run_id, "path": ".", "method": "complain", "permit": ["complain"]}),
    )
    .await;
    assert!(!complained.is_error, "{}", text_of(&complained));
    let later_logs = json_of(
        &call(
            &tools,
            "godot_run_output",
            json!({"run_id": run_id, "level": "error"}),
        )
        .await,
    );
    let later = later_logs["lines"]
        .as_array()
        .and_then(|lines| {
            lines.iter().find(|line| {
                line["text"]
                    .as_str()
                    .unwrap_or("")
                    .contains("observe-later")
            })
        })
        .expect("observe-later was not logged");
    assert_eq!(
        later["scene"]["script_sha256"], loaded_hash,
        "a log line took the hash of the file on disk: {later}"
    );

    let reloaded = call(
        &tools,
        "godot_game_call",
        json!({"run_id": run_id, "path": ".", "method": "reload_scene", "permit": ["reload_scene"]}),
    )
    .await;
    assert!(!reloaded.is_error, "{}", text_of(&reloaded));
    // The scene root left the tree during the call: the answer says so and names the path it
    // had, and the runtime logs no engine error of its own while answering.
    let reloaded_json = json_of(&reloaded);
    assert_eq!(reloaded_json["path"], ".", "{reloaded_json}");
    assert_eq!(reloaded_json["left_tree"], true, "{reloaded_json}");
    assert!(
        reloaded_json["note"]
            .as_str()
            .unwrap_or("")
            .contains("left the scene tree"),
        "{reloaded_json}"
    );
    let waited = call(
        &tools,
        "godot_game_wait",
        json!({"run_id": run_id, "scene_generation": generation, "node": "Mark", "timeout": 180}),
    )
    .await;
    assert!(!waited.is_error, "{}", text_of(&waited));
    let waited_json = json_of(&waited);
    assert!(
        waited_json["scene"]["scene_generation"]
            .as_i64()
            .unwrap_or(0)
            > generation,
        "{waited_json}"
    );
    let reborn = json_of(&call(&tools, "godot_game_snapshot", json!({"run_id": run_id})).await);
    assert_eq!(reborn["scene"], scene_path, "{reborn}");
    assert!(
        reborn["scene_generation"].as_i64().unwrap_or(0) > generation,
        "{reborn}"
    );
    assert_ne!(
        reborn["scene_instance_id"].as_i64(),
        Some(instance),
        "{reborn}"
    );
    assert_eq!(
        reborn["script_sha256"], loaded_hash,
        "reloading the scene reused the script Godot already had loaded: {reborn}"
    );
    let stale = json_of(
        &call(
            &tools,
            "godot_run_output",
            json!({"run_id": run_id, "level": "error"}),
        )
        .await,
    );
    let first_ready = stale["lines"]
        .as_array()
        .and_then(|lines| {
            lines.iter().find(|line| {
                line["text"]
                    .as_str()
                    .unwrap_or("")
                    .contains("observe-ready")
            })
        })
        .expect("the original observe-ready line disappeared");
    assert_eq!(
        first_ready["scene"]["script_sha256"], loaded_hash,
        "an old error was restamped with the reloaded script: {first_ready}"
    );
    let runtime_noise = stale["lines"].as_array().and_then(|lines| {
        lines.iter().find(|line| {
            let text = line["text"].as_str().unwrap_or("");
            text.contains("is_inside_tree") || text.contains("mcp_runtime.gd")
        })
    });
    assert!(
        runtime_noise.is_none(),
        "answering a call that reloaded the scene logged an engine error: {runtime_noise:?}"
    );

    let scaled = call(
        &tools,
        "godot_game_wait",
        json!({"run_id": run_id, "time_scale": 2, "frames": 1}),
    )
    .await;
    assert!(!scaled.is_error, "{}", text_of(&scaled));
    // The first step only parks the tree. The count below starts from that pause,
    // so frames between tool calls cannot add pausable physics ticks.
    let parked = call(
        &tools,
        "godot_game_physics",
        json!({"run_id": run_id, "action": "step", "ticks": 1}),
    )
    .await;
    assert!(!parked.is_error, "{}", text_of(&parked));
    let before = call_value(&json_of(
        &call(
            &tools,
            "godot_game_call",
            json!({"run_id": run_id, "path": ".", "method": "timing", "permit": ["timing"]}),
        )
        .await,
    ));
    assert_eq!(before["paused"], true, "{before}");
    let stepped = call(
        &tools,
        "godot_game_physics",
        json!({"run_id": run_id, "action": "step", "ticks": 4}),
    )
    .await;
    assert!(!stepped.is_error, "{}", text_of(&stepped));
    let stepped_json = json_of(&stepped);
    assert_eq!(stepped_json["ticks"], 4, "{stepped_json}");
    assert_eq!(stepped_json["paused"], true, "{stepped_json}");
    assert_eq!(stepped_json["time_scale"], 2.0, "{stepped_json}");
    assert_eq!(
        stepped_json["physics_ticks_per_second"], before["physics_ticks_per_second"],
        "{stepped_json} vs {before}"
    );
    assert_eq!(
        stepped_json["max_physics_steps_per_frame"], before["max_physics_steps_per_frame"],
        "{stepped_json}"
    );
    let limits = stepped_json["limits"].to_string().to_lowercase();
    assert!(limits.contains("always"), "{stepped_json}");
    assert!(
        limits.contains("_process") || limits.contains("idle"),
        "{stepped_json}"
    );
    assert!(
        limits.contains("gpu") || limits.contains("thread"),
        "{stepped_json}"
    );
    assert!(limits.contains("determin"), "{stepped_json}");

    let mid = call_value(&json_of(
        &call(
            &tools,
            "godot_game_call",
            json!({"run_id": run_id, "path": ".", "method": "timing", "permit": ["timing"]}),
        )
        .await,
    ));
    assert_eq!(mid["paused"], true, "{mid}");
    assert_eq!(mid["time_scale"], 2.0, "{mid}");
    assert_eq!(
        mid["physics_ticks"].as_i64().unwrap_or(-1),
        before["physics_ticks"].as_i64().unwrap_or(-2) + 4,
        "pausable physics did not advance by exactly 4 ticks: before {before} after {mid}"
    );
    // A tick has the normal physics delta: 4 ticks at a time scale of 2 are 4 / ticks per
    // second * 2 seconds of game time, not a sliver of it.
    let per_second = before["physics_ticks_per_second"].as_f64().unwrap_or(60.0);
    let expected_time = 4.0 / per_second * 2.0;
    let advanced_time = mid["physics_time"].as_f64().unwrap_or(-1.0)
        - before["physics_time"].as_f64().unwrap_or(0.0);
    assert!(
        (advanced_time - expected_time).abs() < expected_time * 0.05,
        "four ticks advanced {advanced_time} s of game time, expected {expected_time} s: before {before} after {mid}"
    );
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    let held = call(
        &tools,
        "godot_game_wait",
        json!({"run_id": run_id, "frames": 2}),
    )
    .await;
    assert!(!held.is_error, "{}", text_of(&held));
    let paused_mid = call_value(&json_of(
        &call(
            &tools,
            "godot_game_call",
            json!({"run_id": run_id, "path": ".", "method": "timing", "permit": ["timing"]}),
        )
        .await,
    ));
    assert_eq!(
        paused_mid["physics_ticks"], mid["physics_ticks"],
        "pausable physics ran while the tree stayed paused: {paused_mid}"
    );
    assert!(
        paused_mid["always_ticks"].as_i64().unwrap_or(0)
            > mid["always_ticks"].as_i64().unwrap_or(0),
        "PROCESS_MODE_ALWAYS did not keep ticking while paused: {mid} -> {paused_mid}"
    );

    let resumed = call(
        &tools,
        "godot_game_physics",
        json!({"run_id": run_id, "action": "resume"}),
    )
    .await;
    assert!(!resumed.is_error, "{}", text_of(&resumed));
    assert_eq!(json_of(&resumed)["paused"], false, "{}", text_of(&resumed));
    let running = call(
        &tools,
        "godot_game_wait",
        json!({"run_id": run_id, "frames": 3}),
    )
    .await;
    assert!(!running.is_error, "{}", text_of(&running));
    let after = call_value(&json_of(
        &call(
            &tools,
            "godot_game_call",
            json!({"run_id": run_id, "path": ".", "method": "timing", "permit": ["timing"]}),
        )
        .await,
    ));
    assert!(
        after["physics_ticks"].as_i64().unwrap_or(0)
            > paused_mid["physics_ticks"].as_i64().unwrap_or(0),
        "resume did not continue physics: {after}"
    );

    assert_error(
        &call(
            &tools,
            "godot_game_physics",
            json!({"run_id": run_id, "action": "step", "ticks": 0}),
        )
        .await,
        "ticks",
    );
    assert_error(
        &call(
            &tools,
            "godot_game_physics",
            json!({"run_id": run_id, "action": "step", "ticks": 121}),
        )
        .await,
        "ticks",
    );
    assert_error(
        &call(
            &tools,
            "godot_game_physics",
            json!({"run_id": run_id, "action": "leap", "ticks": 1}),
        )
        .await,
        "action",
    );

    std::fs::write(
        project.path().join("scripts/next.gd"),
        "extends Node2D\nfunc _ready() -> void:\n\tpush_error(\"observe-next\")\n",
    )
    .unwrap();
    std::fs::write(
        project.path().join("scenes/next.tscn"),
        "[gd_scene load_steps=2 format=3]\n\n[ext_resource type=\"Script\" path=\"res://scripts/next.gd\" id=\"1\"]\n\n[node name=\"Next\" type=\"Node2D\"]\nscript = ExtResource(\"1\")\n",
    )
    .unwrap();
    let opened = call(
        &tools,
        "godot_game_call",
        json!({"run_id": run_id, "path": ".", "method": "open_next", "permit": ["open_next"]}),
    )
    .await;
    assert!(!opened.is_error, "{}", text_of(&opened));
    let next_generation = reborn["scene_generation"].as_i64().unwrap_or(0);
    let moved = call(
        &tools,
        "godot_game_wait",
        json!({"run_id": run_id, "scene_generation": next_generation, "node": ".", "timeout": 180}),
    )
    .await;
    assert!(!moved.is_error, "{}", text_of(&moved));
    let next = json_of(&call(&tools, "godot_game_snapshot", json!({"run_id": run_id})).await);
    assert!(
        next["script_path"]
            .as_str()
            .unwrap_or("")
            .ends_with("next.gd"),
        "{next}"
    );
    assert!(is_sha256(&next["script_sha256"]), "{next}");
    assert_ne!(next["script_sha256"], loaded_hash, "{next}");
    assert!(
        next["scene_generation"].as_i64().unwrap_or(0) > next_generation,
        "{next}"
    );

    let stopped = call(&tools, "godot_stop_project", json!({"run_id": run_id})).await;
    assert!(!stopped.is_error, "{}", text_of(&stopped));
}

fn screenshot_size(note: &str) -> (i64, i64) {
    let start = note.find("Game screenshot ").expect("screenshot note");
    let rest = &note[start + "Game screenshot ".len()..];
    let dims = rest.split_whitespace().next().expect("dimensions");
    let (width, height) = dims.split_once('x').expect(dims);
    (width.parse().expect(width), height.parse().expect(height))
}

#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn windowed_screenshot_can_save_and_compare_a_named_baseline() {
    let _ = IGNORE;
    if !windowed_game_can_open() {
        eprintln!("skipped: a windowed screenshot needs a display server");
        return;
    }
    let (_project, tools, run_id) = start_observe(false).await;

    let plain = call(
        &tools,
        "godot_game_screenshot",
        json!({"run_id": run_id, "max_dimension": 64}),
    )
    .await;
    assert!(!plain.is_error, "{}", text_of(&plain));
    assert!(
        text_of(&plain).contains("no earlier capture to compare"),
        "{}",
        text_of(&plain)
    );

    let saved = call(
        &tools,
        "godot_game_screenshot",
        json!({"run_id": run_id, "max_dimension": 64, "save_baseline": "steady"}),
    )
    .await;
    assert!(!saved.is_error, "{}", text_of(&saved));
    let saved_note = text_of(&saved);
    assert!(saved_note.contains("steady"), "{saved_note}");
    let (width, height) = screenshot_size(&saved_note);

    let loose = call(
        &tools,
        "godot_game_screenshot",
        json!({
            "run_id": run_id,
            "max_dimension": 64,
            "baseline": "steady",
            "tolerance": 255
        }),
    )
    .await;
    assert!(!loose.is_error, "{}", text_of(&loose));
    let loose_note = text_of(&loose);
    assert!(loose_note.contains("changed 0"), "{loose_note}");
    assert!(loose_note.contains("exact"), "{loose_note}");
    assert!(loose_note.contains("tolerance 255"), "{loose_note}");
    assert!(loose_note.contains("sampled"), "{loose_note}");

    let nudged = call(
        &tools,
        "godot_game_call",
        json!({"run_id": run_id, "path": ".", "method": "nudge", "permit": ["nudge"]}),
    )
    .await;
    assert!(!nudged.is_error, "{}", text_of(&nudged));
    let _ = call(
        &tools,
        "godot_game_wait",
        json!({"run_id": run_id, "frames": 2}),
    )
    .await;
    let tight = call(
        &tools,
        "godot_game_screenshot",
        json!({"run_id": run_id, "max_dimension": 64, "baseline": "steady", "tolerance": 0}),
    )
    .await;
    assert!(!tight.is_error, "{}", text_of(&tight));
    let tight_note = text_of(&tight);
    assert!(
        tight_note.contains("changed") && !tight_note.contains("changed 0"),
        "{tight_note}"
    );
    assert!(tight_note.contains("exact"), "{tight_note}");
    assert!(tight_note.contains("ratio"), "{tight_note}");
    assert!(tight_note.contains("bounds"), "{tight_note}");

    let roi = call(
        &tools,
        "godot_game_screenshot",
        json!({
            "run_id": run_id,
            "max_dimension": 64,
            "baseline": "steady",
            "tolerance": 0,
            "roi": {"x": width - 8, "y": height - 8, "w": 8, "h": 8}
        }),
    )
    .await;
    assert!(!roi.is_error, "{}", text_of(&roi));
    let roi_note = text_of(&roi);
    assert!(roi_note.contains("changed 0"), "{roi_note}");
    assert!(
        roi_note.contains(&format!("roi {},{},8x8", width - 8, height - 8)),
        "{roi_note}"
    );
    assert!(roi_note.contains("returned_image"), "{roi_note}");

    assert_error(
        &call(
            &tools,
            "godot_game_screenshot",
            json!({"run_id": run_id, "max_dimension": 64, "baseline": "missing"}),
        )
        .await,
        "missing",
    );
    assert_error(
        &call(
            &tools,
            "godot_game_screenshot",
            json!({"run_id": run_id, "max_dimension": 64, "save_baseline": "Bad Name"}),
        )
        .await,
        "name",
    );
    assert_error(
        &call(
            &tools,
            "godot_game_screenshot",
            json!({
                "run_id": run_id,
                "max_dimension": 64,
                "baseline": "steady",
                "roi": {"x": 0, "y": 0, "w": width + 10, "h": 1}
            }),
        )
        .await,
        "roi",
    );
    assert_error(
        &call(
            &tools,
            "godot_game_screenshot",
            json!({"run_id": run_id, "max_dimension": 64, "baseline": "steady", "tolerance": 256}),
        )
        .await,
        "tolerance",
    );
    for name in ["b1", "b2", "b3", "b4", "b5", "b6", "b7"] {
        let shot = call(
            &tools,
            "godot_game_screenshot",
            json!({"run_id": run_id, "max_dimension": 64, "save_baseline": name}),
        )
        .await;
        assert!(!shot.is_error, "{name}: {}", text_of(&shot));
    }
    assert_error(
        &call(
            &tools,
            "godot_game_screenshot",
            json!({"run_id": run_id, "max_dimension": 64, "save_baseline": "b8"}),
        )
        .await,
        "baseline",
    );
    assert_error(
        &call(
            &tools,
            "godot_game_screenshot",
            json!({"run_id": run_id, "max_dimension": 4096, "frames": 4, "save_baseline": "steady"}),
        )
        .await,
        "pixel",
    );

    let sheet = call(
        &tools,
        "godot_game_screenshot",
        json!({"run_id": run_id, "frames": 4, "every": 1, "max_dimension": 64}),
    )
    .await;
    assert!(!sheet.is_error, "{}", text_of(&sheet));
    assert!(text_of(&sheet).contains("4 frames"), "{}", text_of(&sheet));

    let stopped = call(&tools, "godot_stop_project", json!({"run_id": run_id})).await;
    assert!(!stopped.is_error, "{}", text_of(&stopped));
}

#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn language_server_reports_a_real_script_error() {
    let project = project();
    let broken = project.path().join("scripts/broken.gd");
    std::fs::write(&broken, "extends Node\nfunc broken(\n").unwrap();
    let tools = manager(project.path());
    let result = call(
        &tools,
        "godot_code_diagnostics",
        json!({"project_path": project.path(), "script_path": "res://scripts/broken.gd"}),
    )
    .await;
    assert!(!result.is_error, "{}", text_of(&result));
    let body = json_of(&result);
    let diagnostics = body["diagnostics"]
        .as_array()
        .unwrap_or_else(|| panic!("{}", text_of(&result)));
    assert!(
        diagnostics
            .iter()
            .any(|item| item["severity"].as_str() != Some("hint")),
        "{body}"
    );
}

#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn validate_script_lists_a_shadowed_global_when_the_language_server_is_up() {
    let project = project();
    let source = "extends Node\n\nfunc _ready() -> void:\n    var seed := 1\n";
    let script = project.path().join("scripts/seed.gd");
    std::fs::write(&script, source).unwrap();
    let line = source
        .lines()
        .position(|entry| entry.contains("var seed"))
        .expect("seed line")
        + 1;
    let tools = manager(project.path());
    // Starts the language server (or attaches to one already listening) and keeps it on this
    // manager, which is what makes the server reachable for the check below.
    let primed = call(
        &tools,
        "godot_code_diagnostics",
        json!({"project_path": project.path(), "script_path": "res://scripts/seed.gd"}),
    )
    .await;
    assert!(!primed.is_error, "{}", text_of(&primed));
    let result = call(
        &tools,
        "godot_validate_script",
        json!({"project_path": project.path(), "script_path": "res://scripts/seed.gd"}),
    )
    .await;
    assert!(!result.is_error, "{}", text_of(&result));
    let shown = text_of(&result);
    assert!(shown.contains("SHADOWED_GLOBAL_IDENTIFIER"), "{shown}");
    assert!(
        shown.contains(&format!("res://scripts/seed.gd:{line}:")),
        "{shown}"
    );
    assert!(shown.contains("warning"), "{shown}");
}

#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn language_server_answers_the_same_script_again() {
    let project = project();
    let tools = manager(project.path());
    for round in 0..3 {
        let symbols = call(
            &tools,
            "godot_code_symbols",
            json!({"project_path": project.path(), "script_path": "res://scripts/probe.gd"}),
        )
        .await;
        assert!(!symbols.is_error, "round {round}: {}", text_of(&symbols));
        let body = json_of(&symbols);
        assert!(
            body["symbols"].to_string().contains("poke"),
            "round {round}: {body}"
        );
    }
}

/// The diagnostics of `script` in `project` that are errors (LSP severity 1).
async fn script_errors(tools: &Arc<ToolManager>, project: &Path, script: &str) -> Vec<String> {
    let result = call(
        tools,
        "godot_code_diagnostics",
        json!({"project_path": project, "script_path": script}),
    )
    .await;
    assert!(!result.is_error, "{script}: {}", text_of(&result));
    let body = json_of(&result);
    body["diagnostics"]
        .as_array()
        .unwrap_or_else(|| panic!("{script}: {body}"))
        .iter()
        .filter(|item| item["severity"].as_i64() == Some(1))
        .map(|item| item["message"].to_string())
        .collect()
}

/// An editor with another project open holds the language server port, as the first of several
/// open editors does with 6005. Its answers are about its own project: a class the caller's
/// project just declared is unknown there. The tools must use a language server of the caller's
/// project, and one that sees scripts created after it started.
#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn diagnostics_come_from_the_scripts_own_project() {
    let other = project();
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .and_then(|listener| listener.local_addr())
        .expect("free port")
        .port();
    let mut other_editor = tokio::process::Command::new(godot_path())
        .args(["--headless", "--editor", "--path"])
        .arg(other.path())
        .args(["--lsp-port", &port.to_string()])
        .env("GODOT_MCP_NO_BRIDGE", "1")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .expect("start the other project's editor");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while std::net::TcpStream::connect(("127.0.0.1", port)).is_err() {
        assert!(
            std::time::Instant::now() < deadline,
            "the other editor did not open its language server"
        );
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }

    let mine = project();
    let tools = Arc::new(
        ToolManager::new(BridgeClient::new(9_001), CliRunner::new(godot_path()))
            .with_working_dir(mine.path().to_path_buf())
            .with_lsp_port(port),
    );
    let created = call(
        &tools,
        "godot_create_script",
        json!({
            "project_path": mine.path(),
            "script_path": "res://scripts/crate_lift.gd",
            "extends_class": "Node3D",
            "class_name": "CrateLift",
        }),
    )
    .await;
    assert!(!created.is_error, "{}", text_of(&created));
    std::fs::write(
        mine.path().join("scripts/uses_lift.gd"),
        "extends Node\n\nconst LIFT := preload(\"res://scripts/crate_lift.gd\")\n\nfunc _ready() -> void:\n\tvar lift: CrateLift = LIFT.new()\n\tprint(lift)\n",
    )
    .unwrap();
    let errors = script_errors(&tools, mine.path(), "res://scripts/uses_lift.gd").await;
    assert!(errors.is_empty(), "{errors:?}");
    let validated = call(
        &tools,
        "godot_validate_script",
        json!({"project_path": mine.path(), "script_path": "res://scripts/uses_lift.gd"}),
    )
    .await;
    assert!(!validated.is_error, "{}", text_of(&validated));

    // A class declared after the project's language server started is found too.
    let created = call(
        &tools,
        "godot_create_script",
        json!({
            "project_path": mine.path(),
            "script_path": "res://scripts/reed_gate.gd",
            "extends_class": "Node3D",
            "class_name": "ReedGate",
        }),
    )
    .await;
    assert!(!created.is_error, "{}", text_of(&created));
    std::fs::write(
        mine.path().join("scripts/uses_gate.gd"),
        "extends Node\n\nfunc _ready() -> void:\n\tvar gate: ReedGate = ReedGate.new()\n\tprint(gate)\n",
    )
    .unwrap();
    let errors = script_errors(&tools, mine.path(), "res://scripts/uses_gate.gd").await;
    assert!(errors.is_empty(), "{errors:?}");
    let validated = call(
        &tools,
        "godot_validate_script",
        json!({"project_path": mine.path(), "script_path": "res://scripts/uses_gate.gd"}),
    )
    .await;
    assert!(!validated.is_error, "{}", text_of(&validated));

    let _ = other_editor.start_kill();
}

#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn windowed_screenshot_returns_a_sheet_and_a_diff() {
    let _ = IGNORE;
    if !windowed_game_can_open() {
        eprintln!("skipped: a windowed screenshot needs a display server");
        return;
    }
    let project = project();
    let tools = manager(project.path());
    let started = call(
        &tools,
        "godot_run_project",
        json!({
            "project_path": project.path(),
            "scene": "res://scenes/main.tscn",
            "mode": "process",
            "headless": false
        }),
    )
    .await;
    assert!(!started.is_error, "{}", text_of(&started));
    let run_id = run_id_of(&started);

    let first = call(
        &tools,
        "godot_game_screenshot",
        json!({"run_id": run_id, "max_dimension": 128}),
    )
    .await;
    assert!(!first.is_error, "{}", text_of(&first));
    assert!(
        text_of(&first).contains("no earlier capture to compare"),
        "{}",
        text_of(&first)
    );
    assert!(
        text_of(&first).contains("input space") && text_of(&first).contains("image scale"),
        "{}",
        text_of(&first)
    );
    assert!(
        first.content.iter().any(|item| matches!(
            item,
            godot_mcp::protocol::ToolContent::Image { mime_type, .. } if mime_type == "image/png"
        )),
        "windowed capture returned no PNG"
    );

    let _ = call(
        &tools,
        "godot_game_wait",
        json!({"run_id": run_id, "frames": 3}),
    )
    .await;
    let second = call(
        &tools,
        "godot_game_screenshot",
        json!({"run_id": run_id, "max_dimension": 128}),
    )
    .await;
    assert!(!second.is_error, "{}", text_of(&second));
    let second_note = text_of(&second);
    assert!(
        second_note.contains("changed") && !second_note.contains("changed 0 px"),
        "{second_note}"
    );
    let sheet = call(
        &tools,
        "godot_game_screenshot",
        json!({"run_id": run_id, "frames": 4, "every": 1, "max_dimension": 128}),
    )
    .await;
    assert!(!sheet.is_error, "{}", text_of(&sheet));
    let note = text_of(&sheet);
    assert!(note.contains("4 frames, 2 columns"), "{note}");
    assert!(
        note.contains("input space") && note.contains("image scale") && note.contains("per frame"),
        "{note}"
    );
    assert!(
        note.contains("not compared: the previous capture had another size"),
        "{note}"
    );
    let again = call(
        &tools,
        "godot_game_screenshot",
        json!({"run_id": run_id, "frames": 4, "every": 1, "max_dimension": 128}),
    )
    .await;
    assert!(!again.is_error, "{}", text_of(&again));
    assert!(
        text_of(&again).contains("changed"),
        "two sheets of one size are compared: {}",
        text_of(&again)
    );

    let stopped = call(&tools, "godot_stop_project", json!({"run_id": run_id})).await;
    assert!(!stopped.is_error, "{}", text_of(&stopped));
}

/// A window that stops drawing (covered on macOS, minimized) keeps the last drawn frame in its
/// viewport texture, and the engine skips drawing altogether. The capture must draw the current
/// state itself, say so, and stay current: the probe moves its red square every process frame.
#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn windowed_screenshot_draws_a_window_that_stopped_drawing() {
    let _ = IGNORE;
    if !windowed_game_can_open() {
        eprintln!("skipped: a windowed screenshot needs a display server");
        return;
    }
    let project = project();
    let tools = manager(project.path());
    let started = call(
        &tools,
        "godot_run_project",
        json!({
            "project_path": project.path(),
            "scene": "res://scenes/main.tscn",
            "mode": "process",
            "headless": false
        }),
    )
    .await;
    assert!(!started.is_error, "{}", text_of(&started));
    let run_id = run_id_of(&started);

    let fresh = call(
        &tools,
        "godot_game_screenshot",
        json!({"run_id": run_id, "max_dimension": 64}),
    )
    .await;
    assert!(!fresh.is_error, "{}", text_of(&fresh));
    let note = text_of(&fresh);
    assert!(
        note.contains("frames drawn") && !note.contains("STALE"),
        "{note}"
    );
    // A window covered by something outside the test is drawn by the capture as well.
    assert!(
        note.contains("path window") || note.contains("path forced_draw"),
        "{note}"
    );

    let stop = |stopped: bool| json!({"run_id": run_id, "path": ".", "method": "stop_drawing", "permit": ["stop_drawing"], "args": [stopped]});
    let stopped = call(&tools, "godot_game_call", stop(true)).await;
    assert!(!stopped.is_error, "{}", text_of(&stopped));
    for _ in 0..2 {
        let forced = call(
            &tools,
            "godot_game_screenshot",
            json!({"run_id": run_id, "max_dimension": 64}),
        )
        .await;
        assert!(!forced.is_error, "{}", text_of(&forced));
        let note = text_of(&forced);
        assert!(
            note.contains("path forced_draw") && !note.contains("STALE"),
            "{note}"
        );
        assert!(
            !note.contains("changed 0 px"),
            "a frame drawn on demand shows the moved square: {note}"
        );
    }
    let sheet = call(
        &tools,
        "godot_game_screenshot",
        json!({"run_id": run_id, "max_dimension": 64, "frames": 2}),
    )
    .await;
    assert!(!sheet.is_error, "{}", text_of(&sheet));
    assert!(
        text_of(&sheet).contains("path forced_draw") && !text_of(&sheet).contains("STALE"),
        "{}",
        text_of(&sheet)
    );

    let resumed = call(&tools, "godot_game_call", stop(false)).await;
    assert!(!resumed.is_error, "{}", text_of(&resumed));
    let again = call(
        &tools,
        "godot_game_screenshot",
        json!({"run_id": run_id, "max_dimension": 64}),
    )
    .await;
    assert!(!again.is_error, "{}", text_of(&again));
    assert!(!text_of(&again).contains("STALE"), "{}", text_of(&again));

    let ended = call(&tools, "godot_stop_project", json!({"run_id": run_id})).await;
    assert!(!ended.is_error, "{}", text_of(&ended));
}

/// A minimized window draws nothing (the system reports it cannot draw on macOS and Windows).
/// The capture still returns the current state, drawn on demand.
#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn windowed_screenshot_captures_a_minimized_window() {
    let _ = IGNORE;
    if !windowed_game_can_open() {
        eprintln!("skipped: a windowed screenshot needs a display server");
        return;
    }
    let project = project();
    let tools = manager(project.path());
    let started = call(
        &tools,
        "godot_run_project",
        json!({
            "project_path": project.path(),
            "scene": "res://scenes/main.tscn",
            "mode": "process",
            "headless": false
        }),
    )
    .await;
    assert!(!started.is_error, "{}", text_of(&started));
    let run_id = run_id_of(&started);
    let first = call(
        &tools,
        "godot_game_screenshot",
        json!({"run_id": run_id, "max_dimension": 64}),
    )
    .await;
    assert!(!first.is_error, "{}", text_of(&first));

    let minimize = |minimized: bool| json!({"run_id": run_id, "path": ".", "method": "minimize", "permit": ["minimize"], "args": [minimized]});
    let minimized = call(&tools, "godot_game_call", minimize(true)).await;
    assert!(!minimized.is_error, "{}", text_of(&minimized));
    // The window manager minimizes asynchronously; a few captures make sure one is taken while
    // the window is down.
    let mut forced = false;
    for _ in 0..5 {
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        let shot = call(
            &tools,
            "godot_game_screenshot",
            json!({"run_id": run_id, "max_dimension": 1152}),
        )
        .await;
        assert!(!shot.is_error, "{}", text_of(&shot));
        let note = text_of(&shot);
        assert!(!note.contains("STALE"), "{note}");
        // Full size, so the square's few pixels of movement between two captures show.
        assert!(!note.contains("changed 0 px"), "{note}");
        forced |= note.contains("path forced_draw");
    }
    let restored = call(&tools, "godot_game_call", minimize(false)).await;
    assert!(!restored.is_error, "{}", text_of(&restored));
    let ended = call(&tools, "godot_stop_project", json!({"run_id": run_id})).await;
    assert!(!ended.is_error, "{}", text_of(&ended));
    if !forced {
        eprintln!("skipped: this window manager kept drawing the minimized window");
    }
}

/// A window larger than the viewport, so content scale is not 1. Touch positions must arrive
/// in the visible rectangle, and the emulated mouse must share that position and come first.
fn stretched_touch_project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("stretch project");
    let root = dir.path();
    std::fs::create_dir_all(root.join("scripts")).unwrap();
    std::fs::write(
        root.join("project.godot"),
        "config_version=5\n\n[application]\n\nconfig/name=\"StretchProbe\"\nrun/main_scene=\"res://scripts/main.tscn\"\nconfig/features=PackedStringArray(\"4.8\")\n\n[display]\n\nwindow/size/viewport_width=200\nwindow/size/viewport_height=150\nwindow/size/window_width_override=400\nwindow/size/window_height_override=300\nwindow/stretch/mode=\"canvas_items\"\nwindow/stretch/aspect=\"expand\"\n",
    )
    .unwrap();
    std::fs::write(
        root.join("scripts/main.gd"),
        r#"extends Node
var touch_at := Vector2(-1, -1)
var mouse_at := Vector2(-1, -1)
var mouse_device := 0
var order := ""
var emulated := 0

func _input(event: InputEvent) -> void:
    if event is InputEventMouseButton and event.pressed:
        mouse_at = event.position
        mouse_device = event.device
        order += "m"
        if event.device == InputEvent.DEVICE_ID_EMULATION:
            emulated += 1
    elif event is InputEventScreenTouch and event.pressed:
        touch_at = event.position
        order += "t"
"#,
    )
    .unwrap();
    std::fs::write(
        root.join("scripts/main.tscn"),
        "[gd_scene load_steps=2 format=3]\n\n[ext_resource type=\"Script\" path=\"res://scripts/main.gd\" id=\"1\"]\n\n[node name=\"Probe\" type=\"Node\"]\nscript = ExtResource(\"1\")\n",
    )
    .unwrap();
    dir
}

#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn touch_lands_in_viewport_space_and_emulation_can_be_suppressed() {
    let _ = IGNORE;
    if !windowed_game_can_open() {
        eprintln!("skipped: a stretched window needs a display server");
        return;
    }
    let project = stretched_touch_project();
    let tools = manager(project.path());
    let started = call(
        &tools,
        "godot_run_project",
        json!({
            "project_path": project.path(),
            "scene": "res://scripts/main.tscn",
            "mode": "process",
            "headless": false
        }),
    )
    .await;
    assert!(!started.is_error, "{}", text_of(&started));
    let run_id = run_id_of(&started);
    let input = call(
        &tools,
        "godot_game_input",
        json!({
            "run_id": run_id,
            "steps": [{"type": "touch", "x": 40, "y": 50, "phase": "press"}]
        }),
    )
    .await;
    assert!(!input.is_error, "{}", text_of(&input));
    let seen = call(
        &tools,
        "godot_game_get",
        json!({
            "run_id": run_id,
            "path": ".",
            "properties": ["touch_at", "mouse_at", "mouse_device", "order", "emulated"]
        }),
    )
    .await;
    assert!(!seen.is_error, "{}", text_of(&seen));
    let values = &json_of(&seen)["properties"];
    let touch = values["touch_at"].as_array().expect("touch");
    let mouse = values["mouse_at"].as_array().expect("mouse");
    let near = |point: &Vec<Value>, axis: usize, want: f64| {
        (point[axis].as_f64().unwrap_or(999.0) - want).abs() < 1.5
    };
    assert!(near(touch, 0, 40.0) && near(touch, 1, 50.0), "{values}");
    assert!(near(mouse, 0, 40.0) && near(mouse, 1, 50.0), "{values}");
    assert_eq!(values["order"], "mt", "{values}");
    assert_eq!(values["mouse_device"], -1, "{values}");
    assert!(values["emulated"].as_i64().unwrap_or(0) >= 1, "{values}");

    let quiet = call(
        &tools,
        "godot_game_input",
        json!({
            "run_id": run_id,
            "emulate_mouse": false,
            "steps": [{"type": "touch", "x": 10, "y": 12, "index": 1, "phase": "press"}]
        }),
    )
    .await;
    assert!(!quiet.is_error, "{}", text_of(&quiet));
    let after = json_of(
        &call(
            &tools,
            "godot_game_get",
            json!({"run_id": run_id, "path": ".", "properties": ["emulated", "touch_at"]}),
        )
        .await,
    );
    assert_eq!(
        after["properties"]["emulated"], values["emulated"],
        "emulation stayed off: {after}"
    );
    let again = after["properties"]["touch_at"].as_array().expect("touch");
    assert!(near(again, 0, 10.0) && near(again, 1, 12.0), "{after}");

    let stopped = call(&tools, "godot_stop_project", json!({"run_id": run_id})).await;
    assert!(!stopped.is_error, "{}", text_of(&stopped));
}

/// The script of the hold fixture's root node.
const HOLD_SCRIPT: &str = r#"extends Node2D
var physics_ticks := 0
var process_frames := 0
var timer_fires := 0
var trace: Array = []

func _ready() -> void:
    var timer := Timer.new()
    timer.wait_time = 0.02
    timer.autostart = true
    timer.timeout.connect(_on_timer)
    add_child(timer)

func _on_timer() -> void:
    timer_fires += 1

func _process(_delta: float) -> void:
    process_frames += 1
    queue_redraw()

func _physics_process(_delta: float) -> void:
    physics_ticks += 1
    queue_redraw()
    trace.append([physics_ticks, Input.is_key_pressed(KEY_SPACE), Input.is_action_just_pressed("ui_accept")])

func _draw() -> void:
    draw_rect(Rect2(float((process_frames + physics_ticks) % 20) * 40.0, 8, 24, 24), Color(1, 0, 0))

func set_tick_rate(rate: int) -> void:
    Engine.physics_ticks_per_second = rate

func stop_drawing(stopped: bool) -> bool:
    RenderingServer.render_loop_enabled = not stopped
    return stopped

func trace_log() -> Array:
    return trace

func clear_trace() -> void:
    trace = []

func counters() -> Dictionary:
    var always := get_node_or_null("Always")
    return {
        "space_down": Input.is_key_pressed(KEY_SPACE),
        "physics_ticks": physics_ticks,
        "process_frames": process_frames,
        "timer_fires": timer_fires,
        "always_frames": always.frames if always != null else -1,
        "engine_physics_frames": Engine.get_physics_frames(),
        "engine_process_frames": Engine.get_process_frames(),
        "paused": get_tree().paused,
    }
"#;

/// The 1-based line of the first line of the hold fixture's script that has `needle` in it.
fn hold_script_line(needle: &str) -> usize {
    HOLD_SCRIPT
        .lines()
        .position(|line| line.contains(needle))
        .map(|index| index + 1)
        .unwrap_or_else(|| panic!("no line with {needle}"))
}

/// A game with counters for every kind of time: physics ticks, process frames, a timer and a
/// node that keeps running while the tree is paused.
fn hold_project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("project");
    let root = dir.path();
    std::fs::create_dir_all(root.join("scenes")).unwrap();
    std::fs::create_dir_all(root.join("scripts")).unwrap();
    std::fs::write(
        root.join("project.godot"),
        "config_version=5\n\n[application]\n\nconfig/name=\"HoldProbe\"\nrun/main_scene=\"res://scenes/hold.tscn\"\nconfig/features=PackedStringArray(\"4.5\")\n",
    )
    .unwrap();
    std::fs::write(root.join("scripts/hold.gd"), HOLD_SCRIPT).unwrap();
    std::fs::write(
        root.join("scripts/always.gd"),
        "extends Node\nvar frames := 0\nfunc _ready() -> void:\n\tprocess_mode = Node.PROCESS_MODE_ALWAYS\nfunc _process(_delta: float) -> void:\n\tframes += 1\n",
    )
    .unwrap();
    std::fs::write(
        root.join("scenes/hold.tscn"),
        "[gd_scene load_steps=3 format=3]\n\n[ext_resource type=\"Script\" path=\"res://scripts/hold.gd\" id=\"1\"]\n[ext_resource type=\"Script\" path=\"res://scripts/always.gd\" id=\"2\"]\n\n[node name=\"Hold\" type=\"Node2D\"]\nscript = ExtResource(\"1\")\n\n[node name=\"Always\" type=\"Node\" parent=\".\"]\nscript = ExtResource(\"2\")\n",
    )
    .unwrap();
    dir
}

async fn start_hold(headless: bool) -> (tempfile::TempDir, Arc<ToolManager>, String) {
    let project = hold_project();
    let tools = manager(project.path());
    let started = call(
        &tools,
        "godot_run_project",
        json!({
            "project_path": project.path(),
            "scene": "res://scenes/hold.tscn",
            "mode": "process",
            "headless": headless
        }),
    )
    .await;
    assert!(!started.is_error, "{}", text_of(&started));
    let run_id = run_id_of(&started);
    (project, tools, run_id)
}

async fn counters(tools: &Arc<ToolManager>, run_id: &str) -> Value {
    let reply = call(
        tools,
        "godot_game_call",
        json!({"run_id": run_id, "path": ".", "method": "counters", "permit": ["counters"]}),
    )
    .await;
    assert!(!reply.is_error, "{}", text_of(&reply));
    call_value(&json_of(&reply))
}

async fn process_run(tools: &Arc<ToolManager>, run_id: &str) -> Value {
    let status = json_of(&call(tools, "godot_get_run_status", json!({})).await);
    status["process_runs"]
        .as_array()
        .and_then(|runs| runs.iter().find(|run| run["run_id"] == run_id))
        .cloned()
        .unwrap_or_else(|| panic!("no process run {run_id} in {status}"))
}

#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn held_game_stands_still_serves_reads_and_resumes() {
    let _ = IGNORE;
    let (_project, tools, run_id) = start_hold(true).await;

    let running = call(
        &tools,
        "godot_game_wait",
        json!({"run_id": run_id, "frames": 5}),
    )
    .await;
    assert!(!running.is_error, "{}", text_of(&running));
    let free = counters(&tools, &run_id).await;
    assert!(free["physics_ticks"].as_i64().unwrap_or(0) > 0, "{free}");

    let held = call(
        &tools,
        "godot_game_physics",
        json!({"run_id": run_id, "action": "hold"}),
    )
    .await;
    assert!(!held.is_error, "{}", text_of(&held));
    assert_eq!(json_of(&held)["held"], true, "{}", text_of(&held));

    let first = counters(&tools, &run_id).await;
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    let second = counters(&tools, &run_id).await;
    for key in [
        "physics_ticks",
        "process_frames",
        "timer_fires",
        "always_frames",
        "engine_physics_frames",
        "engine_process_frames",
    ] {
        assert_eq!(
            first[key], second[key],
            "{key} advanced while the game was held: {first} -> {second}"
        );
    }
    assert!(
        first["physics_ticks"].as_i64().unwrap_or(0) >= free["physics_ticks"].as_i64().unwrap_or(0),
        "{free} -> {first}"
    );

    let snapshot = call(&tools, "godot_game_snapshot", json!({"run_id": run_id})).await;
    assert!(!snapshot.is_error, "{}", text_of(&snapshot));
    let read = call(
        &tools,
        "godot_game_get",
        json!({"run_id": run_id, "path": ".", "properties": ["physics_ticks"]}),
    )
    .await;
    assert!(!read.is_error, "{}", text_of(&read));
    assert_eq!(
        counters(&tools, &run_id).await["physics_ticks"],
        first["physics_ticks"]
    );

    let status = call(
        &tools,
        "godot_game_physics",
        json!({"run_id": run_id, "action": "status"}),
    )
    .await;
    assert!(!status.is_error, "{}", text_of(&status));
    assert_eq!(json_of(&status)["held"], true, "{}", text_of(&status));

    let resumed = call(
        &tools,
        "godot_game_physics",
        json!({"run_id": run_id, "action": "resume"}),
    )
    .await;
    assert!(!resumed.is_error, "{}", text_of(&resumed));
    assert_eq!(json_of(&resumed)["held"], false, "{}", text_of(&resumed));
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    let after = counters(&tools, &run_id).await;
    for key in [
        "physics_ticks",
        "process_frames",
        "timer_fires",
        "always_frames",
    ] {
        assert!(
            after[key].as_i64().unwrap_or(0) > second[key].as_i64().unwrap_or(0),
            "{key} did not advance after resume: {second} -> {after}"
        );
    }

    // A game that is held when the run is stopped ends with the run.
    let held_again = call(
        &tools,
        "godot_game_physics",
        json!({"run_id": run_id, "action": "hold"}),
    )
    .await;
    assert!(!held_again.is_error, "{}", text_of(&held_again));
    let stopped = call(&tools, "godot_stop_project", json!({"run_id": run_id})).await;
    assert!(!stopped.is_error, "{}", text_of(&stopped));
    assert_eq!(process_run(&tools, &run_id).await["running"], false);
}

#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn held_game_refuses_what_needs_time_to_pass_and_stays_held() {
    let _ = IGNORE;
    let (_project, tools, run_id) = start_hold(true).await;
    let held = call(
        &tools,
        "godot_game_physics",
        json!({"run_id": run_id, "action": "hold"}),
    )
    .await;
    assert!(!held.is_error, "{}", text_of(&held));

    let started = std::time::Instant::now();
    assert_error(
        &call(
            &tools,
            "godot_game_wait",
            json!({"run_id": run_id, "frames": 3}),
        )
        .await,
        "held",
    );
    assert!(
        started.elapsed() < std::time::Duration::from_secs(5),
        "a refusal must not wait for time to pass"
    );
    // The game still answers and still holds after the refusal.
    let before = counters(&tools, &run_id).await;
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert_eq!(
        counters(&tools, &run_id).await["engine_process_frames"],
        before["engine_process_frames"]
    );

    let stopped = call(&tools, "godot_stop_project", json!({"run_id": run_id})).await;
    assert!(!stopped.is_error, "{}", text_of(&stopped));
}

/// A person closing the window of a held game must end the run: the hold keeps the window
/// responsive and hands the close request to the game, which quits as it normally would.
#[cfg(windows)]
#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn closing_the_window_of_a_held_game_ends_the_run() {
    let _ = IGNORE;
    let (_project, tools, run_id) = start_hold(false).await;
    let held = call(
        &tools,
        "godot_game_physics",
        json!({"run_id": run_id, "action": "hold"}),
    )
    .await;
    assert!(!held.is_error, "{}", text_of(&held));
    let pid = process_run(&tools, &run_id).await["pid"]
        .as_u64()
        .expect("pid");

    let closed = std::process::Command::new("powershell")
        .args([
            "-NoProfile",
            "-Command",
            &format!(
                "if ((Get-Process -Id {pid}).CloseMainWindow()) {{ exit 0 }} else {{ exit 3 }}"
            ),
        ])
        .status()
        .expect("powershell");
    assert!(closed.success(), "the game has no window to close");

    let mut ended = false;
    for _ in 0..50 {
        if process_run(&tools, &run_id).await["running"] == false {
            ended = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    assert!(ended, "the held game ignored its window being closed");
}

async fn held_step(tools: &Arc<ToolManager>, run_id: &str, extra: Value) -> ToolResult {
    let mut args = json!({"run_id": run_id, "action": "step"});
    if let (Some(base), Some(more)) = (args.as_object_mut(), extra.as_object()) {
        for (key, value) in more {
            base.insert(key.clone(), value.clone());
        }
    }
    call(tools, "godot_game_physics", args).await
}

async fn hold_now(tools: &Arc<ToolManager>, run_id: &str) {
    let held = call(
        tools,
        "godot_game_physics",
        json!({"run_id": run_id, "action": "hold"}),
    )
    .await;
    assert!(!held.is_error, "{}", text_of(&held));
}

#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn held_step_runs_exactly_the_ticks_asked_and_reports_exactness() {
    let _ = IGNORE;
    let (_project, tools, run_id) = start_hold(true).await;
    hold_now(&tools, &run_id).await;

    let start = counters(&tools, &run_id).await;
    let five = held_step(&tools, &run_id, json!({"ticks": 5})).await;
    assert!(!five.is_error, "{}", text_of(&five));
    let five_json = json_of(&five);
    assert_eq!(five_json["ticks_run"], 5, "{five_json}");
    assert_eq!(five_json["held"], true, "{five_json}");
    // The pause-based step is not what ran: the game was never paused.
    assert_eq!(five_json["paused"], false, "{five_json}");
    let after_five = counters(&tools, &run_id).await;
    let delta = |after: &Value, before: &Value, key: &str| {
        after[key].as_i64().unwrap_or(-1) - before[key].as_i64().unwrap_or(-100)
    };
    assert_eq!(
        delta(&after_five, &start, "physics_ticks"),
        5,
        "{start} -> {after_five}"
    );
    assert_eq!(delta(&after_five, &start, "engine_physics_frames"), 5);

    // Two steps of 3 equal one of 6.
    let a = held_step(&tools, &run_id, json!({"ticks": 3})).await;
    let b = held_step(&tools, &run_id, json!({"ticks": 3})).await;
    assert!(
        !a.is_error && !b.is_error,
        "{} {}",
        text_of(&a),
        text_of(&b)
    );
    let after_split = counters(&tools, &run_id).await;
    let six = held_step(&tools, &run_id, json!({"ticks": 6})).await;
    assert!(!six.is_error, "{}", text_of(&six));
    let after_six = counters(&tools, &run_id).await;
    assert_eq!(
        delta(&after_split, &after_five, "physics_ticks"),
        delta(&after_six, &after_split, "physics_ticks"),
        "{after_five} -> {after_split} -> {after_six}"
    );
    assert_eq!(delta(&after_six, &after_split, "physics_ticks"), 6);

    // A game that was not started held is never reported as exact.
    let report = json_of(&six);
    assert_eq!(report["deterministic"], false, "{report}");
    assert_eq!(
        report["deterministic_reason"], "not_started_held",
        "{report}"
    );
    assert!(
        report["total_ticks"].as_i64().unwrap_or(0) >= 14,
        "total ticks count every held tick: {report}"
    );

    assert_error(
        &held_step(&tools, &run_id, json!({"ticks": 0})).await,
        "ticks",
    );
    assert_error(
        &held_step(&tools, &run_id, json!({"ticks": 601})).await,
        "ticks",
    );
    assert_error(&held_step(&tools, &run_id, json!({})).await, "ticks");
    assert_error(
        &held_step(&tools, &run_id, json!({"ticks": 2, "max": 5})).await,
        "max",
    );

    let stopped = call(&tools, "godot_stop_project", json!({"run_id": run_id})).await;
    assert!(!stopped.is_error, "{}", text_of(&stopped));
}

#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn held_step_stops_on_a_condition_and_is_bounded() {
    let _ = IGNORE;
    let (_project, tools, run_id) = start_hold(true).await;

    // A condition needs a held game.
    assert_error(
        &held_step(
            &tools,
            &run_id,
            json!({"until": {"node": ".", "property": "physics_ticks", "op": "gte", "value": 1}}),
        )
        .await,
        "hold",
    );

    hold_now(&tools, &run_id).await;
    let start = counters(&tools, &run_id).await;
    let target = start["physics_ticks"].as_i64().unwrap_or(0) + 3;

    let reached = held_step(
        &tools,
        &run_id,
        json!({"until": {"node": ".", "property": "physics_ticks", "op": "gte", "value": target}, "max": 100}),
    )
    .await;
    assert!(!reached.is_error, "{}", text_of(&reached));
    let reached_json = json_of(&reached);
    assert_eq!(reached_json["ticks_run"], 3, "{reached_json}");
    assert_eq!(reached_json["until_met"], true, "{reached_json}");
    assert_eq!(reached_json["held"], true, "{reached_json}");

    // A condition that already holds runs no tick.
    let already = held_step(
        &tools,
        &run_id,
        json!({"until": {"node": ".", "property": "physics_ticks", "op": "gte", "value": target}}),
    )
    .await;
    assert!(!already.is_error, "{}", text_of(&already));
    assert_eq!(json_of(&already)["ticks_run"], 0, "{}", text_of(&already));
    assert_eq!(
        json_of(&already)["until_met"],
        true,
        "{}",
        text_of(&already)
    );

    // A condition that never holds stops at the maximum.
    let never = held_step(
        &tools,
        &run_id,
        json!({"until": {"node": ".", "property": "physics_ticks", "op": "lt", "value": 0}, "max": 7}),
    )
    .await;
    assert!(!never.is_error, "{}", text_of(&never));
    assert_eq!(json_of(&never)["ticks_run"], 7, "{}", text_of(&never));
    assert_eq!(json_of(&never)["until_met"], false, "{}", text_of(&never));

    // A condition that cannot be read does not hold, and does not end the step early.
    let missing = held_step(
        &tools,
        &run_id,
        json!({"until": {"node": "Nowhere", "property": "physics_ticks", "op": "gte", "value": 1}, "max": 4}),
    )
    .await;
    assert!(!missing.is_error, "{}", text_of(&missing));
    assert_eq!(json_of(&missing)["ticks_run"], 4, "{}", text_of(&missing));
    assert_eq!(
        json_of(&missing)["until_met"],
        false,
        "{}",
        text_of(&missing)
    );

    // Conditions that do not parse are refused before any tick runs.
    let before = counters(&tools, &run_id).await;
    for bad in [
        json!({"node": ".", "property": "physics_ticks", "op": "zz", "value": 1}),
        json!({"node": ".", "property": "physics_ticks", "op": "gte"}),
        json!({"node": ".", "property": "_secret", "op": "exists"}),
        json!({"property": "physics_ticks", "op": "exists"}),
    ] {
        let refused = held_step(&tools, &run_id, json!({"until": bad})).await;
        assert!(refused.is_error, "{}", text_of(&refused));
    }
    assert_error(
        &held_step(
            &tools,
            &run_id,
            json!({"until": {"node": ".", "property": "physics_ticks", "op": "exists"}, "max": 3601}),
        )
        .await,
        "max",
    );
    assert_error(
        &held_step(
            &tools,
            &run_id,
            json!({"ticks": 2, "until": {"node": ".", "property": "physics_ticks", "op": "exists"}}),
        )
        .await,
        "max",
    );
    assert_eq!(
        counters(&tools, &run_id).await["physics_ticks"],
        before["physics_ticks"],
        "a refused step ran a tick"
    );

    let stopped = call(&tools, "godot_stop_project", json!({"run_id": run_id})).await;
    assert!(!stopped.is_error, "{}", text_of(&stopped));
}

#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn a_hold_clears_the_pause_the_pause_based_step_left() {
    let _ = IGNORE;
    let (_project, tools, run_id) = start_hold(true).await;
    let paused = call(
        &tools,
        "godot_game_physics",
        json!({"run_id": run_id, "action": "step", "ticks": 2}),
    )
    .await;
    assert!(!paused.is_error, "{}", text_of(&paused));
    assert_eq!(json_of(&paused)["paused"], true, "{}", text_of(&paused));

    hold_now(&tools, &run_id).await;
    let start = counters(&tools, &run_id).await;
    assert_eq!(start["paused"], false, "{start}");
    let stepped = held_step(&tools, &run_id, json!({"ticks": 3})).await;
    assert!(!stepped.is_error, "{}", text_of(&stepped));
    let after = counters(&tools, &run_id).await;
    assert_eq!(
        after["physics_ticks"].as_i64().unwrap_or(0) - start["physics_ticks"].as_i64().unwrap_or(0),
        3
    );

    let stopped = call(&tools, "godot_stop_project", json!({"run_id": run_id})).await;
    assert!(!stopped.is_error, "{}", text_of(&stopped));
}

#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn held_game_refuses_every_operation_that_needs_time_and_serves_the_rest() {
    let _ = IGNORE;
    let (_project, tools, run_id) = start_hold(true).await;
    hold_now(&tools, &run_id).await;
    let start = counters(&tools, &run_id).await;

    let refused: Vec<(&str, Value)> = vec![
        ("godot_game_wait", json!({"frames": 3})),
        (
            "godot_game_wait",
            json!({"node": ".", "property": "physics_ticks", "op": "gte", "value": 1}),
        ),
        (
            "godot_game_observe",
            json!({"path": ".", "signals": ["ready"], "frames": 3}),
        ),
        ("godot_game_screenshot", json!({"frames": 2})),
        ("godot_game_screenshot", json!({"camera": "Camera"})),
        ("godot_game_record", json!({"frames": 2})),
        (
            "godot_game_input",
            json!({"steps": [{"type": "wait", "frames": 2}]}),
        ),
        (
            "godot_game_input",
            json!({"steps": [{"type": "text", "text": "hi"}]}),
        ),
        (
            "godot_game_input",
            json!({"steps": [{"type": "swipe", "x": 1, "y": 1, "to_x": 5, "to_y": 5}]}),
        ),
        (
            "godot_game_input",
            json!({"steps": [{"type": "path", "points": [[1, 1], [4, 1], [4, 4]]}]}),
        ),
        (
            "godot_game_input",
            json!({"steps": [{"type": "circle", "center": [20, 20], "radius": 8, "points_per_turn": 8}]}),
        ),
    ];
    for (tool, extra) in refused {
        let mut args = json!({"run_id": run_id});
        for (key, value) in extra.as_object().expect("object") {
            args[key] = value.clone();
        }
        let started = std::time::Instant::now();
        let result = call(&tools, tool, args.clone()).await;
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "{tool} {args} waited for time that cannot pass"
        );
        assert_error(&result, "godot_game_physics");
        assert_error(&result, "held");
    }

    // The refusals ran no tick and the game still answers.
    let after = counters(&tools, &run_id).await;
    assert_eq!(
        after["engine_process_frames"],
        start["engine_process_frames"]
    );
    assert_eq!(after["physics_ticks"], start["physics_ticks"]);

    // What finishes at once is served.
    let set = call(
        &tools,
        "godot_game_set",
        json!({"run_id": run_id, "path": ".", "properties": {"timer_fires": 1000}}),
    )
    .await;
    assert!(!set.is_error, "{}", text_of(&set));
    assert_eq!(counters(&tools, &run_id).await["timer_fires"], 1000);
    for tool in ["godot_game_tree", "godot_game_snapshot", "godot_run_output"] {
        let served = call(&tools, tool, json!({"run_id": run_id})).await;
        assert!(!served.is_error, "{tool}: {}", text_of(&served));
    }
    let signals = call(
        &tools,
        "godot_game_signals",
        json!({"run_id": run_id, "path": "."}),
    )
    .await;
    assert!(!signals.is_error, "{}", text_of(&signals));

    let stopped = call(&tools, "godot_stop_project", json!({"run_id": run_id})).await;
    assert!(!stopped.is_error, "{}", text_of(&stopped));
}

#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn held_windowed_game_captures_the_last_drawn_frame() {
    let _ = IGNORE;
    if !windowed_game_can_open() {
        eprintln!("skipped: a windowed capture needs a display server");
        return;
    }
    let (_project, tools, run_id) = start_hold(false).await;
    hold_now(&tools, &run_id).await;

    let first = call(
        &tools,
        "godot_game_screenshot",
        json!({"run_id": run_id, "max_dimension": 128}),
    )
    .await;
    assert!(!first.is_error, "{}", text_of(&first));
    assert!(
        first.content.iter().any(|item| matches!(
            item,
            godot_mcp::protocol::ToolContent::Image { mime_type, .. } if mime_type == "image/png"
        )),
        "a held capture returned no PNG: {}",
        text_of(&first)
    );
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let second = call(
        &tools,
        "godot_game_screenshot",
        json!({"run_id": run_id, "max_dimension": 128}),
    )
    .await;
    assert!(!second.is_error, "{}", text_of(&second));
    assert!(
        text_of(&second).contains("changed 0 px"),
        "a held game drew something new: {}",
        text_of(&second)
    );

    let stepped = held_step(&tools, &run_id, json!({"ticks": 3})).await;
    assert!(!stepped.is_error, "{}", text_of(&stepped));
    let third = call(
        &tools,
        "godot_game_screenshot",
        json!({"run_id": run_id, "max_dimension": 128}),
    )
    .await;
    assert!(!third.is_error, "{}", text_of(&third));
    assert!(
        !text_of(&third).contains("changed 0 px"),
        "three held ticks changed nothing on screen: {}",
        text_of(&third)
    );

    // A window that stops drawing while the game is held: a step draws nothing, so the capture
    // draws the held state itself.
    let stop = json!({"run_id": run_id, "path": ".", "method": "stop_drawing", "permit": ["stop_drawing"], "args": [true]});
    let no_draw = call(&tools, "godot_game_call", stop).await;
    assert!(!no_draw.is_error, "{}", text_of(&no_draw));
    let unchanged = call(
        &tools,
        "godot_game_screenshot",
        json!({"run_id": run_id, "max_dimension": 128}),
    )
    .await;
    assert!(!unchanged.is_error, "{}", text_of(&unchanged));
    assert!(
        text_of(&unchanged).contains("changed 0 px") && text_of(&unchanged).contains("path held"),
        "no step, so the held frame is current: {}",
        text_of(&unchanged)
    );
    let stepped = held_step(&tools, &run_id, json!({"ticks": 3})).await;
    assert!(!stepped.is_error, "{}", text_of(&stepped));
    let drawn = call(
        &tools,
        "godot_game_screenshot",
        json!({"run_id": run_id, "max_dimension": 128}),
    )
    .await;
    assert!(!drawn.is_error, "{}", text_of(&drawn));
    assert!(
        !text_of(&drawn).contains("changed 0 px") && text_of(&drawn).contains("path forced_draw"),
        "three held ticks with the window not drawing: {}",
        text_of(&drawn)
    );

    let stopped = call(&tools, "godot_stop_project", json!({"run_id": run_id})).await;
    assert!(!stopped.is_error, "{}", text_of(&stopped));
}

async fn game_method(tools: &Arc<ToolManager>, run_id: &str, method: &str) -> Value {
    let reply = call(
        tools,
        "godot_game_call",
        json!({"run_id": run_id, "path": ".", "method": method, "permit": [method]}),
    )
    .await;
    assert!(!reply.is_error, "{}", text_of(&reply));
    call_value(&json_of(&reply))
}

/// Per physics tick since the last clear: whether Space is down and whether the action it is
/// bound to counts as just pressed.
async fn key_trace(tools: &Arc<ToolManager>, run_id: &str) -> (Vec<bool>, Vec<bool>) {
    let rows = game_method(tools, run_id, "trace_log").await;
    let rows = rows.as_array().expect("trace rows").clone();
    (
        rows.iter().map(|row| row[1] == true).collect(),
        rows.iter().map(|row| row[2] == true).collect(),
    )
}

async fn tap_space(tools: &Arc<ToolManager>, run_id: &str, extra: Value) -> ToolResult {
    let mut args = json!({
        "run_id": run_id,
        "steps": [{"type": "key", "key": "Space", "phase": "tap"}]
    });
    if let (Some(base), Some(more)) = (args.as_object_mut(), extra.as_object()) {
        for (key, value) in more {
            base.insert(key.clone(), value.clone());
        }
    }
    call(tools, "godot_game_input", args).await
}

#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn held_tap_lasts_its_ticks_whatever_the_step_sizes() {
    let _ = IGNORE;
    let (_project, tools, run_id) = start_hold(true).await;
    hold_now(&tools, &run_id).await;

    // One tick at a time.
    game_method(&tools, &run_id, "clear_trace").await;
    let tapped = tap_space(&tools, &run_id, json!({})).await;
    assert!(!tapped.is_error, "{}", text_of(&tapped));
    assert_eq!(json_of(&tapped)["hold_ticks"], 2, "{}", text_of(&tapped));
    for _ in 0..6 {
        let stepped = held_step(&tools, &run_id, json!({"ticks": 1})).await;
        assert!(!stepped.is_error, "{}", text_of(&stepped));
    }
    let (down_split, just_split) = key_trace(&tools, &run_id).await;

    // The same tap, one step.
    game_method(&tools, &run_id, "clear_trace").await;
    let tapped = tap_space(&tools, &run_id, json!({})).await;
    assert!(!tapped.is_error, "{}", text_of(&tapped));
    let stepped = held_step(&tools, &run_id, json!({"ticks": 6})).await;
    assert!(!stepped.is_error, "{}", text_of(&stepped));
    let (down_whole, just_whole) = key_trace(&tools, &run_id).await;

    assert_eq!(
        down_split, down_whole,
        "splitting a step changed what the game saw"
    );
    assert_eq!(
        just_split, just_whole,
        "splitting a step changed what the game saw"
    );
    // Measured on a real engine: a press made at the hold point is down from the first tick that
    // follows, but the engine reports it as just pressed on the second one.
    assert_eq!(
        just_whole,
        vec![false, true, false, false, false, false],
        "when a press becomes just pressed changed"
    );
    assert_eq!(
        down_whole,
        vec![true, true, false, false, false, false],
        "a tap is down for its hold ticks"
    );

    // A longer tap.
    game_method(&tools, &run_id, "clear_trace").await;
    let tapped = tap_space(&tools, &run_id, json!({"hold_ticks": 4})).await;
    assert!(!tapped.is_error, "{}", text_of(&tapped));
    held_step(&tools, &run_id, json!({"ticks": 6})).await;
    let (down_long, _) = key_trace(&tools, &run_id).await;
    assert_eq!(down_long, vec![true, true, true, true, false, false]);

    let stopped = call(&tools, "godot_stop_project", json!({"run_id": run_id})).await;
    assert!(!stopped.is_error, "{}", text_of(&stopped));
}

#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn held_input_is_released_when_asked_and_when_the_hold_ends() {
    let _ = IGNORE;
    let (_project, tools, run_id) = start_hold(true).await;
    hold_now(&tools, &run_id).await;

    // An explicit release also cancels the release a tap scheduled, so the scheduled one cannot
    // lift a later press.
    let tapped = tap_space(&tools, &run_id, json!({"hold_ticks": 5})).await;
    assert!(!tapped.is_error, "{}", text_of(&tapped));
    let released = call(
        &tools,
        "godot_game_input_release",
        json!({"run_id": run_id}),
    )
    .await;
    assert!(!released.is_error, "{}", text_of(&released));
    assert_eq!(counters(&tools, &run_id).await["space_down"], false);
    game_method(&tools, &run_id, "clear_trace").await;
    let pressed = call(
        &tools,
        "godot_game_input",
        json!({"run_id": run_id, "steps": [{"type": "key", "key": "Space", "phase": "press"}]}),
    )
    .await;
    assert!(!pressed.is_error, "{}", text_of(&pressed));
    held_step(&tools, &run_id, json!({"ticks": 7})).await;
    let (down, _) = key_trace(&tools, &run_id).await;
    assert_eq!(
        down,
        vec![true; 7],
        "a cancelled release lifted a later press"
    );

    // Leaving the hold lets go of what is still held.
    let resumed = call(
        &tools,
        "godot_game_physics",
        json!({"run_id": run_id, "action": "resume"}),
    )
    .await;
    assert!(!resumed.is_error, "{}", text_of(&resumed));
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    assert_eq!(counters(&tools, &run_id).await["space_down"], false);

    // What finishes at once is accepted; what needs a frame is refused before anything is applied.
    hold_now(&tools, &run_id).await;
    let instant = call(
        &tools,
        "godot_game_input",
        json!({"run_id": run_id, "steps": [
            {"type": "mouse_move", "x": 5, "y": 5},
            {"type": "joy_axis", "axis": "left_x", "value": 0.5},
            {"type": "click", "x": 3, "y": 4},
            {"type": "time_scale", "scale": 1},
            {"type": "joy_button", "button": "a", "phase": "tap"}
        ]}),
    )
    .await;
    assert!(!instant.is_error, "{}", text_of(&instant));
    assert_eq!(
        json_of(&instant)["steps"].as_array().map(Vec::len),
        Some(5),
        "{}",
        text_of(&instant)
    );
    for bad in [json!({"hold_ticks": 0}), json!({"hold_ticks": 601})] {
        assert_error(&tap_space(&tools, &run_id, bad).await, "hold_ticks");
    }
    assert_error(
        &call(
            &tools,
            "godot_game_input",
            json!({"run_id": run_id, "steps": [
                {"type": "key", "key": "Space", "phase": "press"},
                {"type": "wait", "frames": 1}
            ]}),
        )
        .await,
        "godot_game_physics",
    );
    assert_eq!(
        counters(&tools, &run_id).await["space_down"],
        false,
        "a refused call applied the steps before it"
    );

    let stopped = call(&tools, "godot_stop_project", json!({"run_id": run_id})).await;
    assert!(!stopped.is_error, "{}", text_of(&stopped));
}

#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn a_held_launch_steps_exactly_and_the_server_knows_it_is_held() {
    let _ = IGNORE;
    let project = hold_project();
    let tools = manager(project.path());
    let started = call(
        &tools,
        "godot_run_project",
        json!({
            "project_path": project.path(),
            "scene": "res://scenes/hold.tscn",
            "mode": "process",
            "headless": true,
            "held": true
        }),
    )
    .await;
    assert!(!started.is_error, "{}", text_of(&started));
    let run_id = run_id_of(&started);
    assert_eq!(process_run(&tools, &run_id).await["held"], true);

    // The game is held from its first tick, before anything asked for it.
    let status = call(
        &tools,
        "godot_game_physics",
        json!({"run_id": run_id, "action": "status"}),
    )
    .await;
    assert!(!status.is_error, "{}", text_of(&status));
    let status_json = json_of(&status);
    assert_eq!(status_json["held"], true, "{status_json}");
    assert_eq!(status_json["deterministic"], true, "{status_json}");
    assert_eq!(status_json["deterministic_reason"], "", "{status_json}");
    let first = counters(&tools, &run_id).await;
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert_eq!(
        counters(&tools, &run_id).await["engine_process_frames"],
        first["engine_process_frames"],
        "a game started held ran"
    );

    let ten = held_step(&tools, &run_id, json!({"ticks": 10})).await;
    assert!(!ten.is_error, "{}", text_of(&ten));
    let ten_json = json_of(&ten);
    assert_eq!(ten_json["ticks_run"], 10, "{ten_json}");
    assert_eq!(ten_json["frames_advanced"], 10, "{ten_json}");
    assert_eq!(ten_json["deterministic"], true, "{ten_json}");
    assert_eq!(ten_json["deterministic_reason"], "", "{ten_json}");
    let after = counters(&tools, &run_id).await;
    let delta = |key: &str| after[key].as_i64().unwrap_or(-1) - first[key].as_i64().unwrap_or(-100);
    assert_eq!(delta("physics_ticks"), 10, "{first} -> {after}");
    assert_eq!(delta("engine_process_frames"), 10, "{first} -> {after}");
    assert_eq!(delta("engine_physics_frames"), 10, "{first} -> {after}");

    // A game that is already still has nothing to pause.
    let pause = call(
        &tools,
        "godot_debug_control",
        json!({"run_id": run_id, "action": "pause"}),
    )
    .await;
    assert_error(&pause, "held");

    let resumed = call(
        &tools,
        "godot_game_physics",
        json!({"run_id": run_id, "action": "resume"}),
    )
    .await;
    assert!(!resumed.is_error, "{}", text_of(&resumed));
    assert_eq!(process_run(&tools, &run_id).await["held"], false);

    let stopped = call(&tools, "godot_stop_project", json!({"run_id": run_id})).await;
    assert!(!stopped.is_error, "{}", text_of(&stopped));
}

async fn start_held_launch(
    breakpoints: &[String],
) -> (tempfile::TempDir, Arc<ToolManager>, String) {
    let project = hold_project();
    let tools = manager(project.path());
    let started = call(
        &tools,
        "godot_run_project",
        json!({
            "project_path": project.path(),
            "scene": "res://scenes/hold.tscn",
            "mode": "process",
            "headless": true,
            "held": true,
            "breakpoints": breakpoints
        }),
    )
    .await;
    assert!(!started.is_error, "{}", text_of(&started));
    let run_id = run_id_of(&started);
    (project, tools, run_id)
}

#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn exactness_is_withdrawn_when_frames_stop_following_ticks_and_stays_withdrawn() {
    let _ = IGNORE;
    let (_project, tools, run_id) = start_held_launch(&[]).await;

    let exact = held_step(&tools, &run_id, json!({"ticks": 3})).await;
    assert_eq!(
        json_of(&exact)["deterministic"],
        true,
        "{}",
        text_of(&exact)
    );

    // A game that runs two ticks in a frame: its tick rate is doubled for one step.
    let doubled = call(
        &tools,
        "godot_game_call",
        json!({"run_id": run_id, "path": ".", "method": "set_tick_rate", "args": [120], "permit": ["set_tick_rate"]}),
    )
    .await;
    assert!(!doubled.is_error, "{}", text_of(&doubled));
    let uneven = held_step(&tools, &run_id, json!({"ticks": 4})).await;
    assert!(!uneven.is_error, "{}", text_of(&uneven));
    let uneven_json = json_of(&uneven);
    assert_eq!(uneven_json["ticks_run"], 4, "{uneven_json}");
    assert_ne!(uneven_json["frames_advanced"], 4, "{uneven_json}");
    assert_eq!(uneven_json["deterministic"], false, "{uneven_json}");

    // Back at the declared rate, the claim is still not given back.
    let normal = call(
        &tools,
        "godot_game_call",
        json!({"run_id": run_id, "path": ".", "method": "set_tick_rate", "args": [60], "permit": ["set_tick_rate"]}),
    )
    .await;
    assert!(!normal.is_error, "{}", text_of(&normal));
    let after = held_step(&tools, &run_id, json!({"ticks": 3})).await;
    assert_eq!(
        json_of(&after)["deterministic"],
        false,
        "{}",
        text_of(&after)
    );
    assert_eq!(
        json_of(&after)["deterministic_reason"],
        "frames_did_not_follow_ticks",
        "{}",
        text_of(&after)
    );

    let stopped = call(&tools, "godot_stop_project", json!({"run_id": run_id})).await;
    assert!(!stopped.is_error, "{}", text_of(&stopped));
}

#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn exactness_is_withdrawn_when_the_game_changes_its_tick_rate() {
    let _ = IGNORE;
    let (_project, tools, run_id) = start_held_launch(&[]).await;
    let exact = call(
        &tools,
        "godot_game_physics",
        json!({"run_id": run_id, "action": "status"}),
    )
    .await;
    assert_eq!(
        json_of(&exact)["deterministic"],
        true,
        "{}",
        text_of(&exact)
    );

    let changed = call(
        &tools,
        "godot_game_call",
        json!({"run_id": run_id, "path": ".", "method": "set_tick_rate", "args": [30], "permit": ["set_tick_rate"]}),
    )
    .await;
    assert!(!changed.is_error, "{}", text_of(&changed));
    let status = call(
        &tools,
        "godot_game_physics",
        json!({"run_id": run_id, "action": "status"}),
    )
    .await;
    let status_json = json_of(&status);
    assert_eq!(status_json["deterministic"], false, "{status_json}");
    assert_eq!(
        status_json["deterministic_reason"], "fixed_rate_differs",
        "{status_json}"
    );

    let stopped = call(&tools, "godot_stop_project", json!({"run_id": run_id})).await;
    assert!(!stopped.is_error, "{}", text_of(&stopped));
}

#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn a_breakpoint_hit_during_a_held_step_stops_the_game_as_usual() {
    let _ = IGNORE;
    let line = hold_script_line("physics_ticks += 1");
    let (_project, tools, run_id) =
        start_held_launch(&[format!("res://scripts/hold.gd:{line}")]).await;

    let stepping = {
        let tools = Arc::clone(&tools);
        let run_id = run_id.clone();
        tokio::spawn(async move { held_step(&tools, &run_id, json!({"ticks": 3})).await })
    };
    let mut stopped_at_breakpoint = false;
    for _ in 0..100 {
        let state = json_of(&call(&tools, "godot_debug_state", json!({"run_id": run_id})).await);
        if state["stopped"] == true {
            stopped_at_breakpoint = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert!(stopped_at_breakpoint, "the breakpoint was not hit");

    // The game stops at the breakpoint on every tick of the step; letting it go each time
    // finishes the step.
    let mut hits = 0;
    for _ in 0..100 {
        if stepping.is_finished() {
            break;
        }
        let state = json_of(&call(&tools, "godot_debug_state", json!({"run_id": run_id})).await);
        if state["stopped"] == true {
            let resumed = call(
                &tools,
                "godot_debug_control",
                json!({"run_id": run_id, "action": "resume"}),
            )
            .await;
            assert!(!resumed.is_error, "{}", text_of(&resumed));
            hits += 1;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert!(hits >= 1, "the step never stopped at the breakpoint");
    let finished = stepping.await.expect("step task");
    assert!(!finished.is_error, "{}", text_of(&finished));
    assert_eq!(json_of(&finished)["ticks_run"], 3, "{}", text_of(&finished));
    assert_eq!(json_of(&finished)["held"], true, "{}", text_of(&finished));

    let stopped = call(&tools, "godot_stop_project", json!({"run_id": run_id})).await;
    assert!(!stopped.is_error, "{}", text_of(&stopped));
}

/// RO-6: nothing on the refusal list runs, whatever the permit list says, and a permitted call can
/// pass the argument types the method asks for.
#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn game_call_refuses_what_could_take_the_game_apart_and_converts_argument_types() {
    let project = project();
    let tools = manager(project.path());
    let started = call(
        &tools,
        "godot_run_project",
        json!({"project_path": project.path(), "mode": "process", "headless": true}),
    )
    .await;
    assert!(!started.is_error, "{}", text_of(&started));
    let run_id = run_id_of(&started);

    for method in [
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
        let refused = call(
            &tools,
            "godot_game_call",
            json!({"run_id": run_id, "path": "Mark", "method": method, "permit": [method],
                   "args": []}),
        )
        .await;
        assert!(refused.is_error, "{method}: {}", text_of(&refused));
        assert!(
            text_of(&refused).contains("not allowed"),
            "{method}: {}",
            text_of(&refused)
        );
    }
    let tree = call(&tools, "godot_game_tree", json!({"run_id": run_id})).await;
    let tree = json_of(&tree);
    let names: Vec<&str> = tree["nodes"]
        .as_array()
        .expect("nodes")
        .iter()
        .filter_map(|node| node["name"].as_str())
        .collect();
    assert!(
        names.contains(&"Mark") && names.contains(&"Pad"),
        "{names:?}"
    );

    // A permitted call with a StringName and with a NodePath argument.
    let group = call(
        &tools,
        "godot_game_call",
        json!({"run_id": run_id, "path": "Mark", "method": "is_in_group",
               "permit": ["is_in_group"], "args": ["nobody"]}),
    )
    .await;
    assert!(!group.is_error, "{}", text_of(&group));
    assert_eq!(json_of(&group)["value"], false, "{}", text_of(&group));
    let has = call(
        &tools,
        "godot_game_call",
        json!({"run_id": run_id, "path": ".", "method": "has_node",
               "permit": ["has_node"], "args": ["Pad"]}),
    )
    .await;
    assert!(!has.is_error, "{}", text_of(&has));
    assert_eq!(json_of(&has)["value"], true, "{}", text_of(&has));
    // A type that cannot be converted is still a parameter error.
    let wrong = call(
        &tools,
        "godot_game_call",
        json!({"run_id": run_id, "path": ".", "method": "poke",
               "permit": ["poke"], "args": ["three"]}),
    )
    .await;
    assert!(wrong.is_error, "{}", text_of(&wrong));
    assert!(
        text_of(&wrong).contains("BAD_PARAMS"),
        "{}",
        text_of(&wrong)
    );

    let stopped = call(&tools, "godot_stop_project", json!({"run_id": run_id})).await;
    assert!(!stopped.is_error, "{}", text_of(&stopped));
}

/// RO-11: a pointer step is delivered, and when its point lies outside the viewport (a game
/// without a window has one of 64 x 64 pixels) the answer says controls there do not react.
#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn pointer_input_in_a_game_without_a_window_warns_when_it_lands_outside_the_viewport() {
    let project = project();
    let tools = manager(project.path());
    let started = call(
        &tools,
        "godot_run_project",
        json!({"project_path": project.path(), "mode": "process", "headless": true}),
    )
    .await;
    assert!(!started.is_error, "{}", text_of(&started));
    let run_id = run_id_of(&started);

    let counter = |name: &'static str| {
        let tools = Arc::clone(&tools);
        let run_id = run_id.clone();
        async move {
            let got = call(
                &tools,
                "godot_game_get",
                json!({"run_id": run_id, "path": ".", "properties": [name]}),
            )
            .await;
            json_of(&got)["properties"][name].as_i64().unwrap_or(-1)
        }
    };

    // Pad lies at x 100 to 140: outside the 64 x 64 viewport. The event is delivered (_input
    // sees it) but the button cannot react, and the answer says so for every kind of step.
    for step in [
        json!({"type": "click_node", "node": "Pad"}),
        json!({"type": "click", "x": 120, "y": 40}),
        json!({"type": "mouse_button", "button": "left", "x": 120, "y": 40}),
        json!({"type": "mouse_move", "x": 120, "y": 40}),
        json!({"type": "swipe", "x": 10, "y": 10, "to_x": 120, "to_y": 40, "frames": 2}),
    ] {
        let answered = call(
            &tools,
            "godot_game_input",
            json!({"run_id": run_id, "steps": [step.clone(), {"type": "wait", "frames": 3}]}),
        )
        .await;
        assert!(!answered.is_error, "{step}: {}", text_of(&answered));
        let result = &json_of(&answered)["steps"][0];
        assert_eq!(result["ok"], true, "{step}: {result}");
        let warning = result["warning"].as_str().unwrap_or("");
        assert!(
            warning.contains("outside") && warning.contains("64 x 64"),
            "{step}: {result}"
        );
    }
    assert_eq!(
        counter("pad_presses").await,
        0,
        "a button outside the viewport reacted"
    );

    // Tiny lies inside it: the same steps press it, and there is no warning.
    let inside = call(
        &tools,
        "godot_game_input",
        json!({"run_id": run_id, "steps": [
            {"type": "click_node", "node": "Tiny"}, {"type": "wait", "frames": 3}]}),
    )
    .await;
    assert!(!inside.is_error, "{}", text_of(&inside));
    assert!(json_of(&inside)["steps"][0].get("warning").is_none());
    assert_eq!(counter("tiny_presses").await, 1);

    let stopped = call(&tools, "godot_stop_project", json!({"run_id": run_id})).await;
    assert!(!stopped.is_error, "{}", text_of(&stopped));
}

/// The long explanation of `script_sha256` is in an output answer once, not on every line.
#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn an_output_answer_explains_the_script_hash_once() {
    let project = project();
    let tools = manager(project.path());
    let started = call(
        &tools,
        "godot_run_project",
        json!({"project_path": project.path(), "mode": "process", "headless": true}),
    )
    .await;
    assert!(!started.is_error, "{}", text_of(&started));
    let run_id = run_id_of(&started);
    let mut answer = Value::Null;
    for _ in 0..50 {
        let output = call(&tools, "godot_run_output", json!({"run_id": run_id})).await;
        assert!(!output.is_error, "{}", text_of(&output));
        answer = json_of(&output);
        if answer["lines"]
            .as_array()
            .is_some_and(|lines| lines.len() >= 2)
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    let lines = answer["lines"].as_array().expect("lines");
    assert!(lines.len() >= 2, "{answer}");
    let note = answer["script_sha256_note"]
        .as_str()
        .expect("one note per answer");
    assert!(note.contains("not a later read"), "{note}");
    assert_eq!(
        text_of(&call(&tools, "godot_run_output", json!({"run_id": run_id})).await)
            .matches("not a later read")
            .count(),
        1,
        "the explanation is repeated"
    );
    for line in lines {
        assert!(line["scene"].get("script_sha256_note").is_none(), "{line}");
        assert!(line["scene"].get("script_sha256_basis").is_some(), "{line}");
    }
    let stopped = call(&tools, "godot_stop_project", json!({"run_id": run_id})).await;
    assert!(!stopped.is_error, "{}", text_of(&stopped));
}

/// A baseline saved at a reduced size is compared at that size without repeating the option.
#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable and a display"]
async fn a_baseline_is_compared_at_its_own_size_by_default() {
    if !windowed_game_can_open() {
        return;
    }
    let project = project();
    let tools = manager(project.path());
    let started = call(
        &tools,
        "godot_run_project",
        json!({"project_path": project.path(), "mode": "process"}),
    )
    .await;
    assert!(!started.is_error, "{}", text_of(&started));
    let run_id = run_id_of(&started);
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    let saved = call(
        &tools,
        "godot_game_screenshot",
        json!({"run_id": run_id, "save_baseline": "small", "max_dimension": 64}),
    )
    .await;
    assert!(!saved.is_error, "{}", text_of(&saved));
    let compared = call(
        &tools,
        "godot_game_screenshot",
        json!({"run_id": run_id, "baseline": "small", "tolerance": 255}),
    )
    .await;
    assert!(!compared.is_error, "{}", text_of(&compared));
    assert!(
        text_of(&compared).contains("baseline small changed"),
        "{}",
        text_of(&compared)
    );
    let stopped = call(&tools, "godot_stop_project", json!({"run_id": run_id})).await;
    assert!(!stopped.is_error, "{}", text_of(&stopped));
}

/// A `/` write follows an Object field or a Dictionary key. A missing leaf, a private
/// segment, a Node in the middle, and a write that does not stick are errors. A `:`
/// resource path still writes the resource property.
#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn game_set_writes_through_an_object_and_refuses_a_missing_leaf() {
    let _ = IGNORE;
    let project = project();
    let tools = manager(project.path());
    let started = call(
        &tools,
        "godot_run_project",
        json!({
            "project_path": project.path(),
            "scene": "res://scenes/main.tscn",
            "mode": "process",
            "headless": true
        }),
    )
    .await;
    assert!(!started.is_error, "{}", text_of(&started));
    let run_id = run_id_of(&started);

    let letter = call(
        &tools,
        "godot_game_set",
        json!({"run_id": run_id, "path": ".", "properties": {"round_state/letter_index": 22}}),
    )
    .await;
    assert!(!letter.is_error, "{}", text_of(&letter));
    assert_eq!(
        json_of(&letter)["properties"]["round_state/letter_index"],
        22,
        "{}",
        text_of(&letter)
    );
    let letter_read = call(
        &tools,
        "godot_game_get",
        json!({"run_id": run_id, "path": ".", "properties": ["round_state/letter_index"]}),
    )
    .await;
    assert_eq!(
        json_of(&letter_read)["properties"]["round_state/letter_index"],
        22,
        "{}",
        text_of(&letter_read)
    );

    let count = call(
        &tools,
        "godot_game_set",
        json!({"run_id": run_id, "path": ".", "properties": {"bag/count": 9}}),
    )
    .await;
    assert!(!count.is_error, "{}", text_of(&count));
    assert_eq!(
        json_of(&count)["properties"]["bag/count"],
        9,
        "{}",
        text_of(&count)
    );

    let missing = call(
        &tools,
        "godot_game_set",
        json!({"run_id": run_id, "path": ".", "properties": {"round_state/nope": 1}}),
    )
    .await;
    assert!(missing.is_error, "{}", text_of(&missing));
    assert!(text_of(&missing).contains("nope"), "{}", text_of(&missing));
    let spelled = call(
        &tools,
        "godot_game_set",
        json!({"run_id": run_id, "path": ".", "properties": {"not_a_property": 1}}),
    )
    .await;
    assert!(spelled.is_error, "{}", text_of(&spelled));
    assert!(
        text_of(&spelled).contains("not_a_property"),
        "{}",
        text_of(&spelled)
    );
    let extra = call(
        &tools,
        "godot_game_set",
        json!({"run_id": run_id, "path": ".", "properties": {"bag/extra": 1}}),
    )
    .await;
    assert!(extra.is_error, "{}", text_of(&extra));
    assert!(text_of(&extra).contains("extra"), "{}", text_of(&extra));

    let private = call(
        &tools,
        "godot_game_set",
        json!({"run_id": run_id, "path": ".", "properties": {"firefly_law/_hidden": 0}}),
    )
    .await;
    assert!(private.is_error, "{}", text_of(&private));
    assert!(
        text_of(&private).to_lowercase().contains("refus"),
        "{}",
        text_of(&private)
    );

    let through_node = call(
        &tools,
        "godot_game_set",
        json!({"run_id": run_id, "path": ".", "properties": {"mark_ref/position": [1, 2]}}),
    )
    .await;
    assert!(through_node.is_error, "{}", text_of(&through_node));
    let through_text = text_of(&through_node);
    assert!(
        through_text.contains("path") && through_text.contains("mark_ref"),
        "{through_text}"
    );
    let mark = call(
        &tools,
        "godot_game_get",
        json!({"run_id": run_id, "path": "Mark", "properties": ["position"]}),
    )
    .await;
    assert!(!mark.is_error, "{}", text_of(&mark));
    let position = &json_of(&mark)["properties"]["position"];
    assert_eq!(position[0].as_f64(), Some(40.0), "{position}");
    assert_eq!(position[1].as_f64(), Some(70.0), "{position}");

    // Node2D.rotation is a 32-bit float: 0.1 reads back as 0.10000000149 and is still a write that took.
    let turned = call(
        &tools,
        "godot_game_set",
        json!({"run_id": run_id, "path": "Mark", "properties": {"rotation": 0.1}}),
    )
    .await;
    assert!(!turned.is_error, "{}", text_of(&turned));
    let angle = json_of(&turned)["properties"]["rotation"].as_f64();
    assert!(
        angle.is_some_and(|a| (a - 0.1).abs() < 1e-6),
        "{}",
        text_of(&turned)
    );

    let level = call(
        &tools,
        "godot_game_set",
        json!({"run_id": run_id, "path": ".", "properties": {"swatch:level": 7}}),
    )
    .await;
    assert!(!level.is_error, "{}", text_of(&level));
    assert_eq!(
        json_of(&level)["properties"]["swatch:level"],
        7,
        "{}",
        text_of(&level)
    );

    let rejected = call(
        &tools,
        "godot_game_set",
        json!({"run_id": run_id, "path": ".", "properties": {"locked": 5}}),
    )
    .await;
    assert!(rejected.is_error, "{}", text_of(&rejected));
    // A write that does not stick puts back the ones the same call made before it.
    let undone = call(
        &tools,
        "godot_game_set",
        json!({"run_id": run_id, "path": ".", "properties": {"bag/count": 11, "locked": 5}}),
    )
    .await;
    assert!(undone.is_error, "{}", text_of(&undone));
    assert!(
        text_of(&undone).contains("nothing was changed"),
        "{}",
        text_of(&undone)
    );
    let still = call(
        &tools,
        "godot_game_get",
        json!({
            "run_id": run_id,
            "path": ".",
            "properties": ["round_state/letter_index", "bag/count", "swatch:level", "locked", "firefly_law/births"]
        }),
    )
    .await;
    assert!(!still.is_error, "{}", text_of(&still));
    let kept = &json_of(&still)["properties"];
    assert_eq!(kept["round_state/letter_index"], 22, "{kept}");
    assert_eq!(kept["bag/count"], 9, "{kept}");
    assert_eq!(kept["swatch:level"], 7, "{kept}");
    assert_eq!(kept["locked"], 1, "{kept}");
    assert_eq!(kept["firefly_law/births"], 3, "{kept}");
    assert!(
        kept.get("bag/extra").is_none(),
        "a missing dictionary key was created: {kept}"
    );

    let stopped = call(&tools, "godot_stop_project", json!({"run_id": run_id})).await;
    assert!(!stopped.is_error, "{}", text_of(&stopped));
}

/// A Godot literal string is parsed for a typed property, as the editor tool does. A value of
/// the wrong type is refused with the property's type, and no script error reaches the game log.
#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn game_set_parses_a_godot_literal_and_refuses_a_wrong_type_quietly() {
    let _ = IGNORE;
    let project = project();
    let tools = manager(project.path());
    let started = call(
        &tools,
        "godot_run_project",
        json!({
            "project_path": project.path(),
            "scene": "res://scenes/main.tscn",
            "mode": "process",
            "headless": true
        }),
    )
    .await;
    assert!(!started.is_error, "{}", text_of(&started));
    let run_id = run_id_of(&started);

    let moved = call(
        &tools,
        "godot_game_set",
        json!({
            "run_id": run_id,
            "path": "Mark",
            "properties": {"transform": "Transform2D(1, 0, 0, 1, 5, 6)"}
        }),
    )
    .await;
    assert!(!moved.is_error, "{}", text_of(&moved));
    let mark = call(
        &tools,
        "godot_game_get",
        json!({"run_id": run_id, "path": "Mark", "properties": ["position"]}),
    )
    .await;
    let position = &json_of(&mark)["properties"]["position"];
    assert_eq!(position[0].as_f64(), Some(5.0), "{position}");
    assert_eq!(position[1].as_f64(), Some(6.0), "{position}");

    let wrong = call(
        &tools,
        "godot_game_set",
        json!({"run_id": run_id, "path": "Mark", "properties": {"transform": "not a transform"}}),
    )
    .await;
    assert!(wrong.is_error, "{}", text_of(&wrong));
    let wrong_text = text_of(&wrong);
    assert!(
        wrong_text.contains("Transform2D") && wrong_text.contains("String"),
        "{wrong_text}"
    );

    let mut log = String::new();
    for _ in 0..10 {
        log = text_of(&call(&tools, "godot_run_output", json!({"run_id": run_id})).await);
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert!(!log.contains("Invalid operands"), "{log}");
    assert!(!log.contains("rt_live.gd"), "{log}");

    let stopped = call(&tools, "godot_stop_project", json!({"run_id": run_id})).await;
    assert!(!stopped.is_error, "{}", text_of(&stopped));
}

/// A 3D scene whose sun is turned a quarter around Y, so an identity basis is visible.
fn sun_project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("project");
    let root = dir.path();
    std::fs::create_dir_all(root.join("scenes")).unwrap();
    std::fs::write(
        root.join("project.godot"),
        "config_version=5\n\n[application]\n\nconfig/name=\"SunProbe\"\nrun/main_scene=\"res://scenes/main.tscn\"\nconfig/features=PackedStringArray(\"4.5\")\n",
    )
    .unwrap();
    std::fs::write(
        root.join("scenes/main.tscn"),
        "[gd_scene format=3]\n\n[node name=\"Level\" type=\"Node3D\"]\n\n[node name=\"Environment\" type=\"Node3D\" parent=\".\"]\n\n[node name=\"Sun\" type=\"DirectionalLight3D\" parent=\"Environment\"]\ntransform = Transform3D(0, 0, 1, 0, 1, 0, -1, 0, 0, 0, 5, 0)\nlight_energy = 2.0\n",
    )
    .unwrap();
    dir
}

/// The sun's rotation and energy, as `[rotation.x, rotation.y, rotation.z, energy]`.
async fn sun_state(tools: &Arc<ToolManager>, run_id: &str) -> Vec<f64> {
    let read = call(
        tools,
        "godot_game_get",
        json!({"run_id": run_id, "path": "Environment/Sun", "properties": ["rotation", "light_energy"]}),
    )
    .await;
    assert!(!read.is_error, "{}", text_of(&read));
    let props = &json_of(&read)["properties"];
    let rotation = &props["rotation"];
    vec![
        json_number(&rotation[0]),
        json_number(&rotation[1]),
        json_number(&rotation[2]),
        json_number(&props["light_energy"]),
    ]
}

fn assert_sun(state: &[f64], expected: [f64; 4]) {
    for (got, want) in state.iter().zip(expected) {
        assert!((got - want).abs() < 1e-4, "{state:?} != {expected:?}");
    }
}

/// A refused value leaves the node as it was, also when other properties of the same call
/// were fine. A Basis or Transform3D written with Vector3 axes, as GDScript constructs it, is
/// read column by column.
#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn game_set_refusal_leaves_the_node_untouched_and_reads_vector_axes() {
    let _ = IGNORE;
    let project = sun_project();
    let tools = manager(project.path());
    let started = call(
        &tools,
        "godot_run_project",
        json!({
            "project_path": project.path(),
            "scene": "res://scenes/main.tscn",
            "mode": "process",
            "headless": true
        }),
    )
    .await;
    assert!(!started.is_error, "{}", text_of(&started));
    let run_id = run_id_of(&started);
    let quarter = std::f64::consts::FRAC_PI_2;
    assert_sun(&sun_state(&tools, &run_id).await, [0.0, quarter, 0.0, 2.0]);

    let wrong = call(
        &tools,
        "godot_game_set",
        json!({"run_id": run_id, "path": "Environment/Sun", "properties": {"basis": "not a basis"}}),
    )
    .await;
    assert!(wrong.is_error, "{}", text_of(&wrong));
    assert!(text_of(&wrong).contains("Basis"), "{}", text_of(&wrong));
    assert_sun(&sun_state(&tools, &run_id).await, [0.0, quarter, 0.0, 2.0]);

    let mixed = call(
        &tools,
        "godot_game_set",
        json!({
            "run_id": run_id,
            "path": "Environment/Sun",
            "properties": {"light_energy": 0.5, "basis": "Basis(1, 2)"}
        }),
    )
    .await;
    assert!(mixed.is_error, "{}", text_of(&mixed));
    assert_sun(&sun_state(&tools, &run_id).await, [0.0, quarter, 0.0, 2.0]);

    // Columns x = (0, 0, 1), y = (0, 1, 0), z = (-1, 0, 0): a quarter turn the other way.
    let axes = call(
        &tools,
        "godot_game_set",
        json!({
            "run_id": run_id,
            "path": "Environment/Sun",
            "properties": {"basis": "Basis(Vector3(0, 0, 1), Vector3(0, 1, 0), Vector3(-1, 0, 0))"}
        }),
    )
    .await;
    assert!(!axes.is_error, "{}", text_of(&axes));
    assert_sun(&sun_state(&tools, &run_id).await, [0.0, -quarter, 0.0, 2.0]);

    let placed = call(
        &tools,
        "godot_game_set",
        json!({
            "run_id": run_id,
            "path": "Environment/Sun",
            "properties": {"transform": "Transform3D(Vector3(0, 0, -1), Vector3(0, 1, 0), Vector3(1, 0, 0), Vector3(1, 2, 3))"}
        }),
    )
    .await;
    assert!(!placed.is_error, "{}", text_of(&placed));
    assert_sun(&sun_state(&tools, &run_id).await, [0.0, quarter, 0.0, 2.0]);
    let position = call(
        &tools,
        "godot_game_get",
        json!({"run_id": run_id, "path": "Environment/Sun", "properties": ["position"]}),
    )
    .await;
    let origin = &json_of(&position)["properties"]["position"];
    assert_eq!(origin[2].as_f64(), Some(3.0), "{origin}");

    let stopped = call(&tools, "godot_stop_project", json!({"run_id": run_id})).await;
    assert!(!stopped.is_error, "{}", text_of(&stopped));
}

/// A level with a box `Body` (no material) and saved resources to assign to it while it runs.
fn resource_path_project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("project");
    let root = dir.path();
    std::fs::create_dir_all(root.join("scenes")).unwrap();
    std::fs::create_dir_all(root.join("resources")).unwrap();
    std::fs::write(
        root.join("project.godot"),
        "config_version=5\n\n[application]\n\nconfig/name=\"ResourcePathProbe\"\nrun/main_scene=\"res://scenes/main.tscn\"\nconfig/features=PackedStringArray(\"4.5\")\n",
    )
    .unwrap();
    std::fs::write(
        root.join("resources/quad.tres"),
        "[gd_resource type=\"QuadMesh\" format=3]\n\n[resource]\nsize = Vector2(3, 3)\n",
    )
    .unwrap();
    std::fs::write(
        root.join("resources/tint.tres"),
        "[gd_resource type=\"StandardMaterial3D\" format=3]\n\n[resource]\nalbedo_color = Color(1, 0, 0, 1)\n",
    )
    .unwrap();
    std::fs::write(
        root.join("scenes/main.tscn"),
        "[gd_scene load_steps=2 format=3]\n\n[sub_resource type=\"BoxMesh\" id=\"1\"]\n\n[node name=\"Level\" type=\"Node3D\"]\n\n[node name=\"Body\" type=\"MeshInstance3D\" parent=\".\"]\nmesh = SubResource(\"1\")\n",
    )
    .unwrap();
    dir
}

/// The Body's mesh, material and transparency as godot_game_get reads them.
async fn body_state(tools: &Arc<ToolManager>, run_id: &str) -> Value {
    let read = call(
        tools,
        "godot_game_get",
        json!({"run_id": run_id, "path": "Body", "properties": ["mesh", "material_override", "transparency"]}),
    )
    .await;
    assert!(!read.is_error, "{}", text_of(&read));
    json_of(&read)["properties"].clone()
}

/// An empty Object property, which godot_game_get reads as a typed null.
fn no_object(value: &Value) -> bool {
    value.is_null() || value == "<Object#null>"
}

/// A res:// path written to an Object property of a live node loads that resource in the game
/// and assigns it, whether the property held a resource or null. A missing file, a resource of
/// the wrong class and text that is not a path are refused by name, and nothing is written.
#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn game_set_assigns_a_saved_resource_by_its_path() {
    let _ = IGNORE;
    let project = resource_path_project();
    let tools = manager(project.path());
    let started = call(
        &tools,
        "godot_run_project",
        json!({
            "project_path": project.path(),
            "scene": "res://scenes/main.tscn",
            "mode": "process",
            "headless": true
        }),
    )
    .await;
    assert!(!started.is_error, "{}", text_of(&started));
    let run_id = run_id_of(&started);
    let ready = call(
        &tools,
        "godot_game_wait",
        json!({"run_id": run_id, "node": "Body", "timeout": 180}),
    )
    .await;
    assert!(!ready.is_error, "{}", text_of(&ready));
    let before = body_state(&tools, &run_id).await;
    assert!(
        before["mesh"].as_str().unwrap_or("").contains("BoxMesh"),
        "{before}"
    );
    assert!(no_object(&before["material_override"]), "{before}");

    let assigned = call(
        &tools,
        "godot_game_set",
        json!({
            "run_id": run_id,
            "path": "Body",
            "properties": {
                "mesh": "res://resources/quad.tres",
                "material_override": "res://resources/tint.tres"
            }
        }),
    )
    .await;
    assert!(!assigned.is_error, "{}", text_of(&assigned));
    let after = body_state(&tools, &run_id).await;
    assert!(
        after["mesh"].as_str().unwrap_or("").contains("QuadMesh"),
        "{after}"
    );
    assert!(
        after["material_override"]
            .as_str()
            .unwrap_or("")
            .contains("StandardMaterial3D"),
        "{after}"
    );
    // The loaded resource itself, not a copy: its saved size is there.
    let size = call(
        &tools,
        "godot_game_get",
        json!({"run_id": run_id, "path": "Body", "properties": ["mesh:size"]}),
    )
    .await;
    assert_eq!(
        json_of(&size)["properties"]["mesh:size"],
        json!([3.0, 3.0]),
        "{}",
        text_of(&size)
    );

    let refusals = [
        (
            json!({"mesh": "res://resources/tint.tres"}),
            vec!["Mesh", "res://resources/tint.tres", "StandardMaterial3D"],
        ),
        (
            json!({"mesh": "res://resources/missing.tres"}),
            vec!["res://resources/missing.tres"],
        ),
        (
            json!({"mesh": "res://../outside.tres"}),
            vec!["res://../outside.tres"],
        ),
        (json!({"mesh": "QuadMesh"}), vec!["res://", "Mesh"]),
        // A good value before a refused one is not written either.
        (
            json!({"transparency": 0.5, "material_override": "res://resources/quad.tres"}),
            vec!["Material", "QuadMesh"],
        ),
    ];
    for (properties, needles) in refusals {
        let refused = call(
            &tools,
            "godot_game_set",
            json!({"run_id": run_id, "path": "Body", "properties": properties}),
        )
        .await;
        let message = text_of(&refused);
        assert!(refused.is_error, "{properties}: {message}");
        for needle in needles {
            assert!(
                message.contains(needle),
                "{properties}: {message} lacks {needle}"
            );
        }
        assert!(message.contains("Nothing was changed"), "{message}");
        assert_eq!(body_state(&tools, &run_id).await, after, "{properties}");
    }

    let cleared = call(
        &tools,
        "godot_game_set",
        json!({"run_id": run_id, "path": "Body", "properties": {"material_override": null}}),
    )
    .await;
    assert!(!cleared.is_error, "{}", text_of(&cleared));
    let cleared_state = body_state(&tools, &run_id).await;
    assert!(
        no_object(&cleared_state["material_override"]),
        "{cleared_state}"
    );

    let stopped = call(&tools, "godot_stop_project", json!({"run_id": run_id})).await;
    assert!(!stopped.is_error, "{}", text_of(&stopped));
}

#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn headless_game_sets_a_shader_parameter_on_a_material() {
    let _ = IGNORE;
    let project = material_project();
    let tools = manager(project.path());
    let started = call(
        &tools,
        "godot_run_project",
        json!({
            "project_path": project.path(),
            "scene": "res://scenes/main.tscn",
            "mode": "process",
            "headless": true
        }),
    )
    .await;
    assert!(!started.is_error, "{}", text_of(&started));
    let run_id = run_id_of(&started);
    let ready = call(
        &tools,
        "godot_game_wait",
        json!({"run_id": run_id, "node": "Swatch", "timeout": 180}),
    )
    .await;
    assert!(!ready.is_error, "{}", text_of(&ready));

    let before = call(
        &tools,
        "godot_game_get",
        json!({"run_id": run_id, "path": "Swatch", "properties": ["material:shader_parameter/caustics"]}),
    )
    .await;
    assert!(!before.is_error, "{}", text_of(&before));
    // Godot's get returns null for a shader uniform until something writes it, even when the
    // uniform declares a default. The property is still there: the write below reads back.
    assert!(
        json_of(&before)["properties"]["material:shader_parameter/caustics"].is_null(),
        "{}",
        text_of(&before)
    );

    let set = call(
        &tools,
        "godot_game_set",
        json!({"run_id": run_id, "path": "Swatch", "properties": {"material:shader_parameter/caustics": true}}),
    )
    .await;
    assert!(!set.is_error, "{}", text_of(&set));
    assert_eq!(
        json_of(&set)["properties"]["material:shader_parameter/caustics"],
        true,
        "{}",
        text_of(&set)
    );
    let after = call(
        &tools,
        "godot_game_get",
        json!({"run_id": run_id, "path": "Swatch", "properties": ["material:shader_parameter/caustics"]}),
    )
    .await;
    assert_eq!(
        json_of(&after)["properties"]["material:shader_parameter/caustics"],
        true,
        "{}",
        text_of(&after)
    );

    let mark = call(
        &tools,
        "godot_game_set",
        json!({"run_id": run_id, "path": ".", "properties": {"mark": 2}}),
    )
    .await;
    assert!(!mark.is_error, "{}", text_of(&mark));
    assert_eq!(
        json_of(&mark)["properties"]["mark"],
        2,
        "{}",
        text_of(&mark)
    );

    let private = call(
        &tools,
        "godot_game_get",
        json!({"run_id": run_id, "path": ".", "properties": ["_secret", "pack:_tone"]}),
    )
    .await;
    assert!(!private.is_error, "{}", text_of(&private));
    assert_eq!(
        json_of(&private)["properties"]["_secret"],
        9,
        "{}",
        text_of(&private)
    );
    assert_eq!(
        json_of(&private)["properties"]["pack:_tone"],
        4,
        "{}",
        text_of(&private)
    );
    let named = call(
        &tools,
        "godot_game_get",
        json!({"run_id": run_id, "path": ".", "properties": ["script", "name"]}),
    )
    .await;
    assert!(!named.is_error, "{}", text_of(&named));
    let named_props = &json_of(&named)["properties"];
    assert!(
        named_props.get("script").is_none(),
        "a requested script name is omitted, not an error: {}",
        text_of(&named)
    );
    assert_eq!(named_props["name"], "Root", "{}", text_of(&named));

    let nested = call(
        &tools,
        "godot_game_set",
        json!({"run_id": run_id, "path": "WaterPaper", "properties": {"material_override:shader_parameter/caustics": true}}),
    )
    .await;
    assert!(!nested.is_error, "{}", text_of(&nested));
    let nested_read = call(
        &tools,
        "godot_game_get",
        json!({"run_id": run_id, "path": "WaterPaper", "properties": ["material_override:shader_parameter/caustics"]}),
    )
    .await;
    assert_eq!(
        json_of(&nested_read)["properties"]["material_override:shader_parameter/caustics"],
        true,
        "{}",
        text_of(&nested_read)
    );

    for (path, properties) in [
        (".", json!({"_secret": 0})),
        (
            "WaterPaper",
            json!({"material_override:script": "res://scenes/main.tscn"}),
        ),
    ] {
        let denied = call(
            &tools,
            "godot_game_set",
            json!({"run_id": run_id, "path": path, "properties": properties}),
        )
        .await;
        assert!(denied.is_error, "{}", text_of(&denied));
        assert!(
            text_of(&denied).to_lowercase().contains("refus"),
            "{}",
            text_of(&denied)
        );
    }
    let secret_still = call(
        &tools,
        "godot_game_get",
        json!({"run_id": run_id, "path": ".", "properties": ["_secret"]}),
    )
    .await;
    assert_eq!(
        json_of(&secret_still)["properties"]["_secret"],
        9,
        "a refused write changed _secret: {}",
        text_of(&secret_still)
    );

    for properties in [
        json!({"material:script": "res://scenes/main.tscn"}),
        json!({"material:shader_parameter/_hidden": true}),
        json!({"script": "res://scenes/main.tscn"}),
    ] {
        let denied = call(
            &tools,
            "godot_game_set",
            json!({"run_id": run_id, "path": "Swatch", "properties": properties}),
        )
        .await;
        assert!(denied.is_error, "{}", text_of(&denied));
        assert!(
            text_of(&denied).to_lowercase().contains("refus"),
            "{}",
            text_of(&denied)
        );
    }
    let still = call(
        &tools,
        "godot_game_get",
        json!({"run_id": run_id, "path": "Swatch", "properties": ["material:shader_parameter/caustics"]}),
    )
    .await;
    assert_eq!(
        json_of(&still)["properties"]["material:shader_parameter/caustics"],
        true,
        "a refused write changed the uniform: {}",
        text_of(&still)
    );

    // A path may only step through a Resource, and nothing in it may name a script, a private
    // member or an empty segment. Every refused write leaves the game as it was.
    for (path, properties) in [
        (".", json!({"mark:x": 5})),
        (".", json!({"name:x": "Other"})),
        ("Swatch", json!({"owner:name": "Other"})),
        (".", json!({":mark": 5})),
        (".", json!({"mark:": 5})),
        (".", json!({"pack::_tone": 5})),
        (".", json!({"_secret:x": 5})),
        (".", json!({"pack:_tone": 5})),
        (".", json!({"behavior:source_code": "extends Node\n"})),
    ] {
        let denied = call(
            &tools,
            "godot_game_set",
            json!({"run_id": run_id, "path": path, "properties": properties}),
        )
        .await;
        assert!(denied.is_error, "{properties}: {}", text_of(&denied));
        let message = text_of(&denied).to_lowercase();
        assert!(
            message.contains("refus") || message.contains("not a resource"),
            "{properties}: {message}"
        );
    }
    let unchanged = call(
        &tools,
        "godot_game_get",
        json!({"run_id": run_id, "path": ".", "properties": ["mark", "_secret", "pack:_tone", "behavior:resource_path"]}),
    )
    .await;
    assert!(!unchanged.is_error, "{}", text_of(&unchanged));
    let kept = &json_of(&unchanged)["properties"];
    assert_eq!(kept["mark"], 2, "{kept}");
    assert_eq!(kept["_secret"], 9, "{kept}");
    assert_eq!(kept["pack:_tone"], 4, "{kept}");
    assert_eq!(
        kept["behavior:resource_path"], "res://scripts/pack.gd",
        "reading through a script resource still works: {kept}"
    );

    let shots = tempfile::tempdir().expect("record dir");
    let recorded = call(
        &tools,
        "godot_game_record",
        json!({"run_id": run_id, "frames": 2, "directory": shots.path()}),
    )
    .await;
    assert!(recorded.is_error, "{}", text_of(&recorded));
    assert!(
        text_of(&recorded).contains("headless"),
        "{}",
        text_of(&recorded)
    );
    assert!(
        recorded
            .content
            .iter()
            .all(|item| !matches!(item, godot_mcp::protocol::ToolContent::Image { .. })),
        "a headless recording returned image bytes"
    );
    let leftover = std::fs::read_dir(shots.path()).unwrap().count();
    assert_eq!(leftover, 0, "a headless recording wrote files");

    let stopped = call(&tools, "godot_stop_project", json!({"run_id": run_id})).await;
    assert!(!stopped.is_error, "{}", text_of(&stopped));
}

#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn windowed_game_records_frames_as_png_files() {
    let _ = IGNORE;
    if !windowed_game_can_open() {
        eprintln!("skipped: a windowed recording needs a display server");
        return;
    }
    let project = project();
    let tools = manager(project.path());
    let started = call(
        &tools,
        "godot_run_project",
        json!({
            "project_path": project.path(),
            "scene": "res://scenes/main.tscn",
            "mode": "process"
        }),
    )
    .await;
    assert!(!started.is_error, "{}", text_of(&started));
    let run_id = run_id_of(&started);
    let shots = tempfile::tempdir().expect("record dir");
    let recorded = call(
        &tools,
        "godot_game_record",
        json!({
            "run_id": run_id,
            "frames": 8,
            "every": 1,
            "max_dimension": 480,
            "directory": shots.path()
        }),
    )
    .await;
    assert!(!recorded.is_error, "{}", text_of(&recorded));
    assert!(
        recorded
            .content
            .iter()
            .all(|item| matches!(item, godot_mcp::protocol::ToolContent::Text { .. })),
        "a recording must return paths, not image bytes: {}",
        text_of(&recorded)
    );
    let body = json_of(&recorded);
    assert_eq!(body["count"], 8, "{body}");
    assert_eq!(body["video"]["ffmpeg"], "not_requested", "{body}");
    let frames = body["frames"].as_array().expect("frames");
    assert_eq!(frames.len(), 8);
    let mut steps = Vec::new();
    let mut previous = json_number(&frames[0]["msec"]);
    assert!(
        previous <= 5.0,
        "the first frame's msec includes work after the grab: {previous} {body}"
    );
    for (index, frame) in frames.iter().enumerate() {
        let path = shots.path().join(format!("frame-{:04}.png", index + 1));
        let bytes =
            std::fs::read(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
        let (width, height) =
            png_size(&bytes).unwrap_or_else(|| panic!("{} is not a PNG", path.display()));
        assert_eq!(frame["path"], path.display().to_string(), "{frame}");
        assert_eq!(json_number(&frame["width"]), width as f64, "{frame}");
        assert_eq!(json_number(&frame["height"]), height as f64, "{frame}");
        assert!(
            width > 0 && height > 0 && width.max(height) <= 480,
            "{frame}"
        );
        assert!(frame.get("process_frame").is_some(), "{frame}");
        let msec = json_number(&frame["msec"]);
        if index > 0 {
            steps.push(msec - previous);
        }
        previous = msec;
    }
    steps.sort_by(|left, right| left.partial_cmp(right).unwrap_or(std::cmp::Ordering::Equal));
    let median = steps[steps.len() / 2];
    // every 1 captures consecutive process frames. 40 ms covers a 30 fps machine and
    // still fails the old path, which spent the PNG encode inside each frame (~50 ms).
    assert!(
        (4.0..=40.0).contains(&median),
        "median frame step {median} ms is not one frame: {steps:?}"
    );
    let stopped = call(&tools, "godot_stop_project", json!({"run_id": run_id})).await;
    assert!(!stopped.is_error, "{}", text_of(&stopped));
}

fn material_project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("project");
    let root = dir.path();
    std::fs::create_dir_all(root.join("scenes")).unwrap();
    std::fs::create_dir_all(root.join("scripts")).unwrap();
    std::fs::write(
        root.join("project.godot"),
        "config_version=5\n\n[application]\n\nconfig/name=\"MaterialProbe\"\nrun/main_scene=\"res://scenes/main.tscn\"\nconfig/features=PackedStringArray(\"4.5\")\n",
    )
    .unwrap();
    std::fs::write(
        root.join("scripts/pack.gd"),
        "extends Resource\nvar _tone := 4\n",
    )
    .unwrap();
    std::fs::write(
        root.join("scripts/swatch.gd"),
        r#"extends Node2D
var _secret := 9
var mark := 1
var pack: Resource
var behavior: Script

func _ready() -> void:
    pack = load("res://scripts/pack.gd").new()
    behavior = load("res://scripts/pack.gd")
    var paper := MeshInstance3D.new()
    paper.name = "WaterPaper"
    var shader3 := Shader.new()
    shader3.code = "shader_type spatial;\nuniform bool caustics = false;\n"
    var material3 := ShaderMaterial.new()
    material3.shader = shader3
    paper.material_override = material3
    add_child(paper)
    var rect := ColorRect.new()
    rect.name = "Swatch"
    rect.size = Vector2(8, 8)
    var shader := Shader.new()
    shader.code = "shader_type canvas_item;\nuniform bool caustics = false;\n"
    var material := ShaderMaterial.new()
    material.shader = shader
    rect.material = material
    add_child(rect)
"#,
    )
    .unwrap();
    std::fs::write(
        root.join("scenes/main.tscn"),
        "[gd_scene load_steps=2 format=3]\n\n[ext_resource type=\"Script\" path=\"res://scripts/swatch.gd\" id=\"1\"]\n\n[node name=\"Root\" type=\"Node2D\"]\nscript = ExtResource(\"1\")\n",
    )
    .unwrap();
    dir
}

/// godot_run_script with real Godot: a kill keeps what was printed before it, and only the setting
/// that turns off flushing loses `print()` lines.
#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn run_script_timeout_keeps_the_lines_godot_printed_before_the_kill() {
    let _ = IGNORE;
    let project = project_dir_with_scripts();
    std::fs::write(
        project.path().join("scripts/print_and_hang.gd"),
        "extends SceneTree\n\nfunc _initialize() -> void:\n\tfor index: int in 100:\n\t\tprint(\"result \", index)\n\tprinterr(\"on stderr\")\n",
    )
    .unwrap();
    let tools = manager(project.path());
    let run = || async {
        let answer = call(
            &tools,
            "godot_run_script",
            json!({
                "script": "res://scripts/print_and_hang.gd",
                "project_path": project.path(),
                "timeout_seconds": 8
            }),
        )
        .await;
        assert!(answer.is_error, "{}", text_of(&answer));
        let answer_text = text_of(&answer);
        assert!(answer_text.contains("\"timed_out\": true"), "{answer_text}");
        answer_text
    };

    let flushed = run().await;
    assert!(
        flushed.contains("result 0\\n") && flushed.contains("result 99\\n"),
        "{flushed}"
    );
    assert!(flushed.contains("on stderr"), "{flushed}");
    assert!(flushed.contains("\"stdout_buffered\": false"), "{flushed}");

    let settings = project.path().join("project.godot");
    let without_flush = std::fs::read_to_string(&settings).unwrap().replace(
        "[application]\n",
        "[application]\n\nrun/flush_stdout_on_print.debug=false\n",
    );
    std::fs::write(&settings, without_flush).unwrap();
    let buffered = run().await;
    assert!(!buffered.contains("result 0\\n"), "{buffered}");
    assert!(buffered.contains("on stderr"), "{buffered}");
    assert!(buffered.contains("\"stdout_buffered\": true"), "{buffered}");
    assert!(buffered.contains("use printerr()"), "{buffered}");
}

/// godot_run_script with real Godot: fixed_fps runs physics frames without waiting for the clock,
/// and the script counts the same frames and the same game time as in real time.
#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn run_script_with_fixed_fps_counts_the_same_frames_far_faster() {
    let _ = IGNORE;
    let project = project_dir_with_scripts();
    std::fs::write(
        project.path().join("scripts/frames.gd"),
        "extends SceneTree\n\nfunc _initialize() -> void:\n\tvar game_time: float = 0.0\n\tvar first: int = Engine.get_physics_frames()\n\tfor index: int in 600:\n\t\tawait physics_frame\n\t\tgame_time += root.get_physics_process_delta_time()\n\tprint(\"FRAMES \", Engine.get_physics_frames() - first, \" GAME_TIME \", snappedf(game_time, 0.001))\n\tquit()\n",
    )
    .unwrap();
    let tools = manager(project.path());
    let mut answers = Vec::new();
    for fixed_fps in [None, Some(60)] {
        let mut arguments = json!({
            "script": "res://scripts/frames.gd",
            "project_path": project.path(),
            "timeout_seconds": 120
        });
        if let Some(fps) = fixed_fps {
            arguments["fixed_fps"] = json!(fps);
        }
        let started = std::time::Instant::now();
        let answer = call(&tools, "godot_run_script", arguments).await;
        let elapsed = started.elapsed();
        let answer_text = text_of(&answer);
        assert!(!answer.is_error, "{answer_text}");
        let line = answer_text
            .split("\\n")
            .chain(answer_text.lines())
            .find_map(|line| {
                line.find("FRAMES ")
                    .map(|at| line[at..].trim_end_matches(['\"', ',']))
            })
            .unwrap_or_else(|| panic!("no FRAMES line: {answer_text}"))
            .to_string();
        answers.push((line, elapsed));
    }
    let (real_time, real_time_elapsed) = &answers[0];
    let (fast, fast_elapsed) = &answers[1];
    assert_eq!(fast, real_time, "fixed_fps changed what the script counted");
    assert!(
        fast.starts_with("FRAMES 600 GAME_TIME 10"),
        "600 physics frames at 60 per second: {fast}"
    );
    assert!(
        *real_time_elapsed > std::time::Duration::from_secs(9),
        "the real-time run should wait for the clock: {real_time_elapsed:?}"
    );
    assert!(
        *fast_elapsed < std::time::Duration::from_secs(3),
        "600 frames should finish far faster than the 10 s of real time: {fast_elapsed:?}"
    );
}

#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn run_script_returns_an_exit_code_and_kills_a_script_that_never_quits() {
    let _ = IGNORE;
    let project = project_dir_with_scripts();
    let tools = manager(project.path());
    let ok = call(
        &tools,
        "godot_run_script",
        json!({
            "script": "res://scripts/quit.gd",
            "project_path": project.path(),
            "user_args": ["from-e2e"],
            "timeout_seconds": 30
        }),
    )
    .await;
    assert!(!ok.is_error, "{}", text_of(&ok));
    let ok_text = text_of(&ok);
    assert!(
        ok_text.contains("\"exit_code\": 0") && ok_text.contains("from-e2e"),
        "{ok_text}"
    );

    let started = std::time::Instant::now();
    let hung = call(
        &tools,
        "godot_run_script",
        json!({
            "script": "res://scripts/hang.gd",
            "project_path": project.path(),
            "timeout_seconds": 8
        }),
    )
    .await;
    // A busy machine can spend the timeout starting Godot, so the script may never print
    // "HANGING". The kill and the timeout flag are what this half of the test proves.
    assert!(
        started.elapsed() < std::time::Duration::from_secs(30),
        "a SceneTree script that never quits was not killed"
    );
    assert!(hung.is_error, "{}", text_of(&hung));
    let hung_text = text_of(&hung);
    assert!(
        hung_text.contains("killed the process tree") && hung_text.contains("\"timed_out\": true"),
        "{hung_text}"
    );
}

#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn run_script_reports_a_script_that_godot_could_not_load() {
    let _ = IGNORE;
    let project = project_dir_with_scripts();
    let root = project.path();
    std::fs::write(
        root.join("scripts/broken.gd"),
        "extends SceneTree\n\nfunc _initialize() -> void:\n\tvar x =\n\tquit(0)\n",
    )
    .unwrap();
    std::fs::write(
        root.join("scripts/lib.gd"),
        "extends RefCounted\n\nclass Inner extends Node:\n\tpass\n",
    )
    .unwrap();
    // The base check does not follow an inner class, so Godot itself refuses this one.
    std::fs::write(
        root.join("scripts/inner_base.gd"),
        "extends \"res://scripts/lib.gd\".Inner\n\nfunc _init() -> void:\n\tprint(\"RAN\")\n",
    )
    .unwrap();
    std::fs::write(
        root.join("scripts/node.gd"),
        "extends Node\n\nfunc _init() -> void:\n\tprint(\"RAN\")\n",
    )
    .unwrap();
    let tools = manager(root);
    let run = |script: &'static str| {
        let tools = Arc::clone(&tools);
        let root = root.to_path_buf();
        async move {
            call(
                &tools,
                "godot_run_script",
                json!({"script": script, "project_path": root, "timeout_seconds": 60}),
            )
            .await
        }
    };

    let broken = run("res://scripts/broken.gd").await;
    let broken_text = text_of(&broken);
    assert!(broken.is_error, "{broken_text}");
    assert!(
        broken_text.contains("did not run") && broken_text.contains("scripts/broken.gd:4"),
        "{broken_text}"
    );

    let inner = run("res://scripts/inner_base.gd").await;
    let inner_text = text_of(&inner);
    assert!(inner.is_error, "{inner_text}");
    assert!(
        inner_text.contains("did not run")
            && inner_text.contains("doesn't inherit from SceneTree or MainLoop"),
        "{inner_text}"
    );
    assert!(
        !inner_text.contains("\"script_errors\": []"),
        "{inner_text}"
    );

    let node = run("res://scripts/node.gd").await;
    let node_text = text_of(&node);
    assert!(node.is_error, "{node_text}");
    assert!(
        node_text.contains("extends Node, not SceneTree or MainLoop")
            && !node_text.contains("exit_code"),
        "the base check did not refuse before Godot started: {node_text}"
    );
}

#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn run_script_writes_back_the_uids_and_shader_parameters_a_headless_save_drops() {
    let _ = IGNORE;
    let project = project_dir_with_scripts();
    let root = project.path();
    std::fs::write(
        root.join("water.gdshader"),
        "shader_type spatial;\nuniform vec4 tint : source_color = vec4(0.2, 0.5, 0.8, 1.0);\nuniform float speed = 1.5;\nvoid fragment() { ALBEDO = tint.rgb * speed; }\n",
    )
    .unwrap();
    std::fs::write(root.join("water.gdshader.uid"), "uid://dlh0ah0opc20e\n").unwrap();
    std::fs::write(
        root.join("pond.tscn"),
        "[gd_scene load_steps=3 format=3 uid=\"uid://cpond1234abcd\"]\n\n[ext_resource type=\"Shader\" uid=\"uid://dlh0ah0opc20e\" path=\"res://water.gdshader\" id=\"1_abcde\"]\n\n[sub_resource type=\"ShaderMaterial\" id=\"ShaderMaterial_aaaaa\"]\nrender_priority = 0\nshader = ExtResource(\"1_abcde\")\nshader_parameter/speed = 2.0\n\n[sub_resource type=\"BoxMesh\" id=\"BoxMesh_bbbbb\"]\n\n[node name=\"Pond\" type=\"Node3D\"]\n\n[node name=\"Water\" type=\"MeshInstance3D\" parent=\".\"]\nmaterial_override = SubResource(\"ShaderMaterial_aaaaa\")\nmesh = SubResource(\"BoxMesh_bbbbb\")\n",
    )
    .unwrap();
    std::fs::write(
        root.join("scripts/edit_pond.gd"),
        "extends SceneTree\n\nfunc _initialize() -> void:\n\tvar scene: PackedScene = load(\"res://pond.tscn\")\n\tvar pond: Node = scene.instantiate(PackedScene.GEN_EDIT_STATE_MAIN)\n\tvar material: ShaderMaterial = pond.get_node(\"Water\").material_override\n\tmaterial.set_shader_parameter(\"speed\", 3.0)\n\tscene.pack(pond)\n\tprint(\"SAVED \", ResourceSaver.save(scene))\n\tpond.free()\n\tquit(0)\n",
    )
    .unwrap();
    let tools = manager(root);
    let result = call(
        &tools,
        "godot_run_script",
        json!({
            "script": "res://scripts/edit_pond.gd",
            "project_path": root,
            "timeout_seconds": 60
        }),
    )
    .await;
    let answer = text_of(&result);
    assert!(answer.contains("SAVED 0"), "{answer}");
    assert!(!answer.starts_with("WARNING"), "{answer}");
    // The repair entry is what shows the engine still drops the uids and writes null uniforms. If
    // a Godot version keeps them, the list is empty: this test then fails on purpose, and the tool
    // description needs updating.
    assert!(
        answer.contains("\"uid\": \"uid://cpond1234abcd\"")
            && answer.contains("\"ext_resource_uids_restored\": 1")
            && answer.contains("\"removed_null_shader_parameters\": [\n        \"tint\"\n      ]"),
        "{answer}"
    );
    assert!(answer.contains("\"resource_damage\": []"), "{answer}");
    let saved = std::fs::read_to_string(root.join("pond.tscn")).unwrap();
    assert!(
        saved.starts_with("[gd_scene format=3 uid=\"uid://cpond1234abcd\"]")
            && saved.contains("[ext_resource type=\"Shader\" uid=\"uid://dlh0ah0opc20e\" path=\"res://water.gdshader\"")
            && saved.contains("shader_parameter/speed = 3.0")
            && !saved.contains("= null"),
        "the uids were not written back:\n{saved}"
    );
}

fn project_dir_with_scripts() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("script project");
    let root = dir.path();
    std::fs::create_dir_all(root.join("scripts")).unwrap();
    std::fs::write(
        root.join("project.godot"),
        "config_version=5\n\n[application]\n\nconfig/name=\"ScriptRun\"\nconfig/features=PackedStringArray(\"4.8\")\n",
    )
    .unwrap();
    std::fs::write(
        root.join("scripts/quit.gd"),
        "extends SceneTree\n\nfunc _initialize() -> void:\n\tprint(\"USER \", \" \".join(OS.get_cmdline_user_args()))\n\tquit(0)\n",
    )
    .unwrap();
    std::fs::write(
        root.join("scripts/hang.gd"),
        "extends SceneTree\n\nfunc _initialize() -> void:\n\tprint(\"HANGING\")\n",
    )
    .unwrap();
    dir
}

/// godot_run_script with real Godot: the leak reports it prints at exit stay in the output,
/// are counted in exit_leaks with a hint on freeing nodes, and the script's own error stays.
#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn run_script_counts_the_leaks_godot_reports_at_exit() {
    let _ = IGNORE;
    let project = project_dir_with_scripts();
    std::fs::write(
        project.path().join("scripts/leak.gd"),
        "extends SceneTree\n\nfunc _initialize() -> void:\n\tvar kept: Node = Node.new()\n\tkept.set_meta(\"mesh\", BoxMesh.new())\n\tpush_error(\"zone 7 has no shore\")\n\tprint(\"placed\")\n\tquit(0)\n",
    )
    .unwrap();
    let tools = manager(project.path());
    let result = call(
        &tools,
        "godot_run_script",
        json!({
            "script": "res://scripts/leak.gd",
            "project_path": project.path(),
            "timeout_seconds": 60
        }),
    )
    .await;
    let text = text_of(&result);
    assert!(!result.is_error, "{text}");
    let answer: Value = serde_json::from_str(&text).expect("json answer");
    assert!(
        answer["exit_leaks"]["objectdb_instances"].as_u64() >= Some(1),
        "{answer}"
    );
    assert!(
        answer["hint"]
            .as_str()
            .is_some_and(|hint| hint.contains("free()")),
        "{answer}"
    );
    let output = format!("{}{}", answer["stdout"], answer["stderr"]);
    assert!(output.contains("leaked at exit"), "{output}");
    assert!(
        answer["stderr"]
            .as_str()
            .is_some_and(|err| err.contains("ERROR: zone 7 has no shore")),
        "{answer}"
    );
    assert!(
        answer["stdout"]
            .as_str()
            .is_some_and(|out| out.contains("placed")),
        "{answer}"
    );
}

/// A result screen that pauses the tree, as a victory or pause menu does: the root ignores
/// input while paused and the menu's button is the only thing that still reacts.
fn paused_menu_project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("paused menu project");
    let root = dir.path();
    std::fs::create_dir_all(root.join("scripts")).unwrap();
    std::fs::write(
        root.join("project.godot"),
        "config_version=5\n\n[application]\n\nconfig/name=\"PausedMenu\"\nrun/main_scene=\"res://scripts/menu.tscn\"\nconfig/features=PackedStringArray(\"4.5\")\n\n[display]\n\nwindow/size/viewport_width=200\nwindow/size/viewport_height=150\nwindow/size/window_width_override=400\nwindow/size/window_height_override=300\nwindow/stretch/mode=\"canvas_items\"\nwindow/stretch/aspect=\"expand\"\n",
    )
    .unwrap();
    std::fs::write(
        root.join("scripts/menu.gd"),
        r#"extends Node2D
var again_presses := 0
var again_downs := 0
var far_presses := 0
var physics_ticks := 0

func _ready() -> void:
    get_tree().paused = true

func _physics_process(_delta: float) -> void:
    physics_ticks += 1

func _on_again_pressed() -> void:
    again_presses += 1

func _on_again_down() -> void:
    again_downs += 1

func _on_far_pressed() -> void:
    far_presses += 1

func counters() -> Dictionary:
    return {"again_presses": again_presses, "again_downs": again_downs, "far_presses": far_presses, "paused": get_tree().paused, "physics_ticks": physics_ticks}
"#,
    )
    .unwrap();
    std::fs::write(
        root.join("scripts/menu.tscn"),
        "[gd_scene load_steps=2 format=3]\n\n[ext_resource type=\"Script\" path=\"res://scripts/menu.gd\" id=\"1\"]\n\n[node name=\"Menu\" type=\"Node2D\"]\nscript = ExtResource(\"1\")\n\n[node name=\"Layer\" type=\"CanvasLayer\" parent=\".\"]\nprocess_mode = 2\n\n[node name=\"Again\" type=\"Button\" parent=\"Layer\"]\noffset_left = 4.0\noffset_top = 4.0\noffset_right = 44.0\noffset_bottom = 44.0\n\n[node name=\"Far\" type=\"Button\" parent=\"Layer\"]\noffset_left = 130.0\noffset_top = 90.0\noffset_right = 180.0\noffset_bottom = 130.0\n\n[connection signal=\"pressed\" from=\"Layer/Far\" to=\".\" method=\"_on_far_pressed\"]\n[connection signal=\"pressed\" from=\"Layer/Again\" to=\".\" method=\"_on_again_pressed\"]\n[connection signal=\"button_down\" from=\"Layer/Again\" to=\".\" method=\"_on_again_down\"]\n",
    )
    .unwrap();
    dir
}

async fn menu_counters(tools: &Arc<ToolManager>, run_id: &str) -> Value {
    let reply = call(
        tools,
        "godot_game_call",
        json!({"run_id": run_id, "path": ".", "method": "counters", "permit": ["counters"]}),
    )
    .await;
    assert!(!reply.is_error, "{}", text_of(&reply));
    call_value(&json_of(&reply))
}

/// click_node, click and a mouse or touch tap complete a click on a button that works while the
/// game has paused its own tree, in a free-running game and in a held one.
#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn click_node_completes_a_click_while_the_tree_is_paused() {
    let _ = IGNORE;
    paused_menu_clicks(true).await;
    if windowed_game_can_open() {
        paused_menu_clicks(false).await;
    }
}

async fn paused_menu_clicks(headless: bool) {
    let project = paused_menu_project();
    let tools = manager(project.path());
    let started = call(
        &tools,
        "godot_run_project",
        json!({"project_path": project.path(), "mode": "process", "headless": headless}),
    )
    .await;
    assert!(!started.is_error, "{}", text_of(&started));
    let run_id = run_id_of(&started);

    let mut expected = 0;
    for step in [
        json!({"type": "click_node", "node": "Layer/Again"}),
        json!({"type": "click_node", "node": "Layer/Again", "pointer": "touch"}),
        json!({"type": "click", "x": 20, "y": 20}),
        json!({"type": "mouse_button", "button": "left", "x": 20, "y": 20}),
    ] {
        let answered = call(
            &tools,
            "godot_game_input",
            json!({"run_id": run_id, "steps": [step.clone(), {"type": "wait", "frames": 2}]}),
        )
        .await;
        assert!(!answered.is_error, "{step}: {}", text_of(&answered));
        expected += 1;
        let seen = menu_counters(&tools, &run_id).await;
        assert_eq!(seen["paused"], true, "{step}: {seen}");
        assert_eq!(
            seen["again_presses"], expected,
            "{step} did not click (headless {headless}): {seen}"
        );
    }

    // The answer names the space; click_node says where it clicked, in that space.
    let named = call(
        &tools,
        "godot_game_input",
        json!({"run_id": run_id, "steps": [
            {"type": "click_node", "node": "Layer/Again"}, {"type": "wait", "frames": 2}]}),
    )
    .await;
    assert!(!named.is_error, "{}", text_of(&named));
    let named = json_of(&named);
    expected += 1;
    assert_eq!(named["coordinate_space"], "viewport", "{named}");
    let scale = json_number(&named["space"]["window_per_viewport_pixel"][0]);
    let offset = json_number(&named["space"]["window_offset"][0]);
    assert!(
        scale > 0.0 && (scale - 1.0).abs() > 0.01,
        "not stretched: {named}"
    );
    let at = &named["steps"][0]["at"];
    assert!((json_number(&at[0]) - 24.0).abs() < 1.0, "{named}");
    assert!((json_number(&at[1]) - 24.0).abs() < 1.0, "{named}");

    // The same button in window pixels: viewport (20, 20) is window 20 * scale + offset.
    let window_xy = 20.0 * scale + offset;
    let window_y = 20.0 * json_number(&named["space"]["window_per_viewport_pixel"][1])
        + json_number(&named["space"]["window_offset"][1]);
    let in_window = call(
        &tools,
        "godot_game_input",
        json!({"run_id": run_id, "coordinate_space": "window", "steps": [
            {"type": "click", "x": window_xy, "y": window_y}, {"type": "wait", "frames": 2},
            {"type": "click_node", "node": "Layer/Again"}, {"type": "wait", "frames": 2}]}),
    )
    .await;
    assert!(!in_window.is_error, "{}", text_of(&in_window));
    let in_window = json_of(&in_window);
    expected += 2;
    assert_eq!(in_window["coordinate_space"], "window", "{in_window}");
    let at = &in_window["steps"][2]["at"];
    assert!(
        (json_number(&at[0]) - (24.0 * scale + offset)).abs() < 1.0,
        "click_node at is not in window pixels: {in_window}"
    );
    let seen = menu_counters(&tools, &run_id).await;
    assert_eq!(
        seen["again_presses"], expected,
        "window-space clicks: {seen} {in_window}"
    );

    let refused = call(
        &tools,
        "godot_game_input",
        json!({"run_id": run_id, "coordinate_space": "screen", "steps": [
            {"type": "click", "x": 1, "y": 1}]}),
    )
    .await;
    assert!(refused.is_error, "{}", text_of(&refused));
    assert!(
        text_of(&refused).contains("coordinate_space"),
        "{}",
        text_of(&refused)
    );

    // In a stretched window the button far from the origin is clicked where it is drawn.
    if !headless {
        let far = call(
            &tools,
            "godot_game_input",
            json!({"run_id": run_id, "steps": [
                {"type": "click_node", "node": "Layer/Far"}, {"type": "wait", "frames": 2},
                {"type": "click", "x": 155, "y": 110}, {"type": "wait", "frames": 2}]}),
        )
        .await;
        assert!(!far.is_error, "{}", text_of(&far));
        let seen = menu_counters(&tools, &run_id).await;
        assert_eq!(
            seen["far_presses"],
            2,
            "stretched click missed Far: {seen} {}",
            text_of(&far)
        );
    }

    // The same click in a held game: applied at once, released after hold_ticks ticks.
    hold_now(&tools, &run_id).await;
    let held = call(
        &tools,
        "godot_game_input",
        json!({"run_id": run_id, "steps": [{"type": "click_node", "node": "Layer/Again"}]}),
    )
    .await;
    assert!(!held.is_error, "{}", text_of(&held));
    let stepped = held_step(&tools, &run_id, json!({"ticks": 3})).await;
    assert!(!stepped.is_error, "{}", text_of(&stepped));
    let seen = menu_counters(&tools, &run_id).await;
    assert_eq!(
        seen["again_presses"],
        expected + 1,
        "held click_node did not click (headless {headless}): {seen}"
    );

    let stopped = call(&tools, "godot_stop_project", json!({"run_id": run_id})).await;
    assert!(!stopped.is_error, "{}", text_of(&stopped));
}

/// The addon's own scripts as res:// paths, read from this checkout.
fn addon_scripts(root: &Path) -> Vec<String> {
    fn walk(dir: &Path, root: &Path, found: &mut Vec<String>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, root, found);
            } else if path.extension().and_then(|ext| ext.to_str()) == Some("gd") {
                let relative = path.strip_prefix(root).unwrap_or(&path);
                found.push(format!(
                    "res://{}",
                    relative.to_string_lossy().replace('\\', "/")
                ));
            }
        }
    }
    let mut found = Vec::new();
    walk(&root.join("addons/godot_mcp"), root, &mut found);
    found.sort();
    found
}

/// The shipped addon has no GDScript analyzer warnings. `--check-only` prints none at all, so
/// this asks the language server, which runs the full analyzer, about every addon script.
#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn addon_scripts_have_no_analyzer_warnings() {
    let _ = IGNORE;
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let scripts = addon_scripts(&root);
    assert!(scripts.len() > 10, "found only {scripts:?}");
    let tools = manager(&root);
    let mut found = Vec::new();
    for script in &scripts {
        let result = call(
            &tools,
            "godot_code_diagnostics",
            json!({"project_path": root, "script_path": script}),
        )
        .await;
        assert!(!result.is_error, "{script}: {}", text_of(&result));
        let body = json_of(&result);
        let diagnostics = body["diagnostics"]
            .as_array()
            .unwrap_or_else(|| panic!("{script}: {body}"));
        // LSP severity 1 is an error and 2 a warning; 3 and 4 are information and hints.
        for item in diagnostics {
            let severity = &item["severity"];
            let reported = matches!(severity.as_i64(), Some(1 | 2))
                || matches!(severity.as_str(), Some("error" | "warning"));
            if reported {
                found.push(format!("{script}: {item}"));
            }
        }
    }
    assert!(
        found.is_empty(),
        "analyzer diagnostics in the addon:\n{}",
        found.join("\n")
    );
}

/// A game whose scene root keeps handlers of the kind a project hands to the runtime (each takes
/// one dictionary) and records what the engine's logger is told while they run.
fn handler_probe_project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("project");
    let root = dir.path();
    std::fs::create_dir_all(root.join("scenes")).unwrap();
    std::fs::create_dir_all(root.join("scripts")).unwrap();
    std::fs::write(
        root.join("project.godot"),
        "config_version=5

[application]

config/name=\"HandlerProbe\"
run/main_scene=\"res://scenes/handlers.tscn\"
config/features=PackedStringArray(\"4.5\")
",
    )
    .unwrap();
    std::fs::write(
        root.join("scripts/handlers.gd"),
        r#"extends Node

class ProbeLogger extends Logger:
    var seen: Array = []

    func _log_message(_message: String, _error: bool) -> void:
        pass

    func _log_error(function: String, _file: String, _line: int, code: String, rationale: String, _editor_notify: bool, error_type: int, _script_backtraces: Array[ScriptBacktrace]) -> void:
        seen.append({
            "is_script": error_type == Logger.ERROR_TYPE_SCRIPT,
            "is_error": error_type == Logger.ERROR_TYPE_ERROR,
            "is_warning": error_type == Logger.ERROR_TYPE_WARNING,
            "on_main_thread": OS.get_thread_caller_id() == OS.get_main_thread_id(),
            "function": function,
            "code": code,
            "rationale": rationale,
        })

var probe: ProbeLogger
var progress := ""
var outcome: Variant = null
var logged_at_return := -1

func _ready() -> void:
    probe = ProbeLogger.new()
    OS.add_logger(probe)

func _fails_in_script(_args: Dictionary) -> Variant:
    var missing: Variant = {}.get("none")
    return missing.name

func _pushes_an_error(_args: Dictionary) -> Variant:
    push_error("handler-pushed")
    return "after-push"

func _awaits_first(_args: Dictionary) -> Variant:
    await get_tree().process_frame
    return "after-await"

func entries() -> Array:
    var copy := probe.seen.duplicate(true)
    probe.seen.clear()
    return copy

func plain_script_error() -> Array:
    var handler := Callable(self, "_fails_in_script")
    var returned: Variant = handler.call({})
    logged_at_return = probe.seen.size()
    outcome = "null" if returned == null else str(returned)
    return [outcome]

func plain_push_error() -> Array:
    var handler := Callable(self, "_pushes_an_error")
    var returned: Variant = handler.call({})
    logged_at_return = probe.seen.size()
    outcome = str(returned)
    return [outcome]

func plain_awaiting() -> Array:
    progress = "before"
    var handler := Callable(self, "_awaits_first")
    var returned: Variant = handler.call({})
    progress = "after-plain-call"
    outcome = (returned as Object).get_class() if returned is Object else str(returned)
    return [outcome]

func awaited_awaiting() -> void:
    progress = "before"
    var handler := Callable(self, "_awaits_first")
    var returned: Variant = await handler.call({})
    progress = "after-awaited-call"
    outcome = str(returned)

func awaited_plain() -> void:
    progress = "before"
    var handler := Callable(self, "_pushes_an_error")
    var returned: Variant = await handler.call({})
    progress = "after-awaited-call"
    outcome = str(returned)
"#,
    )
    .unwrap();
    std::fs::write(
        root.join("scenes/handlers.tscn"),
        "[gd_scene load_steps=2 format=3]

[ext_resource type=\"Script\" path=\"res://scripts/handlers.gd\" id=\"1\"]

[node name=\"Handlers\" type=\"Node\"]
script = ExtResource(\"1\")
",
    )
    .unwrap();
    dir
}

/// Calls one probe method on the scene root and returns its reply, then what the probe's logger
/// recorded during the call, and the probe's `progress` and `outcome` after it.
async fn probe_handler(
    tools: &Arc<ToolManager>,
    run_id: &str,
    method: &str,
) -> (ToolResult, Vec<Value>, Value) {
    let reply = call(
        tools,
        "godot_game_call",
        json!({"run_id": run_id, "path": ".", "method": method, "permit": [method]}),
    )
    .await;
    let entries = game_method(tools, run_id, "entries").await;
    let state = call(
        tools,
        "godot_game_get",
        json!({"run_id": run_id, "path": ".", "properties": ["progress", "outcome", "logged_at_return"]}),
    )
    .await;
    assert!(!state.is_error, "{}", text_of(&state));
    (
        reply,
        entries.as_array().cloned().unwrap_or_default(),
        json_of(&state)["properties"].clone(),
    )
}

/// A handler the runtime calls can fail in its own script, and nothing can catch that: the
/// engine's logger is the only witness. These are the facts that reading the logger depends on,
/// measured on a real engine: the entry exists when `Callable.call` returns, on the main thread,
/// with the script error type (a pushed error has another); a handler that awaits makes the
/// engine log a script error against the function that called it and end that function, and
/// only that one.
#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn logger_sees_a_script_error_of_a_called_handler_at_once() {
    let _ = IGNORE;
    let project = handler_probe_project();
    let tools = manager(project.path());
    let started = call(
        &tools,
        "godot_run_project",
        json!({
            "project_path": project.path(),
            "scene": "res://scenes/handlers.tscn",
            "mode": "process",
            "headless": true
        }),
    )
    .await;
    assert!(!started.is_error, "{}", text_of(&started));
    let run_id = run_id_of(&started);

    let (reply, entries, state) = probe_handler(&tools, &run_id, "plain_script_error").await;
    assert!(!reply.is_error, "{}", text_of(&reply));
    assert_eq!(entries.len(), 1, "{entries:?}");
    assert_eq!(
        state["logged_at_return"], 1,
        "the entry was in the logger when the call returned: {state}"
    );
    assert_eq!(entries[0]["is_script"], true, "{entries:?}");
    assert_eq!(entries[0]["on_main_thread"], true, "{entries:?}");
    assert_eq!(entries[0]["function"], "_fails_in_script", "{entries:?}");
    // The text is in the entry's code, with no rationale, and the runtime's line shows it.
    assert!(
        entries[0]["code"]
            .as_str()
            .is_some_and(|text| text.contains("Invalid access to property or key 'name'")),
        "{entries:?}"
    );
    assert_eq!(
        state["outcome"], "null",
        "a handler that fails in its script returns null and the caller goes on: {state}"
    );

    let (reply, entries, state) = probe_handler(&tools, &run_id, "plain_push_error").await;
    assert!(!reply.is_error, "{}", text_of(&reply));
    assert_eq!(entries.len(), 1, "{entries:?}");
    assert_eq!(state["logged_at_return"], 1, "{state}");
    assert_eq!(entries[0]["is_script"], false, "{entries:?}");
    assert_eq!(entries[0]["is_error"], true, "{entries:?}");
    assert_eq!(state["outcome"], "after-push", "{state}");

    // A handler that awaits, called without `await`: the engine logs a script error against the
    // caller and ends the caller (`progress` never moves), and the runtime's own dispatch above
    // it goes on and answers.
    let (reply, entries, state) = probe_handler(&tools, &run_id, "plain_awaiting").await;
    assert!(
        !reply.is_error,
        "the dispatch must survive: {}",
        text_of(&reply)
    );
    assert_eq!(entries.len(), 1, "{entries:?}");
    assert_eq!(entries[0]["is_script"], true, "{entries:?}");
    assert_eq!(entries[0]["function"], "plain_awaiting", "{entries:?}");
    assert!(
        entries[0]["code"]
            .as_str()
            .is_some_and(|text| text.contains("async function without")),
        "{entries:?}"
    );
    assert_eq!(state["progress"], "before", "the caller was ended: {state}");

    // Called with `await` from a coroutine, the same handler is fine, and so is one that does
    // not await.
    let (reply, entries, state) = probe_handler(&tools, &run_id, "awaited_awaiting").await;
    assert!(!reply.is_error, "{}", text_of(&reply));
    assert!(entries.is_empty(), "{entries:?}");
    assert_eq!(state["progress"], "after-awaited-call", "{state}");
    assert_eq!(state["outcome"], "after-await", "{state}");
    let (reply, entries, state) = probe_handler(&tools, &run_id, "awaited_plain").await;
    assert!(!reply.is_error, "{}", text_of(&reply));
    assert_eq!(entries.len(), 1, "only the handler's own push: {entries:?}");
    assert_eq!(entries[0]["is_script"], false, "{entries:?}");
    assert_eq!(state["outcome"], "after-push", "{state}");

    // The logger the runtime installs holds the same errors, with the stack that led to them.
    let output = call(
        &tools,
        "godot_run_output",
        json!({"run_id": run_id, "level": "error"}),
    )
    .await;
    assert!(!output.is_error, "{}", text_of(&output));
    let lines = json_of(&output)["lines"].clone();
    let lines = lines.as_array().expect("lines");
    let script_error = lines
        .iter()
        .find(|line| {
            line["text"]
                .as_str()
                .is_some_and(|text| text.contains("Invalid access to property or key 'name'"))
        })
        .unwrap_or_else(|| panic!("the runtime's logger missed the script error: {lines:?}"));
    assert!(
        script_error["stack"]
            .as_array()
            .is_some_and(|stack| stack.iter().any(|frame| frame
                .as_str()
                .is_some_and(|frame| frame.contains("_fails_in_script")))),
        "{script_error}"
    );
    assert!(
        lines.iter().any(|line| line["text"]
            .as_str()
            .is_some_and(|text| text.contains("async function without"))),
        "{lines:?}"
    );

    let stopped = call(&tools, "godot_stop_project", json!({"run_id": run_id})).await;
    assert!(!stopped.is_error, "{}", text_of(&stopped));
}

/// A game that loads the registry and the output marks from the runtime's own copy and reports
/// what they do, so both are proved on a real engine before anything loads an adapter itself.
fn registry_probe_project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("project");
    let root = dir.path();
    std::fs::create_dir_all(root.join("scenes")).unwrap();
    std::fs::write(
        root.join("project.godot"),
        "config_version=5\n\n[application]\n\nconfig/name=\"RegistryProbe\"\nrun/main_scene=\"res://scenes/registry.tscn\"\nconfig/features=PackedStringArray(\"4.5\")\n",
    )
    .unwrap();
    std::fs::write(root.join("adapter.gd"), common::adapter::FIXTURE_ADAPTER).unwrap();
    std::fs::write(
        root.join("registry_probe.gd"),
        r#"extends Node

var registry
var adapter
var runtime_dir := ""

func _ready() -> void:
    runtime_dir = (get_node("/root/GodotMcpRuntime").get_script() as Script).resource_path.get_base_dir()
    registry = load(runtime_dir + "/ops/rt_registry.gd").new()
    adapter = load("res://adapter.gd").new()
    adapter.register(registry)

func registry_report() -> Dictionary:
    return {
        "states": registry.state_names(),
        "commands": registry.command_help(),
        "refused": registry.refused(),
        "balls": registry.call_provider("balls"),
        "level": registry.call_provider("level"),
        "missing": registry.call_provider("no_such_state"),
        "grant": registry.call_command("grant", {"name": "machine_gun"}),
        "refusal": registry.call_command("grant", {}),
        "error_entry": registry.call_command("clear", {}),
        "unknown": registry.call_command("nothing", {}),
        "has_grant": registry.has_command("grant"),
        "has_nothing": registry.has_command("nothing"),
    }

func _fail() -> void:
    var nothing: Variant = {}.get("x")
    nothing.name

func mark_report() -> Dictionary:
    var output = load(runtime_dir + "/ops/rt_output.gd")
    var start: int = output.call("mark")
    print("mark-print")
    push_warning("mark-warning")
    push_error("mark-error")
    _fail()
    var entries: Array = output.call("lines_since", start)
    var later: int = output.call("mark")
    return {"entries": entries, "later_is_after": later > start, "after_later": output.call("lines_since", later).size()}
"#,
    )
    .unwrap();
    std::fs::write(
        root.join("scenes/registry.tscn"),
        "[gd_scene load_steps=2 format=3]\n\n[ext_resource type=\"Script\" path=\"res://registry_probe.gd\" id=\"1\"]\n\n[node name=\"RegistryProbe\" type=\"Node\"]\nscript = ExtResource(\"1\")\n",
    )
    .unwrap();
    dir
}

async fn start_registry_probe() -> (tempfile::TempDir, Arc<ToolManager>, String) {
    let project = registry_probe_project();
    let tools = manager(project.path());
    let started = call(
        &tools,
        "godot_run_project",
        json!({
            "project_path": project.path(),
            "scene": "res://scenes/registry.tscn",
            "mode": "process",
            "headless": true
        }),
    )
    .await;
    assert!(!started.is_error, "{}", text_of(&started));
    let run_id = run_id_of(&started);
    (project, tools, run_id)
}

#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn registry_keeps_what_an_adapter_registers_and_lists_what_it_refused() {
    let _ = IGNORE;
    let (_project, tools, run_id) = start_registry_probe().await;

    let report = game_method(&tools, &run_id, "registry_report").await;
    assert_eq!(report["states"], json!(common::adapter::STATES), "{report}");
    let help = report["commands"].as_object().expect("commands");
    let names: Vec<&str> = help.keys().map(String::as_str).collect();
    assert_eq!(names, common::adapter::COMMANDS, "{report}");
    assert_eq!(help["grant"], "Grants {name}.", "{report}");
    assert_eq!(help["clear"], "Clears the level.", "{report}");
    let refused = report["refused"].as_array().expect("refused");
    let named: Vec<(&str, &str)> = refused
        .iter()
        .map(|entry| {
            (
                entry["name"].as_str().unwrap_or("?"),
                entry["kind"].as_str().unwrap_or("?"),
            )
        })
        .collect();
    assert_eq!(
        named,
        vec![
            ("takes_argument", "state"),
            ("", "state"),
            ("no_argument", "command")
        ],
        "{report}"
    );
    assert!(
        refused.iter().all(|entry| entry["reason"]
            .as_str()
            .is_some_and(|text| !text.is_empty())),
        "{report}"
    );
    assert_eq!(
        report["balls"],
        json!({"value": [1, 2]}),
        "the later registration wins: {report}"
    );
    assert_eq!(report["level"]["value"]["index"], 3, "{report}");
    assert!(report["missing"]["error"].is_string(), "{report}");
    assert_eq!(
        report["grant"],
        json!({"value": {"granted": "machine_gun"}}),
        "{report}"
    );
    assert_eq!(
        report["refusal"],
        json!({"bad_args": "name is required"}),
        "{report}"
    );
    assert_eq!(
        report["error_entry"]["value"]["error"], "an entry named error is a result, not a refusal",
        "{report}"
    );
    assert!(report["unknown"]["error"].is_string(), "{report}");
    assert_eq!(report["has_grant"], true, "{report}");
    assert_eq!(report["has_nothing"], false, "{report}");

    let stopped = call(&tools, "godot_stop_project", json!({"run_id": run_id})).await;
    assert!(!stopped.is_error, "{}", text_of(&stopped));
}

#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn output_marks_return_what_was_logged_since_with_the_script_errors_told_apart() {
    let _ = IGNORE;
    let (_project, tools, run_id) = start_registry_probe().await;

    let report = game_method(&tools, &run_id, "mark_report").await;
    let entries = report["entries"].as_array().expect("entries");
    let seen: Vec<(&str, &str, bool)> = entries
        .iter()
        .map(|entry| {
            (
                entry["level"].as_str().unwrap_or("?"),
                entry["text"].as_str().unwrap_or("?"),
                entry["script"].as_bool().unwrap_or(false),
            )
        })
        .collect();
    assert_eq!(seen.len(), 4, "{report}");
    assert_eq!(seen[0], ("info", "mark-print", false), "{report}");
    assert_eq!(seen[1], ("warning", "mark-warning", false), "{report}");
    assert_eq!(seen[2], ("error", "mark-error", false), "{report}");
    assert_eq!(seen[3].0, "error", "{report}");
    assert!(
        seen[3]
            .1
            .contains("Invalid access to property or key 'name'"),
        "{report}"
    );
    assert!(seen[3].2, "a script error is told apart: {report}");
    assert!(
        entries[3]["stack"]
            .as_array()
            .is_some_and(|stack| !stack.is_empty()),
        "{report}"
    );
    assert_eq!(report["later_is_after"], true, "{report}");
    assert_eq!(report["after_later"], 0, "{report}");

    // The marks are not part of what the output tool shows.
    let output = call(&tools, "godot_run_output", json!({"run_id": run_id})).await;
    assert!(!output.is_error, "{}", text_of(&output));
    assert!(
        json_of(&output)["lines"]
            .as_array()
            .is_some_and(|lines| lines.iter().all(|line| line.get("script").is_none())),
        "{}",
        text_of(&output)
    );

    let stopped = call(&tools, "godot_stop_project", json!({"run_id": run_id})).await;
    assert!(!stopped.is_error, "{}", text_of(&stopped));
}

/// A game with a plain scene and, when asked, a project setting that names an adapter and an
/// adapter script at `res://adapter.gd`.
fn adapter_game_project(setting: Option<&str>, adapter: Option<&str>) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("project");
    let root = dir.path();
    std::fs::create_dir_all(root.join("scenes")).unwrap();
    let mut config = String::from(
        "config_version=5\n\n[application]\n\nconfig/name=\"AdapterProbe\"\nrun/main_scene=\"res://scenes/game.tscn\"\nconfig/features=PackedStringArray(\"4.5\")\n",
    );
    if let Some(setting) = setting {
        config.push_str(&format!("\n[godot_mcp]\n\nadapter=\"{setting}\"\n"));
    }
    std::fs::write(root.join("project.godot"), config).unwrap();
    if let Some(adapter) = adapter {
        std::fs::write(root.join("adapter.gd"), adapter).unwrap();
    }
    std::fs::write(
        root.join("game.gd"),
        "extends Node\n\nfunc ping() -> bool:\n    return true\n\nfunc finish() -> void:\n    get_tree().quit()\n",
    )
    .unwrap();
    std::fs::write(
        root.join("scenes/game.tscn"),
        "[gd_scene load_steps=2 format=3]\n\n[ext_resource type=\"Script\" path=\"res://game.gd\" id=\"1\"]\n\n[node name=\"Game\" type=\"Node\"]\nscript = ExtResource(\"1\")\n",
    )
    .unwrap();
    dir
}

async fn start_adapter_game(project: &Path) -> (Arc<ToolManager>, String) {
    let tools = manager(project);
    let started = call(
        &tools,
        "godot_run_project",
        json!({
            "project_path": project,
            "scene": "res://scenes/game.tscn",
            "mode": "process",
            "headless": true
        }),
    )
    .await;
    assert!(!started.is_error, "{}", text_of(&started));
    let run_id = run_id_of(&started);
    (tools, run_id)
}

/// The adapter status the game announced, as the run status shows it. A call to the game first
/// makes sure the announcement has arrived.
async fn adapter_status(tools: &Arc<ToolManager>, run_id: &str) -> Value {
    let pong = game_method(tools, run_id, "ping").await;
    assert_eq!(pong, true);
    let status = call(tools, "godot_get_run_status", json!({})).await;
    assert!(!status.is_error, "{}", text_of(&status));
    let body = json_of(&status);
    body["process_runs"]
        .as_array()
        .and_then(|runs| runs.iter().find(|run| run["run_id"] == run_id))
        .map(|run| run["adapter"].clone())
        .unwrap_or_else(|| panic!("no run {run_id} in {body}"))
}

#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn a_game_without_an_adapter_setting_announces_none() {
    let _ = IGNORE;
    let project = adapter_game_project(None, Some(common::adapter::FIXTURE_ADAPTER));
    let (tools, run_id) = start_adapter_game(project.path()).await;
    let status = adapter_status(&tools, &run_id).await;
    assert_eq!(status, json!({"configured": false}), "{status}");
    let stopped = call(&tools, "godot_stop_project", json!({"run_id": run_id})).await;
    assert!(!stopped.is_error, "{}", text_of(&stopped));
}

fn assert_fixture_registered(status: &Value) {
    assert_eq!(status["configured"], true, "{status}");
    assert_eq!(status["loaded"], true, "{status}");
    assert!(status.get("error").is_none(), "{status}");
    assert_eq!(status["states"], json!(common::adapter::STATES), "{status}");
    let help = status["commands"].as_object().expect("commands");
    let names: Vec<&str> = help.keys().map(String::as_str).collect();
    assert_eq!(names, common::adapter::COMMANDS, "{status}");
    assert_eq!(help["grant"], "Grants {name}.", "{status}");
    let refused = status["refused"].as_array().expect("refused");
    assert_eq!(refused.len(), 3, "{status}");
    assert_eq!(refused[0]["name"], "takes_argument", "{status}");
}

#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn an_adapter_named_by_path_or_by_uid_registers_what_it_offers() {
    let _ = IGNORE;
    let project = adapter_game_project(
        Some("res://adapter.gd"),
        Some(common::adapter::FIXTURE_ADAPTER),
    );
    let (tools, run_id) = start_adapter_game(project.path()).await;
    let status = adapter_status(&tools, &run_id).await;
    assert_fixture_registered(&status);
    assert_eq!(status["path"], "res://adapter.gd", "{status}");
    let stopped = call(&tools, "godot_stop_project", json!({"run_id": run_id})).await;
    assert!(!stopped.is_error, "{}", text_of(&stopped));

    // Project Settings stores a uid when the script is picked with the file chooser. An import
    // makes the engine write the script's uid file and register it.
    let by_uid = adapter_game_project(None, Some(common::adapter::FIXTURE_ADAPTER));
    let imported = std::process::Command::new(godot_path())
        .args(["--headless", "--path"])
        .arg(by_uid.path())
        .arg("--import")
        .output()
        .expect("import");
    assert!(imported.status.success(), "{imported:?}");
    let uid = std::fs::read_to_string(by_uid.path().join("adapter.gd.uid"))
        .expect("the import writes the script's uid file")
        .trim()
        .to_string();
    assert!(uid.starts_with("uid://"), "{uid}");
    let config = std::fs::read_to_string(by_uid.path().join("project.godot")).unwrap();
    std::fs::write(
        by_uid.path().join("project.godot"),
        format!("{config}\n[godot_mcp]\n\nadapter=\"{uid}\"\n"),
    )
    .unwrap();
    let (tools, run_id) = start_adapter_game(by_uid.path()).await;
    let status = adapter_status(&tools, &run_id).await;
    assert_fixture_registered(&status);
    assert_eq!(status["path"], uid.as_str(), "{status}");
    let stopped = call(&tools, "godot_stop_project", json!({"run_id": run_id})).await;
    assert!(!stopped.is_error, "{}", text_of(&stopped));
}

#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn an_adapter_that_cannot_be_used_says_why_and_keeps_what_it_registered() {
    let _ = IGNORE;
    let raising = "extends RefCounted\n\nfunc register(registry) -> void:\n    registry.add_state(\"early\", _early)\n    var nothing: Variant = {}.get(\"x\")\n    nothing.name\n    registry.add_state(\"late\", _early)\n\nfunc _early() -> int:\n    return 1\n";
    let cases: [(&str, Option<&str>, &str); 5] = [
        (
            "user://adapter.gd",
            Some(common::adapter::FIXTURE_ADAPTER),
            "res://",
        ),
        ("res://missing.gd", None, "no script"),
        (
            "res://adapter.gd",
            Some("extends RefCounted\n"),
            "register(registry)",
        ),
        (
            "res://adapter.gd",
            Some(
                "extends RefCounted\n\nfunc register(registry) -> void:\n    var x: int = \"text\"\n",
            ),
            "cannot be loaded",
        ),
        (
            "res://adapter.gd",
            Some(raising),
            "Invalid access to property or key 'name'",
        ),
    ];
    for (setting, source, expected) in cases {
        let project = adapter_game_project(Some(setting), source);
        let (tools, run_id) = start_adapter_game(project.path()).await;
        let status = adapter_status(&tools, &run_id).await;
        assert_eq!(status["configured"], true, "{setting}: {status}");
        assert_eq!(status["loaded"], false, "{setting}: {status}");
        assert!(
            status["error"]
                .as_str()
                .is_some_and(|text| text.contains(expected)),
            "{setting}: expected {expected:?} in {status}"
        );
        if source == Some(raising) {
            assert_eq!(
                status["states"],
                json!(["early"]),
                "what was registered before the error stays: {status}"
            );
        }
        let stopped = call(&tools, "godot_stop_project", json!({"run_id": run_id})).await;
        assert!(!stopped.is_error, "{}", text_of(&stopped));
    }
}

#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn an_adapter_that_is_a_node_outside_the_tree_is_freed_when_the_game_ends() {
    let _ = IGNORE;
    let adapter = "extends Node\n\nfunc register(registry) -> void:\n    registry.add_state(\"alive\", _alive)\n\nfunc _alive() -> bool:\n    return true\n";
    let project = adapter_game_project(Some("res://adapter.gd"), Some(adapter));
    let (tools, run_id) = start_adapter_game(project.path()).await;
    let status = adapter_status(&tools, &run_id).await;
    assert_eq!(status["loaded"], true, "{status}");
    assert_eq!(status["states"], json!(["alive"]), "{status}");

    let reply = call(
        &tools,
        "godot_game_call",
        json!({"run_id": run_id, "path": ".", "method": "finish", "permit": ["finish"]}),
    )
    .await;
    assert!(!reply.is_error, "{}", text_of(&reply));
    for _ in 0..50 {
        let status = json_of(&call(&tools, "godot_get_run_status", json!({})).await);
        if status["process_runs"][0]["running"] == false {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    let output = call(&tools, "godot_run_output", json!({"run_id": run_id})).await;
    assert!(!output.is_error, "{}", text_of(&output));
    assert!(
        !text_of(&output).contains("leaked"),
        "the adapter node was left behind: {}",
        text_of(&output)
    );
}

async fn fixture_game() -> (tempfile::TempDir, Arc<ToolManager>, String) {
    let project = adapter_game_project(
        Some("res://adapter.gd"),
        Some(common::adapter::FIXTURE_ADAPTER),
    );
    let (tools, run_id) = start_adapter_game(project.path()).await;
    let status = adapter_status(&tools, &run_id).await;
    assert_eq!(status["loaded"], true, "{status}");
    (project, tools, run_id)
}

async fn read_states(tools: &Arc<ToolManager>, run_id: &str, names: Value) -> ToolResult {
    let mut args = json!({"run_id": run_id});
    if !names.is_null() {
        args["names"] = names;
    }
    call(tools, "godot_game_state", args).await
}

async fn run_command(
    tools: &Arc<ToolManager>,
    run_id: &str,
    name: &str,
    args: Value,
) -> ToolResult {
    let mut params = json!({"run_id": run_id, "name": name});
    if !args.is_null() {
        params["args"] = args;
    }
    call(tools, "godot_game_command", params).await
}

#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn states_are_read_by_name_with_each_failure_kept_to_its_own_entry() {
    let _ = IGNORE;
    let (_project, tools, run_id) = fixture_game().await;

    // No names reads every provider; one that fails inside its script fails alone.
    let all = read_states(&tools, &run_id, Value::Null).await;
    assert!(!all.is_error, "{}", text_of(&all));
    let all = json_of(&all);
    let values = &all["values"];
    assert_eq!(values["level"]["index"], 3, "{all}");
    assert_eq!(values["balls"], json!([1, 2]), "{all}");
    assert!(values["nothing"].is_null(), "{all}");
    assert_eq!(values["position"], json!([1.0, 2.0]), "{all}");
    assert_eq!(values["deferred"], 0, "{all}");
    assert!(
        values["broken"]["error"]
            .as_str()
            .is_some_and(|text| text.contains("Invalid access to property or key 'name'")),
        "{all}"
    );
    assert_eq!(
        values.as_object().map(|map| map.len()),
        Some(common::adapter::STATES.len()),
        "{all}"
    );
    assert!(
        all["errors"]
            .as_array()
            .is_some_and(|errors| errors.iter().any(|entry| entry["level"] == "error")),
        "the error the engine logged comes with the answer: {all}"
    );
    assert_eq!(all["adapter"]["loaded"], true, "{all}");

    // An empty list is the same as none; an unknown name is its own entry beside the others.
    let some = json_of(&read_states(&tools, &run_id, json!(["level", "no_such_state"])).await);
    assert_eq!(some["values"]["level"]["index"], 3, "{some}");
    assert!(
        some["values"]["no_such_state"]["error"]
            .as_str()
            .is_some_and(|text| text.contains("no_such_state")),
        "{some}"
    );
    let empty = json_of(&read_states(&tools, &run_id, json!([])).await);
    assert_eq!(empty["values"].as_object().map(|m| m.len()), Some(7));

    // An answer over the bound is refused with the size of each state, never cut.
    let grown = run_command(&tools, &run_id, "grow", Value::Null).await;
    assert!(!grown.is_error, "{}", text_of(&grown));
    let too_big = read_states(&tools, &run_id, json!(["big", "level"])).await;
    assert!(too_big.is_error, "{}", text_of(&too_big));
    let message = text_of(&too_big);
    assert!(
        message.contains("BAD_PARAMS") && message.contains("\"sizes\"") && message.contains("big"),
        "{message}"
    );
    let still_fine = json_of(&read_states(&tools, &run_id, json!(["level"])).await);
    assert_eq!(still_fine["values"]["level"]["index"], 3, "{still_fine}");

    let stopped = call(&tools, "godot_stop_project", json!({"run_id": run_id})).await;
    assert!(!stopped.is_error, "{}", text_of(&stopped));
}

#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn commands_run_by_name_and_every_kind_of_failure_is_told_apart() {
    let _ = IGNORE;
    let (_project, tools, run_id) = fixture_game().await;

    let granted = run_command(&tools, &run_id, "grant", json!({"name": "machine_gun"})).await;
    assert!(!granted.is_error, "{}", text_of(&granted));
    let granted = json_of(&granted);
    assert_eq!(
        granted["value"],
        json!({"granted": "machine_gun"}),
        "{granted}"
    );
    assert_eq!(granted["name"], "grant", "{granted}");
    assert_eq!(granted["run_id"], run_id.as_str(), "{granted}");

    // Arguments arrive as parsed JSON: a whole number is a float.
    let typed = json_of(&run_command(&tools, &run_id, "float_arg", json!({"n": 3})).await);
    assert_eq!(typed["value"]["is_float"], true, "{typed}");

    // A command may do what a method call is refused: here it frees a node.
    let freed = json_of(&run_command(&tools, &run_id, "free_a_node", Value::Null).await);
    assert_eq!(freed["value"]["freed"], true, "{freed}");

    // An entry named error is a result, not a failure.
    let cleared = json_of(&run_command(&tools, &run_id, "clear", Value::Null).await);
    assert_eq!(
        cleared["value"]["error"], "an entry named error is a result, not a refusal",
        "{cleared}"
    );

    // The command's own refusal of its arguments.
    let refused = run_command(&tools, &run_id, "grant", json!({})).await;
    assert!(refused.is_error, "{}", text_of(&refused));
    assert!(
        text_of(&refused).contains("BAD_PARAMS") && text_of(&refused).contains("name is required"),
        "{}",
        text_of(&refused)
    );

    // An unknown name lists the registered ones.
    let unknown = run_command(&tools, &run_id, "no_such_command", Value::Null).await;
    assert!(unknown.is_error, "{}", text_of(&unknown));
    let message = text_of(&unknown);
    assert!(
        message.contains("NOT_FOUND") && message.contains("grant") && message.contains("clear"),
        "{message}"
    );

    // A script error inside the command, and a command that awaits.
    let failed = run_command(&tools, &run_id, "fail", Value::Null).await;
    assert!(failed.is_error, "{}", text_of(&failed));
    let message = text_of(&failed);
    assert!(
        message.contains("GODOT_ERROR")
            && message.contains("Invalid access to property or key 'name'")
            && message.contains("may have changed the game"),
        "{message}"
    );
    let awaited = run_command(&tools, &run_id, "wait", Value::Null).await;
    assert!(awaited.is_error, "{}", text_of(&awaited));
    let message = text_of(&awaited);
    assert!(
        message.contains("GODOT_ERROR") && message.contains("async function without"),
        "{message}"
    );
    assert!(
        message.to_lowercase().contains("must not await"),
        "the hint says why: {message}"
    );
    // Neither ended the dispatch: the next call is answered at once.
    let after = run_command(&tools, &run_id, "grant", json!({"name": "again"})).await;
    assert!(!after.is_error, "{}", text_of(&after));

    // A value over the bound is refused after the command ran, and the answer says so.
    let big = run_command(&tools, &run_id, "big_value", Value::Null).await;
    assert!(big.is_error, "{}", text_of(&big));
    assert!(
        text_of(&big).contains("ran") && text_of(&big).contains("bytes"),
        "{}",
        text_of(&big)
    );

    let stopped = call(&tools, "godot_stop_project", json!({"run_id": run_id})).await;
    assert!(!stopped.is_error, "{}", text_of(&stopped));
}

#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn a_game_without_a_usable_adapter_refuses_both_tools_and_says_how_to_name_one() {
    let _ = IGNORE;
    let project = adapter_game_project(None, Some(common::adapter::FIXTURE_ADAPTER));
    let (tools, run_id) = start_adapter_game(project.path()).await;
    let _ = adapter_status(&tools, &run_id).await;
    for result in [
        read_states(&tools, &run_id, Value::Null).await,
        run_command(&tools, &run_id, "grant", Value::Null).await,
    ] {
        assert!(result.is_error, "{}", text_of(&result));
        let message = text_of(&result);
        assert!(
            message.contains("UNSUPPORTED") && message.contains("godot_mcp/adapter"),
            "{message}"
        );
        assert!(
            message.contains("godot_set_project_setting"),
            "the hint names the way: {message}"
        );
    }
    let stopped = call(&tools, "godot_stop_project", json!({"run_id": run_id})).await;
    assert!(!stopped.is_error, "{}", text_of(&stopped));

    let broken = adapter_game_project(Some("res://missing.gd"), None);
    let (tools, run_id) = start_adapter_game(broken.path()).await;
    let _ = adapter_status(&tools, &run_id).await;
    let result = read_states(&tools, &run_id, Value::Null).await;
    assert!(result.is_error, "{}", text_of(&result));
    assert!(
        text_of(&result).contains("no script exists at res://missing.gd"),
        "{}",
        text_of(&result)
    );
    let stopped = call(&tools, "godot_stop_project", json!({"run_id": run_id})).await;
    assert!(!stopped.is_error, "{}", text_of(&stopped));
}

#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn a_held_game_answers_both_tools_and_what_a_command_defers_lands_on_the_next_tick() {
    let _ = IGNORE;
    let (_project, tools, run_id) = fixture_game().await;
    hold_now(&tools, &run_id).await;

    let read = json_of(&read_states(&tools, &run_id, json!(["deferred", "level"])).await);
    assert_eq!(read["values"]["deferred"], 0, "{read}");
    let scheduled = run_command(&tools, &run_id, "defer", Value::Null).await;
    assert!(!scheduled.is_error, "{}", text_of(&scheduled));
    assert_eq!(json_of(&scheduled)["value"]["scheduled"], true);

    // Nothing advances while the game is held, so the deferred call has not run.
    let before = json_of(&read_states(&tools, &run_id, json!(["deferred"])).await);
    assert_eq!(before["values"]["deferred"], 0, "{before}");
    let stepped = held_step(&tools, &run_id, json!({"ticks": 1})).await;
    assert!(!stepped.is_error, "{}", text_of(&stepped));
    let after = json_of(&read_states(&tools, &run_id, json!(["deferred"])).await);
    assert_eq!(after["values"]["deferred"], 1, "{after}");

    let stopped = call(&tools, "godot_stop_project", json!({"run_id": run_id})).await;
    assert!(!stopped.is_error, "{}", text_of(&stopped));
}

/// The adapter is loaded by the in-game component and only by it: a game that has the component
/// in its autoloads but no debugger attached never runs the adapter's `register`.
#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn a_game_started_without_a_debugger_never_loads_the_adapter() {
    let _ = IGNORE;
    let adapter = "extends RefCounted\n\nfunc register(registry) -> void:\n    print(\"adapter-registered\")\n    registry.add_state(\"alive\", _alive)\n\nfunc _alive() -> bool:\n    return true\n";
    let project = adapter_game_project(Some("res://adapter.gd"), Some(adapter));
    let addon = project.path().join("addons/godot_mcp");
    copy_tree(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("addons/godot_mcp"),
        &addon,
    );
    std::fs::write(
        project.path().join("override.cfg"),
        "[autoload]\n\nGodotMcpRuntime=\"res://addons/godot_mcp/runtime/mcp_runtime.gd\"\n",
    )
    .unwrap();
    let plain = std::process::Command::new(godot_path())
        .args(["--headless", "--path"])
        .arg(project.path())
        .args(["--quit-after", "10"])
        .output()
        .expect("run the game");
    let printed = format!(
        "{}{}",
        String::from_utf8_lossy(&plain.stdout),
        String::from_utf8_lossy(&plain.stderr)
    );
    assert!(
        !printed.contains("adapter-registered"),
        "no debugger, no adapter: {printed}"
    );
    std::fs::remove_file(project.path().join("override.cfg")).unwrap();

    // The same project under the server, which attaches a debugger, loads it.
    let (tools, run_id) = start_adapter_game(project.path()).await;
    let status = adapter_status(&tools, &run_id).await;
    assert_eq!(status["loaded"], true, "{status}");
    let output = call(&tools, "godot_run_output", json!({"run_id": run_id})).await;
    assert!(
        text_of(&output).contains("adapter-registered"),
        "{}",
        text_of(&output)
    );
    let stopped = call(&tools, "godot_stop_project", json!({"run_id": run_id})).await;
    assert!(!stopped.is_error, "{}", text_of(&stopped));
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

/// A tool script that re-saves an existing compressed binary scene, as a level builder does.
/// Headless Godot writes it without its uid; the server writes the old uid back, and a new binary
/// scene gets one.
#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn run_script_writes_back_the_uid_of_a_rewritten_binary_scene() {
    let dir = tempfile::tempdir().expect("project dir");
    let root = dir.path();
    std::fs::write(
        root.join("project.godot"),
        "config_version=5\n\n[application]\n\nconfig/name=\"Binary uids\"\n",
    )
    .expect("project.godot");
    std::fs::create_dir_all(root.join("tools")).expect("tools dir");
    let save = |name: &str, source: &str| {
        std::fs::write(root.join("tools").join(name), source).expect("script");
    };
    save(
        "make.gd",
        "extends SceneTree\n\nfunc _init() -> void:\n\tvar root := Node3D.new()\n\troot.name = \"Gate\"\n\tvar scene := PackedScene.new()\n\tscene.pack(root)\n\tResourceSaver.save(scene, \"res://gate.scn\", ResourceSaver.FLAG_COMPRESS)\n\tResourceSaver.set_uid(\"res://gate.scn\", ResourceUID.text_to_id(\"uid://dap714cpjw8pd\"))\n\tquit()\n",
    );
    save(
        "rebuild.gd",
        "extends SceneTree\n\nfunc _init() -> void:\n\tvar scene: PackedScene = load(\"res://gate.scn\")\n\tvar gate: Node3D = scene.instantiate()\n\tgate.add_child(Node3D.new())\n\tgate.get_child(0).owner = gate\n\tvar packed := PackedScene.new()\n\tpacked.pack(gate)\n\tResourceSaver.save(packed, \"res://gate.scn\", ResourceSaver.FLAG_COMPRESS)\n\tResourceSaver.save(packed, \"res://fresh.res\")\n\tquit()\n",
    );
    let tools = manager(root);
    let run = |script: &'static str| {
        let tools = Arc::clone(&tools);
        let root = root.to_path_buf();
        async move {
            let result = call(
                &tools,
                "godot_run_script",
                json!({"script": script, "project_path": root, "timeout_seconds": 60}),
            )
            .await;
            let text = text_of(&result);
            assert!(!result.is_error, "{text}");
            serde_json::from_str::<Value>(&text[text.find('{').expect("json")..]).expect("JSON")
        }
    };
    run("res://tools/make.gd").await;
    let before = std::fs::read(root.join("gate.scn")).expect("gate.scn");
    assert!(before.starts_with(b"RSCC"), "compressed");

    let body = run("res://tools/rebuild.gd").await;
    let repairs = body["resource_repairs"]
        .as_array()
        .expect("repairs")
        .clone();
    let gate = repairs
        .iter()
        .find(|item| item["path"] == "res://gate.scn")
        .unwrap_or_else(|| panic!("gate.scn repaired: {body}"));
    assert_eq!(gate["uid"], "uid://dap714cpjw8pd", "{body}");
    assert_eq!(gate["new_uid"], false);
    let fresh = repairs
        .iter()
        .find(|item| item["path"] == "res://fresh.res")
        .unwrap_or_else(|| panic!("fresh.res got a uid: {body}"));
    assert_eq!(fresh["new_uid"], true, "{body}");
    assert_eq!(body["resource_damage"], json!([]), "{body}");
    assert!(
        std::fs::read(root.join("gate.scn"))
            .expect("gate.scn")
            .starts_with(b"RSCC"),
        "still compressed"
    );

    assert_eq!(
        binary_uid(&root.join("gate.scn")),
        BinaryUid::Uid("uid://dap714cpjw8pd".into())
    );
    // The repaired file still loads in Godot, with the rebuilt content.
    save(
        "read.gd",
        "extends SceneTree\n\nfunc _init() -> void:\n\tvar gate: Node = (load(\"res://gate.scn\") as PackedScene).instantiate()\n\tprint(\"GATE_CHILDREN=\", gate.get_child_count())\n\tgate.free()\n\tquit()\n",
    );
    let read = run("res://tools/read.gd").await;
    assert!(
        read["stdout"]
            .as_str()
            .unwrap_or_default()
            .contains("GATE_CHILDREN=1"),
        "{read}"
    );
}

/// A tool script that builds new scenes and resources headless leaves them without uids; the
/// server writes new ones back. The project's uid cache must learn them too, or the next script
/// that loads the scenes prints an "invalid UID" warning for every ext_resource.
#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn run_script_records_new_uids_in_the_uid_cache() {
    let dir = tempfile::tempdir().expect("project dir");
    let root = dir.path();
    std::fs::write(
        root.join("project.godot"),
        "config_version=5\n\n[application]\n\nconfig/name=\"Uid cache\"\n",
    )
    .expect("project.godot");
    std::fs::create_dir_all(root.join("tools")).expect("tools dir");
    std::fs::create_dir_all(root.join("zones")).expect("zones dir");
    std::fs::write(
        root.join("zones/seed.tres"),
        "[gd_resource type=\"BoxMesh\" format=3 uid=\"uid://bseedmesh01\"]\n\n[resource]\n",
    )
    .expect("seed.tres");
    // The editor's first scan writes .godot/uid_cache.bin, as in any project opened once.
    let imported = std::process::Command::new(godot_path())
        .args(["--headless", "--path"])
        .arg(root)
        .arg("--import")
        .stdin(std::process::Stdio::null())
        .output()
        .expect("godot --import");
    assert!(imported.status.success(), "{imported:?}");
    assert!(root.join(".godot/uid_cache.bin").is_file(), "uid cache");
    let save = |name: &str, source: &str| {
        std::fs::write(root.join("tools").join(name), source).expect("script");
    };
    save(
        "build.gd",
        "extends SceneTree\n\nfunc _init() -> void:\n\tfor index in 3:\n\t\tvar mesh := BoxMesh.new()\n\t\tvar mesh_path := \"res://zones/mesh_%d.res\" % index\n\t\tResourceSaver.save(mesh, mesh_path)\n\t\tvar material := StandardMaterial3D.new()\n\t\tvar material_path := \"res://zones/material_%d.tres\" % index\n\t\tResourceSaver.save(material, material_path)\n\t\tvar root := Node3D.new()\n\t\troot.name = \"Zone\"\n\t\tvar body := MeshInstance3D.new()\n\t\tbody.mesh = load(mesh_path)\n\t\tbody.material_override = load(material_path)\n\t\troot.add_child(body)\n\t\tbody.owner = root\n\t\tvar scene := PackedScene.new()\n\t\tscene.pack(root)\n\t\tResourceSaver.save(scene, \"res://zones/zone_%d.tscn\" % index)\n\t\troot.free()\n\tquit()\n",
    );
    save(
        "load.gd",
        "extends SceneTree\n\nfunc _init() -> void:\n\tfor index in 3:\n\t\tvar zone: Node = (load(\"res://zones/zone_%d.tscn\" % index) as PackedScene).instantiate()\n\t\tprint(\"ZONE_CHILDREN=\", zone.get_child_count())\n\t\tzone.free()\n\tquit()\n",
    );
    let tools = manager(root);
    let run = |script: &'static str| {
        let tools = Arc::clone(&tools);
        let root = root.to_path_buf();
        async move {
            let result = call(
                &tools,
                "godot_run_script",
                json!({"script": script, "project_path": root, "timeout_seconds": 60}),
            )
            .await;
            let text = text_of(&result);
            assert!(!result.is_error, "{text}");
            serde_json::from_str::<Value>(&text[text.find('{').expect("json")..]).expect("JSON")
        }
    };
    let built = run("res://tools/build.gd").await;
    let new_uids = built["resource_repairs"]
        .as_array()
        .expect("repairs")
        .iter()
        .filter(|item| item["new_uid"] == true)
        .count();
    assert_eq!(new_uids, 9, "{built}");

    let loaded = run("res://tools/load.gd").await;
    let output = format!(
        "{}{}",
        loaded["stdout"].as_str().unwrap_or_default(),
        loaded["stderr"].as_str().unwrap_or_default()
    );
    assert_eq!(output.matches("ZONE_CHILDREN=1").count(), 3, "{loaded}");
    assert!(!output.contains("invalid UID"), "{output}");
    assert_eq!(built["uid_cache"]["recorded"], 9, "{built}");
    assert!(
        built.get("uid_cache_stale").is_none_or(Value::is_null),
        "{built}"
    );
}

/// A small 3D level: a ground slab with its top at y = 0, a bank box on collision layer 2 with
/// its top at y = 2 over x 4..6, z -1..1, a frog sphere standing at the origin, a camera at
/// (0, 6, 8) looking at the origin, and a red marker at x = 30 that the camera cannot see.
fn world_project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("project");
    let root = dir.path();
    std::fs::create_dir_all(root.join("scenes")).unwrap();
    std::fs::create_dir_all(root.join("scripts")).unwrap();
    std::fs::write(
        root.join("project.godot"),
        "config_version=5\n\n[application]\n\nconfig/name=\"WorldProbe\"\nrun/main_scene=\"res://scenes/world.tscn\"\nconfig/features=PackedStringArray(\"4.5\")\n",
    )
    .unwrap();
    std::fs::write(
        root.join("scripts/world.gd"),
        "extends Node3D\n\nfunc _ready() -> void:\n\t($Rig/Camera as Camera3D).look_at_from_position(Vector3(0, 6, 8), Vector3.ZERO)\n",
    )
    .unwrap();
    std::fs::write(
        root.join("scenes/world.tscn"),
        r#"[gd_scene load_steps=7 format=3]

[ext_resource type="Script" path="res://scripts/world.gd" id="1"]

[sub_resource type="BoxShape3D" id="ground"]
size = Vector3(40, 1, 40)

[sub_resource type="BoxShape3D" id="bank"]
size = Vector3(2, 2, 2)

[sub_resource type="SphereShape3D" id="frog"]
radius = 0.5

[sub_resource type="BoxMesh" id="marker"]
size = Vector3(6, 6, 6)

[sub_resource type="StandardMaterial3D" id="red"]
shading_mode = 0
albedo_color = Color(1, 0, 0, 1)

[node name="Level" type="Node3D"]
script = ExtResource("1")

[node name="Ground" type="StaticBody3D" parent="."]
transform = Transform3D(1, 0, 0, 0, 1, 0, 0, 0, 1, 0, -0.5, 0)

[node name="Shape" type="CollisionShape3D" parent="Ground"]
shape = SubResource("ground")

[node name="Bank" type="StaticBody3D" parent="."]
transform = Transform3D(1, 0, 0, 0, 1, 0, 0, 0, 1, 5, 1, 0)
collision_layer = 2

[node name="Shape" type="CollisionShape3D" parent="Bank"]
shape = SubResource("bank")

[node name="Frog" type="CharacterBody3D" parent="."]
transform = Transform3D(1, 0, 0, 0, 1, 0, 0, 0, 1, 0, 0.5, 0)

[node name="Shape" type="CollisionShape3D" parent="Frog"]
shape = SubResource("frog")

[node name="Rig" type="Node3D" parent="."]

[node name="Camera" type="Camera3D" parent="Rig"]
current = true

[node name="Marker" type="MeshInstance3D" parent="."]
transform = Transform3D(1, 0, 0, 0, 1, 0, 0, 0, 1, 30, 2, 0)
mesh = SubResource("marker")
material_override = SubResource("red")
"#,
    )
    .unwrap();
    dir
}

async fn start_world(headless: bool) -> (tempfile::TempDir, Arc<ToolManager>, String) {
    let project = world_project();
    let tools = manager(project.path());
    let started = call(
        &tools,
        "godot_run_project",
        json!({
            "project_path": project.path(),
            "scene": "res://scenes/world.tscn",
            "mode": "process",
            "headless": headless
        }),
    )
    .await;
    assert!(!started.is_error, "{}", text_of(&started));
    let run_id = run_id_of(&started);
    // Let the bodies reach the physics server.
    let waited = call(
        &tools,
        "godot_game_wait",
        json!({"run_id": run_id, "frames": 5}),
    )
    .await;
    assert!(!waited.is_error, "{}", text_of(&waited));
    (project, tools, run_id)
}

async fn probe(tools: &Arc<ToolManager>, run_id: &str, extra: Value) -> Value {
    let mut args = json!({"run_id": run_id});
    if let (Some(base), Some(more)) = (args.as_object_mut(), extra.as_object()) {
        for (key, value) in more {
            base.insert(key.clone(), value.clone());
        }
    }
    let reply = call(tools, "godot_game_probe", args).await;
    assert!(!reply.is_error, "{}", text_of(&reply));
    json_of(&reply)
}

fn close(value: &Value, expected: f64) -> bool {
    (json_number(value) - expected).abs() < 0.01
}

#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn probe_measures_the_world_without_moving_anything() {
    let _ = IGNORE;
    let (_project, tools, run_id) = start_world(true).await;

    let down = probe(&tools, &run_id, json!({"rays": [{"x": 0, "z": 0}]})).await;
    let frog = &down["rays"][0];
    assert_eq!(frog["hit"], true, "{down}");
    assert_eq!(frog["collider"], "Frog", "{down}");
    assert_eq!(frog["class"], "CharacterBody3D", "{down}");
    assert_eq!(frog["shape"], 0, "{down}");
    assert_eq!(frog["shape_node"], "Frog/Shape", "{down}");
    assert!(close(&frog["position"][1], 1.0), "{down}");

    // The frog is skipped, the bank is on layer 2 and the mask can leave it out.
    let rays = probe(
        &tools,
        &run_id,
        json!({
            "exclude": ["Frog"],
            "rays": [
                {"x": 0, "z": 0},
                {"x": 5, "z": 0},
                {"from": [-3, 1.5, 0], "to": [10, 1.5, 0]},
                {"x": 50, "z": 0}
            ]
        }),
    )
    .await;
    let hits = rays["rays"].as_array().expect("rays");
    assert_eq!(hits[0]["collider"], "Ground", "{rays}");
    assert!(close(&hits[0]["position"][1], 0.0), "{rays}");
    assert!(close(&hits[0]["normal"][1], 1.0), "{rays}");
    assert_eq!(hits[1]["collider"], "Bank", "{rays}");
    assert!(close(&hits[1]["position"][1], 2.0), "{rays}");
    assert_eq!(hits[2]["collider"], "Bank", "{rays}");
    assert!(close(&hits[2]["position"][0], 4.0), "{rays}");
    assert!(close(&hits[2]["normal"][0], -1.0), "{rays}");
    assert!(close(&hits[2]["distance"], 7.0), "{rays}");
    assert_eq!(hits[3]["hit"], false, "{rays}");
    assert_eq!(rays["hits"], 3, "{rays}");
    assert_eq!(rays["missed"], 1, "{rays}");

    let masked = probe(
        &tools,
        &run_id,
        json!({"mask": 1, "rays": [{"x": 5, "z": 0}]}),
    )
    .await;
    assert_eq!(masked["rays"][0]["collider"], "Ground", "{masked}");

    // A grid across the bank's edge: x 2.5 .. 7.5, z -0.5 and 0.5.
    let grid = probe(
        &tools,
        &run_id,
        json!({"exclude": ["Frog"], "grid": {"min": [2.5, -0.5], "max": [7.5, 0.5], "step": 1}}),
    )
    .await;
    assert_eq!(grid["grid"]["columns"], 6, "{grid}");
    assert_eq!(grid["grid"]["rows"], 2, "{grid}");
    for row in grid["heights"].as_array().expect("heights") {
        let heights: Vec<f64> = row
            .as_array()
            .expect("row")
            .iter()
            .map(json_number)
            .collect();
        let expected = [0.0, 0.0, 2.0, 2.0, 0.0, 0.0];
        for (got, want) in heights.iter().zip(expected) {
            assert!((got - want).abs() < 0.01, "{grid}");
        }
    }
    assert!(close(&grid["lowest"], 0.0), "{grid}");
    assert!(close(&grid["highest"], 2.0), "{grid}");
    let bank = grid["colliders"]
        .as_array()
        .and_then(|list| list.iter().find(|item| item["path"] == "Bank"))
        .unwrap_or_else(|| panic!("no Bank in {grid}"));
    assert_eq!(bank["count"], 4, "{grid}");
    assert!(grid.get("points").is_none(), "{grid}");
    let detailed = probe(
        &tools,
        &run_id,
        json!({"grid": {"min": [0, 0], "max": [1, 1], "count": [2, 2]}, "detail": true}),
    )
    .await;
    assert_eq!(
        detailed["points"].as_array().map(Vec::len),
        Some(4),
        "{detailed}"
    );

    let bad = call(
        &tools,
        "godot_game_probe",
        json!({"run_id": run_id, "rays": [{"from": [0, 1, 0]}]}),
    )
    .await;
    assert!(bad.is_error, "{}", text_of(&bad));
    assert!(text_of(&bad).contains("from and to"), "{}", text_of(&bad));
    let not_a_body = call(
        &tools,
        "godot_game_probe",
        json!({"run_id": run_id, "exclude": ["Rig"], "rays": [{"x": 0, "z": 0}]}),
    )
    .await;
    assert!(not_a_body.is_error, "{}", text_of(&not_a_body));
    assert!(
        text_of(&not_a_body).contains("CollisionObject3D"),
        "{}",
        text_of(&not_a_body)
    );

    // Nothing moved, and a held game is probed the same way.
    let frog_now = json_of(
        &call(
            &tools,
            "godot_game_get",
            json!({"run_id": run_id, "path": "Frog", "properties": ["position"]}),
        )
        .await,
    );
    assert!(
        close(&frog_now["properties"]["position"][1], 0.5),
        "{frog_now}"
    );
    hold_now(&tools, &run_id).await;
    let held = probe(&tools, &run_id, json!({"rays": [{"x": 5, "z": 0}]})).await;
    assert_eq!(held["rays"][0]["collider"], "Bank", "{held}");

    let stopped = call(&tools, "godot_stop_project", json!({"run_id": run_id})).await;
    assert!(!stopped.is_error, "{}", text_of(&stopped));
}

fn aim_of(note: &str) -> Value {
    let raw = note
        .split_once("Aim: ")
        .map(|(_, raw)| raw.lines().next().unwrap_or_default())
        .unwrap_or_else(|| panic!("no aim in {note}"));
    serde_json::from_str(raw).unwrap_or_else(|_| panic!("aim is not JSON: {note}"))
}

fn baseline_changed(note: &str) -> u64 {
    // Any baseline name: "..., baseline <name> changed <n> px, ...".
    note.split_once(", baseline ")
        .and_then(|(_, rest)| rest.split_once(" changed "))
        .and_then(|(_, rest)| rest.split(' ').next())
        .and_then(|number| number.parse().ok())
        .unwrap_or_else(|| panic!("no baseline comparison in {note}"))
}

#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn an_aimed_screenshot_looks_at_a_point_and_leaves_the_game_camera_alone() {
    let _ = IGNORE;
    if !windowed_game_can_open() {
        eprintln!("skipped: a windowed capture needs a display server");
        return;
    }
    let (_project, tools, run_id) = start_world(false).await;
    let shot = |extra: Value| {
        let tools = Arc::clone(&tools);
        let run_id = run_id.clone();
        async move {
            let mut args = json!({"run_id": run_id, "max_dimension": 128});
            if let (Some(base), Some(more)) = (args.as_object_mut(), extra.as_object()) {
                for (key, value) in more {
                    base.insert(key.clone(), value.clone());
                }
            }
            call(&tools, "godot_game_screenshot", args).await
        }
    };
    let plain = shot(json!({"save_baseline": "plain"})).await;
    assert!(!plain.is_error, "{}", text_of(&plain));

    let aimed =
        shot(json!({"baseline": "plain", "save_baseline": "aimed", "aim": {"at": [30, 2, 0]}}))
            .await;
    assert!(!aimed.is_error, "{}", text_of(&aimed));
    let note = text_of(&aimed);
    assert!(
        baseline_changed(&note) > 500,
        "the marker is not in the picture: {note}"
    );
    let aim = aim_of(&note);
    assert_eq!(aim["mode"], "at", "{aim}");
    assert_eq!(aim["copied_from"], "Rig/Camera", "{aim}");
    assert_eq!(aim["distance_from"], "game_camera_centre_hit", "{aim}");
    // The camera's centre ray meets the frog's sphere 9.4 m away.
    assert!(close(&aim["distance"], 9.4), "{aim}");
    assert!(close(&aim["position"][0], 30.0), "{aim}");
    assert!(close(&aim["position"][1], 2.0 + 0.6 * 9.4), "{aim}");
    assert!(close(&aim["position"][2], 0.8 * 9.4), "{aim}");
    assert_eq!(aim["projection"], "perspective", "{aim}");
    assert_eq!(aim["lens_from"], "game_camera", "{aim}");
    assert!(close(&aim["fov"], 75.0), "{aim}");

    let given = shot(json!({"aim": {"at": [30, 2, 0], "distance": 20}})).await;
    assert!(!given.is_error, "{}", text_of(&given));
    let given_aim = aim_of(&text_of(&given));
    assert_eq!(given_aim["distance_from"], "given", "{given_aim}");
    assert!(close(&given_aim["position"][2], 16.0), "{given_aim}");

    let placed = shot(json!({"aim": {"position": [30, 2, 12], "look_at": [30, 2, 0]}})).await;
    assert!(!placed.is_error, "{}", text_of(&placed));
    let placed_aim = aim_of(&text_of(&placed));
    assert_eq!(placed_aim["mode"], "look_at", "{placed_aim}");
    assert!(close(&placed_aim["forward"][2], -1.0), "{placed_aim}");

    // A wide orthographic overview from the same copy, without touching the game camera.
    let wide = shot(json!({"baseline": "aimed", "aim": {"at": [30, 2, 0], "size": 60}})).await;
    assert!(!wide.is_error, "{}", text_of(&wide));
    let wide_aim = aim_of(&text_of(&wide));
    assert_eq!(wide_aim["projection"], "orthogonal", "{wide_aim}");
    assert_eq!(wide_aim["projection_replaced"], "perspective", "{wide_aim}");
    assert_eq!(wide_aim["lens_from"], "given", "{wide_aim}");
    assert_eq!(wide_aim["aspect_axis"], "height", "{wide_aim}");
    assert!(close(&wide_aim["size"], 60.0), "{wide_aim}");
    assert!(wide_aim.get("fov").is_none(), "{wide_aim}");
    assert!(
        baseline_changed(&text_of(&wide)) > 500,
        "the overview looks like the plain aimed shot: {}",
        text_of(&wide)
    );
    let narrow = shot(json!({"baseline": "aimed", "aim": {"at": [30, 2, 0], "fov": 20}})).await;
    assert!(!narrow.is_error, "{}", text_of(&narrow));
    let narrow_aim = aim_of(&text_of(&narrow));
    assert_eq!(narrow_aim["projection"], "perspective", "{narrow_aim}");
    assert_eq!(narrow_aim["lens_from"], "given", "{narrow_aim}");
    assert!(
        narrow_aim.get("projection_replaced").is_none(),
        "{narrow_aim}"
    );
    assert!(close(&narrow_aim["fov"], 20.0), "{narrow_aim}");
    assert!(
        baseline_changed(&text_of(&narrow)) > 500,
        "a 20 degree view looks like the 75 degree one: {}",
        text_of(&narrow)
    );
    for (lens, expected) in [
        (json!({"size": 0}), "aim.size"),
        (json!({"size": 20000}), "10000"),
        (json!({"size": "wide"}), "aim.size"),
        (json!({"fov": 0.5}), "aim.fov"),
        (json!({"fov": 180}), "179"),
        (json!({"size": 10, "fov": 30}), "not both"),
    ] {
        let mut aim = json!({"at": [30, 2, 0]});
        if let (Some(base), Some(more)) = (aim.as_object_mut(), lens.as_object()) {
            for (key, value) in more {
                base.insert(key.clone(), value.clone());
            }
        }
        let refused = shot(json!({"aim": aim})).await;
        assert!(refused.is_error, "{lens}: {}", text_of(&refused));
        assert!(
            text_of(&refused).contains(expected),
            "{lens}: {}",
            text_of(&refused)
        );
    }

    let wrong = shot(json!({"aim": {"at": [1, 2]}})).await;
    assert!(wrong.is_error, "{}", text_of(&wrong));
    let both = shot(json!({"aim": {"at": [1, 2, 3], "look_at": [0, 0, 0]}})).await;
    assert!(both.is_error, "{}", text_of(&both));

    // The game camera is still current and where it was, and the temporary nodes are gone.
    let camera = json_of(
        &call(
            &tools,
            "godot_game_get",
            json!({"run_id": run_id, "path": "Rig/Camera", "properties": ["current", "global_position", "projection", "fov"]}),
        )
        .await,
    );
    assert_eq!(camera["properties"]["current"], true, "{camera}");
    assert_eq!(camera["properties"]["projection"], 0, "{camera}");
    assert!(close(&camera["properties"]["fov"], 75.0), "{camera}");
    assert!(
        close(&camera["properties"]["global_position"][1], 6.0),
        "{camera}"
    );
    assert!(
        close(&camera["properties"]["global_position"][2], 8.0),
        "{camera}"
    );
    let leftovers = json_of(
        &call(
            &tools,
            "godot_game_tree",
            json!({"run_id": run_id, "root": "/root", "name": "GodotMcpAim"}),
        )
        .await,
    );
    assert_eq!(leftovers["count"], 0, "{leftovers}");
    let again = shot(json!({"baseline": "plain"})).await;
    assert!(!again.is_error, "{}", text_of(&again));
    assert!(
        baseline_changed(&text_of(&again)) < 50,
        "the game's own view changed: {}",
        text_of(&again)
    );

    // A held game draws the aimed view without advancing.
    hold_now(&tools, &run_id).await;
    let held = shot(json!({"baseline": "plain", "aim": {"at": [30, 2, 0]}})).await;
    assert!(!held.is_error, "{}", text_of(&held));
    assert!(
        baseline_changed(&text_of(&held)) > 500,
        "{}",
        text_of(&held)
    );
    let switch = shot(json!({"camera": "Rig/Camera"})).await;
    assert!(
        switch.is_error,
        "a camera switch is still refused while held"
    );

    let stopped = call(&tools, "godot_stop_project", json!({"run_id": run_id})).await;
    assert!(!stopped.is_error, "{}", text_of(&stopped));
}

#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn aimed_capture_preserves_instance_shader_geometry_without_advancing_a_hold() {
    if !windowed_game_can_open() {
        eprintln!("skipped: a windowed capture needs a display server");
        return;
    }
    let project = world_project();
    let settings_path = project.path().join("project.godot");
    let settings = std::fs::read_to_string(&settings_path).unwrap();
    std::fs::write(settings_path, format!("{settings}\n[rendering]\nrenderer/rendering_method=\"gl_compatibility\"\nanti_aliasing/quality/msaa_3d=2\n")).unwrap();
    std::fs::write(
        project.path().join("marker.gdshader"),
        r#"shader_type spatial;
render_mode unshaded;
instance uniform float extent = 0.0;
instance uniform float opacity = 0.0;
void vertex() { VERTEX *= extent; }
void fragment() { ALBEDO = vec3(1.0, 0.0, 0.0); ALPHA = opacity; }
"#,
    )
    .unwrap();
    let scene_path = project.path().join("scenes/world.tscn");
    let scene = std::fs::read_to_string(&scene_path).unwrap()
        .replace("[sub_resource type=\"StandardMaterial3D\" id=\"red\"]\nshading_mode = 0\nalbedo_color = Color(1, 0, 0, 1)",
            "[sub_resource type=\"ShaderMaterial\" id=\"red\"]\nshader = ExtResource(\"2\")")
        .replace("[ext_resource type=\"Script\" path=\"res://scripts/world.gd\" id=\"1\"]", "[ext_resource type=\"Script\" path=\"res://scripts/world.gd\" id=\"1\"]\n[ext_resource type=\"Shader\" path=\"res://marker.gdshader\" id=\"2\"]")
        .replace("1, 30, 2, 0)", "1, 0, 0, 0)")
        .replace("current = true", "current = true\nprojection = 1\nsize = 19.0");
    std::fs::write(scene_path, scene).unwrap();
    std::fs::write(project.path().join("scripts/world.gd"), "extends Node3D\nvar ticks := 0\nvar aim_started_frame := -1\nvar aim_drawn_frame := -1\nvar _aim_viewport: SubViewport\nfunc _ready() -> void:\n\tget_tree().node_added.connect(_node_added)\n\tRenderingServer.frame_post_draw.connect(_after_draw)\n\t$Rig/Camera.look_at_from_position(Vector3(0, 6, 8), Vector3.ZERO)\n\t$Marker.set_instance_shader_parameter(\"extent\", 1.0)\n\t$Marker.set_instance_shader_parameter(\"opacity\", 0.8)\nfunc _process(_delta: float) -> void:\n\tticks += 1\nfunc _node_added(node: Node) -> void:\n\tif node is SubViewport:\n\t\taim_started_frame = Engine.get_process_frames()\n\t\t_aim_viewport = node\nfunc _after_draw() -> void:\n\tif is_instance_valid(_aim_viewport):\n\t\taim_drawn_frame = Engine.get_process_frames()\n").unwrap();
    let tools = manager(project.path());
    let started = call(&tools, "godot_run_project", json!({"project_path": project.path(), "scene": "res://scenes/world.tscn", "mode": "process"})).await;
    assert!(!started.is_error, "{}", text_of(&started));
    let run_id = run_id_of(&started);
    let _ = call(
        &tools,
        "godot_game_wait",
        json!({"run_id": run_id, "frames": 20}),
    )
    .await;
    let plain = call(
        &tools,
        "godot_game_screenshot",
        json!({"run_id":run_id,"max_dimension":128,"save_baseline":"plain"}),
    )
    .await;
    assert!(!plain.is_error, "{}", text_of(&plain));
    let _ = call(
        &tools,
        "godot_game_set",
        json!({"run_id":run_id,"path":"Marker","properties":{"visible":false}}),
    )
    .await;
    let hidden = call(
        &tools,
        "godot_game_screenshot",
        json!({"run_id":run_id,"max_dimension":128,"baseline":"plain"}),
    )
    .await;
    assert!(!hidden.is_error, "{}", text_of(&hidden));
    assert!(
        baseline_changed(&text_of(&hidden)) > 500,
        "fixture shader is not visible in the game window: {}",
        text_of(&hidden)
    );
    let _ = call(
        &tools,
        "godot_game_set",
        json!({"run_id":run_id,"path":"Marker","properties":{"visible":true}}),
    )
    .await;
    let immediate = call(
        &tools,
        "godot_game_screenshot",
        json!({"run_id":run_id,"max_dimension":128,"aim":{"position":[0,6,8],"look_at":[0,0,0]}}),
    )
    .await;
    assert!(!immediate.is_error, "{}", text_of(&immediate));
    let timing = json_of(
        &call(
            &tools,
            "godot_game_get",
            json!({"run_id":run_id,"properties":["aim_started_frame","aim_drawn_frame"]}),
        )
        .await,
    );
    assert!(
        json_number(&timing["properties"]["aim_started_frame"]) >= 0.0,
        "{timing}"
    );
    assert_eq!(
        timing["properties"]["aim_started_frame"], timing["properties"]["aim_drawn_frame"],
        "the game advanced between creating the aim viewport and rendering it: {timing}"
    );
    hold_now(&tools, &run_id).await;
    let before = json_of(
        &call(
            &tools,
            "godot_game_get",
            json!({"run_id":run_id,"properties":["ticks"]}),
        )
        .await,
    );
    let aimed = call(&tools, "godot_game_screenshot", json!({"run_id":run_id,"max_dimension":128,"baseline":"plain","aim":{"position":[0,6,8],"look_at":[0,0,0]}})).await;
    let after = json_of(
        &call(
            &tools,
            "godot_game_get",
            json!({"run_id":run_id,"properties":["ticks"]}),
        )
        .await,
    );
    let stopped = call(&tools, "godot_stop_project", json!({"run_id":run_id})).await;
    assert!(!stopped.is_error, "{}", text_of(&stopped));
    assert!(!aimed.is_error, "{}", text_of(&aimed));
    assert_eq!(
        before["properties"]["ticks"], after["properties"]["ticks"],
        "render initialization advanced game time"
    );
    assert!(
        baseline_changed(&text_of(&aimed)) < 50,
        "the aimed image lost instance-shaded geometry: {}",
        text_of(&aimed)
    );
}

/// Two box scenes, a sphere, and a level with an environment, a sun and an orthogonal camera.
fn review_project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("project");
    let root = dir.path();
    std::fs::create_dir_all(root.join("kit")).unwrap();
    std::fs::write(
        root.join("project.godot"),
        "config_version=5\n\n[application]\n\nconfig/name=\"ReviewProbe\"\nconfig/features=PackedStringArray(\"4.5\")\n",
    )
    .unwrap();
    std::fs::write(
        root.join("kit/box.tscn"),
        "[gd_scene load_steps=2 format=3]\n\n[sub_resource type=\"BoxMesh\" id=\"1\"]\nsize = Vector3(2, 3, 1)\n\n[node name=\"Box\" type=\"Node3D\"]\n\n[node name=\"Mesh\" type=\"MeshInstance3D\" parent=\".\"]\ntransform = Transform3D(1, 0, 0, 0, 1, 0, 0, 0, 1, 0, 1.5, 0)\nmesh = SubResource(\"1\")\n",
    )
    .unwrap();
    std::fs::write(
        root.join("kit/ball.tscn"),
        "[gd_scene load_steps=2 format=3]\n\n[sub_resource type=\"SphereMesh\" id=\"1\"]\n\n[node name=\"Ball\" type=\"MeshInstance3D\"]\ntransform = Transform3D(1, 0, 0, 0, 1, 0, 0, 0, 1, 0, 0.5, 0)\nmesh = SubResource(\"1\")\n",
    )
    .unwrap();
    std::fs::write(
        root.join("level.tscn"),
        "[gd_scene load_steps=2 format=3]\n\n[sub_resource type=\"Environment\" id=\"1\"]\nbackground_mode = 1\nbackground_color = Color(0.2, 0.3, 0.5, 1)\n\n[node name=\"Level\" type=\"Node3D\"]\n\n[node name=\"WorldEnvironment\" type=\"WorldEnvironment\" parent=\".\"]\nenvironment = SubResource(\"1\")\n\n[node name=\"Sun\" type=\"DirectionalLight3D\" parent=\".\"]\ntransform = Transform3D(1, 0, 0, 0, 0.5, 0.866, 0, -0.866, 0.5, 0, 10, 0)\nshadow_enabled = true\n\n[node name=\"Player\" type=\"Node3D\" parent=\".\"]\n\n[node name=\"Camera\" type=\"Camera3D\" parent=\"Player\"]\ntransform = Transform3D(1, 0, 0, 0, 0.7071, 0.7071, 0, -0.7071, 0.7071, 0, 10, 10)\nprojection = 1\nsize = 12.0\ncurrent = true\n",
    )
    .unwrap();
    dir
}

/// Every file under `root` except the engine's `.godot` cache, as sorted relative paths.
fn project_files(root: &Path) -> Vec<String> {
    let mut found = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir).expect("dir").flatten() {
            let path = entry.path();
            if path.file_name().is_some_and(|name| name == ".godot") {
                continue;
            }
            if path.is_dir() {
                pending.push(path);
            } else {
                found.push(path.strip_prefix(root).unwrap().display().to_string());
            }
        }
    }
    found.sort();
    found
}

/// Width and height of the PNG image in a tool answer, read from its IHDR chunk.
fn answer_png_size(result: &ToolResult) -> Option<(u32, u32)> {
    let data = result.content.iter().find_map(|item| match item {
        godot_mcp::protocol::ToolContent::Image { data, mime_type } if mime_type == "image/png" => {
            Some(data.as_str())
        }
        _ => None,
    })?;
    let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut bytes = Vec::new();
    for chunk in data.as_bytes()[..32].chunks(4) {
        let mut triple = 0u32;
        for &symbol in chunk {
            triple = (triple << 6) | alphabet.iter().position(|&c| c == symbol)? as u32;
        }
        bytes.extend_from_slice(&triple.to_be_bytes()[1..]);
    }
    png_size(&bytes)
}

#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn review_assets_renders_a_row_with_counts_and_bounds_and_leaves_the_project_alone() {
    if !windowed_game_can_open() {
        eprintln!("skipped: the asset review renders in a window and needs a display server");
        return;
    }
    let project = review_project();
    let before = project_files(project.path());
    let tools = manager(project.path());

    let row = call(
        &tools,
        "godot_review_assets",
        json!({
            "assets": [
                {"scene": "res://kit/box.tscn"},
                {"scene": "res://kit/ball.tscn"},
                {"scene": "res://kit/box.tscn", "rotation": [0, 90, 0]}
            ],
            "gap": 1,
            "environment": {"scene": "res://level.tscn"},
            "planes": [{"kind": "water", "height": 0.2}],
            "width": 320,
            "height": 180
        }),
    )
    .await;
    assert!(!row.is_error, "{}", text_of(&row));
    assert_eq!(answer_png_size(&row), Some((320, 180)));
    let answer = json_of(&row);
    let assets = answer["assets"].as_array().expect("assets");
    assert_eq!(assets.len(), 3, "{answer}");
    assert_eq!(assets[0]["triangles"], 12, "{answer}");
    assert_eq!(assets[0]["mesh_instances"], 1, "{answer}");
    assert!(json_number(&assets[1]["triangles"]) > 100.0, "{answer}");
    for asset in assets {
        assert!(
            json_number(&asset["rendered"]["draw_calls"]) >= 1.0,
            "{asset}"
        );
        assert!(
            json_number(&asset["rendered"]["primitives"]) >= 12.0,
            "{asset}"
        );
    }
    // A row along X, 1 m apart, centred: box 2 wide, ball 1, the turned box 1.
    let min_x = |index: usize| json_number(&assets[index]["bounds"]["min"][0]);
    let max_x = |index: usize| json_number(&assets[index]["bounds"]["max"][0]);
    assert!((min_x(1) - max_x(0) - 1.0).abs() < 0.01, "{answer}");
    assert!((min_x(2) - max_x(1) - 1.0).abs() < 0.01, "{answer}");
    assert!((min_x(0) + max_x(2)).abs() < 0.01, "{answer}");
    assert!(
        (json_number(&assets[0]["bounds"]["min"][1])).abs() < 0.01,
        "{answer}"
    );
    assert!(
        (json_number(&assets[0]["bounds"]["max"][1]) - 3.0).abs() < 0.01,
        "{answer}"
    );
    assert_eq!(
        answer["totals"]["triangles"],
        json_number(&assets[1]["triangles"]) as i64 + 24
    );
    assert!(
        json_number(&answer["totals"]["rendered"]["draw_calls"]) >= 4.0,
        "the frame has three assets and the water: {answer}"
    );
    assert_eq!(answer["camera"]["projection"], "orthogonal", "{answer}");
    assert_eq!(answer["warnings"], json!([]), "{answer}");

    let copied = call(
        &tools,
        "godot_review_assets",
        json!({
            "assets": [{"scene": "res://kit/box.tscn", "position": [5, 0, 5]}],
            "camera": {"scene": "res://level.tscn", "node": "Player/Camera"},
            "width": 200,
            "height": 100
        }),
    )
    .await;
    assert!(!copied.is_error, "{}", text_of(&copied));
    let answer = json_of(&copied);
    assert_eq!(
        answer["camera"]["size"], 12.0,
        "the copied size is kept: {answer}"
    );
    assert!(
        (json_number(&answer["camera"]["pitch"]) + 45.0).abs() < 0.1,
        "{answer}"
    );
    assert_eq!(answer["assets"][0]["position"], json!([5.0, 0.0, 5.0]));

    let perspective = call(
        &tools,
        "godot_review_assets",
        json!({
            "assets": [{"scene": "res://kit/ball.tscn"}],
            "camera": {"projection": "perspective", "fov": 40, "yaw": 0, "pitch": 0},
            "width": 64,
            "height": 64
        }),
    )
    .await;
    assert!(!perspective.is_error, "{}", text_of(&perspective));
    let answer = json_of(&perspective);
    assert_eq!(answer["camera"]["projection"], "perspective", "{answer}");
    let distance = json_number(&answer["camera"]["position"][2]);
    assert!(
        distance > 1.0 && distance < 4.0,
        "fitted to a 1 m ball: {answer}"
    );

    let missing = call(
        &tools,
        "godot_review_assets",
        json!({"assets": [{"scene": "res://kit/box.tscn"}], "camera": {"scene": "res://level.tscn", "node": "Nowhere"}}),
    )
    .await;
    assert!(missing.is_error, "{}", text_of(&missing));
    assert!(
        text_of(&missing).contains("no Camera3D at Nowhere"),
        "{}",
        text_of(&missing)
    );

    assert_eq!(
        project_files(project.path()),
        before,
        "the review wrote into the project"
    );
}

/// A 2 x 2 grid of 20 m zones: two with ground (a box body and its mesh, one with a nested rock
/// scene), one with lake dressing only, one with nothing to draw, and a file that does not load.
fn world_map_project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("project");
    let root = dir.path();
    std::fs::create_dir_all(root.join("zones")).unwrap();
    std::fs::create_dir_all(root.join("props")).unwrap();
    std::fs::write(
        root.join("project.godot"),
        // Compatibility, as the project the blank-PNG report came from: it leaves alpha 0 in a
        // viewport's target.
        "config_version=5\n\n[application]\n\nconfig/name=\"WorldMapProbe\"\nconfig/features=PackedStringArray(\"4.5\")\n\n[rendering]\n\nrenderer/rendering_method=\"gl_compatibility\"\n",
    )
    .unwrap();
    std::fs::write(
        root.join("props/rock.tscn"),
        "[gd_scene load_steps=2 format=3]\n\n[sub_resource type=\"SphereMesh\" id=\"1\"]\n\n[node name=\"Rock\" type=\"MeshInstance3D\"]\nmesh = SubResource(\"1\")\n",
    )
    .unwrap();
    let ground = |name: &str, x: i32, z: i32, rock: bool, color: &str| {
        let (steps, external, nested) = if rock {
            (
                5,
                "[ext_resource type=\"PackedScene\" path=\"res://props/rock.tscn\" id=\"rock\"]\n\n",
                "\n[node name=\"Rock\" parent=\".\" instance=ExtResource(\"rock\")]\ntransform = Transform3D(1, 0, 0, 0, 1, 0, 0, 0, 1, 2, 1, 2)\n",
            )
        } else {
            (4, "", "")
        };
        format!(
            "[gd_scene load_steps={steps} format=3]\n\n{external}[sub_resource type=\"BoxShape3D\" id=\"1\"]\nsize = Vector3(20, 1, 20)\n\n[sub_resource type=\"StandardMaterial3D\" id=\"3\"]\nalbedo_color = Color({color}, 1)\n\n[sub_resource type=\"BoxMesh\" id=\"2\"]\nmaterial = SubResource(\"3\")\nsize = Vector3(20, 1, 20)\n\n[node name=\"{name}\" type=\"Node3D\"]\ntransform = Transform3D(1, 0, 0, 0, 1, 0, 0, 0, 1, {x}, 0, {z})\n\n[node name=\"Ground\" type=\"StaticBody3D\" parent=\".\"]\n\n[node name=\"Shape\" type=\"CollisionShape3D\" parent=\"Ground\"]\nshape = SubResource(\"1\")\n\n[node name=\"Mesh\" type=\"MeshInstance3D\" parent=\"Ground\"]\nmesh = SubResource(\"2\")\n{nested}"
        )
    };
    std::fs::write(
        root.join("zones/zone_0_0.tscn"),
        ground("Zone00", 0, 0, true, "0.8, 0.15, 0.1"),
    )
    .unwrap();
    std::fs::write(
        root.join("zones/zone_1_0.tscn"),
        ground("Zone10", 20, 0, false, "0.1, 0.25, 0.85"),
    )
    .unwrap();
    std::fs::write(
        root.join("zones/zone_0_1.tscn"),
        "[gd_scene load_steps=2 format=3]\n\n[sub_resource type=\"PlaneMesh\" id=\"1\"]\nsize = Vector2(20, 20)\n\n[node name=\"Zone01\" type=\"Node3D\"]\ntransform = Transform3D(1, 0, 0, 0, 1, 0, 0, 0, 1, 0, 0, 20)\n\n[node name=\"Water\" type=\"MeshInstance3D\" parent=\".\"]\nmesh = SubResource(\"1\")\n\n[node name=\"Reeds\" type=\"Node3D\" parent=\".\"]\n",
    )
    .unwrap();
    std::fs::write(
        root.join("zones/zone_1_1.tscn"),
        "[gd_scene format=3]\n\n[node name=\"Zone11\" type=\"Node3D\"]\ntransform = Transform3D(1, 0, 0, 0, 1, 0, 0, 0, 1, 20, 0, 20)\n\n[node name=\"Marker\" type=\"Node3D\" parent=\".\"]\n",
    )
    .unwrap();
    std::fs::write(root.join("zones/zone_9_9.tscn"), "not a scene\n").unwrap();
    dir
}

/// Every file under `root` except the engine's `.godot` cache and the `.maps` folder the
/// test asks the map to be written to, as sorted relative paths.
fn world_map_project_files(root: &Path) -> Vec<String> {
    let mut found = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir).expect("dir").flatten() {
            let path = entry.path();
            if path
                .file_name()
                .is_some_and(|name| name == ".godot" || name == ".maps")
            {
                continue;
            }
            if path.is_dir() {
                pending.push(path);
            } else {
                found.push(path.strip_prefix(root).unwrap().display().to_string());
            }
        }
    }
    found.sort();
    found
}

/// Compares a saved map PNG with an inline preview in a headless Godot: the PNG's colour type
/// (2 is RGB, so opaque), the mean difference per channel once the PNG is scaled to the preview's
/// size, and the PNG's spread of brightness, which a blank picture lacks.
fn compare_map_with_preview(png: &Path, preview: &[u8]) -> Value {
    let dir = tempfile::tempdir().expect("scratch");
    let preview_file = dir.path().join("preview.jpg");
    std::fs::write(&preview_file, preview).expect("preview");
    let script = dir.path().join("compare.gd");
    std::fs::write(
        &script,
        "extends SceneTree\n\nfunc _initialize() -> void:\n\tvar args: PackedStringArray = OS.get_cmdline_user_args()\n\tvar map: Image = Image.load_from_file(args[0])\n\tvar preview: Image = Image.load_from_file(args[1])\n\tmap.convert(Image.FORMAT_RGBA8)\n\tpreview.convert(Image.FORMAT_RGBA8)\n\tvar min_alpha: float = 1.0\n\tfor y: int in range(0, map.get_height(), 7):\n\t\tfor x: int in range(0, map.get_width(), 7):\n\t\t\tmin_alpha = minf(min_alpha, map.get_pixel(x, y).a)\n\tmap.resize(preview.get_width(), preview.get_height(), Image.INTERPOLATE_CUBIC)\n\tvar diff: float = 0.0\n\tvar low: float = 1.0\n\tvar high: float = 0.0\n\tvar count: int = 0\n\tfor y: int in preview.get_height():\n\t\tfor x: int in preview.get_width():\n\t\t\tvar a: Color = map.get_pixel(x, y)\n\t\t\tvar b: Color = preview.get_pixel(x, y)\n\t\t\tdiff += (absf(a.r - b.r) + absf(a.g - b.g) + absf(a.b - b.b)) / 3.0\n\t\t\tlow = minf(low, a.get_luminance())\n\t\t\thigh = maxf(high, a.get_luminance())\n\t\t\tcount += 1\n\tprint(\"WORLD_MAP_COMPARE=\", JSON.stringify({\"min_alpha\": min_alpha, \"mean_diff\": diff / count, \"spread\": high - low}))\n\tquit()\n",
    )
    .expect("script");
    let output = std::process::Command::new(godot_path())
        .arg("--headless")
        .arg("-s")
        .arg(&script)
        .arg("--")
        .arg(png)
        .arg(&preview_file)
        .output()
        .expect("run Godot");
    let text = String::from_utf8_lossy(&output.stdout);
    let line = text
        .lines()
        .find_map(|line| line.strip_prefix("WORLD_MAP_COMPARE="))
        .unwrap_or_else(|| panic!("no comparison: {text}"));
    let mut answer: Value = serde_json::from_str(line).expect("comparison JSON");
    let bytes = std::fs::read(png).expect("png");
    answer["colour_type"] = json!(bytes.get(25).copied());
    answer
}

/// The mean colour of each tile of a PNG cut into side x side equal squares, read in a headless
/// Godot, row by row from the top left.
fn tile_means(png: &Path, side: u32) -> Vec<[f64; 3]> {
    let dir = tempfile::tempdir().expect("scratch");
    let script = dir.path().join("tiles.gd");
    std::fs::write(
        &script,
        "extends SceneTree\n\nfunc _initialize() -> void:\n\tvar args: PackedStringArray = OS.get_cmdline_user_args()\n\tvar map: Image = Image.load_from_file(args[0])\n\tvar side: int = int(args[1])\n\tvar size: Vector2i = map.get_size() / side\n\tvar means: Array = []\n\tfor row: int in side:\n\t\tfor column: int in side:\n\t\t\tvar sum: Color = Color(0, 0, 0, 0)\n\t\t\tfor y: int in size.y:\n\t\t\t\tfor x: int in size.x:\n\t\t\t\t\tsum += map.get_pixel(column * size.x + x, row * size.y + y)\n\t\t\tvar count: float = float(size.x * size.y)\n\t\t\tmeans.append([sum.r / count, sum.g / count, sum.b / count])\n\tprint(\"WORLD_MAP_TILES=\", JSON.stringify(means))\n\tquit()\n",
    )
    .expect("script");
    let output = std::process::Command::new(godot_path())
        .arg("--headless")
        .arg("-s")
        .arg(&script)
        .arg("--")
        .arg(png)
        .arg(side.to_string())
        .output()
        .expect("run Godot");
    let text = String::from_utf8_lossy(&output.stdout);
    let line = text
        .lines()
        .find_map(|line| line.strip_prefix("WORLD_MAP_TILES="))
        .unwrap_or_else(|| panic!("no tile means: {text}"));
    serde_json::from_str(line).expect("tile means JSON")
}

/// The inline image of a tool answer, decoded from base64, with its media type.
fn world_map_image(result: &ToolResult) -> (Vec<u8>, String) {
    let data = result
        .content
        .iter()
        .find_map(|item| match item {
            godot_mcp::protocol::ToolContent::Image { data, mime_type } => {
                Some((data.as_str(), mime_type.clone()))
            }
            _ => None,
        })
        .expect("an image in the answer");
    let (data, mime_type) = data;
    let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut bytes = Vec::new();
    for chunk in data.trim_end_matches('=').as_bytes().chunks(4) {
        let mut bits = 0u32;
        for &symbol in chunk {
            bits = (bits << 6) | alphabet.iter().position(|&c| c == symbol).expect("base64") as u32;
        }
        bits <<= 6 * (4 - chunk.len() as u32);
        bytes.extend_from_slice(&bits.to_be_bytes()[1..chunk.len()]);
    }
    (bytes, mime_type)
}

#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn world_map_draws_zones_and_names_the_ones_without_ground() {
    if !windowed_game_can_open() {
        eprintln!("skipped: the world map renders in a window and needs a display server");
        return;
    }
    let project = world_map_project();
    let tools = manager(project.path());
    // The first run imports the scenes' sidecars; compare the project after it.
    let map = call(
        &tools,
        "godot_world_map",
        json!({
            "dir": "res://zones",
            "glob": "zone_*.tscn",
            "pixels_per_metre": 8,
            "detail": "scenes",
            "output_path": "res://.maps/zones.png"
        }),
    )
    .await;
    assert!(!map.is_error, "{}", text_of(&map));
    assert!(
        map.content
            .iter()
            .all(|item| matches!(item, godot_mcp::protocol::ToolContent::Text { .. })),
        "with output_path the reply carries paths, not a picture"
    );
    let reply = text_of(&map);
    assert!(
        reply.trim_start().starts_with("{\n  \"totals\""),
        "totals come first: {reply}"
    );
    let png = std::fs::read(project.path().join(".maps/zones.png")).expect("the map PNG");
    let written: Value = serde_json::from_str(
        &std::fs::read_to_string(project.path().join(".maps/zones.json")).expect("the map JSON"),
    )
    .expect("the written JSON parses");
    assert_eq!(written["scenes"].as_array().map(Vec::len), Some(5));
    // The same map inline: the saved PNG must be opaque and show what the preview shows.
    let inline = call(
        &tools,
        "godot_world_map",
        json!({"dir": "res://zones", "glob": "zone_*.tscn", "pixels_per_metre": 8}),
    )
    .await;
    assert!(!inline.is_error, "{}", text_of(&inline));
    let (preview, _) = world_map_image(&inline);
    let compared = compare_map_with_preview(&project.path().join(".maps/zones.png"), &preview);
    assert_eq!(compared["colour_type"], 2, "an RGB PNG: {compared}");
    assert_eq!(compared["min_alpha"], 1.0, "{compared}");
    assert!(
        json_number(&compared["spread"]) > 0.2,
        "the map is not one colour: {compared}"
    );
    assert!(
        json_number(&compared["mean_diff"]) < 0.06,
        "the PNG matches the preview: {compared}"
    );
    if let Ok(out) = std::env::var("WORLD_MAP_PNG_OUT") {
        std::fs::write(out, &png).expect("save the map");
    }
    let answer = json_of(&map);
    // Geometry spans x and z from -10 to 30, plus a 1 m margin on each side, at 8 px/m.
    assert_eq!(png_size(&png), Some((336, 336)), "{answer}");
    assert_eq!(answer["image"]["width"], 336, "{answer}");
    assert_eq!(answer["image"]["rect"], json!([-11.0, -11.0, 31.0, 31.0]));
    assert_eq!(answer["image"]["up"], "-Z");
    assert_eq!(answer["image"]["preview"], json!([336, 336]));
    assert_eq!(answer["files"]["png"], "res://.maps/zones.png", "{answer}");
    assert_eq!(
        answer["totals"],
        json!({"scenes": 5, "no_collision": 2, "no_geometry": 1, "failed": 1}),
        "{answer}"
    );
    let scenes = answer["scenes"].as_array().expect("scenes");
    let scene = |name: &str| {
        scenes
            .iter()
            .find(|scene| scene["scene"] == format!("res://zones/{name}.tscn"))
            .unwrap_or_else(|| panic!("{name}: {answer}"))
    };
    let first = scene("zone_0_0");
    assert_eq!(first["root"]["name"], "Zone00", "{first}");
    assert_eq!(first["top_level"], json!(["Ground", "Rock"]), "{first}");
    assert_eq!(first["mesh_instances"], 2, "{first}");
    assert_eq!(first["collision_objects"], 1, "{first}");
    assert_eq!(first["collision_shapes"], 1, "{first}");
    assert_eq!(first["collision_layers"], json!([1]), "{first}");
    assert_eq!(first["instanced_scenes"], 1, "{first}");
    assert_eq!(first["has_collision"], true, "{first}");
    assert_eq!(
        first["collision_bounds"]["min"],
        json!([-10.0, -0.5, -10.0]),
        "{first}"
    );
    let second = scene("zone_1_0");
    assert_eq!(second["root"]["position"], json!([20.0, 0.0, 0.0]));
    assert_eq!(second["bounds"]["min"][0], 10.0, "{second}");
    let lake = scene("zone_0_1");
    assert_eq!(lake["has_geometry"], true, "{lake}");
    assert_eq!(lake["has_collision"], false, "{lake}");
    assert_eq!(lake["collision_bounds"], Value::Null, "{lake}");
    let empty = scene("zone_1_1");
    assert_eq!(empty["has_geometry"], false, "{empty}");
    assert_eq!(empty["bounds"], Value::Null, "{empty}");
    assert!(scene("zone_9_9")["error"].as_str().is_some(), "{answer}");
    let problems = answer["problems"].to_string();
    for expected in [
        "zone_0_1.tscn: no collision (visible geometry only)",
        "zone_1_1.tscn: no collision and no visible geometry",
        "zone_9_9.tscn:",
    ] {
        assert!(problems.contains(expected), "{expected}: {problems}");
    }
    assert!(!problems.contains("zone_0_0"), "{problems}");
    let before = world_map_project_files(project.path());

    // Small tiles: the map is the same size, built from 16 tiles of at most four zones each.
    let collision = call(
        &tools,
        "godot_world_map",
        json!({
            "scenes": ["res://zones/zone_0_0.tscn", "res://zones/zone_1_0.tscn", "res://zones/zone_0_1.tscn"],
            "rect": [-10, -10, 30, 30],
            "pixels_per_metre": 4,
            "tile_metres": 10,
            "view": "collision",
            "labels": false
        }),
    )
    .await;
    assert!(!collision.is_error, "{}", text_of(&collision));
    let (preview, mime_type) = world_map_image(&collision);
    assert_eq!(mime_type, "image/jpeg");
    assert!(preview.starts_with(&[0xff, 0xd8]), "a JPEG preview");
    if let Ok(out) = std::env::var("WORLD_MAP_COLLISION_PNG_OUT") {
        std::fs::write(out, &preview).expect("save the preview");
    }
    let answer = json_of(&collision);
    assert!(
        answer.get("scenes").is_none(),
        "the default reply lists no scenes: {answer}"
    );
    assert_eq!(answer["image"]["preview"], json!([160, 160]));
    assert_eq!(answer["image"]["view"], "collision", "{answer}");
    assert_eq!(answer["image"]["tiles"], 16, "{answer}");
    assert!(
        json_number(&answer["image"]["max_scenes_per_tile"]) <= 3.0,
        "{answer}"
    );
    assert_eq!(answer["totals"]["no_collision"], 1, "{answer}");

    // One tile per zone, each zone a different colour (red ground, blue ground, the lake's white
    // plane, the empty background): every tile must show its own area, not a neighbour's.
    let tiled = call(
        &tools,
        "godot_world_map",
        json!({
            "dir": "res://zones",
            "glob": "zone_?_?.tscn",
            "rect": [-10, -10, 30, 30],
            "pixels_per_metre": 4,
            "tile_metres": 20,
            "labels": false,
            "output_path": "res://.maps/tiles.png"
        }),
    )
    .await;
    assert!(!tiled.is_error, "{}", text_of(&tiled));
    assert_eq!(json_of(&tiled)["image"]["tiles"], 4, "{}", text_of(&tiled));
    let means = tile_means(&project.path().join(".maps/tiles.png"), 2);
    assert_eq!(means.len(), 4, "{means:?}");
    for first in 0..means.len() {
        for second in first + 1..means.len() {
            let distance: f64 = (0..3)
                .map(|channel| (means[first][channel] - means[second][channel]).abs())
                .sum();
            assert!(
                distance > 0.15,
                "tiles {first} and {second} look alike: {means:?}"
            );
        }
    }
    assert!(
        means[0][0] > means[0][2] && means[1][2] > means[1][0],
        "the red zone is top left, the blue one top right: {means:?}"
    );

    let nothing = call(
        &tools,
        "godot_world_map",
        json!({"scenes": ["res://zones/zone_1_1.tscn"]}),
    )
    .await;
    assert!(nothing.is_error, "{}", text_of(&nothing));
    assert!(
        text_of(&nothing).contains("pass rect"),
        "{}",
        text_of(&nothing)
    );

    assert_eq!(
        world_map_project_files(project.path()),
        before,
        "the map wrote into the project"
    );
}

#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn world_map_refuses_a_map_godot_did_not_draw() {
    if !windowed_game_can_open() {
        eprintln!("skipped: the world map renders in a window and needs a display server");
        return;
    }
    let project = world_map_project();
    // The script runs directly so its test hook can make tiles read back black, as a window
    // does that cannot draw (the display asleep): the tool itself never sets the hook.
    let request = |undrawn: &str| {
        json!({
            "scenes": [
                "res://zones/zone_0_0.tscn",
                "res://zones/zone_0_1.tscn",
                "res://zones/zone_1_0.tscn",
                "res://zones/zone_1_1.tscn"
            ],
            "rect": [-10, -10, 30, 30],
            "pixels_per_metre": 4,
            "tile_metres": 20,
            "view": "geometry",
            "labels": true,
            "default_side": 2048,
            "test_undrawn": undrawn
        })
    };
    let runner = CliRunner::new(godot_path());
    let script = include_str!("../src/tools/world_map.gd");

    let none = runner
        .world_map(project.path(), script, request("all"), 120)
        .await
        .expect("run");
    let answer: Value =
        serde_json::from_str(none.answer.as_deref().expect("an answer")).expect("JSON");
    let error = answer["error"].as_str().unwrap_or_default();
    assert!(
        error.contains("drew none of the 4 tiles"),
        "a black map is refused, not returned: {answer}"
    );
    assert!(none.image.is_none(), "no picture is saved for it");

    let partial = runner
        .world_map(project.path(), script, request("first"), 120)
        .await
        .expect("run");
    let answer: Value =
        serde_json::from_str(partial.answer.as_deref().expect("an answer")).expect("JSON");
    assert!(answer.get("error").is_none(), "{answer}");
    for key in ["warnings", "problems"] {
        assert!(
            answer[key]
                .to_string()
                .contains("tiles 1 of 4 were not drawn"),
            "{key}: {answer}"
        );
    }

    let drawn = runner
        .world_map(project.path(), script, request(""), 120)
        .await
        .expect("run");
    let answer: Value =
        serde_json::from_str(drawn.answer.as_deref().expect("an answer")).expect("JSON");
    assert!(answer.get("error").is_none(), "{answer}");
    assert_eq!(
        answer["problems"].as_array().map(Vec::len),
        Some(2),
        "{answer}"
    );
    assert!(
        !answer["warnings"].to_string().contains("not drawn"),
        "{answer}"
    );
}

// ---- godot_game_run_script ----

/// A scene that counts how often it was loaded and how many ticks it ran, and a method that raises
/// a script error of its own.
const LIVE_SCENE_SCRIPT: &str = r#"extends Node3D
static var loads := 0
var marker := "untouched"
var ticks := 0

func _ready() -> void:
    loads += 1

func _physics_process(_delta: float) -> void:
    ticks += 1

func boom() -> void:
    var nothing: Variant = null
    nothing.missing()
"#;

fn live_scene_project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("project");
    let root = dir.path();
    std::fs::create_dir_all(root.join("tests")).unwrap();
    std::fs::write(
        root.join("project.godot"),
        "config_version=5\n\n[application]\n\nconfig/name=\"LiveScene\"\nrun/main_scene=\"res://live.tscn\"\nconfig/features=PackedStringArray(\"4.5\")\n",
    )
    .unwrap();
    std::fs::write(root.join("live.gd"), LIVE_SCENE_SCRIPT).unwrap();
    std::fs::write(
        root.join("live.tscn"),
        "[gd_scene load_steps=2 format=3]\n\n[ext_resource type=\"Script\" path=\"res://live.gd\" id=\"1\"]\n\n[node name=\"Live\" type=\"Node3D\"]\nscript = ExtResource(\"1\")\n",
    )
    .unwrap();
    dir
}

async fn start_live_scene(held: bool) -> (tempfile::TempDir, Arc<ToolManager>, String) {
    let project = live_scene_project();
    let tools = manager(project.path());
    let started = call(
        &tools,
        "godot_run_project",
        json!({
            "project_path": project.path(),
            "scene": "res://live.tscn",
            "mode": "process",
            "headless": true,
            "held": held
        }),
    )
    .await;
    assert!(!started.is_error, "{}", text_of(&started));
    let run_id = run_id_of(&started);
    (project, tools, run_id)
}

fn write_test(project: &tempfile::TempDir, name: &str, source: &str) -> String {
    std::fs::write(project.path().join("tests").join(name), source).unwrap();
    format!("res://tests/{name}")
}

async fn run_in_game(tools: &Arc<ToolManager>, run_id: &str, extra: Value) -> ToolResult {
    let mut args = json!({"run_id": run_id});
    if let (Some(base), Some(more)) = (args.as_object_mut(), extra.as_object()) {
        for (key, value) in more {
            base.insert(key.clone(), value.clone());
        }
    }
    call(tools, "godot_game_run_script", args).await
}

/// The JSON at the start of an error's text: the hints follow it.
fn failed_answer(result: &ToolResult) -> Value {
    assert!(result.is_error, "{}", text_of(result));
    let text = text_of(result);
    let start = text.find('{').unwrap_or(0);
    let mut values = serde_json::Deserializer::from_str(&text[start..]).into_iter::<Value>();
    match values.next() {
        Some(Ok(value)) => value,
        _ => panic!("no JSON in: {text}"),
    }
}

#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn test_scripts_run_one_after_another_against_a_scene_that_stays_loaded() {
    let _ = IGNORE;
    let (project, tools, run_id) = start_live_scene(false).await;

    // The file did not exist when the game started. It reaches the live scene, prints and returns.
    let path = write_test(
        &project,
        "first.gd",
        "extends Node\nfunc run() -> Dictionary:\n\tvar scene := get_tree().current_scene\n\tscene.marker = \"set by first\"\n\tprint(\"first ran in \", scene.name)\n\treturn {\"loads\": scene.get_script().loads, \"at\": Vector3(1, 2, 3)}\n",
    );
    let first = run_in_game(&tools, &run_id, json!({"script": path})).await;
    assert!(!first.is_error, "{}", text_of(&first));
    let first = json_of(&first);
    assert_eq!(first["ok"], true, "{first}");
    assert_eq!(first["value"]["loads"], 1, "{first}");
    assert_eq!(first["value"]["at"], json!([1.0, 2.0, 3.0]), "{first}");
    assert_eq!(first["output"], json!(["first ran in Live"]), "{first}");
    assert_eq!(first["run_id"], run_id.as_str(), "{first}");
    let instance = first["scene"]["scene_instance_id"].clone();

    // The same file, changed, runs as changed; the change the first call made is still there.
    let path = write_test(
        &project,
        "first.gd",
        "extends Node\nfunc run() -> String:\n\treturn \"changed sees \" + get_tree().current_scene.marker\n",
    );
    let second = json_of(&run_in_game(&tools, &run_id, json!({"script": path})).await);
    assert_eq!(second["value"], "changed sees set by first", "{second}");
    assert_eq!(second["scene"]["scene_instance_id"], instance, "{second}");

    // A named function, with arguments converted to the types it declares, that awaits.
    let path = write_test(
        &project,
        "awaits.gd",
        "extends Node\nfunc settle(frames: int, where: Vector3, tag := \"none\") -> Array:\n\tvar before: int = get_tree().current_scene.ticks\n\tfor _i in frames:\n\t\tawait get_tree().physics_frame\n\tprint(\"settled\")\n\treturn [get_tree().current_scene.ticks - before, where, tag]\n",
    );
    let awaited = json_of(
        &run_in_game(
            &tools,
            &run_id,
            json!({"script": path, "function": "settle", "args": [6, [1, 2, 3]]}),
        )
        .await,
    );
    assert_eq!(awaited["ok"], true, "{awaited}");
    assert!(
        json_number(&awaited["value"][0]) >= 5.0,
        "the scene ran while the function waited: {awaited}"
    );
    assert_eq!(awaited["value"][1], json!([1.0, 2.0, 3.0]), "{awaited}");
    assert_eq!(awaited["value"][2], "none", "{awaited}");
    assert_eq!(awaited["output"], json!(["settled"]), "{awaited}");
    assert!(json_number(&awaited["ticks_run"]) >= 6.0, "{awaited}");
    assert_eq!(awaited["scene"]["scene_instance_id"], instance, "{awaited}");

    // The node of each call is gone, and the scene was loaded once.
    let tree = call(
        &tools,
        "godot_game_tree",
        json!({"run_id": run_id, "root": "/root", "name": "McpTestScript"}),
    )
    .await;
    assert_eq!(json_of(&tree)["count"], 0, "{}", text_of(&tree));
    let path = write_test(
        &project,
        "loads.gd",
        "extends Node\nfunc run() -> int:\n\treturn get_tree().current_scene.get_script().loads\n",
    );
    let loads = json_of(&run_in_game(&tools, &run_id, json!({"script": path})).await);
    assert_eq!(loads["value"], 1, "{loads}");

    let stopped = call(&tools, "godot_stop_project", json!({"run_id": run_id})).await;
    assert!(!stopped.is_error, "{}", text_of(&stopped));
}

#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn a_held_game_runs_a_test_script_tick_by_tick_and_holds_again() {
    let _ = IGNORE;
    let (project, tools, run_id) = start_live_scene(true).await;
    let status = call(
        &tools,
        "godot_game_physics",
        json!({"run_id": run_id, "action": "status"}),
    )
    .await;
    let start = json_of(&status)["total_ticks"].as_i64().unwrap_or(-1);

    // No await: no tick passes.
    let path = write_test(
        &project,
        "now.gd",
        "extends Node\nfunc run() -> int:\n\tprint(\"at once\")\n\treturn get_tree().current_scene.ticks\n",
    );
    let now = json_of(&run_in_game(&tools, &run_id, json!({"script": path})).await);
    assert_eq!(now["ok"], true, "{now}");
    assert_eq!(now["ticks_run"], 0, "{now}");
    assert_eq!(now["held"], true, "{now}");
    assert_eq!(now["total_ticks"], start, "{now}");
    assert_eq!(now["output"], json!(["at once"]), "{now}");
    let ticks_before = now["value"].as_i64().unwrap_or(-1);

    // Awaiting N physics frames from a hold: the function resumes at the start of the Nth tick
    // after the one it stands in, and that tick finishes before the game holds again.
    let path = write_test(
        &project,
        "ticks.gd",
        "extends Node\nfunc run(count: int) -> int:\n\tvar seen: int = get_tree().current_scene.ticks\n\tfor _i in count:\n\t\tawait get_tree().physics_frame\n\treturn get_tree().current_scene.ticks - seen\n",
    );
    let awaited =
        json_of(&run_in_game(&tools, &run_id, json!({"script": path, "args": [5]})).await);
    assert_eq!(awaited["ok"], true, "{awaited}");
    assert_eq!(awaited["value"], 5, "{awaited}");
    assert_eq!(awaited["ticks_run"], 6, "{awaited}");
    assert_eq!(awaited["frames_advanced"], 6, "{awaited}");
    assert_eq!(awaited["held"], true, "{awaited}");
    assert_eq!(awaited["deterministic"], true, "{awaited}");
    assert_eq!(awaited["total_ticks"], start + 6, "{awaited}");

    // The game stands still afterwards, and the same call takes the same ticks again.
    let path = write_test(
        &project,
        "read.gd",
        "extends Node\nfunc run() -> int:\n\treturn get_tree().current_scene.ticks\n",
    );
    let read = json_of(&run_in_game(&tools, &run_id, json!({"script": path.clone()})).await);
    assert_eq!(read["value"], ticks_before + 6, "{read}");
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    let later = json_of(&run_in_game(&tools, &run_id, json!({"script": path})).await);
    assert_eq!(later["value"], ticks_before + 6, "{later}");
    let again = json_of(
        &run_in_game(
            &tools,
            &run_id,
            json!({"script": "res://tests/ticks.gd", "args": [5]}),
        )
        .await,
    );
    assert_eq!(again["ticks_run"], 6, "{again}");
    assert_eq!(again["total_ticks"], start + 12, "{again}");

    // Frames and timers pass too, and a step still works after a script.
    let path = write_test(
        &project,
        "timer.gd",
        "extends Node\nfunc run() -> String:\n\tawait get_tree().process_frame\n\tawait get_tree().create_timer(0.1).timeout\n\treturn \"timer fired\"\n",
    );
    let timer = json_of(&run_in_game(&tools, &run_id, json!({"script": path})).await);
    assert_eq!(timer["value"], "timer fired", "{timer}");
    assert_eq!(timer["held"], true, "{timer}");
    let stepped = held_step(&tools, &run_id, json!({"ticks": 3})).await;
    assert_eq!(json_of(&stepped)["ticks_run"], 3, "{}", text_of(&stepped));
    assert_eq!(json_of(&stepped)["deterministic"], true);

    // A failure while held still answers and holds again.
    let path = write_test(
        &project,
        "late_error.gd",
        "extends Node\nfunc run() -> void:\n\tawait get_tree().physics_frame\n\tprint(\"before the error\")\n\tvar nothing: Variant = null\n\tnothing.missing()\n",
    );
    let failed = failed_answer(&run_in_game(&tools, &run_id, json!({"script": path})).await);
    assert_eq!(failed["failure"], "script_error", "{failed}");
    assert_eq!(failed["output"], json!(["before the error"]), "{failed}");
    assert_eq!(failed["held"], true, "{failed}");

    // What never fires ends at the timeout, and the suspended function is gone.
    let path = write_test(
        &project,
        "forever.gd",
        "extends Node\nsignal never\nfunc run() -> void:\n\tprint(\"waiting\")\n\tawait never\n\tprint(\"resumed\")\n",
    );
    let late = failed_answer(
        &run_in_game(
            &tools,
            &run_id,
            json!({"script": path, "timeout_seconds": 1}),
        )
        .await,
    );
    assert_eq!(late["failure"], "timeout", "{late}");
    assert_eq!(late["output"], json!(["waiting"]), "{late}");
    assert_eq!(late["held"], true, "{late}");
    let after = held_step(&tools, &run_id, json!({"ticks": 2})).await;
    assert!(!after.is_error, "{}", text_of(&after));

    let stopped = call(&tools, "godot_stop_project", json!({"run_id": run_id})).await;
    assert!(!stopped.is_error, "{}", text_of(&stopped));
}

#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn a_test_script_that_cannot_run_or_fails_says_so_and_the_scene_stays() {
    let _ = IGNORE;
    let (project, tools, run_id) = start_live_scene(false).await;
    let instance = |answer: &Value| answer["scene"]["scene_instance_id"].clone();

    let missing = run_in_game(&tools, &run_id, json!({"script": "res://tests/none.gd"})).await;
    assert_error(&missing, "no script exists");

    // A parse error runs nothing; the same file, fixed, runs.
    let path = write_test(&project, "edit.gd", "extends Node\nfunc run(:\n");
    let broken = run_in_game(&tools, &run_id, json!({"script": path.clone()})).await;
    assert_error(&broken, "did not compile");
    assert_error(&broken, "nothing ran");
    write_test(
        &project,
        "edit.gd",
        "extends Node\nfunc run() -> String:\n\treturn \"fixed\"\n",
    );
    let fixed = json_of(&run_in_game(&tools, &run_id, json!({"script": path.clone()})).await);
    assert_eq!(fixed["value"], "fixed", "{fixed}");
    let scene = instance(&fixed);

    // The form of godot_run_script, and a script that is not a node, say what to change.
    let path_tree = write_test(
        &project,
        "tree.gd",
        "extends SceneTree\nfunc _initialize() -> void:\n\tquit()\n",
    );
    let tree = run_in_game(&tools, &run_id, json!({"script": path_tree})).await;
    assert_error(&tree, "extends Node");
    assert_error(&tree, "godot_run_script");
    let path_ref = write_test(&project, "plain.gd", "func run() -> int:\n\treturn 1\n");
    assert_error(
        &run_in_game(&tools, &run_id, json!({"script": path_ref})).await,
        "extends Node",
    );

    // The function: unknown (the answer lists what there is), too few or wrong arguments.
    let unknown = run_in_game(
        &tools,
        &run_id,
        json!({"script": path.clone(), "function": "nope"}),
    )
    .await;
    assert_error(&unknown, "no function nope");
    assert_error(&unknown, "\"run\"");
    let path_args = write_test(
        &project,
        "args.gd",
        "extends Node\nfunc run(count: int, where: Vector3, tag := \"\") -> int:\n\treturn count\n",
    );
    assert_error(
        &run_in_game(&tools, &run_id, json!({"script": path_args.clone()})).await,
        "takes 2 to 3 arguments",
    );
    assert_error(
        &run_in_game(
            &tools,
            &run_id,
            json!({"script": path_args, "args": [1, "not a vector"]}),
        )
        .await,
        "Argument 1",
    );

    // A script error with no await: the printed line comes back with the failure and its stack.
    let path = write_test(
        &project,
        "sync_error.gd",
        "extends Node\nfunc run() -> void:\n\tprint(\"before the error\")\n\tvar nothing: Variant = null\n\tnothing.missing()\n",
    );
    let failed = failed_answer(&run_in_game(&tools, &run_id, json!({"script": path})).await);
    assert_eq!(failed["ok"], false, "{failed}");
    assert_eq!(failed["failure"], "script_error", "{failed}");
    assert_eq!(failed["output"], json!(["before the error"]), "{failed}");
    assert!(
        failed["errors"][0]["stack"][0]
            .as_str()
            .unwrap_or_default()
            .starts_with("res://tests/sync_error.gd:5"),
        "{failed}"
    );
    assert_eq!(instance(&failed), scene, "{failed}");

    // An error in the game's own code while the function waits is reported, and is not its failure.
    let path = write_test(
        &project,
        "elsewhere.gd",
        "extends Node\nfunc run() -> String:\n\tget_tree().current_scene.boom.call_deferred()\n\tawait get_tree().process_frame\n\tawait get_tree().process_frame\n\treturn \"still fine\"\n",
    );
    let elsewhere = json_of(&run_in_game(&tools, &run_id, json!({"script": path})).await);
    assert_eq!(elsewhere["ok"], true, "{elsewhere}");
    assert_eq!(elsewhere["value"], "still fine", "{elsewhere}");
    assert!(
        elsewhere["errors"][0]["stack"][0]
            .as_str()
            .unwrap_or_default()
            .starts_with("res://live.gd"),
        "{elsewhere}"
    );

    // A number JSON cannot hold, and a value that is too large.
    let path = write_test(
        &project,
        "values.gd",
        "extends Node\nfunc infinite() -> Array:\n\treturn [INF, Vector3(NAN, 1, 2)]\nfunc huge() -> String:\n\tprint(\"made it\")\n\treturn \"x\".repeat(300000)\n",
    );
    let infinite = json_of(
        &run_in_game(
            &tools,
            &run_id,
            json!({"script": path.clone(), "function": "infinite"}),
        )
        .await,
    );
    assert_eq!(infinite["value"][0], "inf", "{infinite}");
    assert_eq!(infinite["value"][1][0], "nan", "{infinite}");
    let huge = failed_answer(
        &run_in_game(&tools, &run_id, json!({"script": path, "function": "huge"})).await,
    );
    assert_eq!(huge["failure"], "value_too_large", "{huge}");
    assert_eq!(huge["output"], json!(["made it"]), "{huge}");

    // A timeout ends the function, and the node of the call is gone.
    let path = write_test(
        &project,
        "forever.gd",
        "extends Node\nsignal never\nfunc run() -> void:\n\tawait never\n",
    );
    let late = failed_answer(
        &run_in_game(
            &tools,
            &run_id,
            json!({"script": path, "timeout_seconds": 1}),
        )
        .await,
    );
    assert_eq!(late["failure"], "timeout", "{late}");
    let tree = call(
        &tools,
        "godot_game_tree",
        json!({"run_id": run_id, "root": "/root", "name": "McpTestScript"}),
    )
    .await;
    assert_eq!(json_of(&tree)["count"], 0, "{}", text_of(&tree));

    let stopped = call(&tools, "godot_stop_project", json!({"run_id": run_id})).await;
    assert!(!stopped.is_error, "{}", text_of(&stopped));
}

#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn a_class_created_without_the_editor_is_known_to_the_next_run() {
    let _ = IGNORE;
    let dir = tempfile::tempdir().expect("class project");
    let root = dir.path();
    std::fs::write(
        root.join("project.godot"),
        "config_version=5\n\n[application]\n\nconfig/name=\"ClassCache\"\nconfig/features=PackedStringArray(\"4.8\")\n",
    )
    .unwrap();
    std::fs::create_dir_all(root.join("scripts")).unwrap();
    std::fs::write(
        root.join("scripts/use_class.gd"),
        "extends SceneTree\n\nfunc _init() -> void:\n\tvar flight: PartFlight = PartFlight.new()\n\tprint(\"CLASS=\", flight.get_script().get_global_name())\n\tquit()\n",
    )
    .unwrap();
    let tools = manager(root);
    let godot = godot_path();
    let import = || {
        std::process::Command::new(&godot)
            .args(["--headless", "--path"])
            .arg(root)
            .arg("--import")
            .output()
            .expect("import")
    };
    import();
    let run = |script: &'static str| {
        let tools = Arc::clone(&tools);
        async move {
            call(
                &tools,
                "godot_run_script",
                json!({"script": script, "project_path": root, "timeout_seconds": 60}),
            )
            .await
        }
    };

    // The failure this guards against: a class written to disk is unknown to a run.
    std::fs::write(
        root.join("scripts/stray.gd"),
        "extends RefCounted\nclass_name StrayClass\n",
    )
    .unwrap();
    std::fs::write(
        root.join("scripts/use_stray.gd"),
        "extends SceneTree\n\nfunc _init() -> void:\n\tprint(StrayClass.new())\n\tquit()\n",
    )
    .unwrap();
    let stray = run("res://scripts/use_stray.gd").await;
    assert!(stray.is_error, "{}", text_of(&stray));
    let validated = call(
        &tools,
        "godot_validate_script",
        json!({"script_path": "res://scripts/use_stray.gd", "project_path": root}),
    )
    .await;
    assert!(validated.is_error, "{}", text_of(&validated));
    assert!(
        text_of(&validated).contains("uses StrayClass (res://scripts/stray.gd)"),
        "{}",
        text_of(&validated)
    );
    std::fs::remove_file(root.join("scripts/stray.gd")).unwrap();
    std::fs::remove_file(root.join("scripts/use_stray.gd")).unwrap();

    let created = call(
        &tools,
        "godot_create_script",
        json!({"script_path": "res://scripts/part_flight.gd", "extends_class": "RefCounted",
               "class_name": "PartFlight", "project_path": root}),
    )
    .await;
    assert!(!created.is_error, "{}", text_of(&created));
    let ran = run("res://scripts/use_class.gd").await;
    assert!(!ran.is_error, "{}", text_of(&ran));
    assert!(
        text_of(&ran).contains("CLASS=PartFlight"),
        "{}",
        text_of(&ran)
    );
    let validated = call(
        &tools,
        "godot_validate_script",
        json!({"script_path": "res://scripts/use_class.gd", "project_path": root}),
    )
    .await;
    assert!(!validated.is_error, "{}", text_of(&validated));

    // A class that extends it by path records the base the editor records.
    let child = call(
        &tools,
        "godot_create_script",
        json!({"script_path": "res://scripts/child.gd", "extends_class": "Node",
               "class_name": "ChildFlight", "project_path": root}),
    )
    .await;
    assert!(!child.is_error, "{}", text_of(&child));
    let by_path = call(
        &tools,
        "godot_edit_script",
        json!({"script_path": "res://scripts/child.gd", "old_text": "extends Node\n",
               "new_text": "@tool\nextends \"part_flight.gd\"\n", "project_path": root}),
    )
    .await;
    assert!(!by_path.is_error, "{}", text_of(&by_path));

    // The editor's own scan writes the very same cache.
    let cache = root.join(".godot/global_script_class_cache.cfg");
    let ours = std::fs::read_to_string(&cache).expect("cache");
    import();
    let editors = std::fs::read_to_string(&cache).expect("cache");
    assert_eq!(ours, editors);

    // A rename by edit is what the next run sees.
    let renamed = call(
        &tools,
        "godot_edit_script",
        json!({"script_path": "res://scripts/part_flight.gd", "old_text": "class_name PartFlight",
               "new_text": "class_name RenamedFlight", "project_path": root}),
    )
    .await;
    assert!(!renamed.is_error, "{}", text_of(&renamed));
    std::fs::write(
        root.join("scripts/use_class.gd"),
        "extends SceneTree\n\nfunc _init() -> void:\n\tprint(\"CLASS=\", RenamedFlight.new().get_script().get_global_name())\n\tquit()\n",
    )
    .unwrap();
    let ran = run("res://scripts/use_class.gd").await;
    assert!(!ran.is_error, "{}", text_of(&ran));
    assert!(
        text_of(&ran).contains("CLASS=RenamedFlight"),
        "{}",
        text_of(&ran)
    );
}

// ---- a game that goes away during a call ----

#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn a_game_that_quits_during_a_script_says_so_at_once() {
    let _ = IGNORE;
    let (project, tools, run_id) = start_live_scene(false).await;
    let path = write_test(
        &project,
        "quits.gd",
        "extends Node\nfunc run() -> void:\n\tawait get_tree().create_timer(0.3).timeout\n\tget_tree().quit(7)\n\tawait get_tree().create_timer(600.0).timeout\n",
    );
    let started = std::time::Instant::now();
    let during = run_in_game(
        &tools,
        &run_id,
        json!({"script": path, "timeout_seconds": 120}),
    )
    .await;
    assert!(
        started.elapsed() < Duration::from_secs(30),
        "{:?}",
        started.elapsed()
    );
    assert_error(&during, "code 7");
    assert_error(&during, &run_id);
    assert_error(&during, "game.run_script");

    // Without a run id nothing is live, and the answer is that end and not the editor's port.
    let next = call(
        &tools,
        "godot_game_run_script",
        json!({"script": "res://tests/quits.gd"}),
    )
    .await;
    assert_error(&next, "code 7");
    assert!(!text_of(&next).contains("bridge"), "{}", text_of(&next));

    let status = json_of(&call(&tools, "godot_get_run_status", json!({})).await);
    let run = &status["process_runs"][0];
    assert_eq!(run["exit_code"], 7, "{status}");
    assert_eq!(run["ended_by"], "game", "{status}");
    let stopped = json_of(&call(&tools, "godot_stop_project", json!({})).await);
    assert_eq!(stopped["recently_ended"][0]["exit_code"], 7, "{stopped}");
    assert_eq!(stopped["stopped"]["runs"], json!([]), "{stopped}");
}

#[cfg(unix)]
#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn a_game_killed_during_a_script_names_the_signal_at_once() {
    let (project, tools, run_id) = start_live_scene(false).await;
    let path = write_test(
        &project,
        "killed.gd",
        "extends Node\nfunc run() -> void:\n\tawait get_tree().create_timer(0.3).timeout\n\tOS.kill(OS.get_process_id())\n\tawait get_tree().create_timer(600.0).timeout\n",
    );
    let started = std::time::Instant::now();
    let during = run_in_game(
        &tools,
        &run_id,
        json!({"script": path, "timeout_seconds": 120}),
    )
    .await;
    assert!(
        started.elapsed() < Duration::from_secs(30),
        "{:?}",
        started.elapsed()
    );
    assert_error(&during, "signal");
    assert_error(&during, &run_id);
    let status = json_of(&call(&tools, "godot_get_run_status", json!({})).await);
    let run = &status["process_runs"][0];
    assert!(run["signal"].is_number(), "{status}");
    assert_eq!(run["ended_by"], "game", "{status}");
}

/// The game's own timeout ends the function and leaves the run, and the answer says both.
#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn a_timeout_says_what_it_ended_and_that_the_game_lives() {
    let (project, tools, run_id) = start_live_scene(false).await;
    let forever = write_test(
        &project,
        "forever_frames.gd",
        "extends Node\nfunc run() -> void:\n\twhile true:\n\t\tawait get_tree().physics_frame\n",
    );
    let late = failed_answer(
        &run_in_game(
            &tools,
            &run_id,
            json!({"script": forever, "timeout_seconds": 1}),
        )
        .await,
    );
    assert_eq!(late["failure"], "timeout", "{late}");
    assert_eq!(late["timeout_ended"], "function", "{late}");
    assert_eq!(late["game_running"], true, "{late}");
    let status = json_of(&call(&tools, "godot_get_run_status", json!({})).await);
    assert_eq!(status["process_runs"][0]["running"], true, "{status}");

    // A function with no await cannot be ended: the server stops waiting and ends nothing.
    let spin = write_test(
        &project,
        "spin.gd",
        "extends Node\nfunc run() -> void:\n\twhile true:\n\t\tpass\n",
    );
    let stuck = run_in_game(
        &tools,
        &run_id,
        json!({"script": spin, "timeout_seconds": 1}),
    )
    .await;
    assert_error(&stuck, "did not answer in time");
    assert_error(&stuck, "still alive");
    assert_error(&stuck, "did not stop it");
    assert_error(&stuck, &run_id);
    let status = json_of(&call(&tools, "godot_get_run_status", json!({})).await);
    assert_eq!(status["process_runs"][0]["running"], true, "{status}");

    let stopped = json_of(&call(&tools, "godot_stop_project", json!({"run_id": run_id})).await);
    assert_eq!(stopped["ended_by"], "server", "{stopped}");
    let again = json_of(&call(&tools, "godot_stop_project", json!({})).await);
    assert_eq!(again["recently_ended"][0]["ended_by"], "server", "{again}");
}

#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn a_run_that_ends_while_another_starts_leaves_it_the_runtime() {
    let _ = IGNORE;
    let project = project();
    // Two managers stand for two MCP servers (two agent sessions) running the same project.
    let first_server = manager(project.path());
    let second_server = manager(project.path());
    let start = |tools: Arc<ToolManager>| {
        let path = project.path().to_path_buf();
        async move {
            call(
                &tools,
                "godot_run_project",
                json!({
                    "project_path": path,
                    "scene": "res://scenes/main.tscn",
                    "mode": "process",
                    "headless": true
                }),
            )
            .await
        }
    };
    let first = start(Arc::clone(&first_server)).await;
    assert!(!first.is_error, "{}", text_of(&first));
    let first_id = run_id_of(&first);
    let tree = call(
        &first_server,
        "godot_game_tree",
        json!({"run_id": first_id}),
    )
    .await;
    assert!(!tree.is_error, "{}", text_of(&tree));

    // The second game is still starting, and reading override.cfg, when the first run ends.
    let second = start(Arc::clone(&second_server)).await;
    assert!(!second.is_error, "{}", text_of(&second));
    let second_id = run_id_of(&second);
    let stopped = call(
        &first_server,
        "godot_stop_project",
        json!({"run_id": first_id}),
    )
    .await;
    assert!(!stopped.is_error, "{}", text_of(&stopped));
    assert!(
        std::fs::read_to_string(project.path().join("override.cfg"))
            .is_ok_and(|text| text.contains("GodotMcpRuntime")),
        "the first run's end removed the runtime autoload the second run still needs"
    );

    let tree = call(
        &second_server,
        "godot_game_tree",
        json!({"run_id": second_id}),
    )
    .await;
    let tree_text = text_of(&tree);
    let _ = call(
        &second_server,
        "godot_stop_project",
        json!({"run_id": second_id}),
    )
    .await;
    assert!(!tree.is_error, "{tree_text}");
    assert!(
        !project.path().join("override.cfg").exists(),
        "override.cfg stayed after the last run ended"
    );
}
