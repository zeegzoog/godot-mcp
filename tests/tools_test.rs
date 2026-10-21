//! Host-side tools that never need a running editor: script creation, run/launch validation.

mod common;

use common::{
    fake_godot, free_port, is_tool_error, project_dir, recording_godot, tool_text, wait_for_file,
};
use godot_mcp::bridge::BridgeClient;
use godot_mcp::cli_runner::CliRunner;
use godot_mcp::protocol::ToolResult;
use godot_mcp::tools::ToolManager;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// A manager whose working directory is `cwd`, with no editor and the given Godot binary.
fn manager_in(cwd: &Path, godot: PathBuf) -> Arc<ToolManager> {
    Arc::new(
        ToolManager::new(BridgeClient::new(free_port()), CliRunner::new(godot))
            .with_working_dir(cwd.to_path_buf()),
    )
}

/// Runs a tool from an empty working directory, so no project is found implicitly (the test
/// process itself runs in this repository, which has a project.godot of its own).
async fn call(tool: &str, args: Value) -> ToolResult {
    let cwd = tempfile::tempdir().expect("working dir");
    manager_in(
        cwd.path(),
        PathBuf::from("godot-binary-that-does-not-exist"),
    )
    .call_tool(tool, args)
    .await
}

fn text(result: &ToolResult) -> String {
    tool_text(&json!({"result": serde_json::to_value(result).expect("serialize result")}))
}

#[tokio::test]
async fn creates_a_script_from_a_res_path_inside_the_project() {
    let project = project_dir();
    let result = call(
        "godot_create_script",
        json!({"file_path": "res://scripts/player.gd", "extends_class": "Node2D",
               "class_name": "Player", "project_path": project.path()}),
    )
    .await;

    assert!(!result.is_error, "{}", text(&result));
    let written =
        std::fs::read_to_string(project.path().join("scripts/player.gd")).expect("script exists");
    assert!(written.contains("extends Node2D"));
    assert!(written.contains("class_name Player"));
}

#[tokio::test]
async fn refuses_to_overwrite_an_existing_file_unless_asked() {
    let project = project_dir();
    let target = project.path().join("keep.gd");
    std::fs::write(&target, "precious").expect("seed file");

    let refused = call(
        "godot_create_script",
        json!({"file_path": "res://keep.gd", "project_path": project.path()}),
    )
    .await;
    assert!(refused.is_error);
    assert!(text(&refused).contains("overwrite"), "{}", text(&refused));
    assert_eq!(std::fs::read_to_string(&target).expect("read"), "precious");

    let allowed = call(
        "godot_create_script",
        json!({"file_path": "res://keep.gd", "project_path": project.path(), "overwrite": true}),
    )
    .await;
    assert!(!allowed.is_error, "{}", text(&allowed));
    assert!(
        std::fs::read_to_string(&target)
            .expect("read")
            .starts_with("extends Node")
    );
}

#[tokio::test]
async fn rejects_parent_directory_segments_but_not_double_dots_in_names() {
    let project = project_dir();

    let traversal = call(
        "godot_create_script",
        json!({"file_path": "res://a/../b.gd", "project_path": project.path()}),
    )
    .await;
    assert!(traversal.is_error);
    assert!(!project.path().join("b.gd").exists());

    let harmless = call(
        "godot_create_script",
        json!({"file_path": "res://my..file.gd", "project_path": project.path()}),
    )
    .await;
    assert!(!harmless.is_error, "{}", text(&harmless));
    assert!(project.path().join("my..file.gd").exists());
}

#[tokio::test]
async fn rejects_absolute_paths_outside_the_project() {
    let project = project_dir();
    let outside = tempfile::tempdir().expect("outside dir");
    let target = outside.path().join("escaped.gd");

    let result = call(
        "godot_create_script",
        json!({"file_path": target, "project_path": project.path()}),
    )
    .await;

    assert!(result.is_error);
    assert!(
        !target.exists(),
        "file must not be written outside the project"
    );
}

#[tokio::test]
async fn rejects_code_injection_through_identifiers() {
    let project = project_dir();

    let bad_class = call(
        "godot_create_script",
        json!({"file_path": "res://a.gd", "project_path": project.path(),
               "class_name": "Foo\nfunc _init() -> void:\n\tprint('x')"}),
    )
    .await;
    assert!(bad_class.is_error);
    assert!(!project.path().join("a.gd").exists());

    let bad_base = call(
        "godot_create_script",
        json!({"file_path": "res://b.gd", "project_path": project.path(),
               "extends_class": "Node\nvar injected := 1"}),
    )
    .await;
    assert!(bad_base.is_error);
    assert!(!project.path().join("b.gd").exists());
}

#[tokio::test]
async fn only_gd_files_can_be_created() {
    let project = project_dir();
    let result = call(
        "godot_create_script",
        json!({"file_path": "res://notes.txt", "project_path": project.path()}),
    )
    .await;
    assert!(result.is_error);
    assert!(!project.path().join("notes.txt").exists());
}

#[tokio::test]
async fn needs_a_project_when_the_editor_is_offline() {
    let result = call("godot_create_script", json!({"file_path": "res://a.gd"})).await;
    assert!(result.is_error);
    assert!(text(&result).contains("project_path"), "{}", text(&result));
}

#[tokio::test]
async fn rejects_a_project_path_without_project_godot() {
    let empty = tempfile::tempdir().expect("empty dir");
    let result = call(
        "godot_create_script",
        json!({"file_path": "res://a.gd", "project_path": empty.path()}),
    )
    .await;
    assert!(result.is_error);
    assert!(text(&result).contains("project.godot"), "{}", text(&result));
    assert!(!empty.path().join("a.gd").exists());
}

#[tokio::test]
async fn run_project_rejects_a_scene_that_looks_like_an_engine_option() {
    let project = project_dir();
    let result = call(
        "godot_run_project",
        json!({"project_path": project.path(), "scene": "--script"}),
    )
    .await;
    assert!(result.is_error);
    assert!(text(&result).contains("res://"), "{}", text(&result));
}

#[tokio::test]
async fn launch_editor_requires_a_godot_project_directory() {
    let empty = tempfile::tempdir().expect("empty dir");
    let result = call("godot_launch_editor", json!({"project_path": empty.path()})).await;
    assert!(result.is_error);
    assert!(text(&result).contains("project.godot"), "{}", text(&result));
}

#[tokio::test]
async fn validate_script_reports_a_missing_file_without_spawning_godot() {
    let project = project_dir();
    let result = call(
        "godot_validate_script",
        json!({"script_path": "res://nope.gd", "project_path": project.path()}),
    )
    .await;
    assert!(result.is_error);
    let message = text(&result);
    assert!(message.contains("not found"), "{message}");
    assert!(
        !message.contains("spawn") && !message.contains("syntax"),
        "{message}"
    );
}

#[tokio::test]
async fn create_script_finds_the_project_from_the_working_directory() {
    let project = project_dir();
    let cwd = project.path().join("scripts");
    std::fs::create_dir_all(&cwd).expect("mkdir");
    let result = manager_in(&cwd, PathBuf::from("godot-binary-that-does-not-exist"))
        .call_tool("godot_create_script", json!({"file_path": "res://a.gd"}))
        .await;

    assert!(!result.is_error, "{}", text(&result));
    assert!(project.path().join("a.gd").is_file());
}

#[tokio::test]
async fn an_explicit_project_path_wins_over_the_working_directory() {
    let here = project_dir();
    let there = project_dir();
    let result = manager_in(
        here.path(),
        PathBuf::from("godot-binary-that-does-not-exist"),
    )
    .call_tool(
        "godot_create_script",
        json!({"file_path": "res://a.gd", "project_path": there.path()}),
    )
    .await;

    assert!(!result.is_error, "{}", text(&result));
    assert!(there.path().join("a.gd").is_file());
    assert!(!here.path().join("a.gd").exists());
}

#[tokio::test]
async fn launch_editor_uses_the_project_in_a_child_of_the_working_directory() {
    let root = tempfile::tempdir().expect("root");
    let app = root.path().join("app");
    std::fs::create_dir_all(&app).expect("mkdir");
    std::fs::write(app.join("project.godot"), "config_version=5\n").expect("project.godot");
    let godot = fake_godot(root.path());

    let result = manager_in(root.path(), godot)
        .call_tool("godot_launch_editor", json!({}))
        .await;

    assert!(!result.is_error, "{}", text(&result));
    assert!(
        text(&result).contains(&app.display().to_string()),
        "{}",
        text(&result)
    );
}

#[tokio::test]
async fn launch_editor_without_any_project_asks_for_project_path() {
    let result = call("godot_launch_editor", json!({})).await;
    assert!(result.is_error);
    assert!(text(&result).contains("project_path"), "{}", text(&result));
}

#[tokio::test]
async fn run_project_without_an_editor_runs_the_working_directory_project() {
    let project = project_dir();
    let godot = fake_godot(project.path());

    let result = manager_in(project.path(), godot)
        .call_tool("godot_run_project", json!({}))
        .await;

    assert!(!result.is_error, "{}", text(&result));
    assert!(text(&result).contains("Spawned"), "{}", text(&result));
}

#[tokio::test]
async fn run_project_without_an_editor_or_a_project_asks_for_project_path() {
    let result = call("godot_run_project", json!({})).await;
    assert!(result.is_error);
    assert!(text(&result).contains("project_path"), "{}", text(&result));
}

#[test]
fn launch_editor_schema_no_longer_requires_project_path() {
    let tools = ToolManager::list_tools();
    let launch = tools
        .iter()
        .find(|tool| tool.name == "godot_launch_editor")
        .expect("launch tool");
    assert!(
        launch.input_schema.get("required").is_none(),
        "{}",
        launch.input_schema
    );
}

#[tokio::test]
async fn create_scene_without_an_editor_writes_a_scene_file() {
    let project = project_dir();
    let result = call(
        "godot_create_scene",
        json!({
            "scene_path": "res://scenes/main.tscn",
            "root_type": "Node2D",
            "root_name": "Probe",
            "project_path": project.path(),
        }),
    )
    .await;

    assert!(!result.is_error, "{}", text(&result));
    let written = std::fs::read_to_string(project.path().join("scenes/main.tscn")).expect("scene");
    assert!(written.contains("type=\"Node2D\""), "{written}");
    assert!(written.contains("name=\"Probe\""), "{written}");
    assert!(written.starts_with("[gd_scene"), "{written}");
}

#[tokio::test]
async fn create_resource_without_an_editor_never_replaces_a_file() {
    let project = project_dir();
    let args = |class: &str| {
        json!({
            "resource_path": "res://made.tres",
            "class_name": class,
            "project_path": project.path(),
        })
    };
    let first = call("godot_create_resource", args("Curve")).await;
    assert!(!first.is_error, "{}", text(&first));
    let written = std::fs::read_to_string(project.path().join("made.tres")).expect("resource");
    assert!(written.contains("type=\"Curve\""), "{written}");

    let second = call("godot_create_resource", args("Gradient")).await;
    assert!(second.is_error, "{}", text(&second));
    assert!(
        text(&second).contains("already exists"),
        "{}",
        text(&second)
    );
    assert_eq!(
        std::fs::read_to_string(project.path().join("made.tres")).expect("resource again"),
        written,
        "the second call replaced the file"
    );
}

#[tokio::test]
async fn project_setting_without_an_editor_cannot_add_lines_to_project_godot() {
    let project = project_dir();
    let project_file = project.path().join("project.godot");
    std::fs::write(
        &project_file,
        "config_version=5\n\n[application]\n\nconfig/name=\"Old\"\nrun/max_fps=30\nrun/low=true\n",
    )
    .expect("project.godot");
    let set = |key: &str, value: Value| {
        call(
            "godot_set_project_setting",
            json!({"key": key, "value": value, "project_path": project.path()}),
        )
    };

    // A value with quotes, a backslash and line breaks stays one value on one line.
    let sneaky = "x\"\n[autoload]\nEvil=\"*res://evil.gd\"\\\n";
    let stored = set("application/config/name", json!(sneaky)).await;
    assert!(!stored.is_error, "{}", text(&stored));
    let after = std::fs::read_to_string(&project_file).expect("project.godot");
    assert!(
        after.lines().all(|line| line.trim() != "[autoload]") && !after.contains("\nEvil="),
        "a value added lines to project.godot:\n{after}"
    );
    assert!(
        after.contains(r#"config/name="x\"\n[autoload]\nEvil=\"*res://evil.gd\"\\\n""#),
        "{after}"
    );

    // A key that is not a plain path of letters, digits and _ / . - is refused and changes nothing.
    for bad_key in [
        "application/x]\n[autoload",
        "application/a=b",
        "application/two words",
        "noslash",
        "/leading",
        "trailing/",
        "double//slash",
    ] {
        let refused = set(bad_key, json!("v")).await;
        assert!(refused.is_error, "key {bad_key:?}: {}", text(&refused));
    }

    // A value of another type than the setting has is refused too; the right type is stored.
    for (key, value) in [
        ("application/config/name", json!(5)),
        ("application/run/max_fps", json!("fast")),
        ("application/run/max_fps", json!(2.5)),
        ("application/run/low", json!(1)),
    ] {
        let refused = set(key, value.clone()).await;
        assert!(refused.is_error, "{key} = {value}: {}", text(&refused));
        assert!(text(&refused).contains("expects"), "{}", text(&refused));
    }
    let structure = set("application/config/name", json!({"a": 1})).await;
    assert!(structure.is_error, "{}", text(&structure));
    assert!(
        text(&structure).contains("string, number or boolean"),
        "{}",
        text(&structure)
    );
    let unchanged = std::fs::read_to_string(&project_file).expect("project.godot");
    assert_eq!(unchanged, after, "a refused call changed project.godot");

    let fps = set("application/run/max_fps", json!(60)).await;
    assert!(!fps.is_error, "{}", text(&fps));
    let low = set("application/run/low", json!(false)).await;
    assert!(!low.is_error, "{}", text(&low));
    let added = set("custom/new/ratio", json!(0.5)).await;
    assert!(!added.is_error, "{}", text(&added));
    let end = std::fs::read_to_string(&project_file).expect("project.godot");
    assert!(
        end.contains("run/max_fps=60\n") && end.contains("run/low=false\n"),
        "{end}"
    );
    assert!(
        end.contains("[custom]") && end.contains("new/ratio=0.5\n"),
        "{end}"
    );
}

#[tokio::test]
async fn game_call_requires_an_explicit_permit_before_any_game_is_running() {
    let refused = call(
        "godot_game_call",
        json!({"method": "queue_free", "path": "."}),
    )
    .await;
    assert!(refused.is_error, "{}", text(&refused));
    assert!(text(&refused).contains("permit"), "{}", text(&refused));

    let denied = call(
        "godot_game_call",
        json!({"method": "quit", "permit": ["quit"]}),
    )
    .await;
    assert!(denied.is_error, "{}", text(&denied));
    assert!(text(&denied).contains("not allowed"), "{}", text(&denied));
}

#[test]
fn tool_text_helper_reads_error_flag() {
    let result = ToolResult::error("boom", &["try again"]);
    let value = json!({"result": serde_json::to_value(&result).expect("serialize")});
    assert!(is_tool_error(&value));
}

#[tokio::test]
async fn editor_runtime_waits_for_hello_and_targets_the_ready_session() {
    use common::{MockBridge, Reply};
    use std::sync::atomic::{AtomicUsize, Ordering};
    let polls = Arc::new(AtomicUsize::new(0));
    let observed = polls.clone();
    let bridge = MockBridge::start(move |command| {
        match command["type"].as_str().unwrap_or_default() {
            "get_run_status" => Reply::Ok(json!({"running": true})),
            "game_status" => {
                let poll = observed.fetch_add(1, Ordering::SeqCst);
                if poll == 0 { Reply::Ok(json!({"sessions": []})) }
                else { Reply::Ok(json!({"sessions": [{"session": 7, "hello": if poll >= 2 {"{}"} else {""}}]})) }
            }
            "game_send" => {
                if observed.load(Ordering::SeqCst) < 3 {
                    return Reply::Err("request was sent before the runtime hello".into());
                }
                if command["params"]["session"] != 7 {
                    return Reply::Err("request was not pinned to the ready session".into());
                }
                let request: Value = serde_json::from_str(command["params"]["data"][0].as_str().expect("request")).expect("JSON");
                Reply::OkWithEvent(json!({"sent": true}), json!({
                    "type": "event", "event": "game.message", "data": {
                        "session": 7, "message": "mcp:reply",
                        "data": [json!({"id": request["id"], "result": {"count": 1, "nodes": []}}).to_string()]
                    }
                }))
            }
            _ => Reply::Err("unexpected command".into())
        }
    }).await;
    let manager = ToolManager::new(
        BridgeClient::new(bridge.port),
        CliRunner::new(PathBuf::from("unused-godot")),
    );
    let result = Arc::new(manager)
        .call_tool("godot_game_tree", json!({"run_id": "editor"}))
        .await;
    assert!(!result.is_error, "{}", text(&result));
    assert!(text(&result).contains("count"), "{}", text(&result));
    assert!(polls.load(Ordering::SeqCst) >= 3);
}

#[tokio::test]
async fn editor_runtime_without_a_running_game_does_not_send_input() {
    use common::{MockBridge, Reply};
    let bridge = MockBridge::start(
        |command| match command["type"].as_str().unwrap_or_default() {
            "game_status" => Reply::Ok(json!({"sessions": []})),
            "get_run_status" => Reply::Ok(json!({"running": false})),
            _ => Reply::Err("should not send runtime input".into()),
        },
    )
    .await;
    let manager = ToolManager::new(
        BridgeClient::new(bridge.port),
        CliRunner::new(PathBuf::from("unused-godot")),
    );
    let result = Arc::new(manager)
        .call_tool(
            "godot_game_input",
            json!({"run_id":"editor","steps":[{"type":"key","key":"A"}]}),
        )
        .await;
    assert!(result.is_error);
    assert!(
        text(&result).contains("No game started from the editor is running"),
        "{}",
        text(&result)
    );
    assert!(
        !bridge
            .received()
            .iter()
            .any(|command| command["type"] == "game_send")
    );
}

#[tokio::test]
async fn forced_process_mode_does_not_claim_the_editor_is_offline() {
    let project = project_dir();
    let result = manager_in(project.path(), fake_godot(project.path()))
        .call_tool("godot_run_project", json!({"mode":"process"}))
        .await;
    assert!(!result.is_error, "{}", text(&result));
    assert!(
        !text(&result).contains("No editor bridge connected"),
        "{}",
        text(&result)
    );
}

#[tokio::test]
async fn a_held_run_needs_the_process_mode() {
    let project = project_dir();
    let (godot, _record) = recording_godot(project.path());
    let tools = manager_in(project.path(), godot);

    for args in [json!({"held": true}), json!({"held": true, "mode": "auto"})] {
        let result = Arc::clone(&tools)
            .call_tool("godot_run_project", args)
            .await;
        assert!(result.is_error, "{}", text(&result));
        assert!(text(&result).contains("process"), "{}", text(&result));
    }
}

#[tokio::test]
async fn a_held_run_starts_the_engine_at_the_projects_tick_rate() {
    let project = project_dir();
    let (godot, record) = recording_godot(project.path());

    // The manager stops the games it started when it is dropped, so it lives until the file is read.
    let tools = manager_in(project.path(), godot);
    let result = Arc::clone(&tools)
        .call_tool(
            "godot_run_project",
            json!({"held": true, "mode": "process"}),
        )
        .await;
    assert!(!result.is_error, "{}", text(&result));

    let arguments = wait_for_file(&record).await;
    assert!(arguments.contains("--fixed-fps 60"), "{arguments}");
    // The game's own arguments come after the engine's, behind a bare `--`.
    let (engine, game) = arguments.split_once(" -- ").unwrap_or((&arguments, ""));
    assert!(engine.contains("--remote-debug"), "{arguments}");
    assert!(game.contains("--godot-mcp-hold"), "{arguments}");
    assert!(game.contains("--godot-mcp-fixed-fps=60"), "{arguments}");
}

#[tokio::test]
async fn a_held_run_follows_a_project_that_sets_its_own_tick_rate() {
    let project = project_dir();
    std::fs::write(
        project.path().join("project.godot"),
        "config_version=5\n\n[physics]\n\ncommon/physics_ticks_per_second=30\n",
    )
    .unwrap();
    let (godot, record) = recording_godot(project.path());

    // The manager stops the games it started when it is dropped, so it lives until the file is read.
    let tools = manager_in(project.path(), godot);
    let result = Arc::clone(&tools)
        .call_tool(
            "godot_run_project",
            json!({"held": true, "mode": "process"}),
        )
        .await;
    assert!(!result.is_error, "{}", text(&result));

    let arguments = wait_for_file(&record).await;
    assert!(arguments.contains("--fixed-fps 30"), "{arguments}");
    assert!(
        arguments.contains("--godot-mcp-fixed-fps=30"),
        "{arguments}"
    );
}

#[tokio::test]
async fn a_run_that_is_not_held_gets_no_fixed_rate_and_no_game_arguments() {
    let project = project_dir();
    let (godot, record) = recording_godot(project.path());

    // The manager stops the games it started when it is dropped, so it lives until the file is read.
    let tools = manager_in(project.path(), godot);
    let result = Arc::clone(&tools)
        .call_tool("godot_run_project", json!({"mode": "process"}))
        .await;
    assert!(!result.is_error, "{}", text(&result));

    let arguments = wait_for_file(&record).await;
    assert!(!arguments.contains("--fixed-fps"), "{arguments}");
    assert!(!arguments.contains("godot-mcp"), "{arguments}");
}

#[test]
fn the_physics_tool_offers_hold_conditions_and_the_input_tool_hold_ticks() {
    let tools = ToolManager::list_tools();
    let schema = |name: &str| {
        tools
            .iter()
            .find(|tool| tool.name == name)
            .unwrap_or_else(|| panic!("no tool {name}"))
            .input_schema
            .clone()
    };
    let physics = schema("godot_game_physics");
    for property in ["action", "ticks", "until", "max"] {
        assert!(physics["properties"][property].is_object(), "{physics}");
    }
    assert!(
        physics["properties"]["action"]["description"]
            .as_str()
            .unwrap_or("")
            .contains("hold"),
        "{physics}"
    );
    assert!(schema("godot_game_input")["properties"]["hold_ticks"].is_object());
    assert!(schema("godot_run_project")["properties"]["held"].is_object());
}

#[test]
fn the_descriptions_tell_an_agent_what_a_hold_does_and_what_it_refuses() {
    let tools = ToolManager::list_tools();
    let description = |name: &str| {
        tools
            .iter()
            .find(|tool| tool.name == name)
            .unwrap_or_else(|| panic!("no tool {name}"))
            .description
            .to_lowercase()
    };
    let physics = description("godot_game_physics");
    for word in [
        "hold",
        "until",
        "deterministic",
        "exact",
        "resume",
        "not_started_held",
    ] {
        assert!(
            physics.contains(word),
            "godot_game_physics does not say {word}: {physics}"
        );
    }
    let run = description("godot_run_project");
    assert!(run.contains("held"), "{run}");
    let input = description("godot_game_input");
    assert!(
        input.contains("hold_ticks") && input.contains("held"),
        "{input}"
    );
    for refused in [
        "godot_game_wait",
        "godot_game_observe",
        "godot_game_screenshot",
    ] {
        let text = description(refused);
        assert!(
            text.contains("held"),
            "{refused} does not say it is refused while held: {text}"
        );
    }
    assert!(description("godot_debug_control").contains("held"));
}

fn script_answer(result: &ToolResult) -> Value {
    assert!(!result.is_error, "{}", text(result));
    serde_json::from_str(&text(result)).expect("the answer is JSON")
}

fn write_script(project: &Path, name: &str, bytes: &[u8]) {
    std::fs::write(project.join(name), bytes).expect("write script");
}

#[tokio::test]
async fn read_script_without_an_editor_reads_the_file_and_labels_it() {
    let project = project_dir();
    write_script(project.path(), "other.gd", b"extends Node\n");
    write_script(project.path(), "crlf.gd", b"extends Node\r\n");
    write_script(project.path(), "empty.gd", b"");
    write_script(project.path(), "a.gd", b"a");
    let read = |path: &str| {
        let args = json!({"script_path": path, "project_path": project.path()});
        async move { call("godot_read_script", args).await }
    };

    let other = script_answer(&read("res://other.gd").await);
    assert_eq!(other["path"], "res://other.gd");
    assert_eq!(other["text"], "extends Node\n");
    assert_eq!(other["source"], "file");
    assert_eq!(other["dirty"], false);
    assert_eq!(other["editor_consulted"], false);
    // The label vectors the addon is held to as well.
    assert_eq!(other["version"], "688cd8553db5c904");

    let crlf = script_answer(&read("crlf.gd").await);
    assert_eq!(crlf["path"], "res://crlf.gd");
    assert_eq!(crlf["text"], "extends Node\n", "text is given with \\n");
    assert_eq!(crlf["version"], "688cd8553db5c904");

    assert_eq!(
        script_answer(&read("res://empty.gd").await)["version"],
        "cbf29ce484222325"
    );
    assert_eq!(
        script_answer(&read("res://a.gd").await)["version"],
        "af63dc4c8601ec8c"
    );
}

#[tokio::test]
async fn read_script_refuses_what_it_should_with_a_fix() {
    let project = project_dir();
    write_script(project.path(), "notes.txt", b"hello\n");
    write_script(project.path(), "binary.gd", &[0xff, 0xfe, 0x00, 0x41]);
    write_script(project.path(), "big.gd", "#".repeat(1_048_577).as_bytes());
    let refused = |path: &str, needle: &str| {
        let args = json!({"script_path": path, "project_path": project.path()});
        let needle = needle.to_owned();
        async move {
            let result = call("godot_read_script", args).await;
            assert!(result.is_error, "expected a refusal: {}", text(&result));
            assert!(
                text(&result).contains(&needle),
                "{:?} should mention {needle:?}",
                text(&result)
            );
        }
    };
    refused("res://missing.gd", "godot_create_script").await;
    refused("res://notes.txt", ".gd").await;
    refused("res://../outside.gd", "..").await;
    refused("res://binary.gd", "UTF-8").await;
    refused("res://big.gd", "1 MiB").await;
}

#[tokio::test]
async fn read_script_asks_the_editor_first_and_does_not_hide_its_errors() {
    use common::{MockBridge, Reply};
    let project = project_dir();
    write_script(project.path(), "other.gd", b"extends Node\n");
    let bridge = MockBridge::start(|command| {
        match command["params"]["script_path"]
            .as_str()
            .unwrap_or_default()
        {
            "res://typed.gd" => Reply::Ok(json!({
                "path": "res://typed.gd", "text": "# typed\n", "source": "editor",
                "dirty": true, "version": "0123456789abcdef"
            })),
            _ => Reply::Err("The script editor's state cannot be matched. Close the tabs.".into()),
        }
    })
    .await;
    let manager = Arc::new(ToolManager::new(
        BridgeClient::new(bridge.port),
        CliRunner::new(PathBuf::from("unused-godot")),
    ));

    let typed = manager
        .clone()
        .call_tool(
            "godot_read_script",
            json!({"script_path": "res://typed.gd", "project_path": project.path()}),
        )
        .await;
    let typed = script_answer(&typed);
    assert_eq!(typed["source"], "editor");
    assert_eq!(typed["dirty"], true);
    assert_eq!(typed["text"], "# typed\n");

    // An error from a live editor is returned as it is; the file is not read instead, because
    // that would hide unsaved typing the editor may hold.
    let refused = manager
        .clone()
        .call_tool(
            "godot_read_script",
            json!({"script_path": "res://other.gd", "project_path": project.path()}),
        )
        .await;
    assert!(refused.is_error, "{}", text(&refused));
    assert!(
        text(&refused).contains("cannot be matched"),
        "{}",
        text(&refused)
    );
    assert!(
        bridge
            .received()
            .iter()
            .all(|command| command["type"] == "read_script")
    );
}

const TWO_VARS: &str = "extends Node\n\nvar x = 1\nvar y = 1\n";

fn edit_args(project: &Path, path: &str, old: &str, new: &str) -> Value {
    json!({"script_path": path, "old_text": old, "new_text": new, "project_path": project})
}

fn files_in(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .expect("read dir")
        .map(|entry| {
            entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    names.sort();
    names
}

#[tokio::test]
async fn edit_script_without_an_editor_replaces_one_fragment() {
    let project = project_dir();
    write_script(project.path(), "other.gd", TWO_VARS.as_bytes());
    let before = files_in(project.path());

    let result = call(
        "godot_edit_script",
        edit_args(project.path(), "res://other.gd", "var x = 1", "var x = 2"),
    )
    .await;
    let answer = script_answer(&result);
    assert_eq!(answer["path"], "res://other.gd");
    assert_eq!(answer["replaced"], 1);
    assert_eq!(answer["source"], "file");
    assert_eq!(answer["editor_consulted"], false);
    assert_eq!(answer["version"], "9a79065d76c35fdc");
    assert_eq!(
        std::fs::read_to_string(project.path().join("other.gd")).expect("file"),
        "extends Node\n\nvar x = 2\nvar y = 1\n"
    );
    assert_eq!(
        files_in(project.path()),
        before,
        "no temporary file is left"
    );

    // The label the answer carries is the label a read gives for the new text.
    let read = call(
        "godot_read_script",
        json!({"script_path": "res://other.gd", "project_path": project.path()}),
    )
    .await;
    assert_eq!(script_answer(&read)["version"], answer["version"]);
}

#[tokio::test]
async fn edit_script_refuses_what_it_should_and_leaves_the_file_alone() {
    let project = project_dir();
    write_script(project.path(), "other.gd", TWO_VARS.as_bytes());
    write_script(project.path(), "notes.txt", b"hello\n");
    let before = files_in(project.path());
    let refused = |args: Value, needle: &str| {
        let needle = needle.to_owned();
        async move {
            let result = call("godot_edit_script", args).await;
            assert!(result.is_error, "expected a refusal: {}", text(&result));
            assert!(
                text(&result).contains(&needle),
                "{:?} should mention {needle:?}",
                text(&result)
            );
        }
    };
    let p = project.path();
    refused(
        edit_args(p, "res://other.gd", "var z = 1", "var z = 2"),
        "0 times",
    )
    .await;
    refused(edit_args(p, "res://other.gd", "var ", "let "), "2 times").await;
    refused(edit_args(p, "res://other.gd", "", "x"), "must not be empty").await;
    refused(
        edit_args(p, "res://other.gd", "var x = 1", "var x = 1"),
        "identical",
    )
    .await;
    refused(
        json!({"script_path": "res://other.gd", "old_text": "var y = 1",
               "new_text": "var y = 2", "version": "0000000000000000", "project_path": p}),
        "changed since it was read",
    )
    .await;
    refused(
        edit_args(p, "res://missing.gd", "a", "b"),
        "godot_create_script",
    )
    .await;
    refused(edit_args(p, "res://notes.txt", "hello", "bye"), ".gd").await;
    refused(edit_args(p, "res://../outside.gd", "a", "b"), "..").await;

    assert_eq!(
        std::fs::read_to_string(p.join("other.gd")).expect("file"),
        TWO_VARS
    );
    assert_eq!(
        std::fs::read_to_string(p.join("notes.txt")).expect("file"),
        "hello\n"
    );
    assert_eq!(
        files_in(p),
        before,
        "a refused edit leaves no temporary file"
    );
}

#[tokio::test]
async fn edit_script_accepts_the_current_label_and_treats_text_as_data() {
    let project = project_dir();
    write_script(project.path(), "other.gd", TWO_VARS.as_bytes());
    let read = call(
        "godot_read_script",
        json!({"script_path": "res://other.gd", "project_path": project.path()}),
    )
    .await;
    let version = script_answer(&read)["version"].clone();

    let tricky = "var y = \"%s {0} \\\\ \\n $x\"\nfunc injected() -> void:\n\tOS.crash(\"no\")";
    let result = call(
        "godot_edit_script",
        json!({"script_path": "res://other.gd", "old_text": "var y = 1",
               "new_text": tricky, "version": version, "project_path": project.path()}),
    )
    .await;
    assert_eq!(script_answer(&result)["replaced"], 1);
    assert_eq!(
        std::fs::read_to_string(project.path().join("other.gd")).expect("file"),
        format!("extends Node\n\nvar x = 1\n{tricky}\n")
    );
}

#[tokio::test]
async fn edit_script_keeps_the_line_endings_and_the_byte_order_mark_of_the_file() {
    let project = project_dir();
    write_script(
        project.path(),
        "crlf.gd",
        b"\xef\xbb\xbfextends Node\r\n\r\nvar x = 1\r\nvar y = 1\r\n",
    );

    // The old text is written with \n and still matches across two CRLF lines.
    let result = call(
        "godot_edit_script",
        edit_args(
            project.path(),
            "res://crlf.gd",
            "var x = 1\nvar y = 1",
            "var x = 2\nvar y = 2\nvar z = 3",
        ),
    )
    .await;
    assert_eq!(script_answer(&result)["replaced"], 1);
    assert_eq!(
        std::fs::read(project.path().join("crlf.gd")).expect("file"),
        b"\xef\xbb\xbfextends Node\r\n\r\nvar x = 2\r\nvar y = 2\r\nvar z = 3\r\n".to_vec()
    );
}

#[cfg(windows)]
#[tokio::test]
async fn edit_script_leaves_the_script_unchanged_when_it_cannot_be_replaced() {
    let project = project_dir();
    write_script(project.path(), "locked.gd", TWO_VARS.as_bytes());
    let path = project.path().join("locked.gd");
    let mut permissions = std::fs::metadata(&path).expect("metadata").permissions();
    permissions.set_readonly(true);
    std::fs::set_permissions(&path, permissions.clone()).expect("lock");
    let before = files_in(project.path());

    let result = call(
        "godot_edit_script",
        edit_args(project.path(), "res://locked.gd", "var x = 1", "var x = 2"),
    )
    .await;
    assert!(result.is_error, "{}", text(&result));
    assert!(
        text(&result).contains("Could not write res://locked.gd"),
        "the refusal should come from the write: {}",
        text(&result)
    );
    assert_eq!(std::fs::read_to_string(&path).expect("file"), TWO_VARS);
    assert_eq!(
        files_in(project.path()),
        before,
        "the temporary file is removed"
    );

    #[allow(clippy::permissions_set_readonly_false)]
    permissions.set_readonly(false);
    std::fs::set_permissions(&path, permissions).expect("unlock");
}

#[tokio::test]
async fn edit_script_asks_the_editor_first_and_does_not_hide_its_refusals() {
    use common::{MockBridge, Reply};
    let project = project_dir();
    write_script(project.path(), "other.gd", TWO_VARS.as_bytes());
    let bridge = MockBridge::start(|command| {
        match command["params"]["script_path"].as_str().unwrap_or_default() {
            "res://ok.gd" => Reply::Ok(json!({
                "path": "res://ok.gd", "replaced": 1, "version": "0123456789abcdef", "source": "editor"
            })),
            _ => Reply::Err("res://other.gd has unsaved changes in the script editor. Save it.".into()),
        }
    })
    .await;
    let manager = Arc::new(ToolManager::new(
        BridgeClient::new(bridge.port),
        CliRunner::new(PathBuf::from("unused-godot")),
    ));

    let tricky = "a \"quoted\" %s {0}\nline";
    let done = manager
        .clone()
        .call_tool(
            "godot_edit_script",
            json!({"script_path": "res://ok.gd", "old_text": tricky, "new_text": "b",
                   "version": "fedcba9876543210", "project_path": project.path()}),
        )
        .await;
    assert_eq!(script_answer(&done)["source"], "editor");
    let sent = bridge.received();
    assert_eq!(sent[0]["type"], "edit_script");
    assert_eq!(
        sent[0]["params"]["old_text"], tricky,
        "strings travel as data"
    );
    assert_eq!(sent[0]["params"]["version"], "fedcba9876543210");

    let refused = manager
        .clone()
        .call_tool(
            "godot_edit_script",
            edit_args(project.path(), "res://other.gd", "var x = 1", "var x = 2"),
        )
        .await;
    assert!(refused.is_error, "{}", text(&refused));
    assert!(
        text(&refused).contains("unsaved changes"),
        "{}",
        text(&refused)
    );
    assert_eq!(
        std::fs::read_to_string(project.path().join("other.gd")).expect("file"),
        TWO_VARS,
        "a refusal from the editor is not turned into a file write"
    );
}

/// A link planted where a temporary file used to be named must not redirect the write, and a
/// script that is itself a link is not swapped for a regular file.
#[cfg(unix)]
#[tokio::test]
async fn edit_script_never_follows_a_planted_link() {
    let project = project_dir();
    let outside = tempfile::tempdir().expect("outside dir");
    let victim = outside.path().join("victim.txt");
    std::fs::write(&victim, "keep").expect("victim");
    write_script(project.path(), "other.gd", TWO_VARS.as_bytes());
    std::os::unix::fs::symlink(&victim, project.path().join(".other.gd.godot-mcp-edit.tmp"))
        .expect("plant link");

    let done = call(
        "godot_edit_script",
        edit_args(project.path(), "res://other.gd", "var x = 1", "var x = 2"),
    )
    .await;
    assert_eq!(script_answer(&done)["replaced"], 1);
    assert_eq!(std::fs::read_to_string(&victim).expect("victim"), "keep");

    std::os::unix::fs::symlink(
        project.path().join("other.gd"),
        project.path().join("link.gd"),
    )
    .expect("script link");
    let refused = call(
        "godot_edit_script",
        edit_args(project.path(), "res://link.gd", "var y = 1", "var y = 2"),
    )
    .await;
    assert!(refused.is_error, "{}", text(&refused));
    assert!(
        text(&refused).contains("symbolic link"),
        "{}",
        text(&refused)
    );
}
