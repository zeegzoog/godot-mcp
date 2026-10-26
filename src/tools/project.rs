//! Which project a call acts on: the one named in the call, the one open in the editor, or the
//! one found from the server's working directory.

use super::{ToolManager, bridge_error_result, str_arg};
use crate::error::McpError;
use crate::paths::{ensure_project_dir, find_project, same_directory};
use crate::protocol::ToolResult;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

const PROJECT_PATH_HINTS: &[&str] = &[
    "Pass project_path: the absolute path of a directory that contains project.godot",
    "Or start the MCP server from the project directory (or the folder right above it)",
    "Or open the project in the Godot Editor with the Godot MCP Bridge plugin enabled",
];

/// Where a live-first call goes (TL-10).
pub(super) enum EditorChoice {
    /// To the connected editor; `project_dir` is the project it reported, when it was asked.
    Editor { project_dir: Option<String> },
    /// Not to the editor: the caller names a project the editor does not have open.
    Host,
}

impl EditorChoice {
    /// The editor's answer with the project it acted on added, when it is an object.
    pub(super) fn name_project(&self, mut answer: Value) -> Value {
        if let (
            Self::Editor {
                project_dir: Some(dir),
            },
            Value::Object(map),
        ) = (self, &mut answer)
        {
            map.entry("project_dir").or_insert_with(|| json!(dir));
        }
        answer
    }
}

/// A project directory plus whether it came from the live editor.
pub(super) struct ResolvedProject {
    pub(super) dir: PathBuf,
    pub(super) from_editor: bool,
}

impl ToolManager {
    /// Decides whether a live-first call goes to the connected editor (TL-10).
    ///
    /// A call that names a `project_path` goes to the editor only when the editor has that
    /// project open; otherwise the caller takes its file path. `wants_dir` also asks the editor
    /// for its project when no path is named, so an answer that changed something can name the
    /// project it changed. An editor that cannot say which project it has open is not sent the call.
    pub(super) async fn editor_choice(
        &self,
        args: &Value,
        wants_dir: bool,
    ) -> Result<EditorChoice, ToolResult> {
        let named = str_arg(args, "project_path");
        if named.is_none() && !wants_dir {
            return Ok(EditorChoice::Editor { project_dir: None });
        }
        match self
            .bridge
            .send_command("get_project_info", json!({}))
            .await
        {
            Ok(info) => {
                let Some(dir) = info.get("project_dir").and_then(Value::as_str) else {
                    return Err(ToolResult::error(
                        "The editor did not report which project it has open, so the call was not sent to it",
                        &[
                            "Update the Godot MCP Bridge addon to the same version as the server",
                            "Or close the editor and repeat the call to use the project files directly",
                        ],
                    ));
                };
                match named {
                    Some(named) if !same_directory(Path::new(named), Path::new(dir)) => {
                        Ok(EditorChoice::Host)
                    }
                    _ => Ok(EditorChoice::Editor {
                        project_dir: Some(dir.to_string()),
                    }),
                }
            }
            // No editor: the caller's own command fails the same way and its fallback runs.
            Err(McpError::BridgeUnavailable(_)) => Ok(EditorChoice::Editor { project_dir: None }),
            Err(error) => Err(bridge_error_result(&error)),
        }
    }

    /// False only when the bridge shows that the connected editor has another project open than
    /// `project` (CI-7). With no editor, or an editor that cannot say, nothing shows a mismatch.
    pub(super) async fn editor_may_serve(&self, project: &Path) -> bool {
        match self
            .bridge
            .send_command("get_project_info", json!({}))
            .await
        {
            Ok(info) => info
                .get("project_dir")
                .and_then(Value::as_str)
                .is_none_or(|dir| same_directory(project, Path::new(dir))),
            Err(_) => true,
        }
    }

    /// Finds the project directory without asking the editor: an explicit `project_path`,
    /// else the project found from the server's working directory.
    pub(super) fn host_project(&self, args: &Value) -> Result<PathBuf, ToolResult> {
        let dir = self.locate_host_project(args)?;
        self.feedback.remember_project(&dir);
        Ok(dir)
    }

    fn locate_host_project(&self, args: &Value) -> Result<PathBuf, ToolResult> {
        if let Some(project_path) = str_arg(args, "project_path") {
            let dir = PathBuf::from(project_path);
            return match ensure_project_dir(&dir) {
                Ok(()) => Ok(dir),
                Err(problem) => Err(ToolResult::error(
                    problem,
                    &["Pass the directory that contains project.godot"],
                )),
            };
        }
        self.working_dir_project()
    }

    fn working_dir_project(&self) -> Result<PathBuf, ToolResult> {
        let Some(cwd) = &self.working_dir else {
            return Err(ToolResult::error(
                "No project_path was given and the server's working directory is unknown",
                PROJECT_PATH_HINTS,
            ));
        };
        find_project(cwd).map_err(|problem| ToolResult::error(problem, PROJECT_PATH_HINTS))
    }

    /// Finds the project directory: an explicit `project_path`, else the project open in the
    /// live editor, else the project found from the server's working directory.
    pub(super) async fn resolve_project(
        &self,
        args: &Value,
    ) -> Result<ResolvedProject, ToolResult> {
        let project = self.locate_project(args).await?;
        self.feedback.remember_project(&project.dir);
        Ok(project)
    }

    async fn locate_project(&self, args: &Value) -> Result<ResolvedProject, ToolResult> {
        if let Some(project_path) = str_arg(args, "project_path") {
            let dir = PathBuf::from(project_path);
            return match ensure_project_dir(&dir) {
                Ok(()) => Ok(ResolvedProject {
                    dir,
                    from_editor: false,
                }),
                Err(problem) => Err(ToolResult::error(problem, PROJECT_PATH_HINTS)),
            };
        }

        match self
            .bridge
            .send_command("get_project_info", json!({}))
            .await
        {
            Ok(info) => match info.get("project_dir").and_then(Value::as_str) {
                Some(dir) => {
                    let dir = PathBuf::from(dir);
                    ensure_project_dir(&dir)
                        .map_err(|problem| ToolResult::error(problem, PROJECT_PATH_HINTS))?;
                    Ok(ResolvedProject {
                        dir,
                        from_editor: true,
                    })
                }
                None => Err(ToolResult::error(
                    "The editor did not report its project directory",
                    &["Update the Godot MCP Bridge addon to the same version as the server"],
                )),
            },
            // Only a missing editor justifies looking in the working directory: an editor that
            // answers with an error may have a different project open.
            Err(McpError::BridgeUnavailable(_)) => {
                self.working_dir_project().map(|dir| ResolvedProject {
                    dir,
                    from_editor: false,
                })
            }
            Err(error) => Err(bridge_error_result(&error)),
        }
    }
}
