//! Behavior tests for a real headless Godot process: live inspection, injected input,
//! logs, and the remote debugger. Ignored unless `GODOT_PATH` points at Godot 4.5+.
//!
//! ```bash
//! GODOT_PATH=/path/to/godot cargo test --test runtime_e2e_test -- --ignored --test-threads=1 --nocapture
//! ```

use godot_mcp::bridge::BridgeClient;
use godot_mcp::cli_runner::CliRunner;
use godot_mcp::protocol::ToolResult;
use godot_mcp::tools::ToolManager;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::Arc;

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
var bag := {"count": 2}
var quiet = null
var swatch: Resource
signal pinged(value)

func _ready() -> void:
    firefly_law = Law.new()
    river = River.new()
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
    trace.append([physics_ticks, Input.is_key_pressed(KEY_SPACE), Input.is_action_just_pressed("ui_accept")])

func _draw() -> void:
    draw_rect(Rect2(float(process_frames % 20) * 40.0, 8, 24, 24), Color(1, 0, 0))

func set_tick_rate(rate: int) -> void:
    Engine.physics_ticks_per_second = rate

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
