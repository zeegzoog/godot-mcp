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

/// Like `call`, with a stand-in engine that answers the class check of the authoring fallbacks.
async fn call_classified(tool: &str, args: Value) -> ToolResult {
    let cwd = tempfile::tempdir().expect("working dir");
    let engine = tempfile::tempdir().expect("engine dir");
    manager_in(cwd.path(), common::classifying_godot(engine.path()))
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
async fn an_offline_editor_names_the_bridge_port_the_server_is_using() {
    let port = free_port();
    let tools = Arc::new(ToolManager::new(
        BridgeClient::with_port_origin(port, format!("port {port} from --debug-port")),
        CliRunner::new(PathBuf::from("godot-binary-that-does-not-exist")),
    ));
    let result = tools.call_tool("godot_get_project_info", json!({})).await;
    let message = text(&result);
    assert!(result.is_error, "{message}");
    assert!(message.contains(&port.to_string()), "{message}");
    assert!(message.contains("--debug-port"), "{message}");
    assert!(message.contains("godot_mcp/bridge_port"), "{message}");
    assert!(message.contains("must listen"), "{message}");
    assert!(!message.contains("default: 9333"), "{message}");
    assert!(!message.contains("Verify GODOT_DEBUG_PORT"), "{message}");
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
async fn run_project_warns_when_an_imported_source_is_newer_than_its_cache() {
    let project = project_dir();
    let root = project.path();
    std::fs::create_dir_all(root.join("shaders")).unwrap();
    std::fs::create_dir_all(root.join(".godot/imported")).unwrap();
    let source = root.join("shaders/flow.glsl");
    let cache = root.join(".godot/imported/flow.glsl-abc.res");
    std::fs::write(&source, "void main() {}\n").unwrap();
    std::fs::write(&cache, "old").unwrap();
    std::fs::write(
        root.join("shaders/flow.glsl.import"),
        "path=\"res://.godot/imported/flow.glsl-abc.res\"\n",
    )
    .unwrap();
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
    std::fs::File::options()
        .write(true)
        .open(&cache)
        .unwrap()
        .set_modified(old)
        .unwrap();
    let result = call(
        "godot_run_project",
        json!({"project_path": root, "mode": "process"}),
    )
    .await;
    let message = text(&result);
    assert!(result.is_error, "{message}");
    assert!(message.contains("res://shaders/flow.glsl"), "{message}");
    assert!(message.contains("godot_resave_resources"), "{message}");
    assert!(!message.contains("res://shaders/plain"), "{message}");
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
    let result = call_classified(
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
    let first = call_classified("godot_create_resource", args("Curve")).await;
    assert!(!first.is_error, "{}", text(&first));
    let written = std::fs::read_to_string(project.path().join("made.tres")).expect("resource");
    assert!(written.contains("type=\"Curve\""), "{written}");

    let second = call_classified("godot_create_resource", args("Gradient")).await;
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

#[tokio::test]
async fn user_args_are_placed_after_the_engine_separator_and_shown_in_status() {
    let project = project_dir();
    let (godot, record) = recording_godot(project.path());
    let tools = manager_in(project.path(), godot);
    let result = Arc::clone(&tools)
        .call_tool(
            "godot_run_project",
            json!({"mode": "process", "user_args": ["--headless", "--caustics"]}),
        )
        .await;
    assert!(!result.is_error, "{}", text(&result));

    let arguments = wait_for_file(&record).await;
    let (engine, game) = arguments.split_once(" -- ").unwrap_or((&arguments, ""));
    assert_eq!(arguments.matches(" -- ").count(), 1, "{arguments}");
    assert!(engine.contains("--remote-debug"), "{arguments}");
    assert!(!engine.contains("--headless"), "{arguments}");
    assert!(!engine.contains("--caustics"), "{arguments}");
    assert!(
        game.contains("--headless") && game.contains("--caustics"),
        "{arguments}"
    );

    let status = Arc::clone(&tools)
        .call_tool("godot_get_run_status", json!({}))
        .await;
    assert!(!status.is_error, "{}", text(&status));
    assert!(
        text(&status).contains("\"--caustics\""),
        "{}",
        text(&status)
    );
}

#[tokio::test]
async fn user_args_stay_behind_the_same_separator_when_the_run_is_held() {
    let project = project_dir();
    let (godot, record) = recording_godot(project.path());
    let tools = manager_in(project.path(), godot);
    let result = Arc::clone(&tools)
        .call_tool(
            "godot_run_project",
            json!({"mode": "process", "held": true, "user_args": ["--caustics"]}),
        )
        .await;
    assert!(!result.is_error, "{}", text(&result));
    let arguments = wait_for_file(&record).await;
    let (engine, game) = arguments.split_once(" -- ").unwrap_or((&arguments, ""));
    assert_eq!(arguments.matches(" -- ").count(), 1, "{arguments}");
    assert!(engine.contains("--fixed-fps"), "{arguments}");
    assert!(!engine.contains("--caustics"), "{arguments}");
    assert!(game.contains("--godot-mcp-hold"), "{arguments}");
    assert!(game.contains("--caustics"), "{arguments}");
}

#[tokio::test]
async fn user_args_are_refused_unless_the_mode_is_process() {
    let project = project_dir();
    let (godot, record) = recording_godot(project.path());
    let tools = manager_in(project.path(), godot);
    for args in [
        json!({"user_args": ["--caustics"]}),
        json!({"mode": "auto", "user_args": ["--caustics"]}),
        json!({"mode": "auto", "user_args": []}),
    ] {
        let result = Arc::clone(&tools)
            .call_tool("godot_run_project", args)
            .await;
        assert!(result.is_error, "{}", text(&result));
        assert!(text(&result).contains("process"), "{}", text(&result));
    }
    assert!(
        !record.exists(),
        "a refused user_args request still launched Godot"
    );
}

#[tokio::test]
async fn user_args_are_refused_instead_of_playing_in_the_editor() {
    use common::{MockBridge, Reply};
    let bridge = MockBridge::start(
        |command| match command["type"].as_str().unwrap_or_default() {
            "run_project" => Reply::Ok(json!({"running": true})),
            _ => Reply::Ok(json!({})),
        },
    )
    .await;
    let project = project_dir();
    let tools = Arc::new(ToolManager::new(
        BridgeClient::new(bridge.port),
        CliRunner::new(fake_godot(project.path())),
    ));
    let result = tools
        .call_tool("godot_run_project", json!({"user_args": ["--caustics"]}))
        .await;
    assert!(result.is_error, "{}", text(&result));
    assert!(text(&result).contains("process"), "{}", text(&result));
    assert!(
        !bridge
            .received()
            .iter()
            .any(|command| command["type"] == "run_project"),
        "editor Play was started with user_args"
    );
}

#[tokio::test]
async fn user_args_reject_the_wrong_shape() {
    let not_strings = call(
        "godot_run_project",
        json!({"mode": "process", "user_args": [1]}),
    )
    .await;
    assert!(not_strings.is_error, "{}", text(&not_strings));
    assert!(
        text(&not_strings).contains("string"),
        "{}",
        text(&not_strings)
    );

    let many = vec!["a"; 33];
    let too_many = call(
        "godot_run_project",
        json!({"mode": "process", "user_args": many}),
    )
    .await;
    assert!(too_many.is_error, "{}", text(&too_many));
    assert!(text(&too_many).contains("32"), "{}", text(&too_many));

    let long = "a".repeat(257);
    let too_long = call(
        "godot_run_project",
        json!({"mode": "process", "user_args": [long]}),
    )
    .await;
    assert!(too_long.is_error, "{}", text(&too_long));
    assert!(text(&too_long).contains("256"), "{}", text(&too_long));

    let control = call(
        "godot_run_project",
        json!({"mode": "process", "user_args": ["ok\n--quit"]}),
    )
    .await;
    assert!(control.is_error, "{}", text(&control));
    assert!(text(&control).contains("control"), "{}", text(&control));

    for reserved in ["--godot-mcp-hold", "--godot-mcp-fixed-fps=5"] {
        let refused = call(
            "godot_run_project",
            json!({"mode": "process", "user_args": [reserved]}),
        )
        .await;
        assert!(refused.is_error, "{reserved}: {}", text(&refused));
        assert!(
            text(&refused).contains("--godot-mcp-"),
            "{}",
            text(&refused)
        );
    }
}

#[tokio::test]
async fn record_rejects_a_span_a_relative_directory_and_a_bad_format() {
    let names: Vec<_> = ToolManager::list_tools()
        .into_iter()
        .map(|tool| tool.name)
        .collect();
    assert!(names.iter().any(|name| name == "godot_game_record"));

    let too_many = call("godot_game_record", json!({"frames": 241})).await;
    assert!(too_many.is_error, "{}", text(&too_many));
    assert!(text(&too_many).contains("240"), "{}", text(&too_many));

    let span = call("godot_game_record", json!({"frames": 240, "every": 3})).await;
    assert!(span.is_error, "{}", text(&span));
    assert!(text(&span).contains("480"), "{}", text(&span));

    let relative = call("godot_game_record", json!({"directory": "frames"})).await;
    assert!(relative.is_error, "{}", text(&relative));
    assert!(text(&relative).contains("absolute"), "{}", text(&relative));

    let parent = call(
        "godot_game_record",
        json!({"directory": "/tmp/godot-mcp/../../etc"}),
    )
    .await;
    assert!(parent.is_error, "{}", text(&parent));
    assert!(text(&parent).contains(".."), "{}", text(&parent));

    let webm = call("godot_game_record", json!({"format": "webm"})).await;
    assert!(webm.is_error, "{}", text(&webm));

    let fraction = call("godot_game_record", json!({"frames": 1.5})).await;
    assert!(fraction.is_error, "{}", text(&fraction));
    assert!(text(&fraction).contains("integer"), "{}", text(&fraction));
}

#[tokio::test]
async fn record_writes_only_below_the_system_temporary_directory() {
    let temp = std::env::temp_dir();
    let Some(outside) = temp
        .parent()
        .map(|parent| parent.join("godot-mcp-record-outside"))
    else {
        return;
    };
    let refused = call(
        "godot_game_record",
        json!({"directory": outside.display().to_string()}),
    )
    .await;
    assert!(refused.is_error, "{}", text(&refused));
    assert!(
        text(&refused).contains("temporary directory"),
        "{}",
        text(&refused)
    );
    assert!(
        !outside.exists(),
        "a refused recording created {}",
        outside.display()
    );

    // A recording that is already there is never replaced.
    let taken = tempfile::tempdir().expect("record dir");
    std::fs::write(taken.path().join("recording.mp4"), b"earlier").unwrap();
    let occupied = call(
        "godot_game_record",
        json!({"directory": taken.path().display().to_string(), "format": "mp4"}),
    )
    .await;
    assert!(occupied.is_error, "{}", text(&occupied));
    assert!(
        text(&occupied).contains("already contains"),
        "{}",
        text(&occupied)
    );
    assert_eq!(
        std::fs::read(taken.path().join("recording.mp4")).unwrap(),
        b"earlier"
    );

    // A directory below the temporary one passes the check and fails later, because no game runs.
    let inside = tempfile::tempdir().expect("record dir");
    let allowed = call(
        "godot_game_record",
        json!({"directory": inside.path().display().to_string()}),
    )
    .await;
    assert!(allowed.is_error, "{}", text(&allowed));
    assert!(
        !text(&allowed).contains("temporary directory"),
        "{}",
        text(&allowed)
    );
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
    let open_dir = project.path().to_string_lossy().replace('\\', "/");
    let bridge = MockBridge::start(move |command| {
        if command["type"] == "get_project_info" {
            return Reply::Ok(json!({"project_dir": open_dir}));
        }
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
        bridge.received().iter().all(
            |command| command["type"] == "read_script" || command["type"] == "get_project_info"
        )
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
    let open_dir = project.path().to_string_lossy().replace('\\', "/");
    let bridge = MockBridge::start(move |command| {
        if command["type"] == "get_project_info" {
            return Reply::Ok(json!({"project_dir": open_dir}));
        }
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
    let sent: Vec<Value> = bridge
        .received()
        .into_iter()
        .filter(|command| command["type"] == "edit_script")
        .collect();
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

// TL-10: a named project wins over a connected editor.

/// A manager connected to a fake editor that has `open_project` open and answers `get_project_info`
/// and, for the commands in `served`, a canned success; any other command is an error.
async fn manager_with_editor(
    open_project: &Path,
    served: &'static [&'static str],
    engine: PathBuf,
) -> (common::MockBridge, Arc<ToolManager>) {
    use common::{MockBridge, Reply};
    let dir = open_project.to_string_lossy().replace('\\', "/");
    let bridge = MockBridge::start(move |command| {
        let kind = command["type"].as_str().unwrap_or_default();
        if kind == "get_project_info" {
            Reply::Ok(json!({"project_dir": dir, "name": "Open"}))
        } else if served.contains(&kind) {
            Reply::Ok(json!({"from_editor": kind}))
        } else {
            Reply::Err(format!("unexpected editor command {kind}"))
        }
    })
    .await;
    let manager = Arc::new(ToolManager::new(
        BridgeClient::new(bridge.port),
        CliRunner::new(engine),
    ));
    (bridge, manager)
}

fn sent_types(bridge: &common::MockBridge) -> Vec<String> {
    bridge
        .received()
        .iter()
        .filter_map(|command| command["type"].as_str().map(str::to_string))
        .collect()
}

#[tokio::test]
async fn a_named_project_wins_over_a_connected_editor() {
    let open = project_dir();
    let named = project_dir();
    for project in [open.path(), named.path()] {
        write_script(project, "a.gd", b"extends Node\nvar x = 1\n");
    }
    let engine = tempfile::tempdir().expect("engine dir");
    let (bridge, manager) =
        manager_with_editor(open.path(), &[], common::classifying_godot(engine.path())).await;

    let read = manager
        .clone()
        .call_tool(
            "godot_read_script",
            json!({"script_path": "res://a.gd", "project_path": named.path()}),
        )
        .await;
    let read = script_answer(&read);
    assert_eq!(read["source"], "file");
    assert_eq!(read["editor_consulted"], false);

    let edit = manager
        .clone()
        .call_tool(
            "godot_edit_script",
            edit_args(named.path(), "res://a.gd", "var x = 1", "var x = 2"),
        )
        .await;
    let edit = script_answer(&edit);
    assert_eq!(edit["replaced"], 1);
    assert!(
        edit["project_dir"]
            .as_str()
            .is_some_and(|dir| godot_mcp::paths::same_directory(Path::new(dir), named.path())),
        "the answer names the project it changed: {edit}"
    );
    assert!(
        std::fs::read_to_string(named.path().join("a.gd"))
            .expect("named script")
            .contains("var x = 2")
    );
    assert!(
        std::fs::read_to_string(open.path().join("a.gd"))
            .expect("open script")
            .contains("var x = 1"),
        "the project the editor has open was changed"
    );

    let scene = manager
        .clone()
        .call_tool(
            "godot_create_scene",
            json!({"scene_path": "res://s.tscn", "project_path": named.path()}),
        )
        .await;
    assert!(!scene.is_error, "{}", text(&scene));
    assert!(named.path().join("s.tscn").is_file());
    assert!(!open.path().join("s.tscn").exists());

    let resource = manager
        .clone()
        .call_tool(
            "godot_create_resource",
            json!({"resource_path": "res://r.tres", "class_name": "Curve",
                   "project_path": named.path()}),
        )
        .await;
    assert!(!resource.is_error, "{}", text(&resource));
    assert!(named.path().join("r.tres").is_file());
    assert!(!open.path().join("r.tres").exists());

    let set = manager
        .clone()
        .call_tool(
            "godot_set_project_setting",
            json!({"key": "custom/answer", "value": 7, "project_path": named.path()}),
        )
        .await;
    assert!(!set.is_error, "{}", text(&set));
    assert!(
        std::fs::read_to_string(named.path().join("project.godot"))
            .expect("project.godot")
            .contains("answer=7")
    );
    assert!(
        !std::fs::read_to_string(open.path().join("project.godot"))
            .expect("open project.godot")
            .contains("answer=7")
    );

    let settings = manager
        .clone()
        .call_tool(
            "godot_get_project_settings",
            json!({"prefix": "application/config/name", "project_path": named.path()}),
        )
        .await;
    assert!(!settings.is_error, "{}", text(&settings));
    assert!(
        text(&settings).contains("project.godot"),
        "{}",
        text(&settings)
    );

    assert!(
        sent_types(&bridge)
            .iter()
            .all(|kind| kind == "get_project_info"),
        "the editor was sent a command for another project: {:?}",
        sent_types(&bridge)
    );
}

#[tokio::test]
async fn the_editors_own_project_still_goes_to_the_editor() {
    let open = project_dir();
    let (bridge, manager) = manager_with_editor(
        open.path(),
        &["edit_script", "read_script"],
        PathBuf::from("unused-godot"),
    )
    .await;

    // The same directory, spelled with a trailing separator, and with no project_path at all.
    let spelled = format!("{}/", open.path().to_string_lossy().replace('\\', "/"));
    for project_path in [Some(spelled), None] {
        let mut args = json!({"script_path": "res://a.gd", "old_text": "a", "new_text": "b"});
        if let Some(path) = &project_path {
            args["project_path"] = json!(path);
        }
        let done = manager.clone().call_tool("godot_edit_script", args).await;
        let done = script_answer(&done);
        assert_eq!(done["from_editor"], "edit_script", "{done}");
        assert!(
            done["project_dir"]
                .as_str()
                .is_some_and(|dir| godot_mcp::paths::same_directory(Path::new(dir), open.path())),
            "an editor answer that changed a file names the project: {done}"
        );
    }
    let read = manager
        .clone()
        .call_tool(
            "godot_read_script",
            json!({"script_path": "res://a.gd", "project_path": open.path()}),
        )
        .await;
    assert_eq!(script_answer(&read)["from_editor"], "read_script");
    assert_eq!(
        sent_types(&bridge)
            .iter()
            .filter(|kind| *kind == "edit_script")
            .count(),
        2
    );
}

#[tokio::test]
async fn an_editor_that_cannot_name_its_project_is_not_sent_a_file_change() {
    use common::{MockBridge, Reply};
    let bridge = MockBridge::start(|_| Reply::Err("no project info".into())).await;
    let manager = Arc::new(ToolManager::new(
        BridgeClient::new(bridge.port),
        CliRunner::new(PathBuf::from("unused-godot")),
    ));
    let result = manager
        .clone()
        .call_tool(
            "godot_edit_script",
            json!({"script_path": "res://a.gd", "old_text": "a", "new_text": "b"}),
        )
        .await;
    assert!(result.is_error, "{}", text(&result));
    assert!(
        sent_types(&bridge)
            .iter()
            .all(|kind| kind == "get_project_info"),
        "{:?}",
        sent_types(&bridge)
    );
}

// TL-11 and RO-3: stopping and asking about runs.

/// Starts a managed run with the manager's (fake) Godot executable and returns its id.
async fn start_fake_run(manager: &Arc<ToolManager>, project: &Path) -> String {
    let started = manager
        .clone()
        .call_tool(
            "godot_run_project",
            json!({"mode": "process", "headless": true, "project_path": project}),
        )
        .await;
    assert!(!started.is_error, "{}", text(&started));
    text(&started)
        .split("run_id: ")
        .nth(1)
        .and_then(|rest| rest.split(')').next())
        .expect("the answer names the run id")
        .to_string()
}

async fn status_of(manager: &Arc<ToolManager>) -> Value {
    let status = manager
        .clone()
        .call_tool("godot_get_run_status", json!({}))
        .await;
    script_answer(&status)
}

fn run_in(status: &Value, id: &str) -> Value {
    status["process_runs"]
        .as_array()
        .and_then(|runs| runs.iter().find(|run| run["run_id"] == id))
        .cloned()
        .unwrap_or_else(|| panic!("run {id} not in {status}"))
}

#[tokio::test]
async fn a_finished_run_keeps_its_exit_code_and_says_who_ended_it() {
    let project = project_dir();
    let tools = tempfile::tempdir().expect("tools dir");

    let by_itself = manager_in(project.path(), common::exiting_godot(tools.path(), 3));
    let id = start_fake_run(&by_itself, project.path()).await;
    let mut run = run_in(&status_of(&by_itself).await, &id);
    for _ in 0..100 {
        if run["running"] == false {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        run = run_in(&status_of(&by_itself).await, &id);
    }
    assert_eq!(run["running"], false, "{run}");
    assert_eq!(run["exit_code"], 3, "{run}");
    assert_eq!(run["ended_by"], "game", "{run}");
    assert!(run["ended_at"].as_u64().is_some_and(|at| at > 0), "{run}");

    let sleepers = tempfile::tempdir().expect("tools dir");
    let stopped_by_us = manager_in(project.path(), common::sleeping_godot(sleepers.path()));
    let id = start_fake_run(&stopped_by_us, project.path()).await;
    let alive = run_in(&status_of(&stopped_by_us).await, &id);
    assert_eq!(alive["running"], true, "{alive}");
    assert!(
        alive["ended_by"].is_null() && alive["ended_at"].is_null(),
        "{alive}"
    );
    let stopped = stopped_by_us
        .clone()
        .call_tool("godot_stop_project", json!({"run_id": id}))
        .await;
    assert!(!stopped.is_error, "{}", text(&stopped));
    let gone = run_in(&status_of(&stopped_by_us).await, &id);
    assert_eq!(gone["running"], false, "{gone}");
    assert_eq!(gone["ended_by"], "server", "{gone}");
    assert!(gone["exit_code"].is_number(), "{gone}");
    assert!(gone["ended_at"].as_u64().is_some_and(|at| at > 0), "{gone}");
}

fn run_state_bridge(playing: bool) -> impl Fn(&Value) -> common::Reply + Send + Sync + 'static {
    move |command| match command["type"].as_str().unwrap_or_default() {
        "get_run_status" => common::Reply::Ok(json!({"running": playing, "scene": "res://m.tscn"})),
        "stop_project" => common::Reply::Ok(json!({"stopped": true})),
        other => common::Reply::Err(format!("unexpected {other}")),
    }
}

#[tokio::test]
async fn stop_without_a_run_id_stops_every_run_and_says_what_it_stopped() {
    let project = project_dir();
    let tools = tempfile::tempdir().expect("tools dir");
    let bridge = common::MockBridge::start(run_state_bridge(false)).await;
    let manager = Arc::new(
        ToolManager::new(
            BridgeClient::new(bridge.port),
            CliRunner::new(common::sleeping_godot(tools.path())),
        )
        .with_working_dir(project.path().to_path_buf()),
    );
    let first = start_fake_run(&manager, project.path()).await;
    let second = start_fake_run(&manager, project.path()).await;

    let stopped = manager
        .clone()
        .call_tool("godot_stop_project", json!({}))
        .await;
    let answer = script_answer(&stopped);
    assert_eq!(answer["stopped"]["editor_play"], false, "{answer}");
    let ids: Vec<&str> = answer["stopped"]["runs"]
        .as_array()
        .expect("runs")
        .iter()
        .filter_map(|run| run["run_id"].as_str())
        .collect();
    assert!(
        ids.contains(&first.as_str()) && ids.contains(&second.as_str()),
        "{answer}"
    );
    let status = status_of(&manager).await;
    for id in [&first, &second] {
        assert_eq!(run_in(&status, id)["running"], false, "{status}");
    }
    assert!(
        sent_types(&bridge)
            .iter()
            .all(|kind| kind != "stop_project"),
        "the editor was not playing, so it was not told to stop: {:?}",
        sent_types(&bridge)
    );

    // Nothing left: not an error, and it says so.
    let again = manager
        .clone()
        .call_tool("godot_stop_project", json!({}))
        .await;
    let again = script_answer(&again);
    assert!(
        again["message"]
            .as_str()
            .is_some_and(|m| m.to_lowercase().contains("nothing was running")),
        "{again}"
    );
}

#[tokio::test]
async fn stop_without_a_run_id_also_stops_the_editors_play_mode() {
    let bridge = common::MockBridge::start(run_state_bridge(true)).await;
    let manager = Arc::new(ToolManager::new(
        BridgeClient::new(bridge.port),
        CliRunner::new(PathBuf::from("unused-godot")),
    ));
    let stopped = manager
        .clone()
        .call_tool("godot_stop_project", json!({}))
        .await;
    let answer = script_answer(&stopped);
    assert_eq!(answer["stopped"]["editor_play"], true, "{answer}");
    assert!(sent_types(&bridge).contains(&"stop_project".to_string()));
}

#[tokio::test]
async fn asking_about_runs_with_nothing_running_and_no_editor_is_not_a_failure() {
    let cwd = tempfile::tempdir().expect("cwd");
    let manager = manager_in(cwd.path(), PathBuf::from("unused-godot"));
    for tool in [
        "godot_get_run_status",
        "godot_stop_project",
        "godot_run_output",
    ] {
        let result = manager.clone().call_tool(tool, json!({})).await;
        assert!(!result.is_error, "{tool}: {}", text(&result));
        assert!(
            text(&result).to_lowercase().contains("nothing"),
            "{tool}: {}",
            text(&result)
        );
    }
}

// TL-1, TL-5 and the script_path alias.

#[tokio::test]
async fn a_position_below_the_first_is_refused_before_any_language_server_is_asked() {
    for (tool, args) in [
        (
            "godot_code_complete",
            json!({"script_path": "res://a.gd", "line": 0, "character": 1}),
        ),
        (
            "godot_code_definition",
            json!({"script_path": "res://a.gd", "line": 1, "character": -3}),
        ),
        (
            "godot_code_references",
            json!({"script_path": "res://a.gd", "line": -1, "character": 1}),
        ),
    ] {
        let result = call(tool, args).await;
        assert!(result.is_error, "{tool}");
        assert!(
            text(&result).contains("at least 1"),
            "{tool}: {}",
            text(&result)
        );
    }
}

#[tokio::test]
async fn a_breakpoint_location_is_a_res_path_inside_the_project() {
    let project = project_dir();
    let tools = tempfile::tempdir().expect("tools dir");
    let manager = manager_in(project.path(), common::sleeping_godot(tools.path()));
    let _run = start_fake_run(&manager, project.path()).await;
    for bad in [
        "res://../x.gd:1",
        "res://a/../../x.gd:3",
        "/etc/passwd.gd:1",
        "C:/x/y.gd:1",
        "a.gd:1",
        "res://a.gd:0",
        "res://a.gd",
    ] {
        let result = manager
            .clone()
            .call_tool(
                "godot_debug_breakpoints",
                json!({"action": "add", "breakpoint": bad}),
            )
            .await;
        assert!(result.is_error, "{bad}: {}", text(&result));
        assert!(text(&result).contains("res://"), "{bad}: {}", text(&result));
    }
    let listed = manager
        .clone()
        .call_tool("godot_debug_breakpoints", json!({"action": "list"}))
        .await;
    assert!(!listed.is_error, "{}", text(&listed));
    assert_eq!(script_answer(&listed)["breakpoints"], json!([]));

    let refused_at_start = manager
        .clone()
        .call_tool(
            "godot_run_project",
            json!({"mode": "process", "headless": true, "project_path": project.path(),
                   "breakpoints": ["res://../x.gd:1"]}),
        )
        .await;
    assert!(refused_at_start.is_error, "{}", text(&refused_at_start));
}

#[tokio::test]
async fn create_script_accepts_script_path_and_names_both_when_neither_is_given() {
    let project = project_dir();
    let made = call(
        "godot_create_script",
        json!({"script_path": "res://by_new_name.gd", "project_path": project.path()}),
    )
    .await;
    assert!(!made.is_error, "{}", text(&made));
    assert!(project.path().join("by_new_name.gd").is_file());

    let neither = call(
        "godot_create_script",
        json!({"project_path": project.path()}),
    )
    .await;
    assert!(neither.is_error);
    let message = text(&neither);
    assert!(
        message.contains("script_path") && message.contains("file_path"),
        "{message}"
    );
}

// TL-7: hints by failure class, and project files named by their res:// path.

#[tokio::test]
async fn each_failure_class_gets_its_own_hint_and_never_the_generic_one() {
    use common::{MockBridge, Reply};
    let cases: &[(&str, &str)] = &[
        (
            r#"{"code":"DENIED","message":"Refusing to set 'script'."}"#,
            "by design",
        ),
        (
            r#"{"code":"UNSUPPORTED","message":"This game is headless, so there is no image to capture."}"#,
            "start the game",
        ),
        (
            r#"{"code":"NOT_FOUND","message":"Node not found at 'Player'."}"#,
            "godot_game_tree",
        ),
        (
            r#"{"code":"BAD_PARAMS","message":"steps[0]: unknown type 'action'."}"#,
            "inputSchema",
        ),
        (
            "res://a.gd has unsaved changes in the script editor. Save it in the editor, or discard the changes.",
            "Save the script",
        ),
        (
            "The editor's text of res://a.gd differs from the file on disk. Save or reload it in the editor.",
            "reload",
        ),
        (
            "The script editor's state for res://a.gd cannot be matched (the editor lists 2 open scripts but 1 script tabs).",
            "Close the extra tabs",
        ),
        ("Failed to save resource: Can't open", "folder"),
        (
            "This setting expects a int; a value of another type is not stored.",
            "type the setting already has",
        ),
        ("File already exists: res://a.tres", "another path"),
    ];
    for (message, expected) in cases {
        let message = message.to_string();
        let bridge = MockBridge::start(move |_| Reply::Err(message.clone())).await;
        let manager = Arc::new(ToolManager::new(
            BridgeClient::new(bridge.port),
            CliRunner::new(PathBuf::from("unused-godot")),
        ));
        let result = manager.call_tool("godot_get_scene_tree", json!({})).await;
        let shown = text(&result);
        assert!(result.is_error, "{shown}");
        assert!(
            shown.contains(expected),
            "expected {expected:?} in: {shown}"
        );
        assert!(
            !shown.contains("Correct the arguments"),
            "generic hint used: {shown}"
        );
    }
}

#[tokio::test]
async fn a_godot_executable_that_cannot_run_names_godot_path_not_syntax() {
    let project = project_dir();
    write_script(project.path(), "ok.gd", b"extends Node\n");
    let result = call(
        "godot_validate_script",
        json!({"script_path": "res://ok.gd", "project_path": project.path()}),
    )
    .await;
    assert!(result.is_error);
    let shown = text(&result);
    assert!(shown.contains("GODOT_PATH"), "{shown}");
    assert!(!shown.contains("syntax"), "{shown}");
}

#[tokio::test]
async fn a_setting_of_the_wrong_type_does_not_get_the_key_example_as_its_hint() {
    let project = project_dir();
    std::fs::write(
        project.path().join("project.godot"),
        "config_version=5\n\n[application]\n\nconfig/name=\"Old\"\n",
    )
    .expect("project.godot");
    let wrong = call(
        "godot_set_project_setting",
        json!({"key": "application/config/name", "value": 5, "project_path": project.path()}),
    )
    .await;
    assert!(wrong.is_error);
    assert!(
        text(&wrong).contains("type the setting already has"),
        "{}",
        text(&wrong)
    );
    assert!(!text(&wrong).contains("Use a key like"), "{}", text(&wrong));
}

#[tokio::test]
async fn errors_name_project_files_by_their_res_path() {
    let project = project_dir();
    let dir = project.path().to_string_lossy().to_string();
    let args = |tool: &str| match tool {
        "godot_create_script" => {
            json!({"script_path": "res://keep.gd", "project_path": project.path()})
        }
        "godot_create_scene" => {
            json!({"scene_path": "res://keep.tscn", "project_path": project.path()})
        }
        _ => {
            json!({"resource_path": "res://keep.tres", "class_name": "Curve", "project_path": project.path()})
        }
    };
    for (tool, file) in [
        ("godot_create_script", "keep.gd"),
        ("godot_create_scene", "keep.tscn"),
        ("godot_create_resource", "keep.tres"),
    ] {
        std::fs::write(project.path().join(file), "x").expect("seed");
        let refused = call_classified(tool, args(tool)).await;
        assert!(refused.is_error, "{tool}");
        let shown = text(&refused);
        assert!(shown.contains(&format!("res://{file}")), "{tool}: {shown}");
        assert!(!shown.contains(&dir), "{tool} shows the host path: {shown}");
    }
    let missing = call(
        "godot_validate_script",
        json!({"script_path": "res://nope.gd", "project_path": project.path()}),
    )
    .await;
    assert!(
        text(&missing).contains("res://nope.gd"),
        "{}",
        text(&missing)
    );
    assert!(!text(&missing).contains(&dir), "{}", text(&missing));
}

// CA-11: the file fallbacks accept what the editor accepts.

#[tokio::test]
async fn the_fallbacks_refuse_a_class_the_engine_cannot_use_and_write_no_file() {
    let project = project_dir();
    let scene_with = |class: &str, name: &str| json!({"scene_path": format!("res://{name}.tscn"), "root_type": class, "project_path": project.path()});
    let resource_with = |class: &str, name: &str| json!({"resource_path": format!("res://{name}.tres"), "class_name": class, "project_path": project.path()});

    for (class, name) in [("Resource", "bad_scene"), ("Nope", "unknown_scene")] {
        let refused = call_classified("godot_create_scene", scene_with(class, name)).await;
        assert!(refused.is_error, "{class}: {}", text(&refused));
        assert!(text(&refused).contains("not a Node"), "{}", text(&refused));
        assert!(
            !project.path().join(format!("{name}.tscn")).exists(),
            "{class}"
        );
    }
    for (class, name) in [("Node", "bad_resource"), ("Nope", "unknown_resource")] {
        let refused = call_classified("godot_create_resource", resource_with(class, name)).await;
        assert!(refused.is_error, "{class}: {}", text(&refused));
        assert!(
            text(&refused).contains("Unknown resource class"),
            "{}",
            text(&refused)
        );
        assert!(
            !project.path().join(format!("{name}.tres")).exists(),
            "{class}"
        );
    }
    let scene = call_classified("godot_create_scene", scene_with("Node3D", "good_scene")).await;
    assert!(!scene.is_error, "{}", text(&scene));
    let resource = call_classified(
        "godot_create_resource",
        resource_with("Gradient", "good_resource"),
    )
    .await;
    assert!(!resource.is_error, "{}", text(&resource));

    // An engine that cannot run is not a reason to write a file that may not load.
    let unrunnable = call(
        "godot_create_scene",
        json!({"scene_path": "res://unchecked.tscn", "project_path": project.path()}),
    )
    .await;
    assert!(unrunnable.is_error);
    assert!(
        text(&unrunnable).contains("GODOT_PATH"),
        "{}",
        text(&unrunnable)
    );
    assert!(!project.path().join("unchecked.tscn").exists());
}

#[tokio::test]
async fn a_root_name_is_data_in_the_file_fallback_as_it_is_in_the_editor() {
    let project = project_dir();
    for (name, expected) in [
        ("with space", "with space"),
        ("bad name!", "bad name!"),
        ("a.b:c@d/e%f", "a_b_c_d_e_f"),
        ("quote\"inside", "quote_inside"),
        ("new\nline", "new_line"),
    ] {
        let file = format!("n_{}.tscn", expected.len());
        let made = call_classified(
            "godot_create_scene",
            json!({"scene_path": format!("res://{file}"), "root_name": name,
                   "project_path": project.path()}),
        )
        .await;
        assert!(!made.is_error, "{name:?}: {}", text(&made));
        let body = std::fs::read_to_string(project.path().join(&file)).expect("scene");
        assert!(
            body.contains(&format!("[node name=\"{expected}\" type=\"Node2D\"]")),
            "{name:?}: {body}"
        );
        assert_eq!(
            body.matches("[node").count(),
            1,
            "{name:?} added a node: {body}"
        );
    }
}

#[tokio::test]
async fn a_setting_key_with_a_dot_segment_is_refused_and_the_settings_answer_matches_the_editors() {
    let project = project_dir();
    std::fs::write(
        project.path().join("project.godot"),
        "config_version=5\n\n[application]\n\nconfig/name=\"Open\"\n\n[simqa]\n\nanswer=42\nflag=true\n",
    )
    .expect("project.godot");
    for key in ["simqa/../evil", "simqa/./x", "../x/y", "simqa/a/../b"] {
        let refused = call(
            "godot_set_project_setting",
            json!({"key": key, "value": 1, "project_path": project.path()}),
        )
        .await;
        assert!(refused.is_error, "{key}: {}", text(&refused));
    }
    let after = std::fs::read_to_string(project.path().join("project.godot")).expect("project");
    assert!(!after.contains("evil") && !after.contains("../"), "{after}");

    let settings = call(
        "godot_get_project_settings",
        json!({"prefix": "", "project_path": project.path()}),
    )
    .await;
    let answer = script_answer(&settings);
    assert_eq!(
        answer["settings"]["application/config/name"], "Open",
        "{answer}"
    );
    assert_eq!(answer["settings"]["simqa/answer"], "42", "{answer}");
    assert_eq!(answer["settings"]["simqa/flag"], "true", "{answer}");
    assert_eq!(answer["truncated"], false, "{answer}");
    assert_eq!(answer["source"], "project.godot", "{answer}");
}
