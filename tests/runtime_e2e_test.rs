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

fn godot_path() -> PathBuf {
    let _ = dotenvy::dotenv();
    std::env::var("GODOT_PATH")
        .ok()
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .filter(|path| path.exists())
        .expect("set GODOT_PATH to run the runtime end-to-end tests")
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
var touch_presses := 0
var joy_button := ""
var joy_axis := 0.0
var hits := 0
signal pinged(value)

func _ready() -> void:
    push_error("mcp-probe-error")

func _process(_delta: float) -> void:
    hits += 1

func _input(event: InputEvent) -> void:
    if event is InputEventKey and event.pressed and not event.echo:
        last_key = OS.get_keycode_string(event.keycode)
    elif event is InputEventMouseButton and event.pressed:
        mouse_presses += 1
    elif event is InputEventScreenTouch and event.pressed:
        touch_presses += 1
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
        "[gd_scene load_steps=2 format=3]\n\n[ext_resource type=\"Script\" path=\"res://scripts/probe.gd\" id=\"1\"]\n\n[node name=\"Probe\" type=\"Node2D\"]\nscript = ExtResource(\"1\")\n",
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
    let started_text = text_of(&started);
    let run_id = started_text
        .split("run_id: ")
        .nth(1)
        .and_then(|rest| rest.split(|c: char| !c.is_ascii_alphanumeric()).next())
        .expect("run id")
        .to_string();

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
        json!({"run_id": run_id, "action": "add", "breakpoint": "res://scripts/probe.gd:14"}),
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
}
