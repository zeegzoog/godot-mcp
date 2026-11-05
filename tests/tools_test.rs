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

/// A fake editor with `project` open whose script tab for res://open.gd is in the given state.
async fn editor_with_open_tab(project: &Path, dirty: bool) -> common::MockBridge {
    use common::{MockBridge, Reply};
    let dir = project.to_string_lossy().replace('\\', "/");
    MockBridge::start(move |command| match command["type"].as_str() {
        Some("get_project_info") => Reply::Ok(json!({"project_dir": dir})),
        Some("script_tab") => {
            let reload = command["params"]["reload"] == true;
            Reply::Ok(json!({
                "path": command["params"]["script_path"], "open": true, "dirty": dirty,
                "stale": !dirty && !reload, "reloaded": !dirty && reload
            }))
        }
        _ => Reply::Ok(json!({})),
    })
    .await
}

#[tokio::test]
async fn overwriting_a_script_open_with_unsaved_changes_is_refused() {
    let project = project_dir();
    write_script(project.path(), "open.gd", b"extends Node\n# mine\n");
    let bridge = editor_with_open_tab(project.path(), true).await;
    let manager = Arc::new(ToolManager::new(
        BridgeClient::new(bridge.port),
        CliRunner::new(PathBuf::from("unused-godot")),
    ));
    let result = manager
        .call_tool(
            "godot_create_script",
            json!({"script_path": "res://open.gd", "overwrite": true}),
        )
        .await;
    assert!(result.is_error, "{}", text(&result));
    assert!(
        text(&result).contains("unsaved changes"),
        "{}",
        text(&result)
    );
    assert_eq!(
        std::fs::read_to_string(project.path().join("open.gd")).expect("file"),
        "extends Node\n# mine\n",
        "the file is untouched"
    );
}

#[tokio::test]
async fn overwriting_a_script_open_in_a_clean_tab_reloads_the_tab() {
    let project = project_dir();
    write_script(project.path(), "open.gd", b"extends Node\n# old\n");
    let bridge = editor_with_open_tab(project.path(), false).await;
    let manager = Arc::new(ToolManager::new(
        BridgeClient::new(bridge.port),
        CliRunner::new(PathBuf::from("unused-godot")),
    ));
    let result = manager
        .call_tool(
            "godot_create_script",
            json!({"script_path": "res://open.gd", "overwrite": true}),
        )
        .await;
    assert!(!result.is_error, "{}", text(&result));
    assert!(text(&result).contains("reloaded"), "{}", text(&result));
    let reloads: Vec<Value> = bridge
        .received()
        .into_iter()
        .filter(|command| command["type"] == "script_tab" && command["params"]["reload"] == true)
        .collect();
    assert_eq!(reloads.len(), 1, "{:?}", bridge.received());
    assert_eq!(reloads[0]["params"]["script_path"], "res://open.gd");
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

/// A manager in an empty working directory with `godot` as the engine and no language server.
fn checked_manager(cwd: &Path, godot: PathBuf) -> Arc<ToolManager> {
    Arc::new(
        ToolManager::new(BridgeClient::new(free_port()), CliRunner::new(godot))
            .with_working_dir(cwd.to_path_buf())
            .with_lsp_port(free_port()),
    )
}

#[tokio::test]
async fn creates_a_script_from_the_given_content_and_validates_it() {
    let project = project_dir();
    let engine = tempfile::tempdir().expect("engine dir");
    let cwd = tempfile::tempdir().expect("working dir");
    let content = "extends SceneTree\n\nfunc _init() -> void:\n\tquit()\n";
    let result = checked_manager(cwd.path(), quiet_godot(engine.path()))
        .call_tool(
            "godot_create_script",
            json!({"script_path": "res://tools/patch.gd", "content": content,
                   "project_path": project.path()}),
        )
        .await;
    assert!(!result.is_error, "{}", text(&result));
    let shown = text(&result);
    assert!(shown.contains("from the given content"), "{shown}");
    assert!(shown.contains("No parse errors"), "{shown}");
    assert!(!shown.contains("extending 'Node'"), "{shown}");
    assert_eq!(
        std::fs::read_to_string(project.path().join("tools/patch.gd")).expect("script"),
        content
    );
}

#[tokio::test]
async fn content_that_does_not_validate_is_reported_as_an_error_and_kept() {
    let project = project_dir();
    let engine = tempfile::tempdir().expect("engine dir");
    let cwd = tempfile::tempdir().expect("working dir");
    // The stand-in engine writes to stderr, which the headless check reads as a parse error.
    let result = checked_manager(cwd.path(), fake_godot(engine.path()))
        .call_tool(
            "godot_create_script",
            json!({"script_path": "res://broken.gd", "content": "extends Node\nfunc (\n",
                   "project_path": project.path()}),
        )
        .await;
    assert!(result.is_error);
    let shown = text(&result);
    assert!(shown.contains("does not validate"), "{shown}");
    assert!(shown.contains("NOISE_ON_STDERR"), "{shown}");
    assert!(shown.contains("godot_edit_script"), "{shown}");
    assert_eq!(
        std::fs::read_to_string(project.path().join("broken.gd")).expect("script"),
        "extends Node\nfunc (\n"
    );
}

#[tokio::test]
async fn content_is_written_but_says_so_when_no_engine_can_check_it() {
    let project = project_dir();
    let cwd = tempfile::tempdir().expect("working dir");
    let result = checked_manager(
        cwd.path(),
        PathBuf::from("godot-binary-that-does-not-exist"),
    )
    .call_tool(
        "godot_create_script",
        json!({"script_path": "res://unchecked.gd", "content": "extends Node\n",
               "project_path": project.path()}),
    )
    .await;
    assert!(!result.is_error, "{}", text(&result));
    let shown = text(&result);
    assert!(shown.contains("not validated"), "{shown}");
    assert!(shown.contains("godot_validate_script"), "{shown}");
    assert!(project.path().join("unchecked.gd").exists());
}

#[tokio::test]
async fn content_and_header_parameters_are_not_mixed() {
    let project = project_dir();
    for header in [
        json!({"extends_class": "Node2D"}),
        json!({"class_name": "Patch"}),
    ] {
        let mut args = json!({"script_path": "res://mixed.gd", "content": "extends Node\n",
                              "project_path": project.path()});
        args.as_object_mut()
            .expect("object")
            .extend(header.as_object().expect("object").clone());
        let result = call("godot_create_script", args).await;
        assert!(result.is_error);
        assert!(
            text(&result).contains("either content or"),
            "{}",
            text(&result)
        );
        assert!(!project.path().join("mixed.gd").exists());
    }
}

#[tokio::test]
async fn an_unknown_argument_is_refused_with_its_name_and_the_accepted_ones() {
    let project = project_dir();
    let result = call(
        "godot_create_script",
        json!({"script_path": "res://patch.gd", "contents": "extends SceneTree\n",
               "project_path": project.path()}),
    )
    .await;
    assert!(result.is_error);
    let shown = text(&result);
    assert!(shown.contains("Unknown parameter 'contents'"), "{shown}");
    for accepted in [
        "'script_path'",
        "'file_path'",
        "'extends_class'",
        "'overwrite'",
    ] {
        assert!(shown.contains(accepted), "{accepted} missing: {shown}");
    }
    assert!(
        !project.path().join("patch.gd").exists(),
        "nothing may be written for a call that was refused"
    );
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
async fn validate_script_says_warnings_were_not_checked_when_no_language_server_listens() {
    let project = project_dir();
    write_script(
        project.path(),
        "seed.gd",
        b"extends Node\nfunc _ready() -> void:\n    var seed := 1\n",
    );
    let engine = tempfile::tempdir().expect("engine dir");
    let cwd = tempfile::tempdir().expect("working dir");
    let tools = Arc::new(
        ToolManager::new(
            BridgeClient::new(free_port()),
            CliRunner::new(quiet_godot(engine.path())),
        )
        .with_working_dir(cwd.path().to_path_buf())
        .with_lsp_port(free_port()),
    );
    let result = tools
        .call_tool(
            "godot_validate_script",
            json!({"script_path": "res://seed.gd", "project_path": project.path()}),
        )
        .await;
    assert!(!result.is_error, "{}", text(&result));
    let shown = text(&result);
    assert!(
        shown.contains("No parse errors (headless check)"),
        "{shown}"
    );
    assert!(shown.contains("warnings were not checked"), "{shown}");
    assert!(shown.contains("godot_code_diagnostics"), "{shown}");
    assert!(!shown.contains("SHADOWED_GLOBAL_IDENTIFIER"), "{shown}");

    let tools = ToolManager::list_tools();
    let validate = tools
        .iter()
        .find(|tool| tool.name == "godot_validate_script")
        .expect("validate tool");
    assert!(validate.description.contains("godot_code_diagnostics"));
    assert!(validate.description.contains("warning"));
    let diagnostics = tools
        .iter()
        .find(|tool| tool.name == "godot_code_diagnostics")
        .expect("diagnostics tool");
    assert!(diagnostics.description.contains("godot_validate_script"));
    assert!(diagnostics.description.contains("warning"));
}

/// A Godot stand-in that exits cleanly and prints nothing, so a headless check looks successful.
fn quiet_godot(dir: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        let path = dir.join("quiet_godot.bat");
        std::fs::write(&path, "@echo off\r\nexit /b 0\r\n").expect("quiet godot");
        path
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join("quiet_godot.sh");
        std::fs::write(&path, "#!/bin/sh\nexit 0\n").expect("quiet godot");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .expect("chmod quiet godot");
        path
    }
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

#[test]
fn node_editing_tools_accept_make_editable() {
    let tools = ToolManager::list_tools();
    for name in [
        "godot_set_node_properties",
        "godot_add_node",
        "godot_attach_script",
        "godot_connect_signal",
    ] {
        let tool = tools
            .iter()
            .find(|tool| tool.name == name)
            .unwrap_or_else(|| panic!("{name} is listed"));
        assert_eq!(
            tool.input_schema["properties"]["make_editable"]["type"], "boolean",
            "{name}: {}",
            tool.input_schema
        );
    }
}

#[test]
fn node_property_tools_describe_the_resource_step() {
    let tools = ToolManager::list_tools();
    let schema = |name: &str| {
        tools
            .iter()
            .find(|tool| tool.name == name)
            .unwrap_or_else(|| panic!("{name} is listed"))
    };
    let get = schema("godot_get_node_properties");
    assert_eq!(
        get.input_schema["properties"]["properties"]["items"]["type"], "string",
        "{}",
        get.input_schema
    );
    for name in ["godot_get_node_properties", "godot_set_node_properties"] {
        let tool = schema(name);
        let description = &tool.description;
        assert!(
            description.contains("material_override:shader_parameter/"),
            "{name}: {description}"
        );
    }
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
    // A setting the engine does not define keeps the type of the value.
    let engine = tempfile::tempdir().expect("engine dir");
    let (unknown, _) = common::setting_godot(engine.path(), "{}");
    let added = set_with_engine(
        unknown,
        json!({"key": "custom/new/ratio", "value": 0.5, "project_path": project.path()}),
    )
    .await;
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

/// Calls `godot_set_project_setting` with no editor and the given Godot executable.
async fn set_with_engine(godot: PathBuf, args: Value) -> ToolResult {
    let cwd = tempfile::tempdir().expect("working dir");
    manager_in(cwd.path(), godot)
        .call_tool("godot_set_project_setting", args)
        .await
}

const FROG_PROJECT: &str = "config_version=5\n\n[application]\n\nconfig/name=\"Frog\"\nrun/main_scene=\"res://main.tscn\"\n\n[display]\n\nwindow/stretch/mode=\"viewport\"\n";

#[tokio::test]
async fn project_setting_without_an_editor_stores_the_engine_type() {
    let project = project_dir();
    let project_file = project.path().join("project.godot");
    std::fs::write(&project_file, FROG_PROJECT).expect("project.godot");
    let engine = tempfile::tempdir().expect("engine dir");
    let (godot, request) = common::setting_godot(
        engine.path(),
        r#"{"type":"Color","text":"Color(0.0627451, 0.16862746, 0.21960784, 1)"}"#,
    );

    let result = set_with_engine(
        godot,
        json!({"key": "application/boot_splash/bg_color", "value": "#102b38",
               "project_path": project.path()}),
    )
    .await;

    assert!(!result.is_error, "{}", text(&result));
    // The engine is asked about the setting itself: nothing is stored for it yet.
    let asked: Value =
        serde_json::from_str(&std::fs::read_to_string(&request).expect("request")).expect("json");
    assert_eq!(
        asked,
        json!({"key": "application/boot_splash/bg_color", "value": "#102b38"})
    );
    assert_eq!(
        std::fs::read_to_string(&project_file).expect("project.godot"),
        FROG_PROJECT.replace(
            "run/main_scene=\"res://main.tscn\"\n",
            "run/main_scene=\"res://main.tscn\"\nboot_splash/bg_color=Color(0.0627451, 0.16862746, 0.21960784, 1)\n",
        )
    );
}

#[tokio::test]
async fn project_setting_without_an_editor_asks_about_a_stored_structured_value() {
    let project = project_dir();
    let project_file = project.path().join("project.godot");
    let before = "config_version=5\n\n[application]\n\nboot_splash/bg_color=Color(0, 0, 0, 1)\n";
    std::fs::write(&project_file, before).expect("project.godot");
    let engine = tempfile::tempdir().expect("engine dir");
    let (godot, request) = common::setting_godot(
        engine.path(),
        r#"{"type":"Color","text":"Color(1, 1, 1, 1)"}"#,
    );

    let result = set_with_engine(
        godot,
        json!({"key": "application/boot_splash/bg_color", "value": "white",
               "project_path": project.path()}),
    )
    .await;

    assert!(!result.is_error, "{}", text(&result));
    let asked: Value =
        serde_json::from_str(&std::fs::read_to_string(&request).expect("request")).expect("json");
    assert_eq!(asked["stored"], "Color(0, 0, 0, 1)", "{asked}");
    assert_eq!(
        std::fs::read_to_string(&project_file).expect("project.godot"),
        "config_version=5\n\n[application]\n\nboot_splash/bg_color=Color(1, 1, 1, 1)\n"
    );
}

#[tokio::test]
async fn project_setting_without_an_editor_refuses_a_value_of_another_type() {
    let project = project_dir();
    let project_file = project.path().join("project.godot");
    std::fs::write(&project_file, FROG_PROJECT).expect("project.godot");
    let engine = tempfile::tempdir().expect("engine dir");
    let set = |answer: &str, key: &str, value: Value| {
        let (godot, _) = common::setting_godot(engine.path(), answer);
        set_with_engine(
            godot,
            json!({"key": key, "value": value, "project_path": project.path()}),
        )
    };

    let color = set(
        r#"{"type":"Color"}"#,
        "application/boot_splash/bg_color",
        json!("not a colour"),
    )
    .await;
    assert!(color.is_error, "{}", text(&color));
    assert!(text(&color).contains("expects a Color"), "{}", text(&color));
    let width = set(
        r#"{"type":"int"}"#,
        "display/window/size/viewport_width",
        json!("1280"),
    )
    .await;
    assert!(width.is_error, "{}", text(&width));
    assert!(text(&width).contains("expects a int"), "{}", text(&width));
    let fraction = set(
        r#"{"type":"int"}"#,
        "display/window/size/viewport_width",
        json!(1280.5),
    )
    .await;
    assert!(fraction.is_error, "{}", text(&fraction));
    assert_eq!(
        std::fs::read_to_string(&project_file).expect("project.godot"),
        FROG_PROJECT,
        "a refused value changed project.godot"
    );

    // An int stays an int, also when it comes as 1280.0.
    let stored = set(
        r#"{"type":"int"}"#,
        "display/window/size/viewport_width",
        json!(1280.0),
    )
    .await;
    assert!(!stored.is_error, "{}", text(&stored));
    let text_after = std::fs::read_to_string(&project_file).expect("project.godot");
    assert!(
        text_after.ends_with("window/stretch/mode=\"viewport\"\nwindow/size/viewport_width=1280\n"),
        "{text_after}"
    );
}

#[tokio::test]
async fn project_setting_without_an_editor_or_engine_changes_nothing() {
    let project = project_dir();
    let project_file = project.path().join("project.godot");
    std::fs::write(&project_file, FROG_PROJECT).expect("project.godot");

    let result = call(
        "godot_set_project_setting",
        json!({"key": "application/boot_splash/bg_color", "value": "#102b38",
               "project_path": project.path()}),
    )
    .await;

    assert!(result.is_error, "{}", text(&result));
    assert!(text(&result).contains("GODOT_PATH"), "{}", text(&result));
    assert_eq!(
        std::fs::read_to_string(&project_file).expect("project.godot"),
        FROG_PROJECT
    );
}

/// The real engine's answers for settings it defines, with the Godot build of the line.
#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn project_setting_without_an_editor_converts_with_the_real_engine() {
    let godot = PathBuf::from(std::env::var("GODOT_PATH").expect("set GODOT_PATH"));
    let project = project_dir();
    let project_file = project.path().join("project.godot");
    std::fs::write(&project_file, FROG_PROJECT).expect("project.godot");
    for (key, value) in [
        ("application/boot_splash/bg_color", json!("#102b38")),
        ("display/window/size/viewport_width", json!(1280)),
        ("display/window/size/viewport_height.mobile", json!(720.0)),
        ("physics/2d/default_gravity_vector", json!("Vector2(0, 2)")),
        ("custom/tuning/speed", json!(2.5)),
    ] {
        let result = set_with_engine(
            godot.clone(),
            json!({"key": key, "value": value, "project_path": project.path()}),
        )
        .await;
        assert!(!result.is_error, "{key}: {}", text(&result));
    }
    for (key, value) in [
        ("application/boot_splash/bg_color", json!("#zzzzzz")),
        ("application/boot_splash/bg_color", json!(3)),
        ("display/window/size/viewport_width", json!("wide")),
        ("physics/2d/default_gravity_vector", json!("Object(Node)")),
        (
            "physics/2d/default_gravity_vector",
            json!("Vector3(0, 2, 1)"),
        ),
        ("physics/2d/default_gravity_vector", json!(2)),
        ("display/window/size/viewport_width", json!(1280.5)),
    ] {
        let result = set_with_engine(
            godot.clone(),
            json!({"key": key, "value": value, "project_path": project.path()}),
        )
        .await;
        assert!(result.is_error, "{key} = {value}: {}", text(&result));
    }
    let written = std::fs::read_to_string(&project_file).expect("project.godot");
    for line in [
        "\nboot_splash/bg_color=Color(0.0627451, 0.16862746, 0.21960784, 1)\n",
        "\nwindow/size/viewport_width=1280\n",
        "\nwindow/size/viewport_height.mobile=720\n",
        "\n2d/default_gravity_vector=Vector2(0, 2)\n",
        "\n[custom]\n\ntuning/speed=2.5\n",
    ] {
        assert!(written.contains(line), "{line:?} missing from:\n{written}");
    }
    assert!(
        written.contains("run/main_scene=\"res://main.tscn\"\nboot_splash/bg_color=")
            && written.contains("\n\n[display]\n"),
        "{written}"
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
async fn probe_is_listed_read_only_and_checks_its_size_before_reaching_a_game() {
    let probe = ToolManager::list_tools()
        .into_iter()
        .find(|tool| tool.name == "godot_game_probe")
        .expect("godot_game_probe is listed");
    let schema = serde_json::to_value(&probe).expect("tool");
    assert_eq!(schema["annotations"]["readOnlyHint"], true, "{schema}");
    for key in ["rays", "grid", "mask", "exclude", "top", "bottom"] {
        assert!(
            schema["inputSchema"]["properties"].get(key).is_some(),
            "{key}: {schema}"
        );
    }

    let neither = call("godot_game_probe", json!({})).await;
    assert!(neither.is_error, "{}", text(&neither));
    assert!(
        text(&neither).contains("rays or grid"),
        "{}",
        text(&neither)
    );
    let huge = call(
        "godot_game_probe",
        json!({"grid": {"min": [0, 0], "max": [100, 100], "step": 0.5}}),
    )
    .await;
    assert!(huge.is_error, "{}", text(&huge));
    assert!(text(&huge).contains("4096"), "{}", text(&huge));

    let shot = ToolManager::list_tools()
        .into_iter()
        .find(|tool| tool.name == "godot_game_screenshot")
        .expect("screenshot");
    let shot = serde_json::to_value(&shot).expect("tool");
    let aim = &shot["inputSchema"]["properties"]["aim"];
    for key in ["at", "distance", "position", "look_at", "up", "size", "fov"] {
        assert!(aim["properties"].get(key).is_some(), "{key}: {aim}");
    }
    assert_eq!(aim["properties"]["size"]["minimum"], 0.001, "{aim}");
    assert_eq!(aim["properties"]["size"]["maximum"], 10000, "{aim}");
    assert_eq!(aim["properties"]["fov"]["minimum"], 1, "{aim}");
    assert_eq!(aim["properties"]["fov"]["maximum"], 179, "{aim}");
    assert_eq!(aim["additionalProperties"], false, "{aim}");
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
fn a_wait_value_declares_null_so_a_client_does_not_send_it_as_text() {
    let tools = ToolManager::list_tools();
    let schema = |name: &str| {
        tools
            .iter()
            .find(|tool| tool.name == name)
            .unwrap_or_else(|| panic!("no tool {name}"))
            .input_schema
            .clone()
    };
    let wait = schema("godot_game_wait");
    let physics = schema("godot_game_physics");
    for value in [
        &wait["properties"]["value"],
        &physics["properties"]["until"]["properties"]["value"],
    ] {
        let types = value["type"].as_array().expect("value lists its types");
        assert!(types.contains(&json!("null")), "{value}");
        assert!(types.contains(&json!("string")), "{value}");
        assert!(types.contains(&json!("number")), "{value}");
    }
    assert!(
        wait["properties"]["value"]["description"]
            .as_str()
            .unwrap_or("")
            .contains("null"),
        "{wait}"
    );
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
    let cwd = tempfile::tempdir().expect("working dir");
    // A language server already listening must not answer this: the check is the missing binary.
    let tools = Arc::new(
        ToolManager::new(
            BridgeClient::new(free_port()),
            CliRunner::new(PathBuf::from("godot-binary-that-does-not-exist")),
        )
        .with_working_dir(cwd.path().to_path_buf())
        .with_lsp_port(free_port()),
    );
    let result = tools
        .call_tool(
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

#[tokio::test]
async fn run_script_refuses_a_path_outside_the_project_and_a_bad_timeout() {
    let project = project_dir();
    write_script(project.path(), "ok.gd", b"extends SceneTree\n");
    let tools = manager_in(
        project.path(),
        PathBuf::from("godot-binary-that-does-not-exist"),
    );
    let outside_path = std::env::temp_dir().join("godot-mcp-outside-script.gd");
    std::fs::write(&outside_path, b"extends SceneTree\n").expect("outside script");
    let outside = call_with(
        &tools,
        json!({
            "script": outside_path,
            "project_path": project.path()
        }),
    )
    .await;
    assert!(outside.is_error, "{}", text(&outside));
    assert!(
        text(&outside).contains("outside the project"),
        "{}",
        text(&outside)
    );
    let too_long = call_with(
        &tools,
        json!({
            "script": "ok.gd",
            "project_path": project.path(),
            "timeout_seconds": 3601
        }),
    )
    .await;
    assert!(text(&too_long).contains("3600"), "{}", text(&too_long));
}

#[tokio::test]
async fn run_script_timeout_returns_every_line_written_before_the_kill() {
    let project = project_dir();
    write_script(project.path(), "lines.gd", b"extends SceneTree\n");
    let tools = manager_in(project.path(), scripted_godot(project.path()));
    let answer = call_with(
        &tools,
        json!({"script": "lines.gd", "project_path": project.path(), "timeout_seconds": 2}),
    )
    .await;
    assert!(answer.is_error, "{}", text(&answer));
    let answer_text = text(&answer);
    assert!(answer_text.contains("\"timed_out\": true"), "{answer_text}");
    let body = run_script_body(&answer_text);
    let stdout = body["stdout"].as_str().expect("stdout");
    let lines: Vec<&str> = stdout.lines().map(str::trim).collect();
    for index in 1..=200 {
        assert!(
            lines.contains(&format!("out line {index}").as_str()),
            "{stdout}"
        );
    }
    let stderr = body["stderr"].as_str().expect("stderr");
    assert!(
        stderr.contains("err line 1") && stderr.contains("err line 50"),
        "{stderr}"
    );
    assert!(body["stdout_log"].is_null(), "{body}");
    assert!(
        !project.path().join(".godot/godot-mcp").exists(),
        "a run that fits the answer writes no log"
    );
}

#[tokio::test]
async fn run_script_timeout_says_why_a_script_that_printed_nothing_has_no_output() {
    let project = project_dir();
    write_script(project.path(), "quiet.gd", b"extends SceneTree\n");
    let tools = manager_in(project.path(), common::sleeping_godot(project.path()));
    let answer = call_with(
        &tools,
        json!({"script": "quiet.gd", "project_path": project.path(), "timeout_seconds": 1}),
    )
    .await;
    let answer_text = text(&answer);
    assert!(answer.is_error, "{answer_text}");
    assert!(
        answer_text.contains("printed nothing before the kill")
            && answer_text.contains("print each result as it is made"),
        "{answer_text}"
    );
    assert!(!answer_text.contains("(no output)"), "{answer_text}");
    assert_eq!(run_script_body(&answer_text)["stdout_buffered"], false);
}

#[tokio::test]
async fn run_script_timeout_names_a_project_that_buffers_stdout() {
    let project = project_dir();
    std::fs::write(
        project.path().join("project.godot"),
        "config_version=5\n\n[application]\n\nconfig/name=\"Test\"\nrun/flush_stdout_on_print.debug=false\n",
    )
    .expect("project");
    write_script(project.path(), "hang.gd", b"extends SceneTree\n");
    let tools = manager_in(project.path(), scripted_godot(project.path()));
    let answer_text = text(
        &call_with(
            &tools,
            json!({"script": "hang.gd", "project_path": project.path(), "timeout_seconds": 1}),
        )
        .await,
    );
    assert_eq!(run_script_body(&answer_text)["stdout_buffered"], true);
    assert!(
        answer_text.contains("flush_stdout_on_print.debug") && answer_text.contains("printerr()"),
        "{answer_text}"
    );
}

#[tokio::test]
async fn run_script_keeps_a_stream_that_outgrows_the_answer_in_a_log() {
    let project = project_dir();
    write_script(project.path(), "spill.gd", b"extends SceneTree\n");
    let tools = manager_in(project.path(), scripted_godot(project.path()));
    let answer = call_with(
        &tools,
        json!({"script": "spill.gd", "project_path": project.path(), "timeout_seconds": 60}),
    )
    .await;
    let answer_text = text(&answer);
    let body = run_script_body(&answer_text);
    let stdout = body["stdout"].as_str().expect("stdout");
    assert!(stdout.contains("spill line 6000"), "tail lost");
    assert!(
        !stdout.contains("spill line 1 with"),
        "the answer is bounded"
    );
    let bytes = body["stdout_bytes"].as_u64().expect("stdout_bytes");
    assert!(bytes > 128 * 1024, "{bytes}");
    let log = body["stdout_log"].as_str().expect("stdout_log");
    assert!(log.contains("script-run.stdout.log"), "{log}");
    assert!(
        answer_text.contains("script-run.stdout.log"),
        "{answer_text}"
    );
    let whole = std::fs::read_to_string(log).expect("log");
    assert_eq!(whole.len() as u64, bytes);
    assert!(
        whole.contains("spill line 1 with"),
        "the head of the stream"
    );
    assert!(whole.contains("spill line 6000"), "the end of the stream");

    // The next run that fits the answer leaves no stale log behind.
    write_script(project.path(), "boom.gd", b"extends SceneTree\n");
    let next = call_with(
        &tools,
        json!({"script": "boom.gd", "project_path": project.path()}),
    )
    .await;
    assert!(run_script_body(&text(&next))["stdout_log"].is_null());
    assert!(!std::path::Path::new(log).exists(), "stale log kept");
}

/// The JSON document of a `godot_run_script` answer, without the hints around it.
fn run_script_body(answer: &str) -> Value {
    let start = answer.find("{\n").expect("answer body");
    let end = answer.rfind("\n}").expect("answer body") + 2;
    serde_json::from_str(&answer[start..end]).expect("answer body parses")
}

#[tokio::test]
async fn run_script_passes_fixed_fps_to_godot_only_when_asked() {
    let project = project_dir();
    write_script(
        project.path(),
        "boom.gd",
        b"extends SceneTree\nfunc _initialize():\n\tquit()\n",
    );
    let godot = scripted_godot(project.path());
    let tools = manager_in(project.path(), godot);

    let fast = text(
        &call_with(
            &tools,
            json!({"script": "boom.gd", "project_path": project.path(), "fixed_fps": 60}),
        )
        .await,
    );
    assert!(fast.contains("--headless --fixed-fps 60 --path"), "{fast}");
    assert!(fast.contains("\"fixed_fps\": 60"), "{fast}");

    let real_time = text(
        &call_with(
            &tools,
            json!({"script": "boom.gd", "project_path": project.path()}),
        )
        .await,
    );
    assert!(!real_time.contains("--fixed-fps"), "{real_time}");
    assert!(real_time.contains("\"fixed_fps\": null"), "{real_time}");
}

#[tokio::test]
async fn run_script_refuses_a_fixed_fps_that_is_not_a_whole_number_in_range() {
    let project = project_dir();
    write_script(project.path(), "ok.gd", b"extends SceneTree\n");
    let godot = project.path().join("godot-binary-that-does-not-exist");
    let tools = manager_in(project.path(), godot);
    for bad in [json!(0), json!(1001), json!(-60), json!(1.5), json!("60")] {
        let answer = call_with(
            &tools,
            json!({"script": "ok.gd", "project_path": project.path(), "fixed_fps": bad}),
        )
        .await;
        assert!(answer.is_error, "{bad}: {}", text(&answer));
        assert!(
            text(&answer).contains("fixed_fps"),
            "{bad}: {}",
            text(&answer)
        );
    }
}

#[tokio::test]
async fn run_script_timeout_names_fixed_fps_unless_it_was_used() {
    let project = project_dir();
    write_script(project.path(), "hang.gd", b"extends SceneTree\n");
    let godot = scripted_godot(project.path());
    let tools = manager_in(project.path(), godot);
    let hint = "pass fixed_fps: 60";
    let real_time = text(
        &call_with(
            &tools,
            json!({"script": "hang.gd", "project_path": project.path(), "timeout_seconds": 1}),
        )
        .await,
    );
    assert!(real_time.contains(hint), "{real_time}");
    let fast = text(
        &call_with(
            &tools,
            json!({
                "script": "hang.gd",
                "project_path": project.path(),
                "timeout_seconds": 1,
                "fixed_fps": 60
            }),
        )
        .await,
    );
    assert!(!fast.contains(hint), "{fast}");
}

#[tokio::test]
async fn run_script_parses_script_errors_and_kills_a_hang() {
    let project = project_dir();
    write_script(
        project.path(),
        "boom.gd",
        b"extends SceneTree\nfunc _initialize():\n\tassert(false)\n",
    );
    write_script(
        project.path(),
        "hang.gd",
        b"extends SceneTree\nfunc _initialize():\n\tprint('HANGING')\n",
    );
    let godot = scripted_godot(project.path());
    let tools = manager_in(project.path(), godot);

    let failed = call_with(
        &tools,
        json!({
            "script": "boom.gd",
            "project_path": project.path(),
            "user_args": ["--from-test"],
            "timeout_seconds": 10
        }),
    )
    .await;
    assert!(failed.is_error, "{}", text(&failed));
    let failed_text = text(&failed);
    assert!(
        failed_text.contains("res://scripts/boom.gd") && failed_text.contains("\"line\": 4"),
        "{failed_text}"
    );
    assert!(failed_text.contains("exit_code"), "{failed_text}");
    assert!(
        failed_text.contains("--from-test"),
        "user args were not passed after --: {failed_text}"
    );

    let started = std::time::Instant::now();
    let hung = call_with(
        &tools,
        json!({
            "script": "hang.gd",
            "project_path": project.path(),
            "timeout_seconds": 1
        }),
    )
    .await;
    assert!(
        started.elapsed() < std::time::Duration::from_secs(10),
        "the hang was not killed"
    );
    assert!(hung.is_error, "{}", text(&hung));
    let hung_text = text(&hung);
    assert!(
        hung_text.contains("killed the process tree") && hung_text.contains("HANGING"),
        "{hung_text}"
    );
    assert!(hung_text.contains("\"timed_out\": true"), "{hung_text}");

    write_script(
        project.path(),
        "idle.gd",
        b"extends SceneTree\nfunc _init():\n\tpass\n",
    );
    let started_idle = std::time::Instant::now();
    let idle = call_with(
        &tools,
        json!({
            "script": "idle.gd",
            "project_path": project.path(),
            "timeout_seconds": 30
        }),
    )
    .await;
    assert!(
        started_idle.elapsed() < std::time::Duration::from_secs(5),
        "a SCRIPT ERROR that leaves the process idle waited out the timeout ({:?})",
        started_idle.elapsed()
    );
    let idle_text = text(&idle);
    assert!(idle.is_error, "{idle_text}");
    assert!(
        idle_text.contains("main loop was idle") && idle_text.contains("res://idle.gd"),
        "{idle_text}"
    );
    assert!(idle_text.contains("\"line\": 3"), "{idle_text}");
    assert!(
        idle_text.contains("\"timed_out\": false") && idle_text.contains("\"stopped_idle\": true"),
        "{idle_text}"
    );
}

#[tokio::test]
async fn run_script_refuses_a_script_that_does_not_extend_scene_tree_before_starting_godot() {
    let project = project_dir();
    let root = project.path();
    std::fs::create_dir_all(root.join(".godot")).unwrap();
    std::fs::write(
        root.join(".godot/global_script_class_cache.cfg"),
        "list=[{\n\"base\": &\"Node\",\n\"class\": &\"Pond\",\n\"icon\": \"\",\n\"language\": &\"GDScript\",\n\"path\": \"res://pond.gd\"\n}, {\n\"base\": &\"SceneTree\",\n\"class\": &\"Checker\",\n\"icon\": \"\",\n\"language\": &\"GDScript\",\n\"path\": \"res://checker.gd\"\n}]\n",
    )
    .unwrap();
    write_script(root, "pond.gd", b"class_name Pond\nextends Node\n");
    write_script(
        root,
        "checker.gd",
        b"class_name Checker extends SceneTree\n",
    );
    write_script(root, "node_base.gd", b"extends Node2D\n");
    write_script(root, "tree_base.gd", b"@tool\nextends SceneTree\n");
    // The binary does not exist, so a script that gets past the check fails to start instead.
    let tools = manager_in(root, PathBuf::from("godot-binary-that-does-not-exist"));
    let refused = [
        (
            "node.gd",
            "extends Node\nfunc _init():\n\tprint('hi')\n",
            "Node",
        ),
        (
            "bare.gd",
            "# Patches a shader.\nfunc _init():\n\tpass\n",
            "no extends",
        ),
        (
            "tool.gd",
            "@tool\n@icon(\"res://icon.svg\")\nextends Control\n",
            "Control",
        ),
        ("by_path.gd", "extends \"res://node_base.gd\"\n", "Node2D"),
        ("by_class.gd", "extends Pond\n", "Node"),
        ("unknown.gd", "extends Frog\n", "Frog"),
    ];
    for (name, source, base) in refused {
        write_script(root, name, source.as_bytes());
        let result = call_with(&tools, json!({"script": name, "project_path": root})).await;
        let body = text(&result);
        assert!(result.is_error, "{name}: {body}");
        assert!(
            body.contains(&format!("res://{name}"))
                && body.contains(base)
                && body.contains("SceneTree or MainLoop")
                && !body.contains("Failed to run the script"),
            "{name} was not refused before Godot started: {body}"
        );
    }
    let accepted = [
        ("tree.gd", "## Runs once.\nextends SceneTree\n"),
        ("loop.gd", "extends MainLoop\n"),
        ("named.gd", "class_name Named extends SceneTree\n"),
        ("chain_path.gd", "extends \"tree_base.gd\"\n"),
        ("chain_class.gd", "extends Checker\n"),
        ("inner.gd", "extends \"res://tree_base.gd\".Inner\n"),
    ];
    for (name, source) in accepted {
        write_script(root, name, source.as_bytes());
        let result = call_with(&tools, json!({"script": name, "project_path": root})).await;
        let body = text(&result);
        assert!(
            body.contains("Failed to run the script"),
            "{name} was refused by the base class check: {body}"
        );
    }
}

const POND_EDITOR_SAVED: &str = "[gd_scene load_steps=2 format=3 uid=\"uid://cpond\"]\n\n[ext_resource type=\"Shader\" uid=\"uid://dshader\" path=\"res://water.gdshader\" id=\"1_a\"]\n\n[sub_resource type=\"ShaderMaterial\" id=\"ShaderMaterial_a\"]\nshader = ExtResource(\"1_a\")\nshader_parameter/speed = 2.0\n\n[node name=\"Pond\" type=\"Node3D\"]\n";

/// What Godot 4.8 writes for the same scene after a headless load, edit and save.
const POND_HEADLESS_SAVED: &str = "[gd_scene format=3]\n\n[ext_resource type=\"Shader\" path=\"res://water.gdshader\" id=\"1_a\"]\n\n[sub_resource type=\"ShaderMaterial\" id=\"ShaderMaterial_a\"]\nshader = ExtResource(\"1_a\")\nshader_parameter/tint = null\nshader_parameter/speed = 3.0\n\n[node name=\"Pond\" type=\"Node3D\" unique_id=1]\n";

fn pond_project() -> tempfile::TempDir {
    let project = project_dir();
    let root = project.path();
    std::fs::write(root.join("water.gdshader.uid"), "uid://dshader\n").expect("uid");
    std::fs::write(root.join("pond.tscn"), POND_EDITOR_SAVED).expect("scene");
    std::fs::write(root.join("headless.txt"), POND_HEADLESS_SAVED).expect("headless output");
    write_script(root, "edit.gd", b"extends SceneTree\n");
    write_script(root, "lock.gd", b"extends SceneTree\n");
    write_script(root, "quiet.gd", b"extends SceneTree\n");
    project
}

fn answer_body(answer: &str) -> Value {
    serde_json::from_str(&answer[answer.find('{').expect("json")..])
        .expect("answer is JSON after any warning line")
}

#[tokio::test]
async fn run_script_writes_back_the_uids_a_headless_save_dropped() {
    let project = pond_project();
    let root = project.path();
    let tools = manager_in(root, saving_godot(root));

    let quiet = call_with(&tools, json!({"script": "quiet.gd", "project_path": root})).await;
    let quiet_text = text(&quiet);
    assert!(!quiet.is_error, "{quiet_text}");
    let body = answer_body(&quiet_text);
    assert_eq!(body["resource_repairs"], json!([]), "{quiet_text}");
    assert_eq!(body["resource_damage"], json!([]), "{quiet_text}");
    assert!(!quiet_text.contains("WARNING"), "{quiet_text}");

    let saved = call_with(&tools, json!({"script": "edit.gd", "project_path": root})).await;
    let saved_text = text(&saved);
    assert!(!saved.is_error, "{saved_text}");
    assert!(!saved_text.contains("WARNING"), "{saved_text}");
    let body = answer_body(&saved_text);
    assert_eq!(
        body["resource_repairs"],
        json!([{
            "path": "res://pond.tscn",
            "uid": "uid://cpond",
            "new_uid": false,
            "ext_resource_uids_restored": 1,
            "removed_null_shader_parameters": ["tint"],
        }])
    );
    assert_eq!(body["resource_repairs_total"], 1, "{saved_text}");
    assert_eq!(body["resource_damage"], json!([]), "{saved_text}");
    assert_eq!(
        body["editor_refresh"],
        Value::Null,
        "no editor is connected"
    );
    assert!(
        body["summary"].as_str().is_some_and(|summary| summary
            .contains("repaired 1 file, res://pond.tscn: 1 uid written back, 1 ext_resource uid restored, 1 null shader parameter removed; resource_repairs lists them.")),
        "{saved_text}"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("pond.tscn")).expect("pond"),
        POND_HEADLESS_SAVED
            .replace(
                "[gd_scene format=3]",
                "[gd_scene format=3 uid=\"uid://cpond\"]"
            )
            .replace(
                "type=\"Shader\" path",
                "type=\"Shader\" uid=\"uid://dshader\" path"
            )
            .replace("shader_parameter/tint = null\n", "")
    );
}

/// A script that saves many scenes gets a short answer: the summary counts the repairs and names
/// a few, and resource_repairs lists the first ones with the total.
#[tokio::test]
async fn run_script_keeps_the_answer_short_when_it_repairs_many_files() {
    let project = pond_project();
    let root = project.path();
    write_script(root, "many.gd", b"extends SceneTree\n");
    let tools = manager_in(root, saving_godot(root));

    let run = call_with(&tools, json!({"script": "many.gd", "project_path": root})).await;
    let run_text = text(&run);
    assert!(!run.is_error, "{run_text}");
    let body = answer_body(&run_text);
    assert_eq!(body["resource_repairs_total"], 25, "{run_text}");
    let repairs = body["resource_repairs"].as_array().expect("repairs");
    assert_eq!(repairs.len(), 20, "{run_text}");
    let first = &repairs[0];
    assert_eq!(first["path"], "res://many/pond_01.tscn", "{run_text}");
    assert_eq!(first["new_uid"], true, "{run_text}");
    assert!(
        first["uid"]
            .as_str()
            .is_some_and(|uid| uid.starts_with("uid://")),
        "{run_text}"
    );
    assert_eq!(first["ext_resource_uids_restored"], 1, "{run_text}");
    assert_eq!(
        first["removed_null_shader_parameters"],
        json!(["tint"]),
        "{run_text}"
    );
    let summary = body["summary"].as_str().unwrap_or_default();
    assert!(
        summary.contains("repaired 25 files, among them res://many/pond_01.tscn, res://many/pond_02.tscn, res://many/pond_03.tscn and 22 more: 25 uids written back (25 new), 25 ext_resource uids restored, 25 null shader parameters removed; resource_repairs lists the first 20 of 25 (resource_repairs_total)."),
        "{summary}"
    );
    assert!(!summary.contains("pond_04"), "{summary}");
    assert!(
        run_text.len() < 8_000,
        "{} bytes: {run_text}",
        run_text.len()
    );
}

/// `.godot/uid_cache.bin` as Godot's `ResourceUID::save_to_cache` writes it.
fn uid_cache_bytes(entries: &[(u64, &str)]) -> Vec<u8> {
    let mut bytes = (entries.len() as u32).to_le_bytes().to_vec();
    for (id, path) in entries {
        bytes.extend_from_slice(&id.to_le_bytes());
        bytes.extend_from_slice(&(path.len() as u32).to_le_bytes());
        bytes.extend_from_slice(path.as_bytes());
    }
    bytes
}

/// The `(id, path)` entries of a uid cache file.
fn uid_cache_entries(bytes: &[u8]) -> Vec<(u64, String)> {
    let count = u32::from_le_bytes(bytes[0..4].try_into().expect("count"));
    let mut at = 4;
    let mut entries = Vec::new();
    for _ in 0..count {
        let id = u64::from_le_bytes(bytes[at..at + 8].try_into().expect("id"));
        let length =
            u32::from_le_bytes(bytes[at + 8..at + 12].try_into().expect("length")) as usize;
        let path = String::from_utf8(bytes[at + 12..at + 12 + length].to_vec()).expect("path");
        at += 12 + length;
        entries.push((id, path));
    }
    assert_eq!(at, bytes.len(), "no bytes after the entries");
    entries
}

/// Godot's `ResourceUID::text_to_id`.
fn uid_text_to_id(text: &str) -> u64 {
    text.trim_start_matches("uid://")
        .bytes()
        .fold(0u64, |id, byte| {
            let digit = if byte.is_ascii_lowercase() {
                byte - b'a'
            } else {
                byte - b'0' + 25
            };
            id * 34 + u64::from(digit)
        })
        & 0x7FFF_FFFF_FFFF_FFFF
}

/// New files get new uids, and only the editor writes the uid cache: without them in it, the next
/// script that loads the files prints an "invalid UID" warning per reference. With no editor
/// holding the project, the server records them in the cache itself.
#[tokio::test]
async fn run_script_records_the_new_uids_in_the_uid_cache() {
    let project = pond_project();
    let root = project.path();
    write_script(root, "many.gd", b"extends SceneTree\n");
    std::fs::create_dir_all(root.join(".godot")).expect(".godot");
    let shader_id = uid_text_to_id("uid://dshader");
    std::fs::write(
        root.join(".godot/uid_cache.bin"),
        uid_cache_bytes(&[(shader_id, "res://water.gdshader")]),
    )
    .expect("uid cache");
    let tools = manager_in(root, saving_godot(root));

    let run = call_with(&tools, json!({"script": "many.gd", "project_path": root})).await;
    let run_text = text(&run);
    assert!(!run.is_error, "{run_text}");
    let body = answer_body(&run_text);
    assert_eq!(
        body["uid_cache"],
        json!({"file": ".godot/uid_cache.bin", "recorded": 25}),
        "{run_text}"
    );
    assert_eq!(body["uid_cache_stale"], Value::Null, "{run_text}");
    assert!(
        body["summary"]
            .as_str()
            .is_some_and(|summary| summary.contains("Recorded 25 uids in .godot/uid_cache.bin")),
        "{run_text}"
    );
    let cache =
        uid_cache_entries(&std::fs::read(root.join(".godot/uid_cache.bin")).expect("cache"));
    assert_eq!(cache.len(), 26, "{cache:?}");
    assert_eq!(cache[0], (shader_id, "res://water.gdshader".to_string()));
    for index in 1..=25 {
        let path = format!("res://many/pond_{index:02}.tscn");
        let scene = std::fs::read_to_string(root.join(format!("many/pond_{index:02}.tscn")))
            .expect("scene");
        let uid = scene
            .split("uid=\"")
            .nth(1)
            .and_then(|rest| rest.split('"').next())
            .expect("header uid");
        assert!(
            cache.contains(&(uid_text_to_id(uid), path.clone())),
            "{path} {uid} in {cache:?}"
        );
    }

    // A second run that rewrites the same files records nothing new.
    let again = call_with(&tools, json!({"script": "edit.gd", "project_path": root})).await;
    let body = answer_body(&text(&again));
    assert_eq!(
        body["uid_cache"]["recorded"], 1,
        "pond.tscn was not in the cache yet"
    );
    let again = call_with(&tools, json!({"script": "edit.gd", "project_path": root})).await;
    let body = answer_body(&text(&again));
    assert_eq!(body["uid_cache"]["recorded"], 0, "{body}");
}

/// A project the editor never scanned has no uid cache; the server does not make one up, and the
/// answer says the uids are not cached and how to fix it.
#[tokio::test]
async fn run_script_reports_a_missing_uid_cache_as_stale() {
    let project = pond_project();
    let root = project.path();
    let tools = manager_in(root, saving_godot(root));

    let run = call_with(&tools, json!({"script": "edit.gd", "project_path": root})).await;
    let run_text = text(&run);
    assert!(!run.is_error, "{run_text}");
    let body = answer_body(&run_text);
    assert_eq!(body["uid_cache"], Value::Null, "{run_text}");
    let stale = body["uid_cache_stale"].as_str().expect("uid_cache_stale");
    assert!(stale.contains("no .godot/uid_cache.bin"), "{stale}");
    assert!(stale.contains("godot_resave_resources"), "{stale}");
    assert!(!root.join(".godot/uid_cache.bin").exists());
}

/// An editor that holds the project keeps its own uid cache and refreshes it from the repaired
/// files; the server leaves the file to it.
#[tokio::test]
async fn run_script_leaves_the_uid_cache_to_a_connected_editor() {
    let project = pond_project();
    let root = project.path();
    std::fs::create_dir_all(root.join(".godot")).expect(".godot");
    let cache = uid_cache_bytes(&[(uid_text_to_id("uid://dshader"), "res://water.gdshader")]);
    std::fs::write(root.join(".godot/uid_cache.bin"), &cache).expect("uid cache");
    let (_bridge, tools) =
        manager_with_editor(root, &["resave_resources"], saving_godot(root)).await;

    let run = call_with(&tools, json!({"script": "edit.gd", "project_path": root})).await;
    let run_text = text(&run);
    assert!(!run.is_error, "{run_text}");
    let body = answer_body(&run_text);
    assert_eq!(body["resource_repairs_total"], 1, "{run_text}");
    assert_eq!(body["uid_cache"], Value::Null, "{run_text}");
    assert_eq!(body["uid_cache_stale"], Value::Null, "{run_text}");
    assert_eq!(
        std::fs::read(root.join(".godot/uid_cache.bin")).expect("cache"),
        cache
    );
}

/// A script that rewrites a shader with FileAccess leaves no scene to repair, yet the editor still
/// holds the old Shader: saving a scene then pins its old uniform defaults. The server has the
/// connected editor refresh and reload the shader.
#[tokio::test]
async fn run_script_has_the_editor_reload_a_shader_the_script_rewrote() {
    let project = pond_project();
    let root = project.path();
    std::fs::write(
        root.join("water.gdshader"),
        "shader_type spatial;\nuniform float foam_pulse = 0.35;\n",
    )
    .expect("shader");
    std::fs::write(
        root.join("shader.txt"),
        "shader_type spatial;\nuniform float foam_speed = 1.0;\nuniform float foam_pulse = 0.5;\n",
    )
    .expect("new shader");
    write_script(root, "shader.gd", b"extends SceneTree\n");
    let (bridge, tools) =
        manager_with_editor(root, &["resave_resources"], saving_godot(root)).await;

    let run = call_with(&tools, json!({"script": "shader.gd", "project_path": root})).await;
    let run_text = text(&run);
    assert!(!run.is_error, "{run_text}");
    let body = answer_body(&run_text);
    assert_eq!(body["resource_repairs"], json!([]), "{run_text}");
    assert_eq!(
        body["editor_refresh"],
        json!({"from_editor": "resave_resources"}),
        "{run_text}"
    );
    let resave = bridge
        .received()
        .into_iter()
        .find(|command| command["type"] == "resave_resources")
        .expect("the editor was asked to refresh");
    assert_eq!(
        resave["params"]["paths"],
        json!(["res://water.gdshader"]),
        "{resave}"
    );
    assert!(
        body["summary"]
            .as_str()
            .is_some_and(|summary| summary.contains("reloaded the shaders")),
        "{run_text}"
    );

    // A run that writes nothing does not bother the editor.
    let before = bridge.received().len();
    let quiet = call_with(&tools, json!({"script": "quiet.gd", "project_path": root})).await;
    assert!(!quiet.is_error, "{}", text(&quiet));
    assert!(
        bridge.received()[before..]
            .iter()
            .all(|command| command["type"] != "resave_resources"),
        "{:?}",
        bridge.received()
    );
}

/// Gives `root` a res://art/east.glb whose import cache is an hour older than the source, the
/// way a model regenerated outside Godot leaves it.
fn stale_glb(root: &Path) {
    std::fs::create_dir_all(root.join("art")).expect("art");
    std::fs::create_dir_all(root.join(".godot/imported")).expect("cache dir");
    let cache = root.join(".godot/imported/east.glb-abc.scn");
    std::fs::write(root.join("art/east.glb"), "new model").expect("source");
    std::fs::write(&cache, "old import").expect("cache");
    std::fs::write(
        root.join("art/east.glb.import"),
        "path=\"res://.godot/imported/east.glb-abc.scn\"\n",
    )
    .expect("sidecar");
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
    std::fs::File::options()
        .write(true)
        .open(&cache)
        .and_then(|file| file.set_modified(old))
        .expect("age the cache");
}

#[tokio::test]
async fn run_script_warns_about_stale_imports_without_an_editor() {
    let project = pond_project();
    let root = project.path();
    let tools = manager_in(root, saving_godot(root));

    let current = call_with(&tools, json!({"script": "quiet.gd", "project_path": root})).await;
    let current_text = text(&current);
    assert!(!current.is_error, "{current_text}");
    let body = answer_body(&current_text);
    assert_eq!(body["stale_imports"], Value::Null, "{current_text}");
    assert_eq!(body["import_warning"], Value::Null, "{current_text}");

    stale_glb(root);
    let run = call_with(&tools, json!({"script": "quiet.gd", "project_path": root})).await;
    let run_text = text(&run);
    assert!(!run.is_error, "the script still runs: {run_text}");
    assert!(
        run_text.starts_with("Warning: ") && run_text.contains("godot_resave_resources"),
        "{run_text}"
    );
    let body = answer_body(&run_text);
    assert_eq!(
        body["stale_imports"],
        json!({"paths": ["res://art/east.glb"], "total": 1, "reimported": false}),
        "{run_text}"
    );
    let warning = body["import_warning"].as_str().expect("import_warning");
    assert!(
        warning.contains("res://art/east.glb") && warning.contains("godot_resave_resources"),
        "{warning}"
    );
    assert!(
        body["summary"]
            .as_str()
            .is_some_and(|summary| summary.contains(warning)),
        "{run_text}"
    );
}

#[tokio::test]
async fn run_script_has_the_editor_reimport_stale_imports_before_the_script_starts() {
    let project = pond_project();
    let root = project.path();
    stale_glb(root);
    let (bridge, tools) =
        manager_with_editor(root, &["resave_resources"], saving_godot(root)).await;

    let run = call_with(&tools, json!({"script": "quiet.gd", "project_path": root})).await;
    let run_text = text(&run);
    assert!(!run.is_error, "{run_text}");
    let body = answer_body(&run_text);
    assert_eq!(
        body["stale_imports"],
        json!({"paths": ["res://art/east.glb"], "total": 1, "reimported": true}),
        "{run_text}"
    );
    assert_eq!(body["import_warning"], Value::Null, "{run_text}");
    assert!(
        body["summary"].as_str().is_some_and(|summary| summary
            .contains("Reimported stale resources before the run: res://art/east.glb")),
        "{run_text}"
    );
    let resaves: Vec<Value> = bridge
        .received()
        .into_iter()
        .filter(|command| command["type"] == "resave_resources")
        .collect();
    assert_eq!(resaves.len(), 1, "{:?}", bridge.received());
    assert_eq!(
        resaves[0]["params"]["paths"],
        json!(["res://art/east.glb"]),
        "{}",
        resaves[0]
    );
}

#[tokio::test]
async fn run_script_names_the_uids_and_null_shader_parameters_it_could_not_repair() {
    let project = pond_project();
    let root = project.path();
    let tools = manager_in(root, saving_godot(root));

    // lock.gd leaves pond.tscn read-only, so the server cannot write the uids back.
    let saved = call_with(&tools, json!({"script": "lock.gd", "project_path": root})).await;
    let saved_text = text(&saved);
    let pond = root.join("pond.tscn");
    let mut writable = std::fs::metadata(&pond).expect("pond").permissions();
    #[allow(clippy::permissions_set_readonly_false)]
    writable.set_readonly(false);
    std::fs::set_permissions(&pond, writable).expect("unlock");
    assert!(!saved.is_error, "{saved_text}");
    assert!(
        saved_text.starts_with("WARNING") && saved_text.contains("res://pond.tscn"),
        "{saved_text}"
    );
    let body = answer_body(&saved_text);
    assert_eq!(body["resource_repairs"], json!([]), "{saved_text}");
    assert_eq!(
        body["resource_damage"],
        json!([{
            "path": "res://pond.tscn",
            "lost_uid": "uid://cpond",
            "ext_resources_without_uid": [{"path": "res://water.gdshader", "uid": "uid://dshader"}],
            "null_shader_parameters": ["tint"],
        }])
    );
}

/// A Godot stand-in that, for edit.gd and lock.gd, overwrites pond.tscn with headless.txt; lock.gd
/// then makes it read-only. For shader.gd it overwrites water.gdshader with shader.txt, and for
/// many.gd it writes headless.txt to many/pond_01.tscn .. many/pond_25.tscn.
fn saving_godot(dir: &Path) -> PathBuf {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join("saving_godot.sh");
        std::fs::write(
            &path,
            r#"#!/bin/sh
prev=
proj=
for arg in "$@"; do
  if [ "$prev" = "--path" ]; then
    proj=$arg
  fi
  prev=$arg
done
case "$*" in
  *edit.gd*) cp "$proj/headless.txt" "$proj/pond.tscn" ;;
  *lock.gd*) cp "$proj/headless.txt" "$proj/pond.tscn"; chmod a-w "$proj/pond.tscn" ;;
  *shader.gd*) cp "$proj/shader.txt" "$proj/water.gdshader" ;;
  *many.gd*) mkdir -p "$proj/many"; for i in $(seq -w 1 25); do cp "$proj/headless.txt" "$proj/many/pond_$i.tscn"; done ;;
esac
exit 0
"#,
        )
        .expect("write saving godot");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        path
    }
    #[cfg(windows)]
    {
        let path = dir.join("saving_godot.bat");
        std::fs::write(
            &path,
            "@echo off\r\nsetlocal enabledelayedexpansion\r\nset PREV=\r\nset PROJ=\r\nfor %%A in (%*) do (\r\n  if \"!PREV!\"==\"--path\" set PROJ=%%~A\r\n  set PREV=%%A\r\n)\r\necho %* | findstr edit.gd >nul\r\nif %errorlevel%==0 copy /Y \"%PROJ%\\headless.txt\" \"%PROJ%\\pond.tscn\" >nul\r\necho %* | findstr lock.gd >nul\r\nif %errorlevel%==0 (\r\n  copy /Y \"%PROJ%\\headless.txt\" \"%PROJ%\\pond.tscn\" >nul\r\n  attrib +r \"%PROJ%\\pond.tscn\"\r\n)\r\necho %* | findstr shader.gd >nul\r\nif %errorlevel%==0 copy /Y \"%PROJ%\\shader.txt\" \"%PROJ%\\water.gdshader\" >nul\r\necho %* | findstr many.gd >nul\r\nif %errorlevel%==0 (\r\n  mkdir \"%PROJ%\\many\" 2>nul\r\n  for /L %%I in (101,1,125) do (\r\n    set N=%%I\r\n    copy /Y \"%PROJ%\\headless.txt\" \"%PROJ%\\many\\pond_!N:~1!.tscn\" >nul\r\n  )\r\n)\r\nexit /b 0\r\n",
        )
        .expect("write saving godot");
        path
    }
}

#[tokio::test]
async fn run_script_reports_a_script_that_godot_did_not_load_as_a_failure() {
    let project = project_dir();
    write_script(project.path(), "wrongbase.gd", b"extends SceneTree\n");
    write_script(
        project.path(),
        "broken.gd",
        b"extends SceneTree\nfunc _init():\n\tvar x =\n",
    );
    write_script(project.path(), "gone.gd", b"extends SceneTree\n");
    let tools = manager_in(project.path(), scripted_godot(project.path()));

    // Godot 4.8 exits 0 for each of these, so the exit code alone looked like success.
    let wrong = call_with(
        &tools,
        json!({"script": "wrongbase.gd", "project_path": project.path()}),
    )
    .await;
    let wrong_text = text(&wrong);
    assert!(wrong.is_error, "{wrong_text}");
    assert!(wrong_text.contains("did not run"), "{wrong_text}");
    assert!(
        wrong_text.contains("doesn't inherit from SceneTree or MainLoop"),
        "{wrong_text}"
    );
    assert!(
        !wrong_text.contains("\"script_errors\": []"),
        "script_errors stayed empty: {wrong_text}"
    );
    assert!(
        wrong_text.contains("\"file\": \"res://wrongbase.gd\""),
        "{wrong_text}"
    );

    let broken = call_with(
        &tools,
        json!({"script": "broken.gd", "project_path": project.path()}),
    )
    .await;
    let broken_text = text(&broken);
    assert!(broken.is_error, "{broken_text}");
    assert!(
        broken_text.contains("did not run") && broken_text.contains("res://broken.gd:3"),
        "{broken_text}"
    );
    assert!(
        broken_text.contains("\"load_error\": \"Failed to load script"),
        "{broken_text}"
    );

    let gone = call_with(
        &tools,
        json!({"script": "gone.gd", "project_path": project.path()}),
    )
    .await;
    let gone_text = text(&gone);
    assert!(gone.is_error, "{gone_text}");
    assert!(
        gone_text.contains("did not run") && gone_text.contains("Can't load script"),
        "{gone_text}"
    );

    // A script that loads and fails on its own still reports its own error, not a load failure.
    write_script(project.path(), "fine.gd", b"extends SceneTree\n");
    let fine = call_with(
        &tools,
        json!({"script": "fine.gd", "project_path": project.path()}),
    )
    .await;
    let fine_text = text(&fine);
    assert!(
        fine_text.contains("\"load_error\": null") && !fine_text.contains("did not run"),
        "{fine_text}"
    );
}

#[tokio::test]
async fn resave_without_an_editor_imports_headless_and_names_a_missing_sidecar() {
    let project = project_dir();
    let root = project.path();
    std::fs::create_dir_all(root.join("art/models")).unwrap();
    std::fs::write(root.join("art/models/x.glb"), b"glb").unwrap();
    std::fs::write(root.join("art/models/y.glb"), b"glb").unwrap();
    // The imported data the stand-in's sidecar names, as an earlier import of x.glb left it.
    std::fs::create_dir_all(root.join(".godot/imported")).unwrap();
    std::fs::write(root.join(".godot/imported/x.glb-deadbeef.scn"), b"scene").unwrap();
    std::fs::write(
        root.join(".godot/imported/x.glb-deadbeef.md5"),
        // The md5 of b"glb".
        "source_md5=\"f9091f3defa83aede49cdf349390d0b3\"\n",
    )
    .unwrap();
    let tools = manager_in(root, importing_godot(root));
    let result = Arc::clone(&tools)
        .call_tool(
            "godot_resave_resources",
            json!({
                "project_path": root,
                "paths": ["res://art/models/x.glb", "res://art/models/y.glb"]
            }),
        )
        .await;
    let body = text(&result);
    assert!(!result.is_error, "{body}");
    let parsed: Value = serde_json::from_str(&body).expect("json");
    assert_eq!(parsed["mode"], "headless_import");
    assert!(
        parsed["command"]
            .as_str()
            .unwrap_or("")
            .contains("--import"),
        "{body}"
    );
    let imported = parsed["imported"].as_array().expect("imported");
    assert!(
        imported
            .iter()
            .any(|item| item["path"] == "res://art/models/x.glb"),
        "{body}"
    );
    assert!(
        !imported
            .iter()
            .any(|item| item["path"] == "res://art/models/y.glb"),
        "{body}"
    );
    assert_eq!(parsed["imported_count"], 1, "{body}");
    let missing = parsed["not_scanned"].as_array().expect("not_scanned");
    assert!(
        missing.iter().any(|path| path == "res://art/models/y.glb"),
        "{body}"
    );
    assert!(root.join("art/models/x.glb.import").is_file());
    assert!(!root.join("art/models/y.glb.import").exists());
}

/// A model rewritten with new bytes of the same size: the headless import lists it with its kind
/// when Godot reimports it (the sidecar text stays the same), names it under still_stale when
/// Godot's cache skips it, and force makes Godot import it again.
#[cfg(unix)]
#[tokio::test]
async fn resave_lists_a_reimported_model_names_a_skipped_one_and_forces_it() {
    let project = project_dir();
    let root = project.path();
    std::fs::create_dir_all(root.join("art")).unwrap();
    std::fs::write(root.join("art/m.glb"), b"glb one").unwrap();
    let tools = manager_in(root, cached_importing_godot(root));
    let resave = |args: Value| {
        let tools = Arc::clone(&tools);
        async move {
            let result = tools.call_tool("godot_resave_resources", args).await;
            let body = text(&result);
            // An error carries the same JSON between a prefix and its recovery hints.
            let start = body.find('{').unwrap_or(0);
            let end = body.rfind("\n}").map_or(body.len(), |end| end + 2);
            let parsed: Value =
                serde_json::from_str(&body[start..end]).unwrap_or_else(|_| panic!("json: {body}"));
            (result.is_error, parsed, body)
        }
    };
    let paths = json!(["res://art/m.glb"]);

    let (failed, first, body) = resave(json!({"project_path": root, "paths": paths})).await;
    assert!(!failed, "{body}");
    assert_eq!(
        first["imported"],
        json!([{"path": "res://art/m.glb", "kind": "scene", "type": "PackedScene"}]),
        "{body}"
    );
    assert_eq!(first["still_stale"], json!([]), "{body}");

    // New bytes of the same size. This Godot stand-in trusts its cache, as Godot does for a
    // source rewritten within the second its last scan saw.
    std::fs::write(root.join("art/m.glb"), b"glb two").unwrap();
    let (failed, skipped, body) = resave(json!({"project_path": root, "paths": paths})).await;
    assert!(failed, "{body}");
    assert_eq!(skipped["imported"], json!([]), "{body}");
    assert_eq!(
        skipped["still_stale"][0]["path"], "res://art/m.glb",
        "{body}"
    );
    assert_eq!(
        skipped["still_stale"][0]["reason"], "source_changed",
        "{body}"
    );
    assert!(body.contains("force: true"), "{body}");

    let (failed, forced, body) =
        resave(json!({"project_path": root, "paths": paths, "force": true})).await;
    assert!(!failed, "{body}");
    assert_eq!(forced["forced"], json!(["res://art/m.glb"]), "{body}");
    assert_eq!(forced["imported"][0]["path"], "res://art/m.glb", "{body}");
    assert_eq!(forced["imported"][0]["kind"], "scene", "{body}");
    assert_eq!(forced["still_stale"], json!([]), "{body}");
    assert_eq!(
        std::fs::read(root.join(".godot/imported/m.glb-abc.scn")).unwrap(),
        b"glb two"
    );

    let wrong = Arc::clone(&tools)
        .call_tool(
            "godot_resave_resources",
            json!({"project_path": root, "paths": paths, "force": 1}),
        )
        .await;
    let body = text(&wrong);
    assert!(wrong.is_error && body.contains("'force'"), "{body}");
}

/// A Godot stand-in that imports `art/m.glb` the way Godot does when its cache does not vouch for
/// it: only while Godot's `.md5` record of it is missing. It writes the same sidecar text on every
/// import, as Godot does for a model whose options did not change.
#[cfg(unix)]
fn cached_importing_godot(dir: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join("cached_importing_godot.sh");
    std::fs::write(
        &path,
        r#"#!/bin/sh
prev=
proj=
for arg in "$@"; do
  if [ "$prev" = "--path" ]; then
    proj=$arg
  fi
  prev=$arg
done
src="$proj/art/m.glb"
md5="$proj/.godot/imported/m.glb-abc.md5"
[ -f "$src" ] || exit 0
[ -f "$md5" ] && exit 0
mkdir -p "$proj/.godot/imported"
cp "$src" "$proj/.godot/imported/m.glb-abc.scn"
if command -v md5sum >/dev/null 2>&1; then
  sum=$(md5sum "$src" | cut -d' ' -f1)
else
  sum=$(md5 -q "$src")
fi
printf 'source_md5="%s"
dest_md5="0"
' "$sum" > "$md5"
printf '%s
' '[remap]' '' 'importer="scene"' 'type="PackedScene"' 'uid="uid://b1"' \
  'path="res://.godot/imported/m.glb-abc.scn"' > "$src.import"
exit 0
"#,
    )
    .expect("write cached importing godot");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    path
}

#[tokio::test]
async fn resave_without_a_project_names_the_headless_import_command() {
    let result = call("godot_resave_resources", json!({})).await;
    let body = text(&result);
    assert!(result.is_error, "{body}");
    assert!(body.contains("godot --headless --path"), "{body}");
    assert!(body.contains("--import"), "{body}");
}

/// A Godot stand-in that writes an `.import` sidecar for `x.glb` and leaves `y.glb` alone.
fn importing_godot(dir: &Path) -> PathBuf {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join("importing_godot.sh");
        std::fs::write(
            &path,
            r#"#!/bin/sh
prev=
proj=
for arg in "$@"; do
  if [ "$prev" = "--path" ]; then
    proj=$arg
  fi
  prev=$arg
done
if [ -n "$proj" ] && [ -f "$proj/art/models/x.glb" ]; then
  printf '%s\n' 'path="res://.godot/imported/x.glb-deadbeef.scn"' > "$proj/art/models/x.glb.import"
fi
exit 0
"#,
        )
        .expect("write importing godot");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        path
    }
    #[cfg(windows)]
    {
        let path = dir.join("importing_godot.bat");
        std::fs::write(
            &path,
            "@echo off\r\nsetlocal enabledelayedexpansion\r\nset PREV=\r\nset PROJ=\r\nfor %%A in (%*) do (\r\n  if \"!PREV!\"==\"--path\" set PROJ=%%A\r\n  set PREV=%%A\r\n)\r\nif exist \"%PROJ%\\art\\models\\x.glb\" (\r\n  echo path=\"res://.godot/imported/x.glb-deadbeef.scn\" > \"%PROJ%\\art\\models\\x.glb.import\"\r\n)\r\nexit /b 0\r\n",
        )
        .expect("write importing godot");
        path
    }
}

async fn call_with(tools: &Arc<ToolManager>, args: Value) -> ToolResult {
    Arc::clone(tools).call_tool("godot_run_script", args).await
}

/// A Godot stand-in: prints a SCRIPT ERROR for boom.gd, sleeps (with a child) for hang.gd, and
/// prints what Godot 4.8 prints, with exit code 0, when it cannot load wrongbase.gd, broken.gd or
/// gone.gd.
fn scripted_godot(dir: &Path) -> PathBuf {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join("scripted_godot.sh");
        std::fs::write(
            &path,
            r#"#!/bin/sh
echo "ARGS $*"
case "$*" in
  *hang.gd*)
    echo HANGING
    sleep 30 &
    wait
    ;;
  *lines.gd*)
    i=1
    while [ $i -le 200 ]; do echo "out line $i"; i=$((i + 1)); done
    i=1
    while [ $i -le 50 ]; do echo "err line $i" >&2; i=$((i + 1)); done
    sleep 30 &
    wait
    ;;
  *spill.gd*)
    i=1
    while [ $i -le 6000 ]; do echo "spill line $i with some more text to fill it up"; i=$((i + 1)); done
    exit 0
    ;;
  *wrongbase.gd*)
    echo 'ERROR: Can'"'"'t load the script "wrongbase.gd" as it doesn'"'"'t inherit from SceneTree or MainLoop.' >&2
    echo '   at: start (main/main.cpp:4409)' >&2
    exit 0
    ;;
  *broken.gd*)
    echo 'SCRIPT ERROR: Parse Error: Expected expression for variable initial value after "=".' >&2
    echo '          at: GDScript::reload (res://broken.gd:3)' >&2
    echo 'ERROR: Failed to load script "res://broken.gd" with error "Parse error".' >&2
    exit 0
    ;;
  *gone.gd*)
    echo "ERROR: Attempt to open script 'res://gone.gd' resulted in error 'File not found'." >&2
    echo "ERROR: Can't load script: gone.gd" >&2
    exit 0
    ;;
  *idle.gd*)
    echo 'SCRIPT ERROR: Parse Error: unexpected.'
    echo '   at: _init (res://idle.gd:3)'
    sleep 30
    ;;
  *)
    echo 'SCRIPT ERROR: Assertion failed.'
    echo '   at: _initialize (res://scripts/boom.gd:4)'
    exit 3
    ;;
esac
"#,
        )
        .expect("write scripted godot");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        path
    }
    #[cfg(windows)]
    {
        let path = dir.join("scripted_godot.bat");
        std::fs::write(
            &path,
            "@echo off\r\necho ARGS %*\r\necho %* | findstr hang.gd >nul\r\nif %errorlevel%==0 (\r\n  echo HANGING\r\n  ping -n 30 127.0.0.1 >nul\r\n  exit /b 0\r\n)\r\necho %* | findstr lines.gd >nul\r\nif %errorlevel%==0 (\r\n  for /L %%i in (1,1,200) do echo out line %%i\r\n  for /L %%i in (1,1,50) do echo err line %%i 1>&2\r\n  ping -n 30 127.0.0.1 >nul\r\n  exit /b 0\r\n)\r\necho %* | findstr spill.gd >nul\r\nif %errorlevel%==0 (\r\n  for /L %%i in (1,1,6000) do echo spill line %%i with some more text to fill it up\r\n  exit /b 0\r\n)\r\necho %* | findstr wrongbase.gd >nul\r\nif %errorlevel%==0 (\r\n  echo ERROR: Can't load the script \"wrongbase.gd\" as it doesn't inherit from SceneTree or MainLoop. 1>&2\r\n  exit /b 0\r\n)\r\necho %* | findstr broken.gd >nul\r\nif %errorlevel%==0 (\r\n  echo SCRIPT ERROR: Parse Error: Expected expression. 1>&2\r\n  echo           at: GDScript::reload ^(res://broken.gd:3^) 1>&2\r\n  echo ERROR: Failed to load script \"res://broken.gd\" with error \"Parse error\". 1>&2\r\n  exit /b 0\r\n)\r\necho %* | findstr gone.gd >nul\r\nif %errorlevel%==0 (\r\n  echo ERROR: Can't load script: gone.gd 1>&2\r\n  exit /b 0\r\n)\r\necho %* | findstr idle.gd >nul\r\nif %errorlevel%==0 (\r\n  echo SCRIPT ERROR: Parse Error: unexpected.\r\n  echo    at: _init ^(res://idle.gd:3^)\r\n  ping -n 30 127.0.0.1 >nul\r\n  exit /b 0\r\n)\r\necho SCRIPT ERROR: Assertion failed.\r\necho    at: _initialize (res://scripts/boom.gd:4)\r\nexit /b 3\r\n",
        )
        .expect("write scripted godot");
        path
    }
}

#[tokio::test]
async fn creates_project_without_engine_and_preserves_existing_work() {
    let dir = tempfile::tempdir().expect("directory");
    std::fs::write(dir.path().join("AGENTS.md"), "keep").expect("documentation");
    let args = json!({"project_path": dir.path(), "name": "Frog \"World\"\nPond"});
    let result = call("godot_create_project", args.clone()).await;
    assert!(!result.is_error, "{}", text(&result));
    let body = std::fs::read_to_string(dir.path().join("project.godot")).expect("project");
    assert!(body.contains("config_version=5"));
    assert!(body.contains("gl_compatibility"));
    // The quote and the line break of the name stay inside one quoted value, so the name cannot
    // start a new entry or section of project.godot.
    assert!(
        body.contains(r#"config/name="Frog \"World\"\nPond""#),
        "{body}"
    );
    assert_eq!(body.lines().filter(|line| line.starts_with('[')).count(), 2);
    assert_eq!(
        std::fs::read_to_string(dir.path().join("AGENTS.md")).expect("docs"),
        "keep"
    );
    let second = call("godot_create_project", args).await;
    assert!(second.is_error);
    assert_eq!(
        std::fs::read_to_string(dir.path().join("project.godot")).expect("unchanged"),
        body
    );
}

#[tokio::test]
async fn project_creation_validates_before_creating_directories() {
    let dir = tempfile::tempdir().expect("directory");
    let target = dir.path().join("new-project");
    for args in [
        json!({"project_path": target, "name": " "}),
        json!({"project_path": "relative/project", "name": "Pond"}),
        json!({"project_path": format!("{}/new/../other", dir.path().display()), "name": "Pond"}),
        json!({"project_path": target, "name": 123}),
    ] {
        assert!(call("godot_create_project", args).await.is_error);
    }
    assert!(!target.exists());
    let result = call(
        "godot_create_project",
        json!({"project_path": target, "name": "Pond"}),
    )
    .await;
    assert!(!result.is_error, "{}", text(&result));
    assert!(target.join("project.godot").is_file());
}

#[tokio::test]
async fn a_scenario_is_refused_before_a_game_exists() {
    let no_game = call("godot_game_scenario", json!({"steps": [{"frames": 1}]})).await;
    assert!(
        no_game.is_error && text(&no_game).contains("managed process"),
        "{}",
        text(&no_game)
    );
    let editor = call(
        "godot_game_scenario",
        json!({"run_id": "editor", "steps": [{"frames": 1}]}),
    )
    .await;
    assert!(
        editor.is_error && text(&editor).contains("mode process"),
        "{}",
        text(&editor)
    );
    let empty = call("godot_game_scenario", json!({})).await;
    assert!(text(&empty).contains("steps"), "{}", text(&empty));
    let wide = call("godot_game_scenario", json!({"steps": [{"frames": 601}]})).await;
    assert!(text(&wide).contains("600"), "{}", text(&wide));
    for input in [
        json!({"actions": [{"action": "jump"}]}),
        json!({"pointers": [{"pointer": "mouse", "phase": "tap", "x": 1, "y": 1}]}),
    ] {
        let mut step = input;
        step["frames"] = json!(0);
        let instant = call("godot_game_scenario", json!({"steps": [step]})).await;
        assert!(
            instant.is_error && text(&instant).contains("frames 0"),
            "{}",
            text(&instant)
        );
    }
    let strength = call(
        "godot_game_scenario",
        json!({"steps": [{"frames": 1, "actions": [{"action": "jump", "strength": 2}]}]}),
    )
    .await;
    assert!(text(&strength).contains("strength"), "{}", text(&strength));
    let tools = ToolManager::list_tools();
    let scenario = tools
        .iter()
        .find(|tool| tool.name == "godot_game_scenario")
        .expect("scenario tool");
    let description = scenario.description.to_lowercase();
    for word in [
        "deterministic",
        "physics",
        "pause",
        "characterbody",
        "screenshot",
        "released",
    ] {
        assert!(description.contains(word), "{word} missing: {description}");
    }
}

/// The input map tools edit through the editor only. A request that could never be applied is
/// refused before the editor is asked, and project.godot is never written by hand.
#[tokio::test]
async fn input_action_edits_are_checked_first_and_never_write_project_godot() {
    let project = project_dir();
    let project_file = project.path().join("project.godot");
    let before = std::fs::read_to_string(&project_file).expect("project.godot");
    let key_e = json!({"type": "key", "key": "E"});
    for (args, needle) in [
        (json!({"action": ""}), "action"),
        (json!({"action": "a/b", "add_events": [key_e]}), "action"),
        (
            json!({"action": "say \"hi\"", "add_events": [key_e]}),
            "action",
        ),
        (json!({"action": "tongue"}), "nothing to change"),
        (
            json!({"action": "tongue", "add_events": [{"type": "gesture"}]}),
            "type",
        ),
        (
            json!({"action": "tongue", "add_events": [{"type": "key"}]}),
            "key",
        ),
        (
            json!({"action": "tongue", "add_events": [{"type": "mouse_button", "button": "nose"}]}),
            "button",
        ),
        (
            json!({"action": "tongue", "add_events": [{"type": "joypad_button", "button": 200}]}),
            "button",
        ),
        (
            json!({"action": "tongue", "add_events": [{"type": "joypad_motion", "axis": 1, "axis_value": 0}]}),
            "axis_value",
        ),
        (json!({"action": "tongue", "deadzone": 2}), "deadzone"),
        (
            json!({"action": "tongue", "delete": true, "add_events": [key_e]}),
            "delete",
        ),
        (json!({"action": "ui_accept", "delete": true}), "built-in"),
    ] {
        let refused = call("godot_edit_input_action", args.clone()).await;
        assert!(refused.is_error, "{args}: {}", text(&refused));
        assert!(
            text(&refused).contains(needle),
            "{args}: expected {needle:?} in {}",
            text(&refused)
        );
        assert!(
            !text(&refused).contains("bridge is not reachable"),
            "{args} reached the editor: {}",
            text(&refused)
        );
    }

    // A valid request without an editor names the editor and leaves project.godot alone.
    let unreachable = call(
        "godot_edit_input_action",
        json!({"action": "tongue", "add_events": [key_e], "deadzone": 0.5}),
    )
    .await;
    assert!(unreachable.is_error, "{}", text(&unreachable));
    assert!(
        text(&unreachable).contains("godot_launch_editor"),
        "{}",
        text(&unreachable)
    );
    let as_setting = call(
        "godot_set_project_setting",
        json!({"key": "input/tongue", "value": "E", "project_path": project.path()}),
    )
    .await;
    assert!(as_setting.is_error, "{}", text(&as_setting));
    assert!(
        text(&as_setting).contains("godot_edit_input_action"),
        "{}",
        text(&as_setting)
    );
    let listed = call("godot_get_input_map", json!({})).await;
    assert!(listed.is_error, "{}", text(&listed));
    assert!(
        text(&listed).contains("godot_launch_editor"),
        "{}",
        text(&listed)
    );
    assert_eq!(
        std::fs::read_to_string(&project_file).expect("project.godot again"),
        before
    );

    let tools = ToolManager::list_tools();
    let edit = tools
        .iter()
        .find(|tool| tool.name == "godot_edit_input_action")
        .expect("edit tool listed");
    for word in ["undo", "physical", "deadzone", "joypad"] {
        assert!(
            edit.description.to_lowercase().contains(word),
            "{word} missing: {}",
            edit.description
        );
    }
    assert!(tools.iter().any(|tool| tool.name == "godot_get_input_map"));
}

/// Without an editor, godot_delete_file deletes on disk: a script with its .uid, an imported
/// image with its .import and imported data. A file a scene still names is refused (nothing is
/// deleted while anything is refused) unless force is set, and protected paths never go.
#[tokio::test]
async fn files_are_deleted_on_disk_with_their_companions_unless_still_referenced() {
    let project = project_dir();
    let root = project.path();
    let write = |name: &str, text: &str| {
        let path = root.join(name);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("folder");
        std::fs::write(path, text).expect("fixture file");
    };
    write("old.gd", "extends Node\n");
    write("old.gd.uid", "uid://oldscript\n");
    write("used.gd", "extends Node\n");
    write("used.gd.uid", "uid://usedscript\n");
    write(
        "main.tscn",
        "[gd_scene format=3 uid=\"uid://mainscene\"]\n\n[ext_resource type=\"Script\" uid=\"uid://usedscript\" path=\"res://used.gd\" id=\"1\"]\n\n[node name=\"Main\" type=\"Node\"]\nscript = ExtResource(\"1\")\n",
    );
    write("used.gdshader", "shader_type canvas_item;\n");
    write("art/icon.png", "png bytes");
    write(
        "art/icon.png.import",
        "[remap]\n\nimporter=\"texture\"\ntype=\"CompressedTexture2D\"\nuid=\"uid://iconpng\"\npath=\"res://.godot/imported/icon.png-0123.ctex\"\n\n[deps]\n\nsource_file=\"res://art/icon.png\"\ndest_files=[\"res://.godot/imported/icon.png-0123.ctex\"]\n",
    );
    write(".godot/imported/icon.png-0123.ctex", "texture");
    write(".godot/imported/icon.png-0123.md5", "md5");
    write("empty/.keep_out_of_git", "");
    std::fs::create_dir_all(root.join("hollow")).expect("empty folder");
    let delete = |args: Value| async move {
        let mut args = args;
        args["project_path"] = json!(root);
        call("godot_delete_file", args).await
    };

    // A referenced file is refused, and the unreferenced file in the same call stays too.
    let refused = delete(json!({"paths": ["res://used.gd", "res://old.gd"]})).await;
    assert!(refused.is_error, "{}", text(&refused));
    let message = text(&refused);
    assert!(message.contains("Nothing was deleted"), "{message}");
    assert!(message.contains("res://main.tscn"), "{message}");
    assert!(message.contains("force"), "{message}");
    assert!(
        message.contains("\"uid\""),
        "matched by path and uid: {message}"
    );
    assert!(
        !message.contains("used.gdshader"),
        "a longer name is not a mention: {message}"
    );
    assert!(root.join("used.gd").exists() && root.join("old.gd").exists());

    // A dry run reports and keeps everything.
    let dry = delete(json!({"paths": ["res://art/icon.png"], "dry_run": true})).await;
    assert!(!dry.is_error, "{}", text(&dry));
    assert!(
        text(&dry).contains("res://.godot/imported/icon.png-0123.ctex"),
        "{}",
        text(&dry)
    );
    assert!(root.join("art/icon.png").exists());

    let deleted = delete(json!({"paths": ["res://old.gd", "res://art/icon.png"]})).await;
    assert!(!deleted.is_error, "{}", text(&deleted));
    let report: Value = serde_json::from_str(&text(&deleted)).expect("JSON report");
    assert_eq!(report["source"], "disk", "{report}");
    assert_eq!(report["rescan_needed"], true, "{report}");
    let companions = report["deleted"][1]["companions"].to_string();
    for gone in [
        "old.gd",
        "old.gd.uid",
        "art/icon.png",
        "art/icon.png.import",
        ".godot/imported/icon.png-0123.ctex",
        ".godot/imported/icon.png-0123.md5",
    ] {
        assert!(!root.join(gone).exists(), "{gone} still exists: {report}");
    }
    assert!(companions.contains("res://art/icon.png.import"), "{report}");
    assert_eq!(
        report["deleted"][0]["companions"],
        json!(["res://old.gd.uid"])
    );

    // force deletes a referenced file and still lists who referenced it.
    let forced = delete(json!({"paths": ["res://used.gd"], "force": true})).await;
    assert!(!forced.is_error, "{}", text(&forced));
    assert!(
        text(&forced).contains("res://main.tscn"),
        "{}",
        text(&forced)
    );
    assert!(!root.join("used.gd").exists() && !root.join("used.gd.uid").exists());

    for (path, needle) in [
        ("res://project.godot", "project file"),
        ("res://Project.godot", "project file"),
        ("res://project.godot.", "another file"),
        ("res://.godot/imported", ".godot"),
        ("res://addons/godot_mcp/plugin.cfg", "addon"),
        ("res://main.tscn.uid", "belongs to main.tscn"),
        ("res://hollow", "allow_empty_dir"),
        ("res://empty", "allow_empty_dir"),
        ("res://../outside.gd", ".."),
        ("res://missing.gd", "does not exist"),
    ] {
        let refused = delete(json!({"paths": [path]})).await;
        assert!(refused.is_error, "{path}: {}", text(&refused));
        assert!(
            text(&refused).contains(needle),
            "{path}: {}",
            text(&refused)
        );
    }
    std::fs::write(root.join("keep.gd"), "extends Node\n").expect("keep.gd");
    std::fs::write(root.join("keep.gd.uid"), "uid://keep\n").expect("keep.gd.uid");
    let sidecar = delete(json!({"paths": ["res://keep.gd.uid"]})).await;
    assert!(
        text(&sidecar).contains("delete keep.gd instead"),
        "{}",
        text(&sidecar)
    );
    assert!(root.join("keep.gd.uid").exists());
    assert!(root.join("project.godot").exists());

    let not_empty = delete(json!({"paths": ["res://empty"], "allow_empty_dir": true})).await;
    assert!(
        text(&not_empty).contains("not empty"),
        "{}",
        text(&not_empty)
    );
    let hollow = delete(json!({"paths": ["res://hollow"], "allow_empty_dir": true})).await;
    assert!(!hollow.is_error, "{}", text(&hollow));
    assert!(!root.join("hollow").exists());

    let tools = ToolManager::list_tools();
    let tool = tools
        .iter()
        .find(|tool| tool.name == "godot_delete_file")
        .expect("delete tool listed");
    for word in ["force", ".uid", ".import", "trash", "rescan", "dry_run"] {
        assert!(
            tool.description.contains(word),
            "{word} missing: {}",
            tool.description
        );
    }
}

/// A tool script that only saves a generated file names its path without using it: deleting
/// the file is not refused, and the answer says which script writes it. A script that loads the
/// file still refuses it without force, and every reference reports its kind.
#[tokio::test]
async fn a_file_only_written_by_a_generator_script_is_deleted_without_force() {
    let project = project_dir();
    let root = project.path();
    let write = |name: &str, text: &str| {
        let path = root.join(name);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("folder");
        std::fs::write(path, text).expect("fixture file");
    };
    let scene = "[gd_scene format=3]\n\n[node name=\"Preview\" type=\"Node3D\"]\n";
    write("previews/bed_relief_preview.tscn", scene);
    write("previews/loaded_preview.tscn", scene);
    write(
        "scripts/tools/preview_bed_relief.gd",
        "@tool\nextends EditorScript\n\nconst OUT := \"res://previews/bed_relief_preview.tscn\"\n\nfunc _run() -> void:\n\tvar packed := PackedScene.new()\n\tpacked.pack(Node3D.new())\n\tDirAccess.make_dir_recursive_absolute(OUT.get_base_dir())\n\tResourceSaver.save(packed, OUT)\n\tprint(\"Saved \", OUT)\n",
    );
    write(
        "scripts/tools/preview_loaded.gd",
        "@tool\nextends EditorScript\n\nconst OUT := \"res://previews/loaded_preview.tscn\"\n\nfunc _run() -> void:\n\tResourceSaver.save(PackedScene.new(), OUT)\n\tvar again: PackedScene = load(OUT)\n",
    );
    let delete = |args: Value| async move {
        let mut args = args;
        args["project_path"] = json!(root);
        call("godot_delete_file", args).await
    };

    let used = delete(json!({"paths": ["res://previews/loaded_preview.tscn"]})).await;
    assert!(used.is_error, "{}", text(&used));
    let message = text(&used);
    assert!(message.contains("still used by 1 file(s)"), "{message}");
    assert!(message.contains("\"kind\": \"used\""), "{message}");
    assert!(root.join("previews/loaded_preview.tscn").exists());

    let dry =
        delete(json!({"paths": ["res://previews/bed_relief_preview.tscn"], "dry_run": true})).await;
    assert!(!dry.is_error, "{}", text(&dry));
    let report: Value = serde_json::from_str(&text(&dry)).expect("JSON report");
    assert_eq!(report["refused"], json!([]), "{report}");
    assert_eq!(report["references"][0]["written"], 1, "{report}");
    assert_eq!(report["references"][0]["used"], 0, "{report}");
    assert_eq!(
        report["references"][0]["referenced_by"][0],
        json!({"file": "res://scripts/tools/preview_bed_relief.gd", "by": ["path"], "kind": "written"}),
        "{report}"
    );
    assert!(
        report["warnings"].to_string().contains(
            "named only as a file that 1 script(s) write: res://scripts/tools/preview_bed_relief.gd"
        ),
        "{report}"
    );

    let deleted = delete(json!({"paths": ["res://previews/bed_relief_preview.tscn"]})).await;
    assert!(!deleted.is_error, "{}", text(&deleted));
    assert!(!root.join("previews/bed_relief_preview.tscn").exists());
    assert!(root.join("scripts/tools/preview_bed_relief.gd").exists());
}

#[test]
fn every_tool_declares_all_four_hints() {
    let listed = serde_json::to_value(ToolManager::list_tools()).expect("tools serialize");
    let tools = listed.as_array().expect("tool array");
    let hints = |name: &str| {
        tools
            .iter()
            .find(|tool| tool["name"] == name)
            .unwrap_or_else(|| panic!("no tool {name}"))["annotations"]
            .clone()
    };
    for tool in tools {
        let name = &tool["name"];
        for hint in [
            "readOnlyHint",
            "destructiveHint",
            "idempotentHint",
            "openWorldHint",
        ] {
            assert!(tool["annotations"][hint].is_boolean(), "{name} {hint}");
        }
        if tool["annotations"]["readOnlyHint"] == true {
            assert_eq!(tool["annotations"]["destructiveHint"], false, "{name}");
            assert_eq!(tool["annotations"]["idempotentHint"], true, "{name}");
        }
    }
    for name in [
        "godot_get_scene_tree",
        "godot_read_script",
        "godot_read_shader",
        "godot_validate_shader",
        "godot_game_tree",
        "godot_code_references",
    ] {
        assert_eq!(hints(name)["readOnlyHint"], true, "{name}");
    }
    for name in [
        "godot_delete_file",
        "godot_remove_node",
        "godot_edit_script",
        "godot_edit_shader",
        "godot_create_shader",
        "godot_run_script",
        "godot_save_scene",
    ] {
        assert_eq!(hints(name)["readOnlyHint"], false, "{name}");
        assert_eq!(hints(name)["destructiveHint"], true, "{name}");
    }
    assert_eq!(hints("godot_add_node")["destructiveHint"], false);
    assert_eq!(hints("godot_edit_script")["idempotentHint"], false);
    assert_eq!(hints("godot_edit_shader")["idempotentHint"], false);
    assert_eq!(hints("godot_submit_feedback")["openWorldHint"], true);
    assert_eq!(hints("godot_get_scene_tree")["openWorldHint"], false);
}

#[tokio::test]
async fn the_project_tools_check_their_arguments_before_a_game_is_asked() {
    let many: Vec<String> = (0..65).map(|index| format!("state{index}")).collect();
    let long = "x".repeat(257);
    for (tool, args, needle) in [
        ("godot_game_state", json!({"names": many}), "64"),
        ("godot_game_state", json!({"names": [""]}), "names"),
        ("godot_game_state", json!({"names": [long.clone()]}), "256"),
        ("godot_game_state", json!({"names": [1]}), "names"),
        ("godot_game_command", json!({}), "name"),
        ("godot_game_command", json!({"name": ""}), "name"),
        ("godot_game_command", json!({"name": long}), "256"),
        (
            "godot_game_command",
            json!({"name": "grant", "args": [1]}),
            "args",
        ),
    ] {
        let result = call(tool, args.clone()).await;
        assert!(result.is_error, "{tool} {args}: {}", text(&result));
        assert!(
            text(&result).contains(needle),
            "{tool} {args}: expected {needle:?} in {}",
            text(&result)
        );
    }
    // A valid call without a game fails on the game, not on the arguments.
    for (tool, args) in [
        ("godot_game_state", json!({"names": ["level"]})),
        (
            "godot_game_command",
            json!({"name": "grant", "args": {"name": "x"}}),
        ),
    ] {
        let result = call(tool, args).await;
        assert!(result.is_error, "{tool}: {}", text(&result));
    }
}

#[test]
fn the_project_tools_are_listed_with_their_hints_and_say_what_a_command_may_do() {
    let listed = serde_json::to_value(ToolManager::list_tools()).expect("tools serialize");
    let tools = listed.as_array().expect("tool array");
    let find = |name: &str| {
        tools
            .iter()
            .find(|tool| tool["name"] == name)
            .unwrap_or_else(|| panic!("no tool {name}"))
            .clone()
    };
    let state = find("godot_game_state");
    assert_eq!(state["annotations"]["readOnlyHint"], true);
    assert_eq!(state["annotations"]["idempotentHint"], true);
    assert_eq!(state["inputSchema"]["properties"]["names"]["type"], "array");
    let command = find("godot_game_command");
    assert_eq!(command["annotations"]["readOnlyHint"], false);
    assert_eq!(command["annotations"]["destructiveHint"], true);
    assert_eq!(command["annotations"]["idempotentHint"], false);
    assert_eq!(command["inputSchema"]["required"], json!(["name"]));
    assert_eq!(
        command["inputSchema"]["properties"]["args"]["type"],
        "object"
    );
    let described = command["description"].as_str().unwrap_or("").to_lowercase();
    for word in ["project wrote", "files", "godot_mcp/adapter", "await"] {
        assert!(described.contains(word), "{word} missing: {described}");
    }
    let described = state["description"].as_str().unwrap_or("").to_lowercase();
    for word in ["godot_mcp/adapter", "commands", "bytes"] {
        assert!(described.contains(word), "{word} missing: {described}");
    }
}

#[test]
fn add_node_lists_a_scene_to_instance_beside_the_node_type() {
    let listed = serde_json::to_value(ToolManager::list_tools()).expect("tools serialize");
    let add = listed
        .as_array()
        .expect("tool array")
        .iter()
        .find(|tool| tool["name"] == "godot_add_node")
        .expect("godot_add_node")
        .clone();
    let schema = &add["inputSchema"];
    assert_eq!(schema["properties"]["scene"]["type"], "string", "{add}");
    assert_eq!(
        schema["properties"]["properties"]["type"], "object",
        "{add}"
    );
    assert!(schema.get("required").is_none(), "{add}");
    let described = add["description"].as_str().unwrap_or("");
    for word in ["scene", "node_type", "instance", "undo"] {
        assert!(described.contains(word), "{word} missing: {described}");
    }
}

#[tokio::test]
async fn add_node_needs_exactly_one_of_node_type_and_scene_before_the_editor_is_asked() {
    for (args, needle) in [
        (json!({"parent_node_path": "."}), "node_type or scene"),
        (
            json!({"node_type": "Node2D", "scene": "res://snag.tscn"}),
            "not both",
        ),
        (json!({"scene": " "}), "node_type or scene"),
    ] {
        let result = call("godot_add_node", args.clone()).await;
        assert!(result.is_error, "{args}: {}", text(&result));
        assert!(text(&result).contains(needle), "{args}: {}", text(&result));
        assert!(
            !text(&result).contains("bridge is not reachable"),
            "{args}: refused only after asking the editor: {}",
            text(&result)
        );
    }
}

/// A stand-in for the Godot executable that stays alive for a minute, like an editor left open.
fn lingering_godot(dir: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        let path = dir.join("lingering_godot.bat");
        std::fs::write(&path, "@echo off\r\nping -n 61 127.0.0.1 > nul\r\n")
            .expect("write lingering godot");
        path
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join("lingering_godot.sh");
        std::fs::write(&path, "#!/bin/sh\nexec sleep 60\n").expect("write lingering godot");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .expect("chmod lingering godot");
        path
    }
}

/// Whether a process with this id still runs (Unix only: `kill -0` sends no signal).
#[cfg(unix)]
fn process_runs(pid: u32) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

#[cfg(unix)]
fn launched_pid(result: &ToolResult) -> u32 {
    let shown = text(result);
    shown
        .split("PID: ")
        .nth(1)
        .and_then(|rest| rest.split(')').next())
        .and_then(|pid| pid.parse().ok())
        .unwrap_or_else(|| panic!("no PID in {shown}"))
}

#[test]
fn close_editor_schema_offers_project_discard_and_timeout() {
    let listed = ToolManager::list_tools();
    let close = listed
        .iter()
        .find(|tool| tool.name == "godot_close_editor")
        .expect("godot_close_editor is listed");
    let schema = &close.input_schema;
    assert_eq!(schema["properties"]["project_path"]["type"], "string");
    assert_eq!(schema["properties"]["discard"]["type"], "boolean");
    assert_eq!(schema["properties"]["timeout_seconds"]["type"], "integer");
    assert!(schema.get("required").is_none(), "{schema}");
    for word in ["unsaved", "discard", "another project", "PID"] {
        assert!(
            close.description.contains(word),
            "{word} missing: {}",
            close.description
        );
    }
}

#[tokio::test]
async fn close_editor_asks_the_connected_editor_to_quit_and_waits_for_its_process() {
    use common::{MockBridge, Reply};
    use std::sync::Mutex;

    let project = project_dir();
    let engine = tempfile::tempdir().expect("engine dir");
    // The process that plays the editor; the fake bridge ends it when asked to quit.
    let editor = std::process::Command::new(lingering_godot(engine.path()))
        .spawn()
        .expect("spawn stand-in editor");
    let pid = editor.id();
    let editor = Arc::new(Mutex::new(Some(editor)));
    let dir = project.path().to_string_lossy().to_string();
    let bridge = MockBridge::start({
        let editor = Arc::clone(&editor);
        move |command| match command["type"].as_str() {
            Some("get_project_info") => Reply::Ok(json!({"project_dir": dir})),
            Some("close_editor") => {
                let quitting = editor.lock().expect("editor lock").take();
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_millis(400));
                    if let Some(mut child) = quitting {
                        let _ = child.kill();
                        let _ = child.wait();
                    }
                });
                Reply::Ok(json!({"closing": true, "pid": pid, "discarded_scenes": [], "discarded_scripts": []}))
            }
            _ => Reply::Ok(json!({})),
        }
    })
    .await;
    let cwd = tempfile::tempdir().expect("working dir");
    let manager = Arc::new(
        ToolManager::new(
            BridgeClient::new(bridge.port),
            CliRunner::new(PathBuf::from("unused-godot")),
        )
        .with_working_dir(cwd.path().to_path_buf()),
    );

    let result = manager
        .call_tool(
            "godot_close_editor",
            json!({"project_path": project.path()}),
        )
        .await;
    assert!(!result.is_error, "{}", text(&result));
    let shown = text(&result);
    assert!(shown.contains(&pid.to_string()), "{shown}");
    assert!(shown.contains("exited"), "{shown}");
    let close = bridge
        .received()
        .into_iter()
        .find(|command| command["type"] == "close_editor")
        .expect("close_editor was sent");
    assert_eq!(close["params"]["discard"], false, "{close}");
}

#[tokio::test]
async fn close_editor_reports_unsaved_changes_instead_of_closing() {
    use common::{MockBridge, Reply};

    let project = project_dir();
    let dir = project.path().to_string_lossy().to_string();
    let bridge = MockBridge::start(move |command| match command["type"].as_str() {
        Some("get_project_info") => Reply::Ok(json!({"project_dir": dir})),
        Some("close_editor") => Reply::Ok(json!({
            "closing": false,
            "unsaved_scenes": ["res://main.tscn"],
            "unsaved_scripts": ["res://player.gd"],
        })),
        _ => Reply::Ok(json!({})),
    })
    .await;
    let manager = Arc::new(
        ToolManager::new(
            BridgeClient::new(bridge.port),
            CliRunner::new(PathBuf::from("unused-godot")),
        )
        .with_working_dir(project.path().to_path_buf()),
    );

    let result = manager.call_tool("godot_close_editor", json!({})).await;
    assert!(result.is_error, "{}", text(&result));
    let shown = text(&result);
    for needle in [
        "res://main.tscn",
        "res://player.gd",
        "discard",
        "not closed",
    ] {
        assert!(shown.contains(needle), "{needle} missing: {shown}");
    }
}

#[tokio::test]
async fn close_editor_never_closes_an_editor_that_has_another_project_open() {
    use common::{MockBridge, Reply};

    let mine = project_dir();
    let theirs = project_dir();
    let dir = theirs.path().to_string_lossy().to_string();
    let bridge = MockBridge::start(move |command| match command["type"].as_str() {
        Some("get_project_info") => Reply::Ok(json!({"project_dir": dir})),
        _ => Reply::Ok(json!({"closing": true, "pid": 1})),
    })
    .await;
    let manager = Arc::new(ToolManager::new(
        BridgeClient::new(bridge.port),
        CliRunner::new(PathBuf::from("unused-godot")),
    ));

    let result = manager
        .call_tool(
            "godot_close_editor",
            json!({"project_path": mine.path(), "discard": true}),
        )
        .await;
    assert!(result.is_error, "{}", text(&result));
    let shown = text(&result);
    assert!(
        shown.contains(&theirs.path().display().to_string()),
        "the answer names the editor's own project: {shown}"
    );
    assert!(
        bridge
            .received()
            .iter()
            .all(|command| command["type"] != "close_editor"),
        "an editor of another project was asked to quit: {:?}",
        bridge.received()
    );
}

#[tokio::test]
async fn close_editor_without_any_editor_says_there_is_nothing_to_close() {
    let project = project_dir();
    let result = call(
        "godot_close_editor",
        json!({"project_path": project.path(), "discard": true}),
    )
    .await;
    assert!(result.is_error, "{}", text(&result));
    let shown = text(&result);
    assert!(shown.contains("No Godot editor"), "{shown}");
    assert!(shown.contains("did not launch"), "{shown}");
}

#[cfg(unix)]
#[tokio::test]
async fn close_editor_stops_an_editor_it_launched_only_with_discard_when_no_bridge_answers() {
    let project = project_dir();
    let engine = tempfile::tempdir().expect("engine dir");
    let manager = manager_in(project.path(), lingering_godot(engine.path()));

    let launched = Arc::clone(&manager)
        .call_tool("godot_launch_editor", json!({}))
        .await;
    assert!(!launched.is_error, "{}", text(&launched));
    let pid = launched_pid(&launched);
    assert!(process_runs(pid));

    let refused = Arc::clone(&manager)
        .call_tool("godot_close_editor", json!({}))
        .await;
    assert!(refused.is_error, "{}", text(&refused));
    let shown = text(&refused);
    assert!(shown.contains(&pid.to_string()), "{shown}");
    assert!(shown.contains("discard"), "{shown}");
    assert!(process_runs(pid), "the editor was stopped without discard");

    let closed = Arc::clone(&manager)
        .call_tool("godot_close_editor", json!({"discard": true}))
        .await;
    assert!(!closed.is_error, "{}", text(&closed));
    assert!(
        text(&closed).contains(&pid.to_string()),
        "{}",
        text(&closed)
    );
    assert!(!process_runs(pid), "the editor still runs");

    let again = manager
        .call_tool("godot_close_editor", json!({"discard": true}))
        .await;
    assert!(again.is_error, "{}", text(&again));
    assert!(text(&again).contains("No Godot editor"), "{}", text(&again));
}

#[cfg(unix)]
#[tokio::test]
async fn close_editor_leaves_an_editor_launched_for_another_project_running() {
    let mine = project_dir();
    let theirs = project_dir();
    let engine = tempfile::tempdir().expect("engine dir");
    let cwd = tempfile::tempdir().expect("working dir");
    let manager = manager_in(cwd.path(), lingering_godot(engine.path()));

    let launched = Arc::clone(&manager)
        .call_tool(
            "godot_launch_editor",
            json!({"project_path": theirs.path()}),
        )
        .await;
    assert!(!launched.is_error, "{}", text(&launched));
    let pid = launched_pid(&launched);

    let result = Arc::clone(&manager)
        .call_tool(
            "godot_close_editor",
            json!({"project_path": mine.path(), "discard": true}),
        )
        .await;
    assert!(result.is_error, "{}", text(&result));
    assert!(process_runs(pid), "the other project's editor was stopped");

    let cleanup = manager
        .call_tool(
            "godot_close_editor",
            json!({"project_path": theirs.path(), "discard": true}),
        )
        .await;
    assert!(!cleanup.is_error, "{}", text(&cleanup));
}

/// The presets answer the stand-in engine gives for the preset script: one preset, "Data".
#[cfg(unix)]
const ONE_PRESET: &str = r#"GODOT_MCP_PRESETS={"file_exists":true,"presets":[{"index":0,"name":"Data","platform":"Linux","export_path":"build/data.pck","options":{}}]}"#;

/// A stand-in for the Godot executable for the export tools. It appends each command line to
/// `calls.txt` in `dir`, answers the preset script with `presets`, succeeds on `--import`, and on
/// an export runs `on_export` with the output path as `$out`.
#[cfg(unix)]
fn exporting_godot(dir: &Path, presets: &str, on_export: &str) -> (PathBuf, PathBuf) {
    use std::os::unix::fs::PermissionsExt;
    let calls = dir.join("calls.txt");
    let path = dir.join("exporting_godot.sh");
    std::fs::write(
        &path,
        format!(
            "#!/bin/sh\necho \"$@\" >> '{}'\nfor out; do :; done\ncase \" $* \" in\n  *' --script '*) echo '{presets}' ;;\n  *' --import '*) echo 'imported' ;;\n  *' --export-'*) {on_export} ;;\nesac\n",
            calls.display()
        ),
    )
    .expect("write exporting godot");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
        .expect("chmod exporting godot");
    (path, calls)
}

#[cfg(unix)]
#[tokio::test]
async fn an_export_imports_first_and_reports_the_written_pack() {
    let project = project_dir();
    let engine = tempfile::tempdir().expect("engine dir");
    let (godot, calls) = exporting_godot(
        engine.path(),
        ONE_PRESET,
        "echo 'savepack: end' ; printf 'GDPC0123456789' > \"$out\"",
    );
    let cwd = tempfile::tempdir().expect("working dir");
    let result = manager_in(cwd.path(), godot)
        .call_tool(
            "godot_export_project",
            json!({"preset": "Data", "project_path": project.path()}),
        )
        .await;
    assert!(!result.is_error, "{}", text(&result));
    let report: Value = serde_json::from_str(&text(&result)).expect("JSON report");
    assert_eq!(report["ok"], true);
    assert_eq!(report["mode"], "pack");
    assert_eq!(report["size_bytes"], 14);
    assert_eq!(report["import"]["ran"], true);
    assert!(report["log"].as_str().unwrap_or("").contains("savepack"));
    let output = report["output_path"].as_str().expect("output path");
    assert!(
        Path::new(output).is_absolute() && output.ends_with("build/data.pck"),
        "the preset's export_path, made absolute: {output}"
    );

    let calls = std::fs::read_to_string(&calls).expect("calls");
    let lines: Vec<&str> = calls.lines().collect();
    assert_eq!(lines.len(), 3, "presets, import, export: {calls}");
    assert!(lines[1].ends_with("--import"), "{calls}");
    assert!(
        lines[2].ends_with(&format!("--export-pack Data {output}")),
        "{calls}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn an_unknown_preset_is_refused_before_anything_is_imported() {
    let project = project_dir();
    let engine = tempfile::tempdir().expect("engine dir");
    let (godot, calls) = exporting_godot(engine.path(), ONE_PRESET, "exit 1");
    let cwd = tempfile::tempdir().expect("working dir");
    let result = manager_in(cwd.path(), godot)
        .call_tool(
            "godot_export_project",
            json!({"preset": "Web", "project_path": project.path()}),
        )
        .await;
    assert!(result.is_error);
    let message = text(&result);
    assert!(message.contains("\"Data\""), "names the presets: {message}");
    assert!(message.contains("godot_set_export_preset"), "{message}");
    let calls = std::fs::read_to_string(&calls).expect("calls");
    assert_eq!(
        calls.lines().count(),
        1,
        "only the preset list ran: {calls}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn a_missing_export_template_is_named_with_a_way_out() {
    let project = project_dir();
    let engine = tempfile::tempdir().expect("engine dir");
    let (godot, _) = exporting_godot(
        engine.path(),
        ONE_PRESET,
        "echo 'ERROR: Cannot export project with preset \"Data\" due to configuration errors:' >&2; echo 'No export template found at the expected path:' >&2; exit 1",
    );
    let cwd = tempfile::tempdir().expect("working dir");
    let result = manager_in(cwd.path(), godot)
        .call_tool(
            "godot_export_project",
            json!({"preset": "Data", "mode": "release", "output_path": "build/game.x86_64",
                   "project_path": project.path()}),
        )
        .await;
    assert!(result.is_error);
    let message = text(&result);
    assert!(message.contains("exit code 1"), "{message}");
    assert!(message.contains("Manage Export Templates"), "{message}");
    assert!(message.contains("custom_template/release"), "{message}");
    assert!(message.contains("Cannot export project"), "{message}");
}

#[cfg(unix)]
#[tokio::test]
async fn an_export_that_writes_no_file_is_an_error() {
    let project = project_dir();
    let engine = tempfile::tempdir().expect("engine dir");
    let (godot, _) = exporting_godot(engine.path(), ONE_PRESET, "exit 0");
    let cwd = tempfile::tempdir().expect("working dir");
    let result = manager_in(cwd.path(), godot)
        .call_tool(
            "godot_export_project",
            json!({"preset": "Data", "project_path": project.path()}),
        )
        .await;
    assert!(result.is_error);
    assert!(text(&result).contains("wrote no file"), "{}", text(&result));
}

#[cfg(unix)]
#[tokio::test]
async fn export_outputs_and_preset_names_are_checked_before_godot_runs() {
    let project = project_dir();
    let engine = tempfile::tempdir().expect("engine dir");
    let (godot, calls) = exporting_godot(engine.path(), ONE_PRESET, "exit 0");
    let cwd = tempfile::tempdir().expect("working dir");
    let manager = manager_in(cwd.path(), godot);
    for (args, expected) in [
        (json!({"preset": "--script"}), "starts with '-'"),
        (
            json!({"preset": "Data", "output_path": "../outside.pck"}),
            "Parent directory",
        ),
        (
            json!({"preset": "Data", "output_path": "build/data.exe"}),
            ".pck or .zip",
        ),
        (
            json!({"preset": "Data", "output_path": "project.godot", "mode": "release"}),
            "project file",
        ),
    ] {
        let mut args = args;
        args["project_path"] = json!(project.path());
        let result = Arc::clone(&manager)
            .call_tool("godot_export_project", args.clone())
            .await;
        assert!(result.is_error, "{args}");
        assert!(
            text(&result).contains(expected),
            "{args}: {}",
            text(&result)
        );
    }
    let calls = std::fs::read_to_string(&calls).unwrap_or_default();
    assert!(
        !calls.contains("--export-") && !calls.contains("--import"),
        "nothing was imported or exported: {calls}"
    );
}

#[tokio::test]
async fn a_preset_is_not_changed_while_the_editor_has_the_project_open() {
    let project = project_dir();
    let bridge = editor_with_open_tab(project.path(), false).await;
    let manager = Arc::new(ToolManager::new(
        BridgeClient::new(bridge.port),
        CliRunner::new(PathBuf::from("unused-godot")),
    ));
    let result = manager
        .call_tool(
            "godot_set_export_preset",
            json!({"name": "webOS", "create": true, "platform": "Linux",
                   "options": {"binary_format/architecture": "arm32"},
                   "project_path": project.path()}),
        )
        .await;
    assert!(result.is_error);
    assert!(
        text(&result).contains("godot_close_editor"),
        "{}",
        text(&result)
    );
    assert!(!project.path().join("export_presets.cfg").exists());
}

#[tokio::test]
async fn creating_a_preset_needs_a_platform() {
    let project = project_dir();
    let result = call(
        "godot_set_export_preset",
        json!({"name": "webOS", "create": true, "project_path": project.path()}),
    )
    .await;
    assert!(result.is_error);
    assert!(
        text(&result).contains("platform is required"),
        "{}",
        text(&result)
    );
}

const WATER: &str = "shader_type spatial;\n\nuniform float depth = 1.0;\n\nvoid fragment() {\n\tALBEDO = vec3(0.1, 0.3, depth);\n}\n";

fn shader_args(project: &Path, path: &str) -> Value {
    json!({"shader_path": path, "project_path": project})
}

#[tokio::test]
async fn create_shader_without_an_editor_writes_the_code_and_a_uid() {
    let project = project_dir();
    let p = project.path();
    let mut args = shader_args(p, "res://themes/world/pond_water.gdshader");
    args["code"] = json!(WATER);
    let answer = script_answer(&call("godot_create_shader", args.clone()).await);
    assert_eq!(answer["path"], "res://themes/world/pond_water.gdshader");
    assert_eq!(answer["created"], true);
    assert_eq!(answer["source"], "file");
    assert_eq!(answer["editor_consulted"], false);
    let file = p.join("themes/world/pond_water.gdshader");
    assert_eq!(
        std::fs::read_to_string(&file).expect("shader"),
        WATER,
        "the code is written as given, tabs included"
    );
    let uid = std::fs::read_to_string(p.join("themes/world/pond_water.gdshader.uid"))
        .expect("a new shader gets a .uid file");
    assert_eq!(answer["uid"], uid.trim());
    assert!(uid.starts_with("uid://") && uid.ends_with('\n'), "{uid:?}");

    // The label is the one a read gives, so an edit can follow at once.
    let read = script_answer(
        &call(
            "godot_read_shader",
            shader_args(p, "res://themes/world/pond_water.gdshader"),
        )
        .await,
    );
    assert_eq!(read["text"], WATER);
    assert_eq!(read["version"], answer["version"]);

    // An existing file is kept unless overwrite is asked for; then its uid is kept too.
    args["code"] = json!("shader_type canvas_item;\n");
    let refused = call("godot_create_shader", args.clone()).await;
    assert!(refused.is_error, "{}", text(&refused));
    assert!(
        text(&refused).contains("overwrite: true"),
        "{}",
        text(&refused)
    );
    assert_eq!(std::fs::read_to_string(&file).expect("shader"), WATER);
    args["overwrite"] = json!(true);
    let replaced = script_answer(&call("godot_create_shader", args).await);
    assert_eq!(replaced["created"], false);
    assert_eq!(replaced["uid"], uid.trim());
    assert_eq!(
        std::fs::read_to_string(&file).expect("shader"),
        "shader_type canvas_item;\n"
    );
    assert_eq!(
        files_in(&p.join("themes/world")),
        vec!["pond_water.gdshader", "pond_water.gdshader.uid"],
        "no temporary file is left"
    );

    // An include is a shader file too; its existing .uid is not replaced.
    std::fs::write(p.join("common.gdshaderinc.uid"), "uid://bkeepme\n").expect("uid");
    let mut include = shader_args(p, "res://common.gdshaderinc");
    include["code"] = json!("float wave(float x) {\n\treturn sin(x);\n}\n");
    let answer = script_answer(&call("godot_create_shader", include).await);
    assert_eq!(answer["uid"], "uid://bkeepme");
}

#[tokio::test]
async fn shader_tools_refuse_what_they_should_with_a_fix() {
    let project = project_dir();
    let p = project.path();
    std::fs::write(p.join("water.gdshader"), WATER).expect("shader");
    std::fs::write(p.join("notes.txt"), "hello\n").expect("notes");
    let before = files_in(p);
    let refused = |tool: &'static str, args: Value, needle: &'static str| async move {
        let result = call(tool, args).await;
        assert!(result.is_error, "{tool} should refuse: {}", text(&result));
        assert!(
            text(&result).contains(needle),
            "{tool}: {:?} should mention {needle:?}",
            text(&result)
        );
    };
    let with_code = |path: &str| {
        let mut args = shader_args(p, path);
        args["code"] = json!("shader_type spatial;\n");
        args
    };
    refused(
        "godot_create_shader",
        with_code("res://water.gd"),
        ".gdshader",
    )
    .await;
    refused(
        "godot_create_shader",
        with_code("res://../outside.gdshader"),
        "..",
    )
    .await;
    refused(
        "godot_create_shader",
        shader_args(p, "res://new.gdshader"),
        "code",
    )
    .await;
    let mut big = shader_args(p, "res://big.gdshader");
    big["code"] = json!("/".repeat(1_048_577));
    refused("godot_create_shader", big, "1 MiB").await;
    refused(
        "godot_read_shader",
        shader_args(p, "res://missing.gdshader"),
        "godot_create_shader",
    )
    .await;
    refused(
        "godot_read_shader",
        shader_args(p, "res://notes.txt"),
        ".gdshader",
    )
    .await;
    let edit = |old: &str, new: &str| {
        let mut args = shader_args(p, "res://water.gdshader");
        args["old_text"] = json!(old);
        args["new_text"] = json!(new);
        args
    };
    refused("godot_edit_shader", edit("depth = 3.0", "x"), "0 times").await;
    refused("godot_edit_shader", edit("depth", "x"), "2 times").await;
    let mut stale = edit("depth = 1.0", "depth = 2.0");
    stale["version"] = json!("0000000000000000");
    refused("godot_edit_shader", stale, "godot_read_shader").await;
    refused(
        "godot_validate_shader",
        shader_args(p, "res://missing.gdshader"),
        "godot_create_shader",
    )
    .await;
    refused(
        "godot_validate_shader",
        shader_args(p, "res://notes.txt"),
        ".gdshader",
    )
    .await;

    assert_eq!(
        std::fs::read_to_string(p.join("water.gdshader")).expect("shader"),
        WATER
    );
    assert_eq!(files_in(p), before, "a refusal writes nothing");
}

#[tokio::test]
async fn edit_shader_without_an_editor_replaces_one_fragment() {
    let project = project_dir();
    let p = project.path();
    std::fs::write(p.join("water.gdshader"), WATER.replace('\n', "\r\n")).expect("shader");
    let read =
        script_answer(&call("godot_read_shader", shader_args(p, "res://water.gdshader")).await);
    assert_eq!(read["text"], WATER, "text is given with \\n");
    let mut args = shader_args(p, "res://water.gdshader");
    args["old_text"] = json!("\tALBEDO = vec3(0.1, 0.3, depth);");
    args["new_text"] = json!("\tALBEDO = vec3(0.1, 0.3, depth);\n\tROUGHNESS = 0.2;");
    args["version"] = read["version"].clone();
    let answer = script_answer(&call("godot_edit_shader", args).await);
    assert_eq!(answer["replaced"], 1);
    assert_eq!(answer["source"], "file");
    assert_eq!(
        std::fs::read_to_string(p.join("water.gdshader")).expect("shader"),
        WATER
            .replace("depth);\n", "depth);\n\tROUGHNESS = 0.2;\n")
            .replace('\n', "\r\n"),
        "the file keeps its line endings"
    );
}

#[tokio::test]
async fn shader_tools_ask_the_editor_first() {
    use common::{MockBridge, Reply};
    let project = project_dir();
    std::fs::write(project.path().join("water.gdshader"), WATER).expect("shader");
    let open_dir = project.path().to_string_lossy().replace('\\', "/");
    let bridge = MockBridge::start(move |command| match command["type"].as_str() {
        Some("get_project_info") => Reply::Ok(json!({"project_dir": open_dir})),
        Some("read_shader") => Reply::Ok(json!({
            "path": "res://water.gdshader", "text": "// typed\n", "source": "editor",
            "dirty": true, "version": "0123456789abcdef"
        })),
        Some("create_shader") => Reply::Err(
            "res://water.gdshader has unsaved changes in the shader editor, so it was not overwritten."
                .into(),
        ),
        _ => Reply::Err("unexpected command".into()),
    })
    .await;
    let manager = Arc::new(ToolManager::new(
        BridgeClient::new(bridge.port),
        CliRunner::new(PathBuf::from("unused-godot")),
    ));
    let read = manager
        .clone()
        .call_tool(
            "godot_read_shader",
            shader_args(project.path(), "res://water.gdshader"),
        )
        .await;
    let read = script_answer(&read);
    assert_eq!(read["source"], "editor");
    assert_eq!(
        read["project_dir"],
        project.path().to_string_lossy().replace('\\', "/")
    );

    // A refusal from a live editor is returned as it is; the file is not written instead.
    let mut args = shader_args(project.path(), "res://water.gdshader");
    args["code"] = json!("shader_type spatial;\n");
    args["overwrite"] = json!(true);
    let refused = manager.clone().call_tool("godot_create_shader", args).await;
    assert!(refused.is_error, "{}", text(&refused));
    assert!(
        text(&refused).contains("unsaved changes"),
        "{}",
        text(&refused)
    );
    assert_eq!(
        std::fs::read_to_string(project.path().join("water.gdshader")).expect("shader"),
        WATER
    );
    let sent: Vec<Value> = bridge.received();
    assert!(sent.iter().any(|command| command["type"] == "create_shader"
        && command["params"]["code"] == "shader_type spatial;\n"));
}

/// A project with an imported texture, res://sky.png, and its .import sidecar.
fn project_with_texture() -> tempfile::TempDir {
    let project = project_dir();
    std::fs::write(project.path().join("sky.png"), b"png").expect("sky.png");
    std::fs::write(
        project.path().join("sky.png.import"),
        "[remap]\n\nimporter=\"texture\"\ntype=\"CompressedTexture2D\"\n\n[params]\n\ncompress/mode=0\n",
    )
    .expect("sky.png.import");
    project
}

#[tokio::test]
async fn import_params_of_a_file_never_imported_are_refused_with_the_resave_tool() {
    let project = project_dir();
    std::fs::write(project.path().join("new.png"), b"png").expect("new.png");
    for (tool, args) in [
        ("godot_get_import_params", json!({"path": "res://new.png"})),
        (
            "godot_set_import_params",
            json!({"path": "res://new.png", "params": {"compress/mode": 2}}),
        ),
    ] {
        let mut args = args;
        args["project_path"] = json!(project.path());
        let result = call(tool, args).await;
        assert!(result.is_error, "{tool}");
        let message = text(&result);
        assert!(message.contains("no .import file yet"), "{message}");
        assert!(message.contains("godot_resave_resources"), "{message}");
    }
    assert!(!project.path().join("new.png.import").exists());
}

#[tokio::test]
async fn import_params_need_a_change_and_a_copy_source_with_a_sidecar() {
    let project = project_with_texture();
    std::fs::write(project.path().join("other.png"), b"png").expect("other.png");
    for (args, expected) in [
        (json!({"path": "res://sky.png"}), "Nothing to change"),
        (
            json!({"path": "res://sky.png", "copy_from": "res://other.png"}),
            "godot_resave_resources",
        ),
        (
            json!({"path": "res://sky.png", "copy_from": "sky.png"}),
            "itself",
        ),
        (
            json!({"path": "res://sky.png.import", "params": {"compress/mode": 2}}),
            "Pass res://sky.png",
        ),
        (
            json!({"path": "../sky.png", "params": {"compress/mode": 2}}),
            "Parent directory",
        ),
        (
            json!({"path": "res://sky.png", "params": {"compress mode": 2}}),
            "not one an importer uses",
        ),
    ] {
        let mut args = args;
        args["project_path"] = json!(project.path());
        let result = call("godot_set_import_params", args.clone()).await;
        assert!(result.is_error, "{args}");
        assert!(
            text(&result).contains(expected),
            "{args}: {}",
            text(&result)
        );
    }
}

#[tokio::test]
async fn import_params_go_through_the_editor_that_has_the_project_and_name_dropped_options() {
    use common::{MockBridge, Reply};
    let project = project_with_texture();
    std::fs::write(project.path().join("far.png"), b"png").expect("far.png");
    std::fs::write(
        project.path().join("far.png.import"),
        "[remap]\n\nimporter=\"texture\"\n",
    )
    .expect("far.png.import");
    let dir = project.path().to_string_lossy().replace('\\', "/");
    let bridge = MockBridge::start(move |command| match command["type"].as_str() {
        Some("get_project_info") => Reply::Ok(json!({"project_dir": dir})),
        Some("import_params") => Reply::Ok(json!({
            "importer": "texture",
            "changed": ["compress/mode", "compres/high_quality"],
            "new_options": ["compres/high_quality"],
            "copied": 1,
            "reimported": true,
            "after": {"importer": "texture", "params": {"compress/mode": 2}}
        })),
        _ => Reply::Ok(json!({})),
    })
    .await;
    let manager = Arc::new(ToolManager::new(
        BridgeClient::new(bridge.port),
        CliRunner::new(PathBuf::from("unused-godot")),
    ));
    let result = manager
        .call_tool(
            "godot_set_import_params",
            json!({"path": "far.png", "copy_from": "res://sky.png",
                   "params": {"compress/mode": 2, "compres/high_quality": true}}),
        )
        .await;
    assert!(!result.is_error, "{}", text(&result));
    let report: Value = serde_json::from_str(&text(&result)).expect("JSON report");
    assert_eq!(report["source"], "editor");
    assert_eq!(report["path"], "res://far.png");
    assert_eq!(report["copy_from"], "res://sky.png");
    assert_eq!(report["new_options"], json!(["compres/high_quality"]));
    assert_eq!(report["ignored_options"], json!(["compres/high_quality"]));
    assert!(
        report["warning"]
            .as_str()
            .unwrap_or("")
            .contains("compres/high_quality"),
        "{report}"
    );
    let received = bridge.received.lock().expect("received").clone();
    let sent = received
        .iter()
        .find(|command| command["type"] == "import_params")
        .expect("import_params sent");
    assert_eq!(sent["params"]["path"], "res://far.png");
    assert_eq!(sent["params"]["copy_from"], "res://sky.png");
    assert_eq!(sent["params"]["action"], "set");
    assert_eq!(sent["params"]["params"]["compress/mode"], 2);
}

/// A stand-in engine for import parameter calls: a `--script` run answers `set` or `get` by the
/// request in its throw-away project, `--import` succeeds. Every call is logged.
#[cfg(unix)]
fn import_params_godot(dir: &Path, set_answer: &str, get_answer: &str) -> (PathBuf, PathBuf) {
    use std::os::unix::fs::PermissionsExt;
    let calls = dir.join("calls.txt");
    let path = dir.join("import_params_godot.sh");
    std::fs::write(
        &path,
        format!(
            "#!/bin/sh\necho \"$@\" >> '{}'\nwhile [ $# -gt 0 ]; do [ \"$1\" = --path ] && dir=\"$2\"; shift; done\ncase \"$(cat \"$dir/request.json\" 2>/dev/null)\" in\n  *'\"action\":\"get\"'*) echo 'GODOT_MCP_IMPORT={get_answer}' ;;\n  *'\"action\":\"set\"'*) echo 'GODOT_MCP_IMPORT={set_answer}' ;;\n  *) echo imported ;;\nesac\n",
            calls.display()
        ),
    )
    .expect("write import params godot");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
        .expect("chmod import params godot");
    (path, calls)
}

#[cfg(unix)]
#[tokio::test]
async fn without_the_editor_import_params_are_stored_then_imported_headless_and_read_back() {
    let project = project_with_texture();
    let engine = tempfile::tempdir().expect("engine dir");
    let (godot, calls) = import_params_godot(
        engine.path(),
        r#"{"importer":"texture","changed":["compress/mode","bogus/option"],"new_options":["bogus/option"],"copied":0}"#,
        r#"{"importer":"texture","type":"CompressedTexture2D","params":{"compress/mode":2},"types":{"compress/mode":"int"}}"#,
    );
    let cwd = tempfile::tempdir().expect("working dir");
    let result = manager_in(cwd.path(), godot)
        .call_tool(
            "godot_set_import_params",
            json!({"path": "res://sky.png", "params": {"compress/mode": 2, "bogus/option": 1},
                   "project_path": project.path()}),
        )
        .await;
    assert!(!result.is_error, "{}", text(&result));
    let report: Value = serde_json::from_str(&text(&result)).expect("JSON report");
    assert_eq!(report["source"], "headless");
    assert_eq!(report["reimported"], true);
    assert_eq!(report["after"]["params"]["compress/mode"], 2);
    assert_eq!(report["ignored_options"], json!(["bogus/option"]));
    let calls = std::fs::read_to_string(&calls).expect("calls");
    let lines: Vec<&str> = calls.lines().collect();
    assert_eq!(lines.len(), 3, "set, import, read back: {calls}");
    assert!(lines[0].contains("--script"), "{calls}");
    assert!(lines[1].ends_with("--import"), "{calls}");
    assert!(lines[2].contains("--script"), "{calls}");
}

#[cfg(unix)]
#[tokio::test]
async fn import_params_that_change_nothing_are_not_imported_again() {
    let project = project_with_texture();
    let engine = tempfile::tempdir().expect("engine dir");
    let (godot, calls) = import_params_godot(
        engine.path(),
        r#"{"importer":"texture","changed":[],"new_options":[],"copied":0}"#,
        r#"{"importer":"texture","params":{"compress/mode":0},"types":{}}"#,
    );
    let cwd = tempfile::tempdir().expect("working dir");
    let result = manager_in(cwd.path(), godot)
        .call_tool(
            "godot_set_import_params",
            json!({"path": "res://sky.png", "params": {"compress/mode": 0},
                   "project_path": project.path()}),
        )
        .await;
    assert!(!result.is_error, "{}", text(&result));
    let report: Value = serde_json::from_str(&text(&result)).expect("JSON report");
    assert_eq!(report["reimported"], false);
    assert!(
        report["note"]
            .as_str()
            .unwrap_or("")
            .contains("Nothing changed")
    );
    let calls = std::fs::read_to_string(&calls).expect("calls");
    assert!(!calls.contains("--import"), "{calls}");
}

#[tokio::test]
async fn an_importer_is_checked_against_the_ones_godot_offers_for_the_file() {
    let project = project_with_texture();
    for (importer, expected) in [
        ("wav", "3d_texture"),
        ("svg", "Godot offers"),
        ("3d texture", "letters, digits and _"),
    ] {
        let result = call(
            "godot_set_import_params",
            json!({"path": "res://sky.png", "importer": importer,
                   "project_path": project.path()}),
        )
        .await;
        assert!(result.is_error, "{importer}");
        assert!(
            text(&result).contains(expected),
            "{importer}: {}",
            text(&result)
        );
    }
    let sidecar = std::fs::read_to_string(project.path().join("sky.png.import")).expect("sidecar");
    assert!(sidecar.contains("importer=\"texture\""), "{sidecar}");
}

#[cfg(unix)]
#[tokio::test]
async fn without_the_editor_a_new_importer_is_stored_imported_headless_and_reported() {
    let project = project_with_texture();
    let engine = tempfile::tempdir().expect("engine dir");
    let (godot, calls) = import_params_godot(
        engine.path(),
        r#"{"importer":"3d_texture","previous_importer":"texture","importer_changed":true,"changed":["slices/horizontal"],"new_options":["slices/horizontal"],"copied":0}"#,
        r#"{"importer":"3d_texture","type":"CompressedTexture3D","params":{"slices/horizontal":33,"slices/vertical":1},"types":{}}"#,
    );
    let cwd = tempfile::tempdir().expect("working dir");
    let result = manager_in(cwd.path(), godot)
        .call_tool(
            "godot_set_import_params",
            json!({"path": "res://sky.png", "importer": "3d_texture",
                   "params": {"slices/horizontal": 33}, "project_path": project.path()}),
        )
        .await;
    assert!(!result.is_error, "{}", text(&result));
    let report: Value = serde_json::from_str(&text(&result)).expect("JSON report");
    assert_eq!(report["importer"], "3d_texture");
    assert_eq!(report["previous_importer"], "texture");
    assert_eq!(report["importer_changed"], true);
    assert_eq!(report["type"], "CompressedTexture3D");
    assert_eq!(report["reimported"], true);
    let calls = std::fs::read_to_string(&calls).expect("calls");
    assert!(calls.contains("--import"), "{calls}");
}

#[cfg(unix)]
#[tokio::test]
async fn a_new_importer_alone_is_a_change_and_one_godot_does_not_take_is_an_error() {
    let project = project_with_texture();
    let engine = tempfile::tempdir().expect("engine dir");
    let (godot, _) = import_params_godot(
        engine.path(),
        r#"{"importer":"my_plugin_importer","previous_importer":"texture","importer_changed":true,"changed":[],"new_options":[],"copied":0}"#,
        r#"{"importer":"texture","type":"CompressedTexture2D","params":{},"types":{}}"#,
    );
    let cwd = tempfile::tempdir().expect("working dir");
    let result = manager_in(cwd.path(), godot)
        .call_tool(
            "godot_set_import_params",
            json!({"path": "res://sky.png", "importer": "my_plugin_importer",
                   "project_path": project.path()}),
        )
        .await;
    assert!(result.is_error, "{}", text(&result));
    let message = text(&result);
    assert!(message.contains("my_plugin_importer"), "{message}");
    assert!(message.contains("imported it with texture"), "{message}");
}

#[tokio::test]
async fn an_importer_change_goes_to_the_editor_that_has_the_project() {
    use common::{MockBridge, Reply};
    let project = project_with_texture();
    let dir = project.path().to_string_lossy().replace('\\', "/");
    let bridge = MockBridge::start(move |command| match command["type"].as_str() {
        Some("get_project_info") => Reply::Ok(json!({"project_dir": dir})),
        Some("import_params") => Reply::Ok(json!({
            "importer": "3d_texture",
            "previous_importer": "texture",
            "importer_changed": true,
            "changed": ["slices/horizontal"],
            "new_options": ["slices/horizontal"],
            "copied": 0,
            "reimported": true,
            "after": {"importer": "3d_texture", "type": "CompressedTexture3D",
                      "params": {"slices/horizontal": 33}}
        })),
        _ => Reply::Ok(json!({})),
    })
    .await;
    let manager = Arc::new(ToolManager::new(
        BridgeClient::new(bridge.port),
        CliRunner::new(PathBuf::from("unused-godot")),
    ));
    let result = manager
        .call_tool(
            "godot_set_import_params",
            json!({"path": "sky.png", "importer": "3d_texture",
                   "params": {"slices/horizontal": 33}}),
        )
        .await;
    assert!(!result.is_error, "{}", text(&result));
    let report: Value = serde_json::from_str(&text(&result)).expect("JSON report");
    assert_eq!(report["source"], "editor");
    assert_eq!(report["type"], "CompressedTexture3D");
    assert_eq!(report["previous_importer"], "texture");
    let received = bridge.received.lock().expect("received").clone();
    let sent = received
        .iter()
        .find(|command| command["type"] == "import_params")
        .expect("import_params sent");
    assert_eq!(sent["params"]["importer"], "3d_texture");
}

/// A project holding the addon of an older server, and a fake editor of it that runs that addon:
/// it knows `get_project_info` and answers every command in `unknown` as the old addon does.
async fn editor_with_old_addon(
    unknown: &'static [&'static str],
) -> (tempfile::TempDir, common::MockBridge) {
    use common::{MockBridge, Reply};
    let project = project_dir();
    let addon = project.path().join("addons/godot_mcp");
    std::fs::create_dir_all(&addon).expect("addon dir");
    std::fs::write(
        addon.join("plugin.cfg"),
        "[plugin]\n\nname=\"Godot MCP Bridge\"\nversion=\"0.11.1-dev+godot.4.8\"\nscript=\"plugin.gd\"\n",
    )
    .expect("old manifest");
    let dir = project.path().to_string_lossy().replace('\\', "/");
    let bridge = MockBridge::start(move |command| match command["type"].as_str() {
        Some("get_project_info") => Reply::Ok(json!({"project_dir": dir})),
        Some(other) if unknown.contains(&other) => {
            Reply::Err(format!("Unknown command type: {other}"))
        }
        _ => Reply::Ok(json!({})),
    })
    .await;
    (project, bridge)
}

fn editor_manager(bridge: &common::MockBridge) -> Arc<ToolManager> {
    Arc::new(ToolManager::new(
        BridgeClient::new(bridge.port),
        CliRunner::new(PathBuf::from("unused-godot")),
    ))
}

#[tokio::test]
async fn a_command_the_editors_addon_lacks_names_both_versions_and_the_update() {
    let (project, bridge) = editor_with_old_addon(&["get_scene_tree"]).await;
    let result = editor_manager(&bridge)
        .call_tool("godot_get_scene_tree", json!({}))
        .await;
    assert!(result.is_error);
    let message = text(&result);
    assert!(message.contains("0.11.1-dev+godot.4.8"), "{message}");
    assert!(
        message.contains(env!("CARGO_PKG_VERSION")),
        "the server's version: {message}"
    );
    assert!(message.contains("get_scene_tree"), "{message}");
    assert!(message.contains("godot_update_addon"), "{message}");
    // The hint names the project as the editor reports it, with forward slashes on every OS.
    assert!(
        message.contains(&project.path().to_string_lossy().replace('\\', "/")),
        "the project to update: {message}"
    );
    assert!(!message.contains("retry in a moment"), "{message}");
}

#[tokio::test]
async fn shaders_are_read_and_edited_in_their_file_when_the_editors_addon_is_older() {
    let (project, bridge) = editor_with_old_addon(&["read_shader", "edit_shader"]).await;
    let shader = "shader_type spatial;\n\nuniform float sway = 1.0;\n";
    std::fs::write(project.path().join("wind.gdshader"), shader).expect("shader");
    let tools = editor_manager(&bridge);

    let read = Arc::clone(&tools)
        .call_tool(
            "godot_read_shader",
            json!({"shader_path": "res://wind.gdshader", "project_path": project.path()}),
        )
        .await;
    assert!(!read.is_error, "{}", text(&read));
    let answer: Value = serde_json::from_str(&text(&read)).expect("JSON answer");
    assert!(
        answer.to_string().contains("uniform float sway"),
        "{answer}"
    );
    assert_eq!(answer["addon_version"], "0.11.1-dev+godot.4.8");
    assert!(
        answer["note"]
            .as_str()
            .unwrap_or_default()
            .contains("godot_update_addon"),
        "{answer}"
    );

    let edit = tools
        .call_tool(
            "godot_edit_shader",
            json!({"shader_path": "res://wind.gdshader", "project_path": project.path(),
                   "old_text": "uniform float sway = 1.0;",
                   "new_text": "uniform float sway = 1.0;\nuniform float gust = 0.5;"}),
        )
        .await;
    assert!(!edit.is_error, "{}", text(&edit));
    assert!(
        text(&edit).contains("older than this server"),
        "{}",
        text(&edit)
    );
    assert!(
        std::fs::read_to_string(project.path().join("wind.gdshader"))
            .expect("shader")
            .contains("uniform float gust = 0.5;")
    );
}

#[tokio::test]
async fn the_addon_is_installed_from_this_build_and_enabled() {
    let project = project_dir();
    let addon = project.path().join("addons/godot_mcp");
    std::fs::create_dir_all(&addon).expect("addon dir");
    std::fs::write(addon.join("plugin.cfg"), "version=\"0.11.1\"\n").expect("old manifest");
    std::fs::write(addon.join("retired.gd"), "extends RefCounted\n").expect("old script");
    std::fs::write(project.path().join("keep.txt"), "mine").expect("project file");

    let result = call(
        "godot_update_addon",
        json!({"project_path": project.path()}),
    )
    .await;
    assert!(!result.is_error, "{}", text(&result));
    let report: Value = serde_json::from_str(&text(&result)).expect("JSON report");
    assert_eq!(report["previous_version"], "0.11.1");
    assert_eq!(report["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(report["installed"], true);
    assert_eq!(report["removed"], json!(["retired.gd"]));
    assert_eq!(report["enabled_now"], true);
    let manifest = std::fs::read_to_string(addon.join("plugin.cfg")).expect("manifest");
    assert!(manifest.contains(env!("CARGO_PKG_VERSION")), "{manifest}");
    assert!(addon.join("bridge_server.gd").is_file());
    assert!(!addon.join("retired.gd").exists());
    assert_eq!(
        std::fs::read_to_string(project.path().join("keep.txt")).expect("kept"),
        "mine"
    );
    let settings = std::fs::read_to_string(project.path().join("project.godot")).expect("project");
    assert!(
        settings.contains("enabled=PackedStringArray(\"res://addons/godot_mcp/plugin.cfg\")"),
        "{settings}"
    );

    let again = call(
        "godot_update_addon",
        json!({"project_path": project.path()}),
    )
    .await;
    let again: Value = serde_json::from_str(&text(&again)).expect("JSON report");
    assert_eq!(again["installed"], false, "{again}");
    assert_eq!(again["enabled_now"], false, "{again}");
}

#[tokio::test]
async fn the_addon_is_not_replaced_under_a_connected_editor_without_restart_editor() {
    let (project, bridge) = editor_with_old_addon(&[]).await;
    let result = editor_manager(&bridge)
        .call_tool(
            "godot_update_addon",
            json!({"project_path": project.path()}),
        )
        .await;
    assert!(result.is_error, "{}", text(&result));
    assert!(
        text(&result).contains("restart_editor: true"),
        "{}",
        text(&result)
    );
    let manifest = std::fs::read_to_string(project.path().join("addons/godot_mcp/plugin.cfg"))
        .expect("manifest");
    assert!(manifest.contains("0.11.1"), "untouched: {manifest}");
}

#[tokio::test]
async fn the_servers_own_source_tree_is_not_overwritten_by_the_update() {
    let result = call(
        "godot_update_addon",
        json!({"project_path": env!("CARGO_MANIFEST_DIR")}),
    )
    .await;
    assert!(result.is_error);
    assert!(text(&result).contains("source tree"), "{}", text(&result));
}

#[tokio::test]
async fn an_update_passes_discard_to_the_editor_it_closes() {
    use common::{MockBridge, Reply};
    let project = project_dir();
    let dir = project.path().to_string_lossy().replace('\\', "/");
    let bridge = MockBridge::start(move |command| match command["type"].as_str() {
        Some("get_project_info") => {
            Reply::Ok(json!({"project_dir": dir, "addon_version": "0.11.1"}))
        }
        Some("close_editor") => Reply::Ok(json!({
            "closing": false, "unsaved_scenes": ["res://level.tscn"], "unsaved_scripts": []
        })),
        _ => Reply::Ok(json!({})),
    })
    .await;
    let tools = editor_manager(&bridge);
    for discard in [false, true] {
        let result = Arc::clone(&tools)
            .call_tool(
                "godot_update_addon",
                json!({"project_path": project.path(), "restart_editor": true, "discard": discard}),
            )
            .await;
        assert!(result.is_error, "{}", text(&result));
        let message = text(&result);
        assert!(message.contains("res://level.tscn"), "{message}");
        assert!(message.contains("discard: true"), "{message}");
        let received = bridge.received.lock().expect("received").clone();
        let close = received
            .iter()
            .rev()
            .find(|command| command["type"] == "close_editor")
            .expect("close_editor sent");
        assert_eq!(close["params"]["discard"], discard, "{close}");
    }
    assert!(
        !project.path().join("addons/godot_mcp").exists(),
        "nothing installed"
    );
}

/// godot_run_script keeps the leak reports Godot prints while it shuts down (leaked instance
/// dependencies, RID allocations, ObjectDB instances...) but shows a repeated one once with its
/// count, counts them by kind in exit_leaks, says in hint what causes them, and leaves every
/// other line where it was.
#[cfg(unix)]
#[tokio::test]
async fn run_script_collapses_repeated_exit_time_leaks_and_says_what_causes_them() {
    use std::os::unix::fs::PermissionsExt;
    let project = project_dir();
    std::fs::write(
        project.path().join("place.gd"),
        "extends SceneTree\n\nfunc _initialize() -> void:\n\tquit()\n",
    )
    .expect("write script");
    let godot = project.path().join("leaking_godot.sh");
    let mut script = String::from(
        "#!/bin/sh\necho 'placed 100 zones'\necho \"ERROR: 7 RID allocations of type 'N13RendererDummy9DummyMeshE' were leaked at exit.\"\n",
    );
    script.push_str("echo 'ERROR: Failed to load resource res://zones/missing.tscn.' >&2\n");
    script.push_str("echo '   at: _load (core/io/resource_loader.cpp:283)' >&2\n");
    for _ in 0..150 {
        script.push_str(
            "echo 'WARNING: Leaked instance dependency: Bug - did not call instance_notify_deleted when freeing.' >&2\n",
        );
        script.push_str(
            "echo '     at: ~DependencyTracker (servers/rendering/renderer_rd/storage_rd/utilities.h:79)' >&2\n",
        );
    }
    script.push_str("echo 'WARNING: 5 ObjectDB instances were leaked at exit (run with `--verbose` for details).' >&2\n");
    script.push_str("echo '   at: cleanup (core/object/object.cpp:2609)' >&2\n");
    script.push_str(
        "echo 'ERROR: 3 resources still in use at exit (run with --verbose for details).' >&2\n",
    );
    script.push_str("echo '   at: clear (core/io/resource.cpp:599)' >&2\n");
    script.push_str("echo 'ERROR: Pages in use exist at exit in PagedAllocator: N7Skeleton' >&2\n");
    script.push_str("echo '   at: ~PagedAllocator (./core/templates/paged_allocator.h:170)' >&2\n");
    std::fs::write(&godot, script).expect("write fake godot");
    std::fs::set_permissions(&godot, std::fs::Permissions::from_mode(0o755))
        .expect("chmod fake godot");

    let result = manager_in(project.path(), godot)
        .call_tool("godot_run_script", json!({"script": "res://place.gd"}))
        .await;
    assert!(!result.is_error, "{}", text(&result));
    let answer: Value = serde_json::from_str(&text(&result)).expect("json answer");
    assert_eq!(
        answer["exit_leaks"],
        json!({
            "leaked_instance_dependency": 150,
            "objectdb_instances": 1,
            "paged_allocator": 1,
            "resources_in_use": 1,
            "rid_allocations": 1,
        }),
        "{answer}"
    );
    assert_eq!(
        answer["stdout"],
        "placed 100 zones\nERROR: 7 RID allocations of type 'N13RendererDummy9DummyMeshE' were leaked at exit.",
        "{answer}"
    );
    assert_eq!(
        answer["stderr"],
        concat!(
            "ERROR: Failed to load resource res://zones/missing.tscn.\n",
            "   at: _load (core/io/resource_loader.cpp:283)\n",
            "WARNING: Leaked instance dependency: Bug - did not call instance_notify_deleted when freeing. ×150\n",
            "     at: ~DependencyTracker (servers/rendering/renderer_rd/storage_rd/utilities.h:79)\n",
            "WARNING: 5 ObjectDB instances were leaked at exit (run with `--verbose` for details).\n",
            "   at: cleanup (core/object/object.cpp:2609)\n",
            "ERROR: 3 resources still in use at exit (run with --verbose for details).\n",
            "   at: clear (core/io/resource.cpp:599)\n",
            "ERROR: Pages in use exist at exit in PagedAllocator: N7Skeleton\n",
            "   at: ~PagedAllocator (./core/templates/paged_allocator.h:170)",
        ),
        "{answer}"
    );
    let hint = answer["hint"].as_str().unwrap_or_default();
    assert!(
        hint.contains("free()") && hint.contains("quit()") && hint.contains("--verbose"),
        "{answer}"
    );
}

/// A run with no exit-time reports answers exit_leaks and hint null.
#[cfg(unix)]
#[tokio::test]
async fn run_script_without_exit_time_leaks_answers_exit_leaks_null() {
    let project = project_dir();
    std::fs::write(
        project.path().join("place.gd"),
        "extends SceneTree\n\nfunc _initialize() -> void:\n\tquit()\n",
    )
    .expect("write script");
    let result = manager_in(project.path(), fake_godot(project.path()))
        .call_tool("godot_run_script", json!({"script": "res://place.gd"}))
        .await;
    assert!(!result.is_error, "{}", text(&result));
    let answer: Value = serde_json::from_str(&text(&result)).expect("json answer");
    assert!(answer["exit_leaks"].is_null(), "{answer}");
    assert!(answer["hint"].is_null(), "{answer}");
    assert!(
        answer["stdout"]
            .as_str()
            .is_some_and(|out| out.contains("NOISE_ON_STDOUT")),
        "{answer}"
    );
}

#[tokio::test]
async fn review_assets_is_listed_and_checks_its_request_before_starting_godot() {
    let review = ToolManager::list_tools()
        .into_iter()
        .find(|tool| tool.name == "godot_review_assets")
        .expect("godot_review_assets is listed");
    let schema = serde_json::to_value(&review).expect("tool");
    assert_eq!(schema["annotations"]["readOnlyHint"], false, "{schema}");
    assert_eq!(schema["annotations"]["destructiveHint"], false, "{schema}");
    for key in [
        "assets",
        "environment",
        "camera",
        "planes",
        "width",
        "height",
    ] {
        assert!(
            schema["inputSchema"]["properties"].get(key).is_some(),
            "{key}: {schema}"
        );
    }
    assert_eq!(schema["inputSchema"]["required"], json!(["assets"]));

    let project = project_dir();
    std::fs::write(project.path().join("rock.tscn"), "").expect("scene");
    let tools = manager_in(
        project.path(),
        PathBuf::from("godot-binary-that-does-not-exist"),
    );
    let outside = Arc::clone(&tools)
        .call_tool(
            "godot_review_assets",
            json!({"assets": [{"scene": "res://../rock.tscn"}]}),
        )
        .await;
    assert!(outside.is_error, "{}", text(&outside));
    assert!(
        text(&outside).contains("assets[0].scene"),
        "{}",
        text(&outside)
    );
    let bad_camera = Arc::clone(&tools)
        .call_tool(
            "godot_review_assets",
            json!({"assets": [{"scene": "res://rock.tscn"}], "camera": {"yaw": "left"}}),
        )
        .await;
    assert!(bad_camera.is_error, "{}", text(&bad_camera));
    assert!(
        text(&bad_camera).contains("camera.yaw"),
        "{}",
        text(&bad_camera)
    );
    // A valid request reaches the engine, which is missing here.
    let no_engine = Arc::clone(&tools)
        .call_tool(
            "godot_review_assets",
            json!({"assets": [{"scene": "res://rock.tscn"}]}),
        )
        .await;
    assert!(no_engine.is_error, "{}", text(&no_engine));
    assert!(
        text(&no_engine).contains("GODOT_PATH"),
        "{}",
        text(&no_engine)
    );
    let names: Vec<String> = std::fs::read_dir(project.path())
        .expect("project")
        .map(|entry| {
            entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    assert!(
        !names.iter().any(|name| name.contains("review")),
        "{names:?}"
    );
}

#[tokio::test]
async fn world_map_is_listed_and_checks_its_request_before_starting_godot() {
    let map = ToolManager::list_tools()
        .into_iter()
        .find(|tool| tool.name == "godot_world_map")
        .expect("godot_world_map is listed");
    let schema = serde_json::to_value(&map).expect("tool");
    assert_eq!(schema["annotations"]["readOnlyHint"], false, "{schema}");
    assert_eq!(schema["annotations"]["destructiveHint"], false, "{schema}");
    for key in [
        "dir",
        "glob",
        "recursive",
        "scenes",
        "rect",
        "pixels_per_metre",
        "view",
        "labels",
        "tile_metres",
    ] {
        assert!(
            schema["inputSchema"]["properties"].get(key).is_some(),
            "{key}: {schema}"
        );
    }

    let project = project_dir();
    std::fs::create_dir_all(project.path().join("zones")).expect("zones");
    std::fs::write(project.path().join("zones/zone_0_0.tscn"), "").expect("scene");
    let tools = manager_in(
        project.path(),
        PathBuf::from("godot-binary-that-does-not-exist"),
    );
    let outside = Arc::clone(&tools)
        .call_tool("godot_world_map", json!({"dir": "res://../zones"}))
        .await;
    assert!(outside.is_error, "{}", text(&outside));
    assert!(text(&outside).contains("dir:"), "{}", text(&outside));
    let unmatched = Arc::clone(&tools)
        .call_tool(
            "godot_world_map",
            json!({"dir": "res://zones", "glob": "tile_*.tscn"}),
        )
        .await;
    assert!(unmatched.is_error, "{}", text(&unmatched));
    assert!(
        text(&unmatched).contains("matches tile_*.tscn"),
        "{}",
        text(&unmatched)
    );
    // A valid request reaches the engine, which is missing here.
    let no_engine = Arc::clone(&tools)
        .call_tool("godot_world_map", json!({"dir": "res://zones"}))
        .await;
    assert!(no_engine.is_error, "{}", text(&no_engine));
    assert!(
        text(&no_engine).contains("GODOT_PATH"),
        "{}",
        text(&no_engine)
    );
}
