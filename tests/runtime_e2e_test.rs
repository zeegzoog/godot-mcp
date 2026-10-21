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
