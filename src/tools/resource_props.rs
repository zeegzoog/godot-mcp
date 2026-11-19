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

pub fn handles(name: &str) -> bool {
    name == NAME
}

pub fn catalog() -> Vec<Tool> {
    vec![super::tool(
        NAME,
        "Sets properties of a resource saved in its own .tres or .res file (a material, a ShaderMaterial, a Curve, a theme, a custom Resource) and saves the file. This is the tool for a resource that godot_set_node_properties refuses because a .tres file is shared. Values are converted to each property's current type as godot_set_node_properties converts them: vectors, rects and colors accept arrays ([10, 20]) or objects ({\"x\": 10, \"y\": 20}), colors also '#rrggbb' or names, resources a res:// path (of a class the property takes) or null, typed arrays and dictionaries a JSON array or object, and any Godot literal string ('Vector2(1, 2)') is parsed. A shader uniform of a ShaderMaterial is shader_parameter/<uniform>, as the inspector lists it, and must be one of the shader's uniforms; the error names the ones it has. Unknown properties, unconvertible values, ':' steps, resource_path, script and private names reject the whole call without changing anything, and so does a value that reads back differently from the one set (a setter that clamps or refuses it). The answer lists each property's value before and after in changed, as Godot's text form; a shader parameter the material did not set reads null before (the shader's default applied). With the editor bridge connected to the project, the editor's loaded copy is changed, so open scenes that use it follow, as one undoable action whose undo and redo save the file again (mode editor, undoable true). Without it, a headless Godot run changes and saves the file (mode headless), and the server then writes back what headless Godot drops, as after godot_run_script: the file's uid, its ext_resource uids and no shader_parameter lines written as null for unset uniforms (resource_repairs, repair_summary); resource_damage lists what it could not write back.",
        json!({
            "resource_path": {
                "type": "string",
                "description": "res:// path of the .tres or .res file, such as res://materials/water.tres."
            },
            "properties": {
                "type": "object",
                "description": "Property names to new values, such as {\"albedo_color\": \"#3a7bd5\"} or {\"shader_parameter/wave_height\": 0.4}."
            },
            "project_path": {
                "type": "string",
                "description": "Project directory. Optional: the connected editor's project, else the project the server was started in."
            }
        }),
        &["resource_path", "properties"],
    )]
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
