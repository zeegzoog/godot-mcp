use crate::bridge::BridgeClient;
use crate::cli_runner::{CliRunner, ScriptRun};
use crate::config::FeedbackMode;
use crate::error::McpError;
use crate::godot::{
    GameHub, IMPORTED_EXTENSIONS, ProcessStart, changed_imports, import_fingerprints, stale_imports,
};
use crate::journal::EvidenceKind;
use crate::lsp::LspHub;
use crate::paths::{
    ensure_project_dir, find_project, is_valid_identifier, res_display, resolve_in_project,
    same_directory,
};
use crate::protocol::{Tool, ToolResult};
use crate::validation::validate_arguments;
use serde_json::{Value, json};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use tokio::fs;
use tokio::io::AsyncWriteExt;

mod authoring;
mod code;
mod feedback;
mod game;
mod scripts;

const PROJECT_PATH_HINTS: &[&str] = &[
    "Pass project_path: the absolute path of a directory that contains project.godot",
    "Or start the MCP server from the project directory (or the folder right above it)",
    "Or open the project in the Godot Editor with the Godot MCP Bridge plugin enabled",
];

pub struct ToolManager {
    bridge: BridgeClient,
    cli: CliRunner,
    /// Where the server was started; the project is looked up from here when no
    /// `project_path` is given and no editor is connected.
    working_dir: Option<PathBuf>,
    feedback: feedback::FeedbackState,
    games: GameHub,
    lsp: LspHub,
}

/// Where a live-first call goes (TL-10).
enum EditorChoice {
    /// To the connected editor; `project_dir` is the project it reported, when it was asked.
    Editor { project_dir: Option<String> },
    /// Not to the editor: the caller names a project the editor does not have open.
    Host,
}

impl EditorChoice {
    /// The editor's answer with the project it acted on added, when it is an object.
    fn name_project(&self, mut answer: Value) -> Value {
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
struct ResolvedProject {
    dir: PathBuf,
    from_editor: bool,
}

impl ToolManager {
    pub fn new(bridge: BridgeClient, cli: CliRunner) -> Self {
        let godot = cli.binary().to_path_buf();
        Self {
            bridge,
            cli,
            working_dir: std::env::current_dir().ok(),
            feedback: feedback::FeedbackState::new(FeedbackMode::default(), None),
            games: GameHub::new(godot.clone()),
            lsp: LspHub::new(godot),
        }
    }

    /// Probes an already-running language server on `port` instead of `GODOT_MCP_LSP_PORT`.
    pub fn with_lsp_port(mut self, port: u16) -> Self {
        self.lsp = LspHub::with_port(self.cli.binary().to_path_buf(), port);
        self
    }

    /// Sets how much of the agent feedback channel is active and where the GitHub client is.
    pub fn with_feedback(mut self, mode: FeedbackMode, gh_path: Option<PathBuf>) -> Self {
        self.feedback = feedback::FeedbackState::new(mode, gh_path);
        self
    }

    /// Chooses which feedback tools the catalog holds. Call it once at start-up, before the
    /// catalog is first read; a later call has no effect.
    pub fn configure_catalog(mode: FeedbackMode) {
        let _ = CATALOG_MODE.set(mode);
    }

    /// Looks up implicit projects from `dir` instead of the process working directory.
    pub fn with_working_dir(mut self, dir: PathBuf) -> Self {
        self.working_dir = Some(dir);
        self
    }

    /// Return the list of all registered tools with their input schemas
    pub fn list_tools() -> Vec<Tool> {
        tool_catalog().to_vec()
    }

    pub fn has_tool(name: &str) -> bool {
        tool_catalog().iter().any(|tool| tool.name == name)
    }

    /// Dispatch tool calls to live bridge or CLI runner
    pub async fn call_tool(self: Arc<Self>, name: &str, args: Value) -> ToolResult {
        let Some(tool) = tool_catalog().iter().find(|tool| tool.name == name) else {
            return ToolResult::error(
                format!("Tool '{name}' is not recognized"),
                &["Check tool name spelling against tools/list"],
            );
        };
        if let Err(problem) = validate_arguments(&tool.input_schema, &args) {
            let result = ToolResult::error(
                format!("Invalid arguments for {name}: {problem}"),
                &["Check the tool's inputSchema returned by tools/list and retry"],
            );
            self.observe(name, &result, EvidenceKind::InvalidArguments);
            return result;
        }

        let result = match name {
            "godot_get_version" => self.handle_get_version().await,
            "godot_get_project_info" => self.handle_bridge_call("get_project_info", args).await,
            "godot_launch_editor" => self.handle_launch_editor(args).await,
            "godot_run_project" => self.handle_run_project(args).await,
            "godot_stop_project" => self.handle_stop_project(args).await,
            "godot_get_run_status" => self.handle_run_status().await,
            "godot_get_scene_tree" => self.handle_bridge_call("get_scene_tree", args).await,
            "godot_get_open_scenes" => self.handle_bridge_call("get_open_scenes", args).await,
            "godot_add_node" => self.handle_bridge_call("add_node", args).await,
            "godot_remove_node" => self.handle_bridge_call("remove_node", args).await,
            "godot_get_node_properties" => {
                self.handle_bridge_call("get_node_properties", args).await
            }
            "godot_set_node_properties" => {
                self.handle_bridge_call("set_node_properties", args).await
            }
            "godot_capture_viewport" => self.handle_capture_viewport(args).await,
            "godot_open_scene" => self.handle_bridge_call("open_scene", args).await,
            "godot_save_scene" => self.handle_bridge_call("save_scene", args).await,
            "godot_create_script" => self.handle_create_script(args).await,
            "godot_validate_script" => self.handle_validate_script(args).await,
            "godot_run_script" => self.handle_run_script(args).await,
            "godot_resave_resources" => self.handle_resave_resources(args).await,
            name if authoring::handles(name) => authoring::call(&self, name, args).await,
            name if scripts::handles(name) => scripts::call(&self, name, args).await,
            name if game::handles(name) => game::call(&self, name, args).await,
            name if code::handles(name) => code::call(&self, name, args).await,
            feedback::NOTE => self.handle_note_feedback(args).await,
            feedback::DRAFT => self.handle_draft_feedback(args).await,
            feedback::SUBMIT => self.handle_submit_feedback(args).await,
            _ => ToolResult::error(
                format!("Tool '{name}' has no handler"),
                &["This is a server bug; report it with the tool name"],
            ),
        };
        self.observe(name, &result, EvidenceKind::Error);
        result
    }

    async fn handle_get_version(&self) -> ToolResult {
        // Try live editor bridge first
        match self.bridge.send_command("get_version", json!({})).await {
            Ok(result) => {
                return ToolResult::text(format!(
                    "Godot Engine (Live Editor Connected):\n{}",
                    serde_json::to_string_pretty(&result).unwrap_or_default()
                ));
            }
            // An editor that answers with an error (e.g. a token problem) must not be hidden.
            Err(error @ McpError::ToolExecution(_)) => return bridge_error_result(&error),
            Err(_) => {}
        }

        // Fallback to CLI
        match self.cli.get_version().await {
            Ok(ver) => ToolResult::text(format!(
                "Godot Engine (Headless CLI):\nVersion: {ver}\nNote: Editor bridge is offline. Launch Godot Editor and enable Godot MCP Bridge plugin for live manipulation."
            )),
            Err(e) => ToolResult::error(
                format!("Failed to retrieve Godot version: {e}"),
                &[
                    "Verify GODOT_PATH in .env",
                    "Ensure Godot executable is installed and reachable",
                ],
            ),
        }
    }

    async fn handle_bridge_call(&self, command_type: &str, params: Value) -> ToolResult {
        match self.bridge.send_command(command_type, params).await {
            Ok(res) => ToolResult::text(serde_json::to_string_pretty(&res).unwrap_or_default()),
            Err(error) => bridge_error_result(&error),
        }
    }

    async fn handle_capture_viewport(&self, params: Value) -> ToolResult {
        match self.bridge.send_command("capture_viewport", params).await {
            Ok(res) => {
                let data = res.get("data").and_then(Value::as_str);
                let mime = res.get("mime_type").and_then(Value::as_str);
                match (data, mime) {
                    (Some(data), Some(mime)) => {
                        let note = format!(
                            "Editor viewport capture: {}x{} px ({} view)",
                            res.get("width").and_then(Value::as_u64).unwrap_or(0),
                            res.get("height").and_then(Value::as_u64).unwrap_or(0),
                            res.get("view").and_then(Value::as_str).unwrap_or("2d"),
                        );
                        ToolResult::image_with_text(note, data, mime)
                    }
                    _ => ToolResult::error(
                        "Invalid image response received from the Godot viewport",
                        &["Update the Godot MCP Bridge addon to the same version as the server"],
                    ),
                }
            }
            Err(error) => bridge_error_result(&error),
        }
    }

    async fn handle_launch_editor(&self, args: Value) -> ToolResult {
        let path = match self.host_project(&args) {
            Ok(path) => path,
            Err(result) => return result,
        };

        match self.cli.launch_editor(&path).await {
            Ok(pid) => ToolResult::text(format!(
                "Godot Editor launched successfully for '{}' (PID: {pid}).",
                path.display()
            )),
            Err(e) => ToolResult::error(
                format!("Failed to launch Godot Editor: {e}"),
                &["Verify GODOT_PATH points to a working Godot executable"],
            ),
        }
    }

    async fn handle_run_project(&self, args: Value) -> ToolResult {
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

    async fn handle_stop_project(&self, args: Value) -> ToolResult {
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

    async fn handle_run_status(&self) -> ToolResult {
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

    async fn handle_create_script(&self, args: Value) -> ToolResult {
        let Some(file_path) = str_arg(&args, "script_path").or_else(|| str_arg(&args, "file_path"))
        else {
            return ToolResult::error(
                "Missing required parameter: script_path (or its older name file_path)",
                &["Pass script_path: a res:// path ending in .gd"],
            );
        };
        if !file_path.ends_with(".gd") {
            return ToolResult::error(
                format!("Script files must have the .gd extension: {file_path}"),
                &["Use a path such as res://scripts/player.gd"],
            );
        }

        let extends_class = str_arg(&args, "extends_class").unwrap_or("Node");
        if !is_valid_identifier(extends_class) {
            return invalid_identifier("extends_class", extends_class);
        }
        let class_name = str_arg(&args, "class_name");
        if let Some(name) = class_name {
            if !is_valid_identifier(name) {
                return invalid_identifier("class_name", name);
            }
        }
        let overwrite = args
            .get("overwrite")
            .and_then(Value::as_bool)
            .unwrap_or(false);

        let project = match self.resolve_project(&args).await {
            Ok(project) => project,
            Err(result) => return result,
        };
        let target = match resolve_in_project(&project.dir, file_path) {
            Ok(target) => target,
            Err(problem) => {
                return ToolResult::error(
                    problem,
                    &["Use a res:// path or a path relative to the project directory"],
                );
            }
        };

        let mut content = format!("extends {extends_class}\n");
        if let Some(name) = class_name {
            content.push_str(&format!("class_name {name}\n"));
        }
        content.push_str("\nfunc _ready() -> void:\n\tpass\n");

        if let Some(parent) = target.parent() {
            if let Err(err) = fs::create_dir_all(parent).await {
                return ToolResult::error(
                    format!("Failed to create directory {}: {err}", parent.display()),
                    &["Check that the project directory is writable"],
                );
            }
        }
        let mut options = fs::OpenOptions::new();
        options.write(true);
        if overwrite {
            options.create(true).truncate(true);
        } else {
            options.create_new(true);
        }
        let write_result = match options.open(&target).await {
            Ok(mut file) => match file.write_all(content.as_bytes()).await {
                // tokio's File writes on a worker thread and does not flush when dropped, so
                // without this the content can be lost after the tool already reported success.
                Ok(()) => file.flush().await,
                Err(err) => Err(err),
            },
            Err(err) => Err(err),
        };
        if let Err(err) = write_result {
            return if err.kind() == ErrorKind::AlreadyExists {
                ToolResult::error(
                    format!(
                        "File already exists: {}",
                        res_display(&project.dir, &target)
                    ),
                    &["Pass overwrite: true to replace it, or choose another file name"],
                )
            } else {
                ToolResult::error(
                    format!(
                        "Failed to write script file {}: {err}",
                        res_display(&project.dir, &target)
                    ),
                    &["Check that the project directory is writable"],
                )
            };
        }

        let res_path = target
            .strip_prefix(&project.dir)
            .ok()
            .map(|relative| format!("res://{}", relative.to_string_lossy().replace('\\', "/")));
        if project.from_editor {
            if let Some(res_path) = &res_path {
                // Let the editor pick the file up (and give it a .uid) right away.
                let _ = self
                    .bridge
                    .send_command("resave_resources", json!({ "paths": [res_path] }))
                    .await;
            }
        }

        ToolResult::text(format!(
            "Created GDScript file {} extending '{extends_class}'.",
            res_path.unwrap_or_else(|| target.display().to_string()),
        ))
    }

    async fn handle_validate_script(&self, args: Value) -> ToolResult {
        let Some(script_path) = str_arg(&args, "script_path") else {
            return missing_argument("script_path");
        };
        if !script_path.ends_with(".gd") {
            return ToolResult::error(
                format!("Only .gd scripts can be validated: {script_path}"),
                &["Use a path such as res://scripts/player.gd"],
            );
        }

        let (project_dir, from_editor) =
            if str_arg(&args, "project_path").is_some() || script_path.starts_with("res://") {
                match self.resolve_project(&args).await {
                    Ok(project) => (Some(project.dir), project.from_editor),
                    Err(result) => return result,
                }
            } else {
                (None, false)
            };

        let file = match &project_dir {
            Some(dir) => match resolve_in_project(dir, script_path) {
                Ok(file) => file,
                Err(problem) => {
                    return ToolResult::error(
                        problem,
                        &["Use a res:// path or a path inside the project directory"],
                    );
                }
            },
            None => PathBuf::from(script_path),
        };
        if !file.is_file() {
            let shown = match &project_dir {
                Some(dir) => res_display(dir, &file),
                None => file.display().to_string(),
            };
            return ToolResult::error(
                format!("Script file not found: {shown}"),
                &["Check the path; res:// paths are resolved against the project directory"],
            );
        }

        // Set when a language server was reached but did not answer; the headless check runs instead.
        let mut lsp_problem = None;
        if let Some(dir) = &project_dir {
            let external_ok = from_editor || self.editor_may_serve(dir).await;
            match self
                .lsp
                .diagnostics_if_reachable(dir, external_ok, &file)
                .await
            {
                Ok(Some(value)) => {
                    let report = code::report_diagnostics(script_path, &value);
                    return if report.failed {
                        ToolResult::error(
                            report.text,
                            &[
                                "Fix the error lines. A warning is listed and does not by itself fail this check",
                                "godot_code_diagnostics returns the same diagnostics",
                            ],
                        )
                    } else {
                        ToolResult::text(report.text)
                    };
                }
                Ok(None) => {}
                Err(error) => lsp_problem = Some(error),
            }
        }

        match self
            .cli
            .validate_script(project_dir.as_deref(), Path::new(script_path))
            .await
        {
            Ok(msg) => match lsp_problem {
                Some(error) => ToolResult::text(format!(
                    "No parse errors (headless check). Analyzer warnings were not checked: the language server was reached but did not answer ({error}); retry, or use godot_code_diagnostics."
                )),
                None => ToolResult::text(msg),
            },
            // The engine could not be run at all: that is a path problem, not a script problem.
            Err(e @ McpError::Process(_)) => ToolResult::error(
                format!("Script validation failed: {e}"),
                &[
                    "Verify GODOT_PATH points to a working Godot executable",
                    "Run the executable with --version to see whether it starts",
                ],
            ),
            Err(e) => ToolResult::error(
                format!("Script validation failed: {e}"),
                &[
                    "Check GDScript syntax line numbers",
                    "Ensure base class and node types exist in the engine",
                ],
            ),
        }
    }

    /// The editor answers a scan-and-wait within 60s, which is the addon's limit.
    /// This waits a little longer so the addon's own timeout is the one the caller sees.
    const RESAVE_BRIDGE_WAIT: std::time::Duration = std::time::Duration::from_secs(90);

    async fn handle_resave_resources(&self, args: Value) -> ToolResult {
        if let Some(wait) = args.get("wait") {
            if !wait.is_null() && !wait.is_boolean() {
                return ToolResult::error(
                    "wait must be a boolean",
                    &["Use wait: true to block until the scan and import finish"],
                );
            }
        }
        let named = str_arg(&args, "project_path");
        if let Some(named) = named {
            if !self.editor_may_serve(Path::new(named)).await {
                return self.import_headless(&args).await;
            }
        }
        let bridge = self.bridge.clone().with_timeout(Self::RESAVE_BRIDGE_WAIT);
        match bridge.send_command("resave_resources", args.clone()).await {
            Ok(result) => {
                let text = serde_json::to_string_pretty(&result).unwrap_or_default();
                if result.get("timed_out").and_then(Value::as_bool) == Some(true) {
                    return ToolResult::error(
                        text,
                        &[
                            "The editor was still scanning or importing when the wait timed out",
                            "Retry godot_resave_resources with wait: true",
                        ],
                    );
                }
                ToolResult::text(text)
            }
            Err(McpError::BridgeUnavailable(_)) => self.import_headless(&args).await,
            Err(error) => bridge_error_result(&error),
        }
    }

    /// `godot --headless --path <project> --import`, used when the editor bridge is closed.
    async fn import_headless(&self, args: &Value) -> ToolResult {
        let project = match self.host_project(args) {
            Ok(project) => project,
            Err(_) => {
                return ToolResult::error(
                    "The editor bridge is not reachable, and no project was found for `godot --headless --path <project> --import`.",
                    &[
                        "Pass project_path: the directory that contains project.godot",
                        "Or start the MCP server from the project directory",
                        "Or open the project in the editor and enable the Godot MCP Bridge plugin",
                    ],
                );
            }
        };
        let requested = match resave_paths(args) {
            Ok(paths) => paths,
            Err(result) => return result,
        };
        let mut skipped_missing = Vec::new();
        let mut skipped_invalid = Vec::new();
        let mut wanted: Vec<String> = Vec::new();
        for path in &requested {
            match resolve_in_project(&project, path) {
                Ok(file) if file.is_file() => wanted.push(res_display(&project, &file)),
                Ok(_) => skipped_missing.push(path.clone()),
                Err(_) => skipped_invalid.push(path.clone()),
            }
        }
        if !requested.is_empty() && wanted.is_empty() {
            return ToolResult::error(
                format!(
                    "None of the given paths exist inside the project: {}",
                    skipped_missing
                        .iter()
                        .chain(skipped_invalid.iter())
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                &["Pass res:// paths of files that exist in the project"],
            );
        }
        let before = import_fingerprints(&project);
        let command = format!("godot --headless --path {} --import", project.display());
        let report = match self.cli.import_project(&project).await {
            Ok(report) => report,
            Err(error) => {
                return ToolResult::error(
                    format!("Failed to import resources with `{command}`: {error}"),
                    &[
                        "Verify GODOT_PATH points to a working Godot executable",
                        "The same command is `godot --headless --path <project> --import`",
                    ],
                );
            }
        };
        let imported = changed_imports(&before, &import_fingerprints(&project));
        let mut not_scanned = Vec::new();
        for path in &wanted {
            if imported.iter().any(|item| item == path) {
                continue;
            }
            let extension = Path::new(path)
                .extension()
                .and_then(|ext| ext.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            let sidecar = resolve_in_project(&project, &format!("{path}.import")).ok();
            let has_sidecar = sidecar.as_ref().is_some_and(|file| file.is_file());
            if IMPORTED_EXTENSIONS.contains(&extension.as_str()) && !has_sidecar {
                not_scanned.push(path.clone());
            }
        }
        let body = json!({
            "mode": "headless_import",
            "command": command,
            "imported": imported,
            "imported_count": imported.len(),
            "not_scanned": not_scanned,
            "skipped_missing": skipped_missing,
            "skipped_invalid": skipped_invalid,
            "exit_code": report.exit_code,
            "timed_out": report.timed_out,
        });
        let text = serde_json::to_string_pretty(&body).unwrap_or_default();
        if report.timed_out || report.exit_code != Some(0) {
            let tail = last_output_lines(&report.stdout, &report.stderr, 20);
            return ToolResult::error(
                format!("{text}\n`{command}` did not finish cleanly.\n{tail}"),
                &[
                    "The command is `godot --headless --path <project> --import`",
                    "A path in not_scanned has no .import file, so load() will not find a loader",
                ],
            );
        }
        ToolResult::text(text)
    }

    async fn handle_run_script(&self, args: Value) -> ToolResult {
        let Some(script) = str_arg(&args, "script") else {
            return missing_argument("script");
        };
        if !script.ends_with(".gd") {
            return ToolResult::error(
                format!("Only .gd scripts can be run: {script}"),
                &["Use a SceneTree script such as res://tests/check.gd"],
            );
        }
        let project = match self.resolve_project(&args).await {
            Ok(project) => project,
            Err(result) => return result,
        };
        let file = match resolve_in_project(&project.dir, script) {
            Ok(file) => file,
            Err(problem) => {
                return ToolResult::error(
                    problem,
                    &["Use a res:// path or a path inside the project directory"],
                );
            }
        };
        if !file.is_file() {
            return ToolResult::error(
                format!(
                    "Script file not found: {}",
                    res_display(&project.dir, &file)
                ),
                &["Check the path; res:// paths are resolved against the project directory"],
            );
        }
        let user_args = match user_args_of(&args) {
            Ok(values) => values.unwrap_or_default(),
            Err(result) => return result,
        };
        let timeout_seconds = match script_timeout_seconds(&args) {
            Ok(seconds) => seconds,
            Err(result) => return result,
        };
        match self
            .cli
            .run_script(&project.dir, &file, &user_args, timeout_seconds)
            .await
        {
            Ok(report) => script_run_result(&report, timeout_seconds),
            Err(error) => ToolResult::error(
                format!("Failed to run the script: {error}"),
                &["Verify GODOT_PATH points to a working Godot executable"],
            ),
        }
    }

    /// Decides whether a live-first call goes to the connected editor (TL-10).
    ///
    /// A call that names a `project_path` goes to the editor only when the editor has that
    /// project open; otherwise the caller takes its file path. `wants_dir` also asks the editor
    /// for its project when no path is named, so an answer that changed something can name the
    /// project it changed. An editor that cannot say which project it has open is not sent the call.
    async fn editor_choice(
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
    async fn editor_may_serve(&self, project: &Path) -> bool {
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
    fn host_project(&self, args: &Value) -> Result<PathBuf, ToolResult> {
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
    async fn resolve_project(&self, args: &Value) -> Result<ResolvedProject, ToolResult> {
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

const DEFAULT_SCRIPT_TIMEOUT: u64 = 120;
const MAX_SCRIPT_TIMEOUT: u64 = 600;

fn script_timeout_seconds(args: &Value) -> std::result::Result<u64, ToolResult> {
    let Some(value) = args.get("timeout_seconds").filter(|value| !value.is_null()) else {
        return Ok(DEFAULT_SCRIPT_TIMEOUT);
    };
    let seconds = value
        .as_u64()
        .or_else(|| value.as_i64().and_then(|number| u64::try_from(number).ok()));
    let Some(seconds) = seconds else {
        return Err(ToolResult::error(
            "timeout_seconds must be a whole number of seconds",
            &["Use timeout_seconds: 120"],
        ));
    };
    if !(1..=MAX_SCRIPT_TIMEOUT).contains(&seconds) {
        return Err(ToolResult::error(
            format!("timeout_seconds must be from 1 to {MAX_SCRIPT_TIMEOUT}"),
            &["The default is 120"],
        ));
    }
    Ok(seconds)
}

fn script_run_result(report: &ScriptRun, timeout_seconds: u64) -> ToolResult {
    let exit = report
        .exit_code
        .map(|code| code.to_string())
        .unwrap_or_else(|| "none".into());
    let summary = if report.stopped_idle {
        let detail = report.script_errors.first().map_or_else(
            || String::new(),
            |error| format!(" at {}:{}: {}", error.file, error.line, error.message),
        );
        format!(
            "Stopped after a SCRIPT ERROR while the main loop was idle{detail}. The process was still running, so it was stopped without waiting out the {timeout_seconds}s timeout."
        )
    } else if report.timed_out {
        format!(
            "Timed out after {timeout_seconds}s and killed the process tree.\nLast output:\n{}",
            last_output_lines(&report.stdout, &report.stderr, 20)
        )
    } else if let Some(error) = report.script_errors.first() {
        format!(
            "Script error at {}:{}: {} (exit code {exit}).",
            error.file, error.line, error.message
        )
    } else {
        format!("Script finished with exit code {exit}.")
    };
    let body = json!({
        "summary": summary,
        "exit_code": report.exit_code,
        "timed_out": report.timed_out,
        "stopped_idle": report.stopped_idle,
        "stdout": report.stdout,
        "stderr": report.stderr,
        "script_errors": report.script_errors.iter().map(|error| json!({
            "message": error.message,
            "file": error.file,
            "line": error.line,
        })).collect::<Vec<_>>(),
    });
    let text = serde_json::to_string_pretty(&body).unwrap_or(summary);
    if report.timed_out {
        ToolResult::error(
            text,
            &[
                "The process tree was killed",
                "Make the script call quit(), or raise timeout_seconds up to 600",
            ],
        )
    } else if report.exit_code != Some(0) || !report.script_errors.is_empty() {
        ToolResult::error(
            text,
            &["script_errors lists each SCRIPT ERROR with its file and line"],
        )
    } else {
        ToolResult::text(text)
    }
}

fn resave_paths(args: &Value) -> std::result::Result<Vec<String>, ToolResult> {
    let Some(value) = args.get("paths").filter(|value| !value.is_null()) else {
        return Ok(Vec::new());
    };
    let Some(items) = value.as_array() else {
        return Err(ToolResult::error(
            "paths must be an array of res:// paths",
            &["Omit paths for a full import, or pass paths: [\"res://art/models/x.glb\"]"],
        ));
    };
    let mut paths = Vec::new();
    for item in items {
        let Some(path) = item.as_str() else {
            return Err(ToolResult::error(
                "paths must be an array of strings",
                &["Each path is a res:// path"],
            ));
        };
        if path.trim().is_empty() {
            return Err(ToolResult::error(
                "paths contains an empty string",
                &["Each path is a res:// path"],
            ));
        }
        paths.push(path.to_string());
    }
    Ok(paths)
}

fn last_output_lines(stdout: &str, stderr: &str, count: usize) -> String {
    let mut lines: Vec<&str> = stdout.lines().chain(stderr.lines()).collect();
    if lines.len() > count {
        lines = lines.split_off(lines.len() - count);
    }
    if lines.is_empty() {
        "(no output)".into()
    } else {
        lines.join("\n")
    }
}

fn str_arg<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(Value::as_str)
}

const MAX_USER_ARGS: usize = 32;
const MAX_USER_ARG_CHARS: usize = 256;

/// `Some` when the caller sent `user_args`, including an empty array. `None` when the key is absent.
fn user_args_of(args: &Value) -> Result<Option<Vec<String>>, ToolResult> {
    let Some(value) = args.get("user_args").filter(|value| !value.is_null()) else {
        return Ok(None);
    };
    let Some(items) = value.as_array() else {
        return Err(ToolResult::error(
            "user_args must be an array of strings",
            &["Pass user_args: [\"--caustics\"] with mode: \"process\""],
        ));
    };
    if items.len() > MAX_USER_ARGS {
        return Err(ToolResult::error(
            format!("user_args accepts at most {MAX_USER_ARGS} strings"),
            &["Pass only the flags the game itself reads"],
        ));
    }
    let mut user_args = Vec::with_capacity(items.len());
    for item in items {
        let Some(text) = item.as_str() else {
            return Err(ToolResult::error(
                "Every user_args item must be a string",
                &["Pass user_args: [\"--caustics\"]"],
            ));
        };
        if text.chars().count() > MAX_USER_ARG_CHARS {
            return Err(ToolResult::error(
                format!("Each user_args string must be at most {MAX_USER_ARG_CHARS} characters"),
                &["Shorten the argument"],
            ));
        }
        if text.chars().any(char::is_control) {
            return Err(ToolResult::error(
                "user_args cannot contain control characters",
                &["Pass plain text the game reads with OS.get_cmdline_user_args()"],
            ));
        }
        // The runtime reads these two from the same list: a caller's copy would hold or retime a
        // game the server believes is running freely.
        if text.starts_with("--godot-mcp-") {
            return Err(ToolResult::error(
                "user_args cannot start with --godot-mcp-: the server reserves those for itself",
                &[
                    "Use held: true on godot_run_project to start the game held",
                    "Pass only the flags the game itself reads",
                ],
            ));
        }
        user_args.push(text.to_string());
    }
    Ok(Some(user_args))
}

fn string_list(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn missing_argument(name: &str) -> ToolResult {
    ToolResult::error(
        format!("Missing required parameter: {name}"),
        &["Check the tool's inputSchema returned by tools/list"],
    )
}

fn invalid_identifier(parameter: &str, value: &str) -> ToolResult {
    ToolResult::error(
        format!(
            "Invalid {parameter} {value:?}: use letters, digits and underscores only, not starting with a digit"
        ),
        &["Example: Node2D, CharacterBody3D, Player"],
    )
}

fn with_notice(mut result: ToolResult, notice: &str) -> ToolResult {
    if notice.is_empty() {
        return result;
    }
    if let Some(crate::protocol::ToolContent::Text { text }) = result.content.first_mut() {
        text.push_str(notice);
    }
    result
}

fn list_paths(paths: &[String]) -> String {
    const SHOWN: usize = 32;
    if paths.len() <= SHOWN {
        paths.join(", ")
    } else {
        format!(
            "{}, and {} more",
            paths[..SHOWN].join(", "),
            paths.len() - SHOWN
        )
    }
}

/// Turns a bridge failure into a tool error whose recovery hints match what actually failed.
fn bridge_error_result(error: &McpError) -> ToolResult {
    match error {
        McpError::BridgeUnavailable(detail) => ToolResult::error(
            format!("The Godot Editor bridge is not reachable ({detail})"),
            &[
                "Launch Godot Editor with your project (godot_launch_editor can do it)",
                "Ensure 'Godot MCP Bridge' plugin is enabled in Project Settings -> Plugins",
                "Set the addon's project setting godot_mcp/bridge_port to the port named in the error, or start the editor with that same GODOT_DEBUG_PORT",
            ],
        ),
        McpError::Bridge(detail) => ToolResult::error(
            detail.clone(),
            &[
                "The editor may be busy (importing, modal dialog) or restarting; retry in a moment",
                "Look for [Godot MCP] messages in the editor's Output panel",
            ],
        ),
        McpError::ToolExecution(message) => {
            ToolResult::error(message.clone(), editor_error_hints(message))
        }
        McpError::Process(detail) => ToolResult::error(
            format!("Engine process error: {detail}"),
            &["Verify GODOT_PATH points to a working Godot executable"],
        ),
        other => ToolResult::error(
            other.to_string(),
            &["This is unexpected: retry once, and note it with godot_note_feedback if it repeats"],
        ),
    }
}

/// The `code` of a failure the running game reported, which arrives as a JSON object.
fn runtime_code(message: &str) -> Option<String> {
    let value: Value = serde_json::from_str(message).ok()?;
    value.get("code")?.as_str().map(str::to_string)
}

fn editor_error_hints(message: &str) -> &'static [&'static str] {
    if let Some(code) = runtime_code(message) {
        return match code.as_str() {
            "DENIED" => &[
                "The runtime refuses this by design: the script property, private names, and methods that quit, evaluate code, call other methods or free nodes",
                "Use a public property or a gameplay method named in permit",
            ],
            "UNSUPPORTED" => &[
                "If the message says the game has no window, start the game without headless with godot_run_project",
                "Otherwise read the message for what the running game needs",
            ],
            "NOT_FOUND" => &[
                "Look up the node path with godot_game_tree; paths are relative to the current scene",
            ],
            "BAD_PARAMS" => &[
                "Fix the parameter the message names; its shape is in the tool's inputSchema from tools/list",
            ],
            _ => &["Read the message; the running game refused the request"],
        };
    }
    let lower = message.to_lowercase();
    if lower.contains("unauthorized") {
        &[
            "Restart the MCP client so the server re-reads the bridge token",
            "Run the MCP server and the Godot editor as the same OS user, or set the same GODOT_MCP_TOKEN for both",
        ]
    } else if lower.contains("no active scene") {
        &["Open a scene first with godot_open_scene"]
    } else if lower.contains("unsaved changes") {
        &[
            "Save the script in the editor (Ctrl+S) or discard its changes, then call godot_read_script again and repeat the edit",
        ]
    } else if lower.contains("differs from the file") {
        &[
            "Save or reload the script in the editor so that it matches the file, then call godot_read_script again",
        ]
    } else if lower.contains("cannot be matched") {
        &[
            "Close the extra tabs in the script editor (help pages, text files), or save the script, then read it again",
        ]
    } else if lower.contains("already exists") {
        &["Choose another path; an existing file is never replaced"]
    } else if lower.contains("can't open") || lower.contains("failed to save") {
        &[
            "Check that the folder exists or can be created inside the project and that it is writable, then retry",
        ]
    } else if lower.contains("expects a") {
        &[
            "Send a value of the type the setting already has, or change it in the editor's Project Settings",
        ]
    } else if lower.contains("not found")
        || lower.contains("does not exist")
        || lower.contains("not currently open")
    {
        &["Verify the path with godot_get_scene_tree or godot_get_open_scenes"]
    } else {
        &["Correct the arguments according to the message above and retry"]
    }
}

/// Which feedback tools the catalog holds; set once at start-up by `configure_catalog`.
static CATALOG_MODE: OnceLock<FeedbackMode> = OnceLock::new();

fn tool_catalog() -> &'static [Tool] {
    static CATALOG: OnceLock<Vec<Tool>> = OnceLock::new();
    CATALOG.get_or_init(|| build_catalog(CATALOG_MODE.get().copied().unwrap_or_default()))
}

fn tool(name: &str, description: &str, properties: Value, required: &[&str]) -> Tool {
    let mut input_schema = json!({ "type": "object", "properties": properties });
    if !required.is_empty() {
        input_schema["required"] = json!(required);
    }
    Tool {
        name: name.into(),
        description: description.into(),
        input_schema,
    }
}

fn build_catalog(feedback_mode: FeedbackMode) -> Vec<Tool> {
    let mut tools = vec![
        tool(
            "godot_get_version",
            "Retrieves the Godot Engine version and build information.",
            json!({}),
            &[],
        ),
        tool(
            "godot_get_project_info",
            "Reports the name, main scene, enabled features and directory of the project open in the live editor. Requires the editor bridge.",
            json!({}),
            &[],
        ),
        tool(
            "godot_launch_editor",
            "Launches the Godot Editor for a project directory. Without project_path, opens the project the server was started in (its working directory or a parent of it, or the only subdirectory that contains project.godot).",
            json!({
                "project_path": {
                    "type": "string",
                    "description": "Absolute path to the Godot project directory containing project.godot. Optional when the server runs inside the project."
                }
            }),
            &[],
        ),
        tool(
            "godot_run_project",
            "Runs the Godot project or a specific scene. Uses the live editor's Play mode when the bridge is connected. Otherwise starts a managed process (debugger attached) that godot_stop_project can stop. held true (with mode process) starts the game held at its first physics tick at a fixed frame rate equal to the project's tick rate, so godot_game_physics steps are exact. user_args (mode process only) are strings placed after --, so the game reads them with OS.get_cmdline_user_args() and Godot does not treat them as engine options. mode auto and editor Play reject user_args instead of dropping them. Before the run, files with a .import sidecar are compared with their .godot/imported cache. When the editor bridge is connected to this project, stale imports are reimported first, the same way godot_resave_resources does. Otherwise the result warns, lists the res:// paths, and names godot_resave_resources.",
            json!({
                "project_path": {
                    "type": "string",
                    "description": "Path to the project root directory. Only used by the CLI fallback (i.e. when no editor is connected); defaults to the project the server was started in."
                },
                "scene": {
                    "type": "string",
                    "description": "Optional specific scene path (e.g., 'res://scenes/Main.tscn'). Defaults to the project's main scene."
                },
                "breakpoints": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Optional res://script.gd:LINE breakpoints for a process run."
                },
                "headless": {
                    "type": "boolean",
                    "description": "Process runs only. When true, start without a window (screenshots will be unavailable)."
                },
                "break_on_errors": {
                    "type": "boolean",
                    "description": "Process runs only. When true, a script error pauses the game. Default false: the error is still logged, but the game keeps running."
                },
                "held": {
                    "type": "boolean",
                    "description": "Process runs only (needs mode process). Start the game held at its first physics tick, at a fixed frame rate equal to the project's tick rate, so held steps are exact."
                },
                "mode": {
                    "type": "string",
                    "enum": ["auto", "process"],
                    "description": "auto (default) uses the live editor when the bridge is connected. process always starts a managed debugger-attached process that godot_stop_project can stop."
                },
                "user_args": {
                    "type": "array",
                    "items": {"type": "string"},
                    "maxItems": 32,
                    "description": "Process mode only. Up to 32 strings, each at most 256 characters, passed to the game after --. They are never Godot engine options. Refused unless mode is process."
                }
            }),
            &[],
        ),
        tool(
            "godot_stop_project",
            "Stops what runs: without run_id, the editor's Play mode when it plays and every managed process run that is still running, and the answer lists each one it stopped (or says that nothing was running). Pass run_id to stop one process run.",
            json!({
                "run_id": {"type": "string", "description": "Optional id of a managed process run. Omit to stop everything that runs."}
            }),
            &[],
        ),
        tool(
            "godot_get_run_status",
            "Reports whether a scene is playing in the editor and lists managed process runs, including each run's user_args.",
            json!({}),
            &[],
        ),
        tool(
            "godot_get_scene_tree",
            "Retrieves the complete node hierarchy of a scene open in Godot Editor. Node paths in the result are relative to the scene root ('.' is the root). This read does not reload the scene; when the file on disk differs, the result sets stale to true.",
            json!({
                "scene_path": {
                    "type": "string",
                    "description": "Optional res:// path of an open scene tab to inspect. Defaults to the currently active/focused scene tab. Use godot_get_open_scenes to list valid values."
                }
            }),
            &[],
        ),
        tool(
            "godot_get_open_scenes",
            "Lists the res:// paths of all scenes currently open as tabs in the editor, and which one is active.",
            json!({}),
            &[],
        ),
        tool(
            "godot_add_node",
            "Adds a new node to the active scene in Godot Editor with undo/redo support. node_type is any instantiable Node class or a global script class_name.",
            json!({
                "parent_node_path": {
                    "type": "string",
                    "description": "Path to the parent node relative to the scene root (e.g. '.' for root or 'Player/Sprite')."
                },
                "node_type": {
                    "type": "string",
                    "description": "Godot class name to instantiate (e.g. 'Sprite2D', 'Camera3D', 'Node')."
                },
                "node_name": {
                    "type": "string",
                    "description": "Optional custom name for the new node. Godot may adjust invalid characters; the result reports the final name."
                }
            }),
            &["node_type"],
        ),
        tool(
            "godot_remove_node",
            "Removes a node from the active scene tree with undo/redo support. The scene root cannot be removed.",
            json!({
                "node_path": {
                    "type": "string",
                    "description": "Path to the node to remove, relative to the scene root."
                }
            }),
            &["node_path"],
        ),
        tool(
            "godot_get_node_properties",
            "Inspects the editor-visible properties of a node. Each property is returned as {type, value}, with the value in Godot's text format (e.g. 'Vector2(5, 7)').",
            json!({
                "node_path": {
                    "type": "string",
                    "description": "Path to the target node relative to the scene root ('.' or empty for the root)."
                }
            }),
            &["node_path"],
        ),
        tool(
            "godot_set_node_properties",
            "Modifies properties of a node in the active scene with undo/redo support. Values are converted to the property's type: vectors, rects and colors accept arrays ([10, 20]) or objects ({\"x\": 10, \"y\": 20}), colors also accept '#rrggbb' or names, resources accept res:// paths, and any Godot literal string (e.g. 'Vector2(1, 2)') is parsed. Unknown properties or unconvertible values reject the whole call without changing anything.",
            json!({
                "node_path": {
                    "type": "string",
                    "description": "Path to the target node relative to the scene root ('.' for the root)."
                },
                "properties": {
                    "type": "object",
                    "description": "Key-value map of property names to new values."
                }
            }),
            &["node_path", "properties"],
        ),
        tool(
            "godot_capture_viewport",
            "Captures a screenshot of the Godot 2D or 3D editor viewport as a PNG image. Renders the scene content (without editor gizmos or grid). Switches the editor's main screen to 2D/3D to render it, so it needs a visible (non-headless) editor with a scene open.",
            json!({
                "view": {
                    "type": "string",
                    "enum": ["auto", "2d", "3d"],
                    "description": "Which editor viewport to capture. 'auto' (default) picks 3D for Node3D scenes and 2D otherwise."
                },
                "max_dimension": {
                    "type": "integer",
                    "description": "Downscale the image so neither side exceeds this many pixels (default 1568)."
                }
            }),
            &[],
        ),
        tool(
            "godot_open_scene",
            "Opens a scene file in the Godot Editor by its res:// path. An absolute path inside the project is converted, including both macOS spellings of /var and /private/var. Reloading an already-open scene from disk drops that scene's undo history.",
            json!({
                "scene_path": {
                    "type": "string",
                    "description": "Resource path to scene (e.g. 'res://scenes/level1.tscn')."
                },
                "reload": {
                    "type": "boolean",
                    "description": "When the scene is already open, reload it from disk and drop its undo history. Automatic when the file bytes differ and the editor has no unsaved changes."
                },
                "discard": {
                    "type": "boolean",
                    "description": "Reload from disk even when the editor has unsaved changes, whether made by hand or through this bridge. Those changes and the scene's undo history are discarded."
                }
            }),
            &["scene_path"],
        ),
        tool(
            "godot_save_scene",
            "Saves changes to the current open scene in Godot Editor.",
            json!({}),
            &[],
        ),
        tool(
            "godot_create_script",
            "Creates a new GDScript file with a standard header inside a Godot project. Refuses to overwrite an existing file unless overwrite is true. script_path is the same parameter the other script tools use; file_path is its older name.",
            json!({
                "script_path": {
                    "type": "string",
                    "description": "Path of the .gd script: a res:// path or a path relative to the project directory."
                },
                "file_path": {
                    "type": "string",
                    "description": "The older name of script_path; both are accepted."
                },
                "extends_class": {
                    "type": "string",
                    "description": "Base class to extend (e.g. 'Node2D', 'CharacterBody3D'). Defaults to 'Node'."
                },
                "class_name": {
                    "type": "string",
                    "description": "Optional global class_name registration."
                },
                "project_path": {
                    "type": "string",
                    "description": "Project directory containing project.godot. Optional: defaults to the project open in the connected editor, else the project the server was started in."
                },
                "overwrite": {
                    "type": "boolean",
                    "description": "Replace the file if it already exists. Defaults to false."
                }
            }),
            &[],
        ),
        tool(
            "godot_validate_script",
            "Checks a GDScript file. When a language server is already running (the open editor, or one godot_code_diagnostics started), returns its errors and warnings as file:line:col, code and message, with 1-based lines. A warning does not fail the check; an error does. Without a language server, runs godot --headless --check-only, which sees parse errors only, and says that analyzer warnings were not checked. godot_code_diagnostics is the tool that starts a language server when none is running.",
            json!({
                "script_path": {
                    "type": "string",
                    "description": "Path to the .gd script file (res:// paths need a project)."
                },
                "project_path": {
                    "type": "string",
                    "description": "Optional project root path for class resolution. For res:// paths it defaults to the editor's project, else the project the server was started in."
                }
            }),
            &["script_path"],
        ),
        tool(
            "godot_run_script",
            "Runs a GDScript that extends SceneTree with the configured Godot binary: godot --headless --path <project> -s <script> [-- user_args]. Captures stdout and stderr (they never touch this server's stdout). script_errors lists each SCRIPT ERROR with its file and line. Returns the exit code. timeout_seconds defaults to 120 and cannot exceed 600. On timeout the process tree is killed and the answer says so, with the last output lines. A SCRIPT ERROR that leaves the main loop idle (the process stays up, as a script error in _init does) stops the run as soon as that idle is seen, and the answer says so, instead of waiting out the timeout. Paths outside the project are refused.",
            json!({
                "script": {
                    "type": "string",
                    "description": "SceneTree script: a res:// path or a path inside the project, ending in .gd."
                },
                "user_args": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Arguments after --, read by OS.get_cmdline_user_args(). At most 32 strings."
                },
                "timeout_seconds": {
                    "type": "integer",
                    "description": "Kill the process tree after this many seconds. Default 120, maximum 600.",
                    "minimum": 1,
                    "maximum": 600
                },
                "project_path": {
                    "type": "string",
                    "description": "Project directory containing project.godot. Optional: defaults to the project open in the connected editor, else the project the server was started in."
                }
            }),
            &["script"],
        ),
        tool(
            "godot_resave_resources",
            "Refreshes assets and resources after they were changed externally. With the editor connected, importable files the editor already knows are re-imported synchronously and other files are re-scanned. A path the editor has never scanned is updated and, if needed, scanned first; it is listed under reimported only after a .import file exists, and otherwise under not_scanned (load() would fail with no loader). Without paths, rescans the whole project in the background. wait true holds the reply until scanning and importing finish (up to 60s) and reports how many files were imported; timed_out means they may not be loadable yet. Without the editor, runs `godot --headless --path <project> --import` and reports the sources whose .import sidecar appeared or changed.",
            json!({
                "paths": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "res:// paths to refresh. Importable assets (images, audio, models, fonts) the editor already knows are re-imported synchronously. A path it has never scanned is scanned first and listed under not_scanned when no .import file was written. Other files (scripts, scenes, resources) are re-scanned. If omitted or empty, triggers a full project filesystem rescan instead."
                },
                "wait": {
                    "type": "boolean",
                    "description": "Full scan only. When true, return after the editor finishes scanning and importing, or after 60s. The result includes imported (how many files were imported) and timed_out. Default false, which returns while the scan is still running. Ignored when paths is set. Without the editor, the headless import always waits until Godot exits."
                },
                "project_path": {
                    "type": "string",
                    "description": "Project directory containing project.godot. Used when the editor is closed, for `godot --headless --path <project> --import`. Optional: defaults to the project the server was started in. If the editor has a different project open, the import runs headless on this path."
                }
            }),
            &[],
        ),
    ];
    tools.extend(authoring::catalog());
    tools.extend(scripts::catalog());
    tools.extend(game::catalog());
    tools.extend(code::catalog());
    tools.extend(feedback::catalog(feedback_mode));
    tools
}
