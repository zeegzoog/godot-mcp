//! `godot_set_resource_properties`: changes the properties of a resource saved in its own
//! `.tres` / `.res` file and saves it.
//!
//! With the editor connected to the project, the bridge changes the copy the editor has loaded
//! (so every open scene that uses it follows) as one undoable action and saves the file. Without
//! it, a headless Godot run changes and saves the file through the same addon code
//! (`addons/godot_mcp/core/property_values.gd`), and the server repairs what a headless save
//! loses, as it does after `godot_run_script`.

use super::{EditorChoice, ToolManager, str_arg};
use crate::error::McpError;
use crate::paths::{res_display, resolve_in_project};
use crate::protocol::{Tool, ToolResult};
use serde_json::{Value, json};

pub const NAME: &str = "godot_set_resource_properties";

pub fn catalog() -> Vec<Tool> {
    vec![super::tool(
        NAME,
        "Sets properties of a resource in its own .tres/.res and saves it; use it for a shared .tres that godot_set_node_properties refuses. Values convert as there; a ShaderMaterial uniform is shader_parameter/<uniform> and must exist in the shader. A count is set first, so the entries it adds go in the same call ({\"streams_count\": 1, \"stream_0/stream\": ...}). All or nothing: unknown properties, unconvertible values, ':' steps, resource_path, script, private names or values that read back differently change nothing. changed lists values before and after. With the editor: one undoable action (mode editor). Without: saved headless (mode headless) with uids restored (resource_repairs; resource_damage: what could not be; headless_data_loss: a MultiMesh whose instance data headless Godot dropped).",
        json!({
            "resource_path": {
                "type": "string",
                "description": "res:// path of the .tres or .res."
            },
            "properties": {
                "type": "object",
                "description": "Name to value, such as {\"shader_parameter/wave_height\": 0.4}."
            },
            "project_path": {
                "type": "string",
                "description": "Default: editor's, else server's project."
            }
        }),
        &["resource_path", "properties"],
    ).destructive_idempotent()]
}

pub async fn call(tools: &ToolManager, args: Value) -> ToolResult {
    let Some(path) = str_arg(&args, "resource_path") else {
        return ToolResult::error(
            "Missing resource_path",
            &["Pass resource_path: the res:// path of a .tres or .res file"],
        );
    };
    if !(path.ends_with(".tres") || path.ends_with(".res")) {
        return ToolResult::error(
            format!("resource_path must name a .tres or .res file: {path}"),
            &[
                "Use a path such as res://materials/water.tres",
                "A resource built into a scene is changed with godot_set_node_properties and a ':' step (material_override:shader_parameter/glow_amount)",
            ],
        );
    }
    let properties = match args.get("properties") {
        Some(Value::Object(map)) if !map.is_empty() => Value::Object(map.clone()),
        _ => {
            return ToolResult::error(
                "properties must be a non-empty object of property names and values",
                &["Pass properties such as {\"shader_parameter/wave_height\": 0.4}"],
            );
        }
    };
    let choice = match tools.editor_choice(&args, true).await {
        Ok(choice) => choice,
        Err(result) => return result,
    };
    let sent = if matches!(choice, EditorChoice::Host) {
        Err(McpError::BridgeUnavailable(
            "another project than the editor's was named".into(),
        ))
    } else {
        tools
            .bridge
            .send_command(
                "set_resource_properties",
                json!({"resource_path": path, "properties": properties}),
            )
            .await
    };
    match sent {
        Ok(mut answer) => {
            if let Some(fields) = answer.as_object_mut() {
                fields.insert("mode".into(), json!("editor"));
            }
            let answer = choice.name_project(answer);
            ToolResult::text(serde_json::to_string_pretty(&answer).unwrap_or_default())
        }
        Err(McpError::BridgeUnavailable(_)) => {
            let project = match tools.host_project(&args) {
                Ok(project) => project,
                Err(result) => return result,
            };
            let file = match resolve_in_project(&project, path) {
                Ok(file) => file,
                Err(problem) => {
                    return ToolResult::error(
                        problem,
                        &["Use a res:// path or a path relative to the project directory"],
                    );
                }
            };
            let res_path = res_display(&project, &file);
            if !file.is_file() {
                return ToolResult::error(
                    format!("Resource file not found: {res_path}"),
                    &[
                        "Check the path; res:// paths are resolved against the project directory",
                        "Create a new resource with godot_create_resource",
                    ],
                );
            }
            tools
                .set_resource_without_editor(&project, &res_path, &properties)
                .await
        }
        Err(error) => super::bridge_error_result(&error),
    }
}
