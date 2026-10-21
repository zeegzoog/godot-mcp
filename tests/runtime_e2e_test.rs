//! Behavior tests for a real headless Godot process: live inspection, injected input,
//! logs, and the remote debugger. Ignored unless `GODOT_PATH` points at Godot 4.5+.
//!
//! ```bash
//! GODOT_PATH=/path/to/godot cargo test --test runtime_e2e_test -- --ignored --nocapture
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
signal pinged(value)

func _ready() -> void:
    push_error("mcp-probe-error")

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

func poke(value: int) -> int:
    pinged.emit(value)
    return value + 1
"#,
    )
    .unwrap();
    std::fs::write(
        root.join("scenes/main.tscn"),
        "[gd_scene load_steps=2 format=3]\n\n[ext_resource type=\"Script\" path=\"res://scripts/probe.gd\" id=\"1\"]\n\n[node name=\"Probe\" type=\"Node2D\"]\nscript = ExtResource(\"1\")\n\n[node name=\"Mark\" type=\"Node2D\" parent=\".\"]\nposition = Vector2(40, 70)\n\n[node name=\"Pad\" type=\"Button\" parent=\".\"]\noffset_left = 100.0\noffset_top = 20.0\noffset_right = 140.0\noffset_bottom = 60.0\n",
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
        text_of(&too_many).contains("frames must be from 1 to 8"),
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
    let resumed = call(
        &tools,
        "godot_debug_control",
        json!({"run_id": run_id, "action": "resume"}),
    )
    .await;
    assert!(!resumed.is_error, "{}", text_of(&resumed));

    let armed = call(
        &tools,
        "godot_debug_breakpoints",
        json!({"run_id": run_id, "action": "add", "breakpoint": "res://scripts/probe.gd:20"}),
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
