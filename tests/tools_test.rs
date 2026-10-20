//! Host-side tools that never need a running editor: script creation, run/launch validation.

mod common;

use common::{fake_godot, free_port, is_tool_error, project_dir, tool_text};
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
