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
            "GDScript errors and warnings from Godot's language server (1-based lines), including analyzer warnings such as SHADOWED_GLOBAL_IDENTIFIER. Uses an editor already listening on GODOT_MCP_LSP_PORT (default 6005) or starts a private headless one. Does not report a clean file if the server never answers. godot_validate_script reports the same diagnostics when this server is already running, and otherwise only a headless parse check.",
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

/// How `godot_validate_script` presents one language-server answer.
pub(super) struct AnalyzerReport {
    pub text: String,
    pub failed: bool,
}

/// Turns the JSON from [`crate::lsp::LspHub::diagnostics`] into lines of
/// `file:line:col severity code: message`. A warning does not fail the check; an error does.
/// Information and hints are left out. Lines and columns are already 1-based.
pub(super) fn report_diagnostics(script_path: &str, value: &Value) -> AnalyzerReport {
    let Some(items) = value.get("diagnostics").and_then(Value::as_array) else {
        return AnalyzerReport {
            text: "The language server returned no diagnostics list.".into(),
            failed: true,
        };
    };
    let mut lines = Vec::new();
    let mut errors = 0_usize;
    for item in items {
        let Some(line) = diagnostic_line(script_path, item) else {
            continue;
        };
        if line.starts_with_error {
            errors += 1;
        }
        lines.push(line.text);
    }
    if lines.is_empty() {
        return AnalyzerReport {
            text: "No errors or analyzer warnings.".into(),
            failed: false,
        };
    }
    let head = if errors == 0 {
        "No errors. Analyzer warnings:".to_string()
    } else {
        format!(
            "{errors} error{} from the language server:",
            if errors == 1 { "" } else { "s" }
        )
    };
    AnalyzerReport {
        text: format!("{head}\n{}", lines.join("\n")),
        failed: errors > 0,
    }
}

struct DiagnosticLine {
    text: String,
    starts_with_error: bool,
}

fn diagnostic_line(script_path: &str, item: &Value) -> Option<DiagnosticLine> {
    let severity = item.get("severity").and_then(Value::as_i64).unwrap_or(1);
    let kind = match severity {
        1 => "error",
        2 => "warning",
        _ => return None,
    };
    let start = item.pointer("/range/start")?;
    let line = start.get("line").and_then(Value::as_u64)?;
    let column = start.get("character").and_then(Value::as_u64).unwrap_or(1);
    let code = match item.get("code") {
        Some(Value::String(code)) => code.clone(),
        Some(Value::Number(code)) => code.to_string(),
        _ => "diagnostic".to_string(),
    };
    let message = item.get("message").and_then(Value::as_str).unwrap_or("");
    Some(DiagnosticLine {
        text: format!("{script_path}:{line}:{column} {kind} {code}: {message}"),
        starts_with_error: severity == 1,
    })
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

    #[test]
    fn a_warning_is_listed_and_an_error_fails_the_check() {
        let warning = report_diagnostics(
            "res://scripts/seed.gd",
            &json!({
                "diagnostics": [{
                    "range": {"start": {"line": 4, "character": 9}, "end": {"line": 4, "character": 13}},
                    "severity": 2,
                    "code": "SHADOWED_GLOBAL_IDENTIFIER",
                    "message": "The local variable \"seed\" is shadowing a global."
                }, {
                    "range": {"start": {"line": 5, "character": 1}},
                    "severity": 4,
                    "code": "UNUSED_SIGNAL",
                    "message": "left out"
                }]
            }),
        );
        assert!(!warning.failed, "{}", warning.text);
        assert!(
            warning.text.contains("No errors. Analyzer warnings:"),
            "{}",
            warning.text
        );
        assert!(
            warning
                .text
                .contains("res://scripts/seed.gd:4:9 warning SHADOWED_GLOBAL_IDENTIFIER:"),
            "{}",
            warning.text
        );
        assert!(!warning.text.contains("UNUSED_SIGNAL"), "{}", warning.text);

        let error = report_diagnostics(
            "res://scripts/seed.gd",
            &json!({
                "diagnostics": [{
                    "range": {"start": {"line": 2, "character": 1}},
                    "severity": 1,
                    "code": "PARSE_ERROR",
                    "message": "Expected expression."
                }, {
                    "range": {"start": {"line": 4, "character": 9}},
                    "severity": 2,
                    "code": "SHADOWED_GLOBAL_IDENTIFIER",
                    "message": "still listed"
                }]
            }),
        );
        assert!(error.failed, "{}", error.text);
        assert!(
            error
                .text
                .contains("res://scripts/seed.gd:2:1 error PARSE_ERROR:"),
            "{}",
            error.text
        );
        assert!(
            error.text.contains("SHADOWED_GLOBAL_IDENTIFIER"),
            "{}",
            error.text
        );
    }
}
