//! Starting, watching and stopping runs of the project: the editor's Play mode and managed
//! game processes.

use super::{
    ToolManager, bridge_error_result, game, list_paths, str_arg, string_list, user_args_of,
    with_notice,
};
use crate::error::McpError;
use crate::godot::{ProcessStart, stale_imports};
use crate::paths::same_directory;
use crate::protocol::ToolResult;
use serde_json::{Value, json};
use std::path::Path;

impl ToolManager {
    pub(super) async fn handle_run_project(&self, args: Value) -> ToolResult {
        let scene = str_arg(&args, "scene");
        if let Some(scene) = scene {
            if !scene.starts_with("res://") || scene.split(['/', '\\']).any(|part| part == "..") {
                return ToolResult::error(
                    format!(
                        "Invalid scene '{scene}': scene must be a res:// path inside the project"
                    ),
                    &["Use a path such as res://scenes/Main.tscn"],
                );
            }
        }
        for spec in string_list(args.get("breakpoints")) {
            if let Err(result) = game::check_breakpoint(&spec) {
                return result;
            }
        }
        let mode = str_arg(&args, "mode").unwrap_or("auto");
        if args.get("held").and_then(Value::as_bool).unwrap_or(false) && mode != "process" {
            return ToolResult::error(
                "held needs mode \"process\": only a game this server starts can run at a fixed frame rate",
                &[
                    "Pass mode: \"process\" with held: true",
                    "In the editor's Play mode a game can still be held later with godot_game_physics (action hold), but its steps are not exact",
                ],
            );
        }
        let user_args = match user_args_of(&args) {
            Ok(user_args) => user_args,
            Err(result) => return result,
        };
        if user_args.is_some() && mode != "process" {
            return ToolResult::error(
                "user_args is only passed to a managed process. Editor Play and mode \"auto\" cannot receive them.",
                &[
                    "Pass mode: \"process\" to start a game this server launches",
                    "The strings are placed after --, so the game reads them with OS.get_cmdline_user_args() and Godot does not treat them as engine options",
                ],
            );
        }
        if mode == "process" {
            return self
                .spawn_managed(&args, scene, user_args.as_deref().unwrap_or(&[]))
                .await;
        }

        // Reimport before Play, so the editor's game does not keep the old cache.
        let notice = match self.resolve_project(&args).await {
            Ok(project) if project.from_editor => self.import_notice(&project.dir).await,
            _ => String::new(),
        };

        // Prefer the live editor: it can be stopped/queried later via
        // godot_stop_project / godot_get_run_status, unlike a spawned process.
        let bridge_params = json!({ "scene_path": scene.unwrap_or("") });
        match self.bridge.send_command("run_project", bridge_params).await {
            Ok(result) => {
                return with_notice(
                    ToolResult::text(format!(
                        "Project running via live editor:\n{}",
                        serde_json::to_string_pretty(&result).unwrap_or_default()
                    )),
                    &notice,
                );
            }
            // Only a missing editor justifies spawning a separate process.
            Err(McpError::BridgeUnavailable(_)) => {}
            Err(error) => return with_notice(bridge_error_result(&error), &notice),
        }

        self.spawn_managed(&args, scene, user_args.as_deref().unwrap_or(&[]))
            .await
    }

    async fn spawn_managed(
        &self,
        args: &Value,
        scene: Option<&str>,
        user_args: &[String],
    ) -> ToolResult {
        let path = match self.host_project(args) {
            Ok(path) => path,
            Err(result) => return result,
        };
        let breakpoints = string_list(args.get("breakpoints"));
        let headless = args
            .get("headless")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let break_on_errors = args
            .get("break_on_errors")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let held = args.get("held").and_then(Value::as_bool).unwrap_or(false);
        let notice = self.import_notice(&path).await;
        match self
            .games
            .start(
                &path,
                ProcessStart {
                    scene,
                    breakpoints: &breakpoints,
                    headless,
                    break_on_errors,
                    held,
                    user_args,
                },
            )
            .await
        {
            Ok(info) => with_notice(
                ToolResult::text(format!(
                    "Spawned a managed Godot process (PID: {}, run_id: {}). Stop it with godot_stop_project. Project: '{}'.",
                    info.pid,
                    info.id,
                    path.display()
                )),
                &notice,
            ),
            Err(error) => with_notice(
                ToolResult::error(
                    format!("Failed to run project: {error}"),
                    &["Verify GODOT_PATH points to a working Godot executable"],
                ),
                &notice,
            ),
        }
    }

    /// Reimports stale imports through the editor when it has this project open, and otherwise
    /// returns a warning that names `godot_resave_resources`. Empty when nothing is stale.
    async fn import_notice(&self, project: &Path) -> String {
        let root = project.to_path_buf();
        // The walk reads every folder of the project, so it runs off the async workers.
        let stale = tokio::task::spawn_blocking(move || stale_imports(&root))
            .await
            .unwrap_or_default();
        if stale.is_empty() {
            return String::new();
        }
        let listed = list_paths(&stale);
        match self.reimport_stale(project, &stale).await {
            Some(Ok(())) => format!(
                "\nReimported stale resources before the run: {listed}. The game loads the new import."
            ),
            Some(Err(error)) => format!(
                "\nWarning: these imported resources are newer than the cache ({error}): {listed}. Call godot_resave_resources with those paths, then run again."
            ),
            None => format!(
                "\nWarning: these imported resources are newer than the cache the game will load: {listed}. Call godot_resave_resources with those paths, then run again."
            ),
        }
    }

    /// `Some` when the connected editor has this project open (`Ok` after a reimport). `None`
    /// when no editor is there, or it has a different project, so the caller warns instead.
    async fn reimport_stale(&self, project: &Path, paths: &[String]) -> Option<Result<(), String>> {
        let info = match self
            .bridge
            .send_command("get_project_info", json!({}))
            .await
        {
            Ok(info) => info,
            Err(McpError::BridgeUnavailable(_)) => return None,
            Err(error) => return Some(Err(error.to_string())),
        };
        let Some(dir) = info.get("project_dir").and_then(Value::as_str) else {
            return Some(Err("the editor did not report its project directory".into()));
        };
        if !same_directory(Path::new(dir), project) {
            return None;
        }
        match self
            .bridge
            .send_command("resave_resources", json!({ "paths": paths }))
            .await
        {
            Ok(_) => Some(Ok(())),
            Err(McpError::BridgeUnavailable(_)) => None,
            Err(error) => Some(Err(error.to_string())),
        }
    }

    pub(super) async fn handle_stop_project(&self, args: Value) -> ToolResult {
        if let Some(run_id) = str_arg(&args, "run_id") {
            return match self.games.stop(Some(run_id)).await {
                Ok(summary) => {
                    ToolResult::text(serde_json::to_string_pretty(&summary).unwrap_or_default())
                }
                Err(error) => ToolResult::error(
                    error.to_string(),
                    &["Check godot_get_run_status for live run ids"],
                ),
            };
        }
        // TL-11: stop everything that runs, and say what was stopped. The managed runs are
        // independent of the editor, so they are stopped first and an editor failure cannot
        // leave them running.
        let runs = self.games.stop_all().await;
        let mut editor_play = false;
        let mut editor_error = None;
        match self.bridge.send_command("get_run_status", json!({})).await {
            Ok(status) if status.get("running").and_then(Value::as_bool) == Some(true) => {
                match self.bridge.send_command("stop_project", json!({})).await {
                    Ok(_) => editor_play = true,
                    Err(error) => editor_error = Some(error),
                }
            }
            Ok(_) | Err(McpError::BridgeUnavailable(_)) => {}
            Err(error) => editor_error = Some(error),
        }
        if let Some(error) = editor_error {
            if runs.is_empty() {
                return bridge_error_result(&error);
            }
            return ToolResult::error(
                format!(
                    "Stopped {} managed run(s), but the editor's Play mode could not be stopped: {error}",
                    runs.len()
                ),
                &["Call godot_stop_project again to stop the editor's Play mode"],
            );
        }
        let message = match (editor_play, runs.len()) {
            (false, 0) => "Nothing was running.".to_string(),
            (true, 0) => "Stopped the editor's Play mode.".to_string(),
            (false, count) => format!("Stopped {count} managed run(s)."),
            (true, count) => format!("Stopped the editor's Play mode and {count} managed run(s)."),
        };
        ToolResult::text(
            serde_json::to_string_pretty(&json!({
                "message": message,
                "stopped": {"editor_play": editor_play, "runs": runs},
            }))
            .unwrap_or_default(),
        )
    }

    pub(super) async fn handle_run_status(&self) -> ToolResult {
        let processes = self.games.status();
        let answer = match self.bridge.send_command("get_run_status", json!({})).await {
            Ok(mut combined) => {
                if let Value::Object(map) = &mut combined {
                    map.insert("process_runs".into(), Value::Array(processes));
                }
                combined
            }
            // No editor is not a failure of this question: managed runs are still reported.
            Err(McpError::BridgeUnavailable(detail)) => {
                let nothing = processes.iter().all(|run| run["running"] != true);
                let mut answer = json!({
                    "running": false,
                    "editor": format!("not connected ({detail})"),
                    "process_runs": processes,
                });
                if nothing {
                    answer["message"] = json!("Nothing is running.");
                }
                answer
            }
            Err(error) => return bridge_error_result(&error),
        };
        ToolResult::text(serde_json::to_string_pretty(&answer).unwrap_or_default())
    }
}
