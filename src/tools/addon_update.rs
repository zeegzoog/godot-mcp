//! The editor addon as a version of its own: `godot_update_addon` installs the addon of this
//! server's build into a project, and a command an older addon does not know is explained with
//! both versions and that tool, instead of the addon's bare "Unknown command type".

use super::{ToolManager, str_arg};
use crate::addon::{self, Installed, SERVER_VERSION};
use crate::error::McpError;
use crate::paths::same_directory;
use crate::protocol::{Tool, ToolContent, ToolResult};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

pub const UPDATE: &str = "godot_update_addon";

/// What an addon answers for a command it does not have.
const UNKNOWN_COMMAND: &str = "Unknown command type: ";

pub fn handles(name: &str) -> bool {
    name == UPDATE
}

pub fn catalog() -> Vec<Tool> {
    vec![super::tool(
        UPDATE,
        "Installs or updates the Godot MCP Bridge addon (res://addons/godot_mcp) in a project with the addon of this server's own build, so both have the same version, and keeps the plugin enabled in project.godot. The old addon folder is replaced as a whole (files the new addon no longer has are removed; nothing outside addons/godot_mcp is touched). An editor that has the project open runs the old addon until it loads the plugin again, so while one is connected the files are replaced only with restart_editor: true, which closes it (refused while scenes or scripts are unsaved, unless discard: true), installs, and opens it again. The answer gives previous_version, version, the added, updated and removed files and what the editor needs. Nothing is written when the addon is already this version. Use it when a tool says the editor's addon is older than the server.",
        json!({
            "project_path": {
                "type": "string",
                "description": "Project directory containing project.godot. Optional: defaults to the project open in the connected editor, else the project the server was started in."
            },
            "discard": {
                "type": "boolean",
                "description": "With restart_editor: close the editor even when it has unsaved scenes or scripts, losing those changes, as godot_close_editor's discard does. Default false: the call is refused and lists them."
            },
            "restart_editor": {
                "type": "boolean",
                "description": "Close the connected editor of this project before installing and open it again after. Default false: with the editor connected the call is refused and says so."
            }
        }),
        &[],
    )]
}

pub async fn call(tools: &ToolManager, args: Value) -> ToolResult {
    let restart = args
        .get("restart_editor")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let discard = args
        .get("discard")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let project = match tools.resolve_project(&args).await {
        Ok(project) => project.dir,
        Err(result) => return result,
    };
    if same_directory(&project, Path::new(env!("CARGO_MANIFEST_DIR"))) {
        return ToolResult::error(
            format!(
                "{} is the source tree this server was built from; its addons/godot_mcp is the addon itself and is not replaced",
                project.display()
            ),
            &["Pass project_path: the game project whose addon should be updated"],
        );
    }
    let previous = addon::installed(&project);
    let loaded = match editor_of(tools, &project).await {
        Ok(loaded) => loaded,
        Err(result) => return result,
    };
    // Current: the files on disk are this build's, not only its version number (a -dev
    // version stays the same while the addon changes).
    let current = match addon::plan(&project) {
        Ok(plan) => !plan.changed(),
        Err(error) => return install_failed(&project, &error),
    };

    let mut restarted = false;
    if let Some(loaded) = &loaded {
        let loaded_current = loaded.as_deref() == Some(SERVER_VERSION);
        if current && loaded_current {
            return report(
                &project,
                &previous,
                None,
                false,
                "connected, runs this version",
            );
        }
        if !restart {
            let running = loaded.clone().unwrap_or_else(|| previous.describe());
            let what = if current {
                format!(
                    "The addon files on disk are already this server's ({SERVER_VERSION}), but the editor that has the project open still runs the addon it loaded before ({running})"
                )
            } else {
                format!(
                    "The editor has the project open and runs addon {running}; its files are replaced only while the editor is closed, so nothing was installed"
                )
            };
            return ToolResult::error(
                what,
                &[
                    "Call godot_update_addon again with restart_editor: true: it closes the editor (refused while scenes or scripts are unsaved), installs and opens it again",
                    "Or disable and enable Godot MCP Bridge in Project → Project Settings → Plugins after closing the editor and updating",
                ],
            );
        }
        let closed = tools
            .handle_close_editor(json!({
                "project_path": project.display().to_string(),
                "discard": discard,
            }))
            .await;
        if closed.is_error {
            return prefixed(
                closed,
                "The editor was not closed, so the addon was not updated. ",
            );
        }
        restarted = true;
    }

    let installation = if current {
        None
    } else {
        match addon::install(&project) {
            Ok(installation) => Some(installation),
            Err(error) => return install_failed(&project, &error),
        }
    };
    let enabled_now = match enable(&project) {
        Ok(changed) => changed,
        Err(error) => {
            return ToolResult::error(
                format!("The addon was installed, but project.godot could not be updated: {error}"),
                &["Enable Godot MCP Bridge in Project → Project Settings → Plugins"],
            );
        }
    };

    let editor = if restarted {
        let launched = tools
            .handle_launch_editor(json!({"project_path": project.display().to_string()}))
            .await;
        if launched.is_error {
            return prefixed(
                launched,
                "The addon was updated and the editor closed, but it did not start again. ",
            );
        }
        "restarted with this version"
    } else {
        "not connected: the addon loads when the project is opened"
    };
    let mut answer = report_value(
        &project,
        &previous,
        installation.as_ref(),
        enabled_now,
        editor,
    );
    if !restarted && enabled_now {
        answer["note"] = json!(
            "If an editor has this project open with the plugin disabled, it keeps its own copy of project.godot: enable Godot MCP Bridge in Project → Project Settings → Plugins there"
        );
    }
    ToolResult::text(serde_json::to_string_pretty(&answer).unwrap_or_default())
}

fn install_failed(project: &Path, error: &std::io::Error) -> ToolResult {
    ToolResult::error(
        format!(
            "Could not install the addon into {}: {error}",
            project.join("addons/godot_mcp").display()
        ),
        &[
            "Check that addons/ in the project is writable and not open in another program",
            "The old addon is kept when the new one cannot be put in place; call again once the cause is fixed",
        ],
    )
}

/// The addon version the connected editor of `project` runs (`Some(None)` for an addon too old
/// to say), or `None` when no editor of that project is connected.
async fn editor_of(
    tools: &ToolManager,
    project: &Path,
) -> Result<Option<Option<String>>, ToolResult> {
    match tools
        .bridge
        .send_command("get_project_info", json!({}))
        .await
    {
        Ok(info) => {
            let same = info
                .get("project_dir")
                .and_then(Value::as_str)
                .is_some_and(|dir| same_directory(project, Path::new(dir)));
            Ok(same.then(|| {
                str_arg(&info, "addon_version")
                    .filter(|version| !version.is_empty())
                    .map(str::to_string)
            }))
        }
        Err(McpError::BridgeUnavailable(_)) => Ok(None),
        Err(error) => Err(super::bridge_error_result(&error)),
    }
}

/// Adds the addon to the enabled plugins of `project.godot`; true when it was not there.
fn enable(project: &Path) -> std::io::Result<bool> {
    let file = project.join("project.godot");
    let text = std::fs::read_to_string(&file)?;
    match addon::enable_in(&text) {
        Some(enabled) => std::fs::write(&file, enabled).map(|()| true),
        None => Ok(false),
    }
}

fn report(
    project: &Path,
    previous: &Installed,
    installation: Option<&addon::Installation>,
    enabled_now: bool,
    editor: &str,
) -> ToolResult {
    let answer = report_value(project, previous, installation, enabled_now, editor);
    ToolResult::text(serde_json::to_string_pretty(&answer).unwrap_or_default())
}

fn report_value(
    project: &Path,
    previous: &Installed,
    installation: Option<&addon::Installation>,
    enabled_now: bool,
    editor: &str,
) -> Value {
    let (added, updated, removed) = installation.map_or((vec![], vec![], vec![]), |done| {
        (
            done.added.clone(),
            done.updated.clone(),
            done.removed.clone(),
        )
    });
    json!({
        "project_dir": project.display().to_string(),
        "previous_version": previous.describe(),
        "version": SERVER_VERSION,
        "installed": installation.is_some_and(addon::Installation::changed),
        "added": added,
        "updated": updated,
        "removed": removed,
        "enabled_now": enabled_now,
        "editor": editor,
    })
}

fn prefixed(mut result: ToolResult, prefix: &str) -> ToolResult {
    if let Some(ToolContent::Text { text }) = result.content.first_mut() {
        if let Some(rest) = text.strip_prefix("Error: ") {
            *text = format!("Error: {prefix}{rest}");
        }
    }
    result
}

/// The command an addon refused as unknown in `error`.
pub(super) fn unknown_command(error: &McpError) -> Option<String> {
    match error {
        McpError::ToolExecution(message) | McpError::Bridge(message) => command_in(message),
        _ => None,
    }
}

fn command_in(text: &str) -> Option<String> {
    let rest = &text[text.find(UNKNOWN_COMMAND)? + UNKNOWN_COMMAND.len()..];
    let command: String = rest
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect();
    (!command.is_empty()).then_some(command)
}

/// The addon the connected editor runs and the project it has open, for a message.
pub(super) async fn editor_addon(tools: &ToolManager) -> (String, Option<PathBuf>) {
    let info = tools
        .bridge
        .send_command("get_project_info", json!({}))
        .await
        .unwrap_or(Value::Null);
    let dir = str_arg(&info, "project_dir").map(PathBuf::from);
    let version = match str_arg(&info, "addon_version").filter(|version| !version.is_empty()) {
        Some(version) => version.to_string(),
        None => match &dir {
            Some(dir) => addon::installed(dir).describe(),
            None => "unknown".to_string(),
        },
    };
    (version, dir)
}

/// The call that updates the addon of `dir`, for a hint.
fn update_hint(dir: Option<&Path>) -> String {
    let target = dir.map_or(String::new(), |dir| {
        format!(" project_path {:?},", dir.display().to_string())
    });
    format!(
        "Update the addon to this server's version: godot_update_addon with{target} restart_editor: true (it closes the editor, refused while scenes or scripts are unsaved, installs and opens it again)"
    )
}

/// A tool error that only carries an addon's "Unknown command type", made to say that the
/// editor's addon is older than the server, with both versions and the update call.
pub(super) async fn explain_unknown_command(
    tools: &ToolManager,
    tool: &str,
    result: ToolResult,
) -> ToolResult {
    let command = match result.content.first() {
        Some(ToolContent::Text { text }) if result.is_error => command_in(text),
        _ => None,
    };
    let Some(command) = command else {
        return result;
    };
    let (version, dir) = editor_addon(tools).await;
    let on_disk = dir.as_deref().map(addon::installed);
    let what = if on_disk == Some(Installed::Version(SERVER_VERSION.to_string())) {
        format!(
            "The editor runs Godot MCP addon {version}, which has no '{command}' command that {tool} needs. The addon on disk is already {SERVER_VERSION} (this server's version), but the editor loaded the older one"
        )
    } else {
        format!(
            "The editor's Godot MCP addon ({version}) is older than this server ({SERVER_VERSION}): it has no '{command}' command, which {tool} needs"
        )
    };
    ToolResult::error(
        what,
        &[
            &update_hint(dir.as_deref()),
            "Or disable and enable Godot MCP Bridge in Project → Project Settings → Plugins once the addon on disk is this server's version",
            "Or close the editor: tools that work without it then use the project files",
        ],
    )
}

/// The note a tool adds when it worked on the file because the editor's addon is too old.
pub(super) async fn file_fallback_note(tools: &ToolManager, command: &str, done: &str) -> Value {
    let (version, dir) = editor_addon(tools).await;
    json!({
        "addon_version": version,
        "server_version": SERVER_VERSION,
        "note": format!(
            "The editor's Godot MCP addon ({version}) is older than this server ({SERVER_VERSION}) and has no '{command}' command, so {done}. {}",
            update_hint(dir.as_deref())
        ),
    })
}

/// `result`'s JSON answer with the fields of `extra` added; a text answer gets them appended.
pub(super) fn with_fields(mut result: ToolResult, extra: &Value) -> ToolResult {
    if result.is_error {
        return result;
    }
    if let Some(ToolContent::Text { text }) = result.content.first_mut() {
        match serde_json::from_str::<Value>(text) {
            Ok(Value::Object(mut map)) => {
                if let Value::Object(fields) = extra {
                    for (key, value) in fields {
                        map.insert(key.clone(), value.clone());
                    }
                }
                *text = serde_json::to_string_pretty(&Value::Object(map)).unwrap_or_default();
            }
            _ => {
                if let Some(note) = extra.get("note").and_then(Value::as_str) {
                    text.push_str("\n\n");
                    text.push_str(note);
                }
            }
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unknown_command_is_found_in_an_error() {
        let error = McpError::ToolExecution("Unknown command type: read_shader".into());
        assert_eq!(unknown_command(&error).as_deref(), Some("read_shader"));
        assert_eq!(
            command_in("Error: Unknown command type: edit_shader\n\nActionable").as_deref(),
            Some("edit_shader")
        );
        assert_eq!(
            unknown_command(&McpError::ToolExecution("Node not found".into())),
            None
        );
        assert_eq!(command_in("Unknown command type: "), None);
    }
}
