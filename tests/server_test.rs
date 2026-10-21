use godot_mcp::protocol::{JsonRpcResponse, ToolResult};
use godot_mcp::tools::ToolManager;
use serde_json::json;

#[test]
fn test_jsonrpc_success_response() {
    let resp = JsonRpcResponse::success(json!(1), json!({"status": "ok"}));
    let serialized = serde_json::to_string(&resp).expect("Serialization failed");

    assert!(serialized.contains("\"jsonrpc\":\"2.0\""));
    assert!(serialized.contains("\"id\":1"));
    assert!(serialized.contains("\"status\":\"ok\""));
}

#[test]
fn test_jsonrpc_error_response() {
    let resp = JsonRpcResponse::error(json!("req-123"), -32601, "Method not found");
    let serialized = serde_json::to_string(&resp).expect("Serialization failed");

    assert!(serialized.contains("\"code\":-32601"));
    assert!(serialized.contains("Method not found"));
}

#[test]
fn test_tool_definitions() {
    let tools = ToolManager::list_tools();
    assert!(!tools.is_empty());

    let tool_names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
    assert!(tool_names.contains(&"godot_get_version"));
    assert!(tool_names.contains(&"godot_get_scene_tree"));
    assert!(tool_names.contains(&"godot_add_node"));
    assert!(tool_names.contains(&"godot_remove_node"));
    assert!(tool_names.contains(&"godot_capture_viewport"));
    assert!(tool_names.contains(&"godot_create_script"));
    assert!(tool_names.contains(&"godot_validate_script"));
    assert!(tool_names.contains(&"godot_stop_project"));
    assert!(tool_names.contains(&"godot_get_run_status"));
    assert!(tool_names.contains(&"godot_get_open_scenes"));
    assert!(tool_names.contains(&"godot_resave_resources"));
    assert!(tool_names.contains(&"godot_read_script"));
    assert!(tool_names.contains(&"godot_edit_script"));

    // Verify all input schemas are valid JSON objects
    for tool in tools {
        assert!(tool.input_schema.is_object());
        assert_eq!(tool.input_schema["type"], "object");
    }
}

#[test]
fn test_tool_result_helpers() {
    let text_res = ToolResult::text("Sample output");
    assert!(!text_res.is_error);
    assert_eq!(text_res.content.len(), 1);

    let err_res = ToolResult::error("Sample error", &["Recovery suggestion 1"]);
    assert!(err_res.is_error);
    assert_eq!(err_res.content.len(), 1);
}
