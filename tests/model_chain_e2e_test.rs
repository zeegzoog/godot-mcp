//! Froxel MCP export and real Godot import, with generated, isolated fixtures.
//! FROXEL_BIN=/path/to/froxel GODOT_PATH=/path/to/godot cargo test --test model_chain_e2e_test -- --ignored --test-threads=1

use godot_mcp::bridge::BridgeClient;
use godot_mcp::cli_runner::CliRunner;
use godot_mcp::protocol::ToolResult;
use godot_mcp::tools::ToolManager;
use serde_json::{Value, json};
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Arc;

fn fixture() -> (tempfile::TempDir, Arc<ToolManager>, PathBuf) {
    let _ = dotenvy::dotenv();
    let froxel = std::env::var_os("FROXEL_BIN").expect("set FROXEL_BIN");
    let godot = PathBuf::from(std::env::var_os("GODOT_PATH").expect("set GODOT_PATH"));
    let project = tempfile::tempdir().expect("project");
    std::fs::write(project.path().join("project.godot"),
        "config_version=5\n\n[application]\n\nconfig/name=\"ModelChain\"\nconfig/features=PackedStringArray(\"4.8\")\n").unwrap();
    let vox = project.path().join("source.vox");
    // Generate the source through the public Froxel MCP interface, not a local fixture path.
    let mut child = Command::new(froxel)
        .arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    for request in [
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"model-chain-test","version":"1"}}}),
        json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
        json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"set_voxel","arguments":{"x":0,"y":0,"z":0,"palette":3}}}),
        json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"save_file","arguments":{"path":vox}}}),
    ] {
        writeln!(stdin, "{request}").unwrap();
    }
    drop(stdin);
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(vox.is_file(), "{}", String::from_utf8_lossy(&output.stdout));
    let tools = Arc::new(
        ToolManager::new(BridgeClient::new(9_351), CliRunner::new(godot))
            .with_working_dir(project.path().to_path_buf()),
    );
    (project, tools, vox)
}

async fn check(tools: Arc<ToolManager>, args: Value) -> ToolResult {
    tools.call_tool("godot_check_voxel_model", args).await
}

#[tokio::test]
#[ignore = "needs FROXEL_BIN and GODOT_PATH"]
async fn a_voxel_model_is_exported_imported_and_described() {
    let (project, tools, vox) = fixture();
    let result = check(tools,json!({"project_path":project.path(),"vox":vox,"dest":"res://art/model.glb","preview":true})).await;
    let body = json_of(&result);
    assert!(!result.is_error, "{body}");
    assert_eq!(body["ok"], true, "{body}");
    assert_eq!(
        body["export"]["models"][0]["structure"]["nodes"][0]["name"], "voxels",
        "{body}"
    );
    assert!(
        body["export"]["models"][0]["structure"]["warnings"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(
        project.path().join("art/model.glb.import").is_file(),
        "{body}"
    );
    assert!(
        project.path().join("art/model.preview.png").is_file(),
        "{body}"
    );
}

#[tokio::test]
#[ignore = "needs FROXEL_BIN and GODOT_PATH"]
async fn a_repeated_check_of_an_imported_model_is_not_stale() {
    let (project, tools, vox) = fixture();
    let args = json!({"project_path":project.path(),"vox":vox,"dest":"res://again.glb"});
    for _ in 0..2 {
        let result = check(Arc::clone(&tools), args.clone()).await;
        let body = json_of(&result);
        assert!(!result.is_error, "{body}");
        assert!(
            body["stale_imports"].as_array().unwrap().is_empty(),
            "{body}"
        );
    }
}

#[tokio::test]
#[ignore = "needs FROXEL_BIN and GODOT_PATH"]
async fn all_failed_exports_keep_the_original_error_and_skip_import() {
    let (project, tools, vox) = fixture();
    std::fs::write(&vox, b"malformed fixture").unwrap();
    let result = check(
        tools,
        json!({"project_path":project.path(),"vox":vox,"dest":"res://bad.glb"}),
    )
    .await;
    let body = json_of(&result);
    assert!(result.is_error, "{body}");
    assert_eq!(body["export"]["failed"], 1);
    assert!(
        body["export"]["models"][0]["error"]
            .as_str()
            .unwrap()
            .contains("not a .vox"),
        "{body}"
    );
    assert_eq!(body["import"]["skipped"], true, "{body}");
}

#[tokio::test]
#[ignore = "needs FROXEL_BIN and GODOT_PATH"]
async fn a_mixed_batch_imports_only_successful_exports() {
    let (project, tools, vox) = fixture();
    let bad = project.path().join("bad.vox");
    std::fs::write(&bad, b"malformed fixture").unwrap();
    let result = check(
        tools,
        json!({"project_path":project.path(),"models":[
        {"vox":vox,"dest":"res://good.glb"},{"vox":bad,"dest":"res://bad.glb"}
    ],"preview":true}),
    )
    .await;
    let body = json_of(&result);
    assert!(result.is_error, "{body}");
    assert_eq!(body["export"]["models"][0]["ok"], true);
    assert_eq!(body["export"]["failed"], 1);
    assert!(
        body["import"]["skipped_missing"]
            .as_array()
            .unwrap()
            .is_empty(),
        "{body}"
    );
    assert_eq!(body["previews"].as_array().unwrap().len(), 1, "{body}");
    assert!(project.path().join("good.glb.import").is_file());
    assert!(!project.path().join("bad.glb").exists());
}

#[tokio::test]
#[ignore = "needs FROXEL_BIN and GODOT_PATH"]
async fn an_import_failure_preserves_the_export_report() {
    let (project, _, vox) = fixture();
    let tools = Arc::new(
        ToolManager::new(
            BridgeClient::new(9_351),
            CliRunner::new(PathBuf::from("nonexistent-godot-model-chain-test")),
        )
        .with_working_dir(project.path().to_path_buf()),
    );
    let result = check(
        tools,
        json!({"project_path":project.path(),"vox":vox,"dest":"res://model.glb"}),
    )
    .await;
    let body = json_of(&result);
    assert!(result.is_error, "{body}");
    assert_eq!(body["export"]["models"][0]["ok"], true, "{body}");
    assert!(
        body["import"]["text"]
            .as_str()
            .unwrap()
            .contains("Failed to import"),
        "{body}"
    );
}

fn json_of(result: &ToolResult) -> Value {
    let text = result
        .content
        .iter()
        .find_map(|item| match item {
            godot_mcp::protocol::ToolContent::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .unwrap_or("");
    serde_json::from_str(text).unwrap_or_else(|_| panic!("not json: {text}"))
}
