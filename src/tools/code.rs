//! GDScript queries against Godot's language server.
//!
//! The client speaks LSP over TCP and sends only a Content-Length header. Godot
//! reads every digit after that label, so a Content-Type header breaks framing.

use super::{ToolManager, str_arg};
use crate::error::McpError;
use crate::paths::resolve_in_project;
use crate::protocol::{Tool, ToolResult};
use serde_json::{Value, json};

pub fn handles(name: &str) -> bool {
    matches!(
        name,
        "godot_code_diagnostics"
            | "godot_code_complete"
            | "godot_code_symbols"
            | "godot_code_definition"
            | "godot_code_references"
    )
}

pub fn catalog() -> Vec<Tool> {
    vec![
        tool(
            "godot_code_diagnostics",
            "GDScript diagnostics from Godot's language server (1-based lines). Does not report a clean file if the server never answers.",
            json!({"script_path": s("res:// path of a .gd file."), "project_path": s("")}),
            &["script_path"],
        ),
        tool(
            "godot_code_complete",
            "Code completion at a 1-based line and column, from Godot's language server.",
            json!({"script_path": s(""), "line": position("Line, counted from 1."), "character": position("Column, counted from 1."), "project_path": s("")}),
            &["script_path", "line", "character"],
        ),
        tool(
            "godot_code_symbols",
            "Document symbols of a GDScript file, from Godot's language server.",
            json!({"script_path": s(""), "project_path": s("")}),
            &["script_path"],
        ),
        tool(
            "godot_code_definition",
            "Go to definition at a 1-based position, from Godot's language server.",
            json!({"script_path": s(""), "line": position("Line, counted from 1."), "character": position("Column, counted from 1."), "project_path": s("")}),
            &["script_path", "line", "character"],
        ),
        tool(
            "godot_code_references",
            "Find references at a 1-based position, from Godot's language server.",
            json!({"script_path": s(""), "line": position("Line, counted from 1."), "character": position("Column, counted from 1."), "project_path": s("")}),
            &["script_path", "line", "character"],
        ),
    ]
}

fn s(description: &str) -> Value {
    if description.is_empty() {
        json!({"type": "string"})
    } else {
        json!({"type": "string", "description": description})
    }
}

fn tool(name: &str, description: &str, properties: Value, required: &[&str]) -> Tool {
    super::tool(name, description, properties, required)
}

pub async fn call(tools: &ToolManager, name: &str, args: Value) -> ToolResult {
    match name {
        "godot_code_diagnostics" => code_diagnostics(tools, &args).await,
        "godot_code_complete" => code_at(tools, &args, CodeOp::Complete).await,
        "godot_code_symbols" => code_symbols(tools, &args).await,
        "godot_code_definition" => code_at(tools, &args, CodeOp::Definition).await,
        "godot_code_references" => code_at(tools, &args, CodeOp::References).await,
        _ => ToolResult::error(format!("Tool '{name}' has no handler"), &[]),
    }
}

enum CodeOp {
    Complete,
    Definition,
    References,
}

async fn code_diagnostics(tools: &ToolManager, args: &Value) -> ToolResult {
    let (project, file, external_ok) = match script_file(tools, args).await {
        Ok(found) => found,
        Err(result) => return result,
    };
    match tools.lsp.diagnostics(&project, external_ok, &file).await {
        Ok(value) => ToolResult::text(serde_json::to_string_pretty(&value).unwrap_or_default()),
        Err(error) => ToolResult::error(
            error.to_string(),
            &[
                "Open this project in the Godot editor (language server port 6005): a language server of an editor with another project open is not used",
                "Or set GODOT_PATH so that a headless editor can be started for this project",
            ],
        ),
    }
}

async fn code_symbols(tools: &ToolManager, args: &Value) -> ToolResult {
    let (project, file, external_ok) = match script_file(tools, args).await {
        Ok(found) => found,
        Err(result) => return result,
    };
    text_result(tools.lsp.symbols(&project, external_ok, &file).await)
}

async fn code_at(tools: &ToolManager, args: &Value, op: CodeOp) -> ToolResult {
    let (project, file, external_ok) = match script_file(tools, args).await {
        Ok(found) => found,
        Err(result) => return result,
    };
    let line = position_arg(args, "line");
    let character = position_arg(args, "character");
    let result = match op {
        CodeOp::Complete => {
            tools
                .lsp
                .complete(&project, external_ok, &file, line, character)
                .await
        }
        CodeOp::Definition => {
            tools
                .lsp
                .definition(&project, external_ok, &file, line, character)
                .await
        }
        CodeOp::References => {
            tools
                .lsp
                .references(&project, external_ok, &file, line, character)
                .await
        }
    };
    text_result(result)
}

async fn script_file(
    tools: &ToolManager,
    args: &Value,
) -> Result<(std::path::PathBuf, std::path::PathBuf, bool), ToolResult> {
    let Some(script_path) = str_arg(args, "script_path") else {
        return Err(ToolResult::error(
            "Missing script_path",
            &["Pass script_path: a res:// path to a .gd file"],
        ));
    };
    if !script_path.ends_with(".gd") {
        return Err(ToolResult::error(
            "script_path must be a .gd file",
            &["Use a path such as res://scripts/player.gd"],
        ));
    }
    let resolved = match tools.resolve_project(args).await {
        Ok(project) => project,
        Err(result) => return Err(result),
    };
    let project = resolved.dir.clone();
    let file = match resolve_in_project(&project, script_path) {
        Ok(file) => file,
        Err(problem) => {
            return Err(ToolResult::error(
                problem,
                &["Use a res:// path or a path relative to the project directory"],
            ));
        }
    };
    if !file.is_file() {
        return Err(ToolResult::error(
            format!(
                "Script not found: {}",
                crate::paths::res_display(&project, &file)
            ),
            &["Check the path; res:// paths are resolved against the project directory"],
        ));
    }
    // The editor's own project is the editor's by definition; for a project the caller named,
    // an editor with another project open must not answer for it (CI-7).
    let external_ok = resolved.from_editor || tools.editor_may_serve(&project).await;
    Ok((project, file, external_ok))
}

fn text_result(result: Result<Value, McpError>) -> ToolResult {
    match result {
        Ok(value) => ToolResult::text(
            serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string()),
        ),
        Err(error) => runtime_error(&error),
    }
}

fn runtime_error(error: &McpError) -> ToolResult {
    match error {
        McpError::BridgeUnavailable(_) | McpError::Bridge(_) | McpError::ToolExecution(_) => {
            super::bridge_error_result(error)
        }
        other => ToolResult::error(
            other.to_string(),
            &[
                "Start the game with godot_run_project",
                "For breakpoints, the server must launch the process (editor Play uses Godot's own debugger)",
            ],
        ),
    }
}

/// A line or column parameter: an integer of at least 1, as in the editor.
fn position(description: &str) -> Value {
    json!({"type": "integer", "minimum": 1, "description": description})
}

/// A 1-based line or column the caller named, 1 when absent. A number too large for the
/// protocol's 32 bits is kept at the largest value instead of wrapping around to a small one.
fn position_arg(args: &Value, key: &str) -> u32 {
    args.get(key)
        .and_then(Value::as_u64)
        .map_or(1, |value| u32::try_from(value).unwrap_or(u32::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_position_beyond_32_bits_does_not_wrap_to_a_small_one() {
        assert_eq!(position_arg(&json!({}), "line"), 1);
        assert_eq!(position_arg(&json!({"line": 7}), "line"), 7);
        // 2^32 + 5 used to become 5 and point at the wrong place.
        assert_eq!(
            position_arg(&json!({"line": (1_u64 << 32) + 5}), "line"),
            u32::MAX
        );
    }
}
