//! A real Godot process plays `godot_game_scenario`.
//!
//! ```bash
//! GODOT_PATH=/path/to/godot cargo test --test scenario_e2e_test -- --ignored --test-threads=1 --nocapture
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
        .expect("set GODOT_PATH to run the scenario end-to-end tests")
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

fn number(value: &Value) -> f64 {
    value
        .as_f64()
        .or_else(|| value.as_i64().map(|number| number as f64))
        .unwrap_or(f64::NAN)
}

fn manager(project: &Path) -> Arc<ToolManager> {
    Arc::new(
        ToolManager::new(BridgeClient::new(9_341), CliRunner::new(godot_path()))
            .with_working_dir(project.to_path_buf()),
    )
}

async fn call(tools: &Arc<ToolManager>, name: &str, args: Value) -> ToolResult {
    Arc::clone(tools).call_tool(name, args).await
}

fn run_id_of(started: &ToolResult) -> String {
    text_of(started)
        .split("run_id: ")
        .nth(1)
        .and_then(|rest| rest.split(|c: char| !c.is_ascii_alphanumeric()).next())
        .expect("run id")
        .to_string()
}

fn position(report: &Value, step: usize) -> [f64; 3] {
    let point = &report["steps"][step]["snapshot"]["bodies"][0]["position"];
    [number(&point[0]), number(&point[1]), number(&point[2])]
}

fn on_floor(report: &Value, step: usize) -> bool {
    report["steps"][step]["snapshot"]["bodies"][0]["is_on_floor"]
        .as_bool()
        .unwrap_or(false)
}

fn property(report: &Value, step: usize, name: &str) -> f64 {
    number(&report["steps"][step]["snapshot"]["nodes"][0]["properties"][name])
}

fn probe_project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("project");
    let root = dir.path();
    std::fs::create_dir_all(root.join("scripts")).unwrap();
    std::fs::create_dir_all(root.join("scenes")).unwrap();
    std::fs::write(
        root.join("project.godot"),
        "config_version=5\n\n[application]\n\nconfig/name=\"ScenarioProbe\"\nrun/main_scene=\"res://scenes/main.tscn\"\nconfig/features=PackedStringArray(\"4.5\")\n",
    )
    .unwrap();
    std::fs::write(
        root.join("scripts/world.gd"),
        r#"extends Node3D

func _ready() -> void:
	push_error("before-scenario")
	for action_name in ["move_left", "move_right", "move_forward", "move_backward", "jump"]:
		if not InputMap.has_action(action_name):
			InputMap.add_action(action_name)
	var floor_body := StaticBody3D.new()
	floor_body.position = Vector3(0, -0.5, 0)
	var floor_shape := CollisionShape3D.new()
	var box := BoxShape3D.new()
	box.size = Vector3(40, 1, 40)
	floor_shape.shape = box
	floor_body.add_child(floor_shape)
	add_child(floor_body)
	var body := CharacterBody3D.new()
	body.name = "Player"
	body.position = Vector3(0, 2, 0)
	body.set_script(load("res://scripts/player.gd"))
	var shape := CollisionShape3D.new()
	var capsule := CapsuleShape3D.new()
	capsule.radius = 0.4
	capsule.height = 1.2
	shape.shape = capsule
	body.add_child(shape)
	add_child(body)
"#,
    )
    .unwrap();
    std::fs::write(
        root.join("scripts/player.gd"),
        r#"extends CharacterBody3D
var jump_count := 0
var touch_presses := 0
var report_error := false
var delay_ms := 0
var trace_moves := false

func _input(event: InputEvent) -> void:
	if event is InputEventScreenTouch and event.pressed:
		touch_presses += 1

func _physics_process(delta: float) -> void:
	if delay_ms > 0:
		OS.delay_msec(delay_ms)
	if trace_moves and Input.is_action_pressed("move_right"):
		trace_moves = false
		push_warning("scenario-input-started")
	if report_error:
		push_error("during-scenario")
	var direction := Input.get_vector("move_left", "move_right", "move_forward", "move_backward")
	velocity.x = direction.x * 4.0
	velocity.z = direction.y * 4.0
	if not is_on_floor():
		velocity.y -= 20.0 * delta
	elif Input.is_action_just_pressed("jump"):
		velocity.y = 8.0
		jump_count += 1
	else:
		velocity.y = 0.0
	move_and_slide()
"#,
    )
    .unwrap();
    std::fs::write(
        root.join("scenes/main.tscn"),
        "[gd_scene load_steps=2 format=3]\n\n[ext_resource type=\"Script\" path=\"res://scripts/world.gd\" id=\"1\"]\n\n[node name=\"World\" type=\"Node3D\"]\nscript = ExtResource(\"1\")\n",
    )
    .unwrap();
    dir
}

#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn a_scenario_moves_jumps_pauses_and_releases_input() {
    let _ = IGNORE;
    let project = probe_project();
    let tools = manager(project.path());
    let started = call(
        &tools,
        "godot_run_project",
        json!({
            "project_path": project.path(),
            "mode": "process",
            "held": true,
            "headless": true
        }),
    )
    .await;
    assert!(!started.is_error, "{}", text_of(&started));
    let run_id = run_id_of(&started);

    let armed = call(
        &tools,
        "godot_game_set",
        json!({"run_id": run_id, "path": "Player", "properties": {"report_error": true}}),
    )
    .await;
    assert!(!armed.is_error, "{}", text_of(&armed));

    let moved = json_of(
        &call(
            &tools,
            "godot_game_scenario",
            json!({
                "run_id": run_id,
                "bodies": ["Player"],
                "nodes": [{"path": "Player", "properties": ["jump_count", "touch_presses"]}],
                "steps": [
                    {"frames": 20},
                    {"frames": 20, "actions": [{"action": "move_right", "phase": "press", "strength": 1}]}
                ]
            }),
        )
        .await,
    );
    assert_eq!(moved["ok"], true, "{moved}");
    assert_eq!(moved["deterministic"], true, "{moved}");
    assert!(on_floor(&moved, 1), "{moved}");
    let start = position(&moved, 0);
    let later = position(&moved, 1);
    assert!(
        (later[0] - start[0]).abs() > 0.2,
        "move_right did not change position: {start:?} -> {later:?}\n{moved}"
    );
    let during = moved["output"]["errors"]
        .as_array()
        .expect("errors")
        .iter()
        .any(|line| line.to_string().contains("during-scenario"));
    let before = moved["output"]["errors"]
        .as_array()
        .unwrap()
        .iter()
        .any(|line| line.to_string().contains("before-scenario"));
    assert!(during, "this run's error is missing: {moved}");
    assert!(!before, "an earlier error was included: {moved}");

    let jumped = json_of(
        &call(
            &tools,
            "godot_game_scenario",
            json!({
                "run_id": run_id,
                "bodies": ["Player"],
                "nodes": [{"path": "Player", "properties": ["jump_count", "touch_presses"]}],
                "steps": [
                    {"frames": 1, "actions": [{"action": "move_right", "phase": "release"}]},
                    {"frames": 12, "actions": [{"action": "jump", "phase": "tap"}]},
                    {"frames": 8, "actions": [{"action": "jump", "phase": "tap"}]},
                    {"frames": 60}
                ]
            }),
        )
        .await,
    );
    assert_eq!(jumped["ok"], true, "{jumped}");
    assert!(
        position(&jumped, 1)[1] > position(&jumped, 0)[1] + 0.2,
        "jump did not rise: {jumped}"
    );
    assert!(
        !on_floor(&jumped, 1),
        "still on the floor after the jump: {jumped}"
    );
    assert_eq!(property(&jumped, 1, "jump_count"), 1.0, "{jumped}");
    assert_eq!(
        property(&jumped, 2, "jump_count"),
        1.0,
        "a second jump in the air counted: {jumped}"
    );
    assert!(on_floor(&jumped, 3), "did not land: {jumped}");

    let held_still = json_of(
        &call(
            &tools,
            "godot_game_scenario",
            json!({
                "run_id": run_id,
                "bodies": ["Player"],
                "nodes": [{"path": "Player", "properties": ["jump_count", "touch_presses"]}],
                "steps": [
                    {"frames": 5, "pause": true},
                    {"frames": 20, "actions": [{"action": "move_right", "phase": "press", "strength": 1}]}
                ]
            }),
        )
        .await,
    );
    assert_eq!(held_still["ok"], true, "{held_still}");
    assert_eq!(held_still["paused"], true, "{held_still}");
    let before_move = position(&held_still, 0);
    let after_move = position(&held_still, 1);
    assert!(
        (after_move[0] - before_move[0]).abs() < 0.01
            && (after_move[2] - before_move[2]).abs() < 0.01,
        "pause did not hold the body: {before_move:?} -> {after_move:?}"
    );

    let touches = json_of(
        &call(
            &tools,
            "godot_game_scenario",
            json!({
                "run_id": run_id,
                "bodies": ["Player"],
                "nodes": [{"path": "Player", "properties": ["jump_count", "touch_presses"]}],
                "steps": [
                    {"frames": 1, "pause": false, "pointers": [
                        {"pointer": "touch", "index": 0, "phase": "press", "x": 8, "y": 8},
                        {"pointer": "touch", "index": 1, "phase": "press", "x": 20, "y": 20}
                    ]},
                    {"frames": 1, "pointers": [
                        {"pointer": "touch", "index": 0, "phase": "move", "x": 12, "y": 10}
                    ]}
                ]
            }),
        )
        .await,
    );
    assert_eq!(touches["ok"], true, "{touches}");
    assert!(
        property(&touches, 1, "touch_presses") >= 2.0,
        "two touches were not delivered together: {touches}"
    );
    let held = &touches["released"]["held"];
    assert_eq!(number(&held["actions"]), 0.0, "{touches}");
    assert_eq!(number(&held["touches"]), 0.0, "{touches}");

    let failed = call(
        &tools,
        "godot_game_scenario",
        json!({
            "run_id": run_id,
            "steps": [
                {"frames": 1, "actions": [{"action": "move_right", "phase": "press"}]},
                {"frames": 1, "actions": [{"action": "not_an_action"}]}
            ]
        }),
    )
    .await;
    assert!(failed.is_error, "{}", text_of(&failed));
    let failed_body = json_of(&failed);
    assert_eq!(
        number(&failed_body["released"]["held"]["actions"]),
        0.0,
        "{failed_body}"
    );
    assert_eq!(
        number(&failed_body["released"]["held"]["touches"]),
        0.0,
        "{failed_body}"
    );

    let stopped = call(&tools, "godot_stop_project", json!({"run_id": run_id})).await;
    assert!(!stopped.is_error, "{}", text_of(&stopped));
}

#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn frog_world_moves_jumps_and_uses_the_touch_controls() {
    let _ = IGNORE;
    let Some(project) = std::env::var_os("FROG_WORLD_PROJECT").map(PathBuf::from) else {
        eprintln!("FROG_WORLD_PROJECT is not set; skipping the optional game integration");
        return;
    };
    if !project.join("project.godot").is_file() {
        eprintln!("frog-world is not at {project:?}; skipping");
        return;
    }
    let tools = manager(&project);
    let started = call(
        &tools,
        "godot_run_project",
        json!({
            "project_path": project,
            "scene": "res://scenes/main/main.tscn",
            "mode": "process",
            "held": true,
            "headless": false,
            "user_args": [format!("--save-profile=scenario_{}", uuid::Uuid::new_v4().simple())]
        }),
    )
    .await;
    assert!(!started.is_error, "{}", text_of(&started));
    let run_id = run_id_of(&started);
    let begin = call(
        &tools,
        "godot_game_call",
        json!({"run_id": run_id, "path": ".", "method": "start_game", "permit": ["start_game"]}),
    )
    .await;
    assert!(!begin.is_error, "{}", text_of(&begin));

    let settle = json_of(&scenario(&tools, &run_id, json!({"steps": [{"frames": 30}]})).await);
    assert_eq!(settle["ok"], true, "{settle}");
    assert_eq!(settle["deterministic"], true, "{settle}");
    assert!(on_floor(&settle, 0), "the frog did not land: {settle}");
    let origin = position(&settle, 0);

    let moved = json_of(
        &scenario(
            &tools,
            &run_id,
            json!({"steps": [
                {"frames": 40, "actions": [{"action": "move_right", "phase": "press", "strength": 1}]},
                {"frames": 5, "actions": [{"action": "move_right", "phase": "release"}]}
            ]}),
        )
        .await,
    );
    assert_eq!(moved["ok"], true, "{moved}");
    let walked = position(&moved, 0);
    let distance = ((walked[0] - origin[0]).powi(2) + (walked[2] - origin[2]).powi(2)).sqrt();
    assert!(
        distance > 0.3,
        "move_right left the frog at {origin:?} -> {walked:?}"
    );

    let jumped = json_of(
        &scenario(
            &tools,
            &run_id,
            json!({"steps": [
                {"frames": 15, "actions": [{"action": "jump", "phase": "tap"}]},
                {"frames": 10, "actions": [{"action": "jump", "phase": "tap"}]},
                {"frames": 90}
            ]}),
        )
        .await,
    );
    assert_eq!(jumped["ok"], true, "{jumped}");
    assert!(
        position(&jumped, 0)[1] > walked[1] + 0.15,
        "jump did not rise: {jumped}"
    );
    assert_eq!(property(&jumped, 0, "jump_count"), 1.0, "{jumped}");
    assert_eq!(
        property(&jumped, 1, "jump_count"),
        1.0,
        "second jump counted: {jumped}"
    );
    assert!(on_floor(&jumped, 2), "did not land: {jumped}");
    assert!(property(&jumped, 2, "landing_count") >= 1.0, "{jumped}");

    let paused = json_of(
        &scenario(
            &tools,
            &run_id,
            json!({"steps": [
                {"frames": 5, "pause": true},
                {"frames": 30, "actions": [{"action": "move_right", "phase": "press", "strength": 1}]}
            ]}),
        )
        .await,
    );
    assert_eq!(paused["ok"], true, "{paused}");
    let still = position(&paused, 0);
    let held = position(&paused, 1);
    assert!(
        (held[0] - still[0]).abs() < 0.01 && (held[2] - still[2]).abs() < 0.01,
        "pause let the frog move: {still:?} -> {held:?}"
    );

    let router = call(
        &tools,
        "godot_game_set",
        json!({"run_id": run_id, "path": "InputRouter", "properties": {"method": "touch"}}),
    )
    .await;
    assert!(!router.is_error, "{}", text_of(&router));
    let hud = hud_path(&tools, &run_id).await;
    let shown_touch = call(
        &tools,
        "godot_game_call",
        json!({"run_id": run_id, "path": hud, "method": "refresh_touch", "permit": ["refresh_touch"]}),
    )
    .await;
    assert!(!shown_touch.is_error, "{}", text_of(&shown_touch));
    let shown = call(
        &tools,
        "godot_game_physics",
        json!({"run_id": run_id, "action": "step", "ticks": 2}),
    )
    .await;
    assert!(!shown.is_error, "{}", text_of(&shown));
    let stick = center(&tools, &run_id, "VirtualJoystick").await;
    let button = center(&tools, &run_id, "TouchAction").await;
    let touched = json_of(
        &scenario(
            &tools,
            &run_id,
            json!({"steps": [
                {"frames": 1, "pause": false},
                {"frames": 20, "pointers": [
                    {"pointer": "touch", "index": 0, "phase": "press", "x": stick.0, "y": stick.1},
                    {"pointer": "touch", "index": 0, "phase": "move", "x": stick.0 + 70.0, "y": stick.1},
                    {"pointer": "touch", "index": 1, "phase": "press", "x": button.0, "y": button.1}
                ]}
            ]}),
        )
        .await,
    );
    assert_eq!(touched["ok"], true, "{touched}");
    let after_touch = position(&touched, 1);
    let touch_distance =
        ((after_touch[0] - held[0]).powi(2) + (after_touch[2] - held[2]).powi(2)).sqrt();
    assert!(
        touch_distance > 0.15,
        "the stick did not move the frog: {held:?} -> {after_touch:?}\n{touched}"
    );
    assert!(
        property(&touched, 1, "jump_count") >= 2.0,
        "the jump button did not jump: {touched}"
    );
    assert_eq!(
        number(&touched["released"]["held"]["touches"]),
        0.0,
        "{touched}"
    );
    assert_eq!(
        number(&touched["released"]["held"]["actions"]),
        0.0,
        "{touched}"
    );

    let stopped = call(&tools, "godot_stop_project", json!({"run_id": run_id})).await;
    assert!(!stopped.is_error, "{}", text_of(&stopped));
}

async fn scenario(tools: &Arc<ToolManager>, run_id: &str, mut args: Value) -> ToolResult {
    args["run_id"] = json!(run_id);
    args["bodies"] = json!(["OldPond/Player"]);
    args["nodes"] = json!([{
        "path": "OldPond/Player",
        "properties": ["jump_count", "landing_count"]
    }]);
    call(tools, "godot_game_scenario", args).await
}

async fn hud_path(tools: &Arc<ToolManager>, run_id: &str) -> String {
    let tree = json_of(
        &call(
            tools,
            "godot_game_tree",
            json!({"run_id": run_id, "name": "GameHUD", "depth": 16, "max_nodes": 20}),
        )
        .await,
    );
    let path = tree["nodes"]
        .as_array()
        .and_then(|nodes| nodes.first())
        .and_then(|node| node.get("path"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    assert!(!path.is_empty(), "GameHUD was not in the tree: {tree}");
    path
}

async fn center(tools: &Arc<ToolManager>, run_id: &str, name: &str) -> (f64, f64) {
    let tree = json_of(
        &call(
            tools,
            "godot_game_tree",
            json!({"run_id": run_id, "root": "/root", "name": name, "depth": 16, "max_nodes": 400}),
        )
        .await,
    );
    let path = tree["nodes"]
        .as_array()
        .and_then(|nodes| nodes.first())
        .and_then(|node| node.get("path"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    assert!(!path.is_empty(), "no node named {name}: {tree}");
    let read = json_of(
        &call(
            tools,
            "godot_game_get",
            json!({"run_id": run_id, "path": path, "properties": ["global_position", "size"]}),
        )
        .await,
    );
    let position = &read["properties"]["global_position"];
    let size = &read["properties"]["size"];
    (
        number(&position[0]) + number(&size[0]) / 2.0,
        number(&position[1]) + number(&size[1]) / 2.0,
    )
}

#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.8 executable"]
async fn a_default_scenario_stays_on_its_original_run() {
    let first_project = probe_project();
    let second_project = probe_project();
    let tools = manager(first_project.path());
    let launch = |project: &Path| {
        json!({
            "project_path": project, "mode": "process", "held": true, "headless": true
        })
    };
    let first = call(&tools, "godot_run_project", launch(first_project.path())).await;
    assert!(!first.is_error, "{}", text_of(&first));
    let first_id = run_id_of(&first);
    let prepared = call(
        &tools,
        "godot_game_set",
        json!({
            "run_id": first_id, "path": "Player",
            "properties": {"delay_ms": 20, "trace_moves": true}
        }),
    )
    .await;
    assert!(!prepared.is_error, "{}", text_of(&prepared));
    let playing = {
        let tools = Arc::clone(&tools);
        tokio::spawn(async move {
            call(
                &tools,
                "godot_game_scenario",
                json!({
                    "bodies": ["Player"],
                    "steps": [
                        {"frames": 120, "actions": [{"action": "move_right", "phase": "press"}]},
                        {"frames": 5}
                    ]
                }),
            )
            .await
        })
    };
    let mut input_started = false;
    for _ in 0..100 {
        let logs = call(&tools, "godot_run_output", json!({"run_id": first_id})).await;
        if text_of(&logs).contains("scenario-input-started") {
            input_started = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    let second = call(&tools, "godot_run_project", launch(second_project.path())).await;
    let overlapped = !playing.is_finished();
    let result = playing.await.expect("scenario task");
    let body = json_of(&result);
    let _ = call(&tools, "godot_stop_project", json!({})).await;
    assert!(input_started && overlapped, "the runs did not overlap");
    assert!(!second.is_error, "{}", text_of(&second));
    assert!(!result.is_error, "{body}");
    assert!(
        position(&body, 0)[0] > 5.0,
        "snapshot switched runs: {body}"
    );
    assert!(
        position(&body, 1)[0] > position(&body, 0)[0],
        "steps switched runs: {body}"
    );
}

#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.8 executable"]
async fn zero_tick_scenarios_preserve_initial_runtime_status() {
    let project = probe_project();
    let tools = manager(project.path());
    let started = call(
        &tools,
        "godot_run_project",
        json!({"project_path":project.path(),"mode":"process","held":true,"headless":true}),
    )
    .await;
    assert!(!started.is_error, "{}", text_of(&started));
    let run_id = run_id_of(&started);
    let paused = call(
        &tools,
        "godot_game_scenario",
        json!({"run_id":run_id,"steps":[{"frames":0,"pause":true}]}),
    )
    .await;
    let observed = call(
        &tools,
        "godot_game_scenario",
        json!({"run_id":run_id,"steps":[{"frames":0}]}),
    )
    .await;
    let _ = call(&tools, "godot_stop_project", json!({"run_id":run_id})).await;
    for result in [paused, observed] {
        let body = json_of(&result);
        assert!(!result.is_error, "{body}");
        assert_eq!(body["paused"], true, "{body}");
        assert_eq!(body["deterministic"], true, "{body}");
        assert_eq!(body["ticks_run"], 0, "{body}");
    }
}
