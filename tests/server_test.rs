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

fn schema_of(name: &str) -> serde_json::Value {
    ToolManager::list_tools()
        .into_iter()
        .find(|tool| tool.name == name)
        .unwrap_or_else(|| panic!("tool {name} not in the catalog"))
        .input_schema
}

/// TL-1: ranges the tool documents are in its schema, and object parameters describe their fields.
#[test]
fn schemas_carry_the_documented_ranges_and_describe_object_fields() {
    let complete = schema_of("godot_code_complete");
    assert_eq!(complete["properties"]["line"]["minimum"], 1);
    assert_eq!(complete["properties"]["character"]["minimum"], 1);
    assert_eq!(
        schema_of("godot_code_definition")["properties"]["line"]["minimum"],
        1
    );
    assert_eq!(
        schema_of("godot_code_references")["properties"]["character"]["minimum"],
        1
    );

    let shot = schema_of("godot_game_screenshot");
    assert_eq!(shot["properties"]["tolerance"]["minimum"], 0);
    assert_eq!(shot["properties"]["tolerance"]["maximum"], 255);
    assert_eq!(shot["properties"]["frames"]["maximum"], 8);
    for field in ["x", "y", "w", "h"] {
        assert_eq!(
            shot["properties"]["roi"]["properties"][field]["type"], "integer",
            "roi.{field}"
        );
    }

    let physics = schema_of("godot_game_physics");
    assert_eq!(physics["properties"]["ticks"]["minimum"], 1);
    assert_eq!(physics["properties"]["max"]["maximum"], 3600);
    let until = &physics["properties"]["until"]["properties"];
    for field in ["node", "property", "op", "value"] {
        assert!(until.get(field).is_some(), "until.{field}");
    }
    assert!(
        until["op"]["enum"]
            .as_array()
            .is_some_and(|ops| ops.len() >= 6)
    );

    let input = schema_of("godot_game_input");
    assert_eq!(input["properties"]["hold_ticks"]["maximum"], 600);
    let step = &input["properties"]["steps"]["items"];
    let kinds: Vec<&str> = step["properties"]["type"]["enum"]
        .as_array()
        .expect("step types are listed")
        .iter()
        .filter_map(|kind| kind.as_str())
        .collect();
    for kind in [
        "key",
        "click_node",
        "swipe",
        "wait",
        "time_scale",
        "joy_axis",
    ] {
        assert!(kinds.contains(&kind), "step type {kind} in {kinds:?}");
    }
    assert_eq!(step["required"], serde_json::json!(["type"]));
}

#[test]
fn create_script_takes_script_path_like_the_other_script_tools() {
    let schema = schema_of("godot_create_script");
    assert!(schema["properties"].get("script_path").is_some());
    assert!(schema["properties"].get("file_path").is_some());
    assert!(
        schema
            .get("required")
            .is_none_or(|required| required.as_array().is_some_and(|r| r.is_empty())),
        "neither name may be required alone: {schema}"
    );
}
