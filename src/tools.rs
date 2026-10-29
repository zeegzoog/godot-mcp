use crate::bridge::BridgeClient;
use crate::cli_runner::CliRunner;
use crate::config::FeedbackMode;
use crate::error::McpError;
use crate::godot::GameHub;
use crate::journal::EvidenceKind;
use crate::lsp::LspHub;
use crate::paths::{is_valid_identifier, res_display, resolve_in_project};
use crate::protocol::{Tool, ToolAnnotations, ToolResult};
use crate::validation::validate_arguments;
use serde_json::{Value, json};
use std::io::ErrorKind;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use tokio::fs;
use tokio::io::AsyncWriteExt;

mod authoring;
mod code;
mod feedback;
mod files;
mod game;
mod headless;
mod input_map;
mod model_chain;
mod project;
mod run;
mod scenario;
mod scripts;

use project::EditorChoice;

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
            "godot_check_voxel_model" => model_chain::call(&self, &args).await,
            name if authoring::handles(name) => authoring::call(&self, name, args).await,
            name if input_map::handles(name) => input_map::call(&self, name, args).await,
            name if files::handles(name) => files::call(&self, args).await,
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

        let res_path = target
            .strip_prefix(&project.dir)
            .ok()
            .map(|relative| format!("res://{}", relative.to_string_lossy().replace('\\', "/")));
        let replacing = overwrite && fs::try_exists(&target).await.unwrap_or(false);
        // A named project may still be the one the editor has open, with the script in a tab.
        let editor_holds_project = project.from_editor
            || (replacing
                && str_arg(&args, "project_path").is_some()
                && self.editor_may_serve(&project.dir).await);
        let open_tab = match (&res_path, editor_holds_project && replacing) {
            (Some(res_path), true) => match self.script_tab(res_path, false).await {
                Ok(tab) => tab,
                Err(result) => return result,
            },
            _ => None,
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

        let mut reload_note = String::new();
        if editor_holds_project {
            if let Some(res_path) = &res_path {
                // Let the editor pick the file up (and give it a .uid) right away.
                let _ = self
                    .bridge
                    .send_command("resave_resources", json!({ "paths": [res_path] }))
                    .await;
                if open_tab.is_some() {
                    // The refresh above already reloads a clean tab; this call catches an
                    // older addon or a tab the refresh did not reach.
                    reload_note = match self.script_tab(res_path, true).await {
                        Ok(Some(tab))
                            if tab.get("stale").and_then(Value::as_bool) == Some(true) =>
                        {
                            format!(
                                " Warning: the open script tab still shows the old text; call godot_resave_resources with paths [\"{res_path}\"] to reload it."
                            )
                        }
                        Ok(Some(_)) => {
                            " The open script tab was reloaded with the new text.".to_string()
                        }
                        _ => String::new(),
                    };
                }
            }
        }

        ToolResult::text(format!(
            "Created GDScript file {} extending '{extends_class}'.{reload_note}",
            res_path.unwrap_or_else(|| target.display().to_string()),
        ))
    }

    /// The editor's script tab for `res_path` when it is open, `None` when it is not or the editor
    /// cannot say (an editor without the command, or none at all). Refuses, as the error, an
    /// overwrite the person could lose typing to: a tab with unsaved changes, or one that cannot
    /// be matched to the file.
    async fn script_tab(&self, res_path: &str, reload: bool) -> Result<Option<Value>, ToolResult> {
        let Ok(tab) = self
            .bridge
            .send_command(
                "script_tab",
                json!({ "script_path": res_path, "reload": reload }),
            )
            .await
        else {
            return Ok(None);
        };
        if reload {
            return Ok(Some(tab));
        }
        if tab.get("dirty").and_then(Value::as_bool) == Some(true) {
            return Err(ToolResult::error(
                format!(
                    "{res_path} has unsaved changes in the Godot script editor, so it was not overwritten"
                ),
                &[
                    "Ask the person to save or discard the changes in the script editor, then call godot_create_script again",
                    "Or change the script with godot_read_script and godot_edit_script once it is saved",
                ],
            ));
        }
        if tab.get("editor_state").and_then(Value::as_str) == Some("unknown") {
            let reason = tab.get("reason").and_then(Value::as_str).unwrap_or("");
            return Err(ToolResult::error(
                format!(
                    "The script editor's tab for {res_path} cannot be matched ({reason}), so it was not overwritten"
                ),
                &[
                    "Close the extra tabs in the script editor, or save the script, then call godot_create_script again",
                ],
            ));
        }
        Ok((tab.get("open").and_then(Value::as_bool) == Some(true)).then_some(tab))
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
        annotations: annotations(name),
    }
}

/// Hints for a listed tool. Read-only means it changes nothing in the project,
/// the editor, a running game or files on disk; scratch state kept by this
/// server (screenshot baselines) does not count. Destructive means it may
/// overwrite or remove something there, or run code that can. Idempotent means
/// repeating the call with the same arguments changes nothing more. Only
/// godot_submit_feedback reaches outside this machine. A tool missing here is
/// listed without hints, which hosts read as the most cautious defaults.
fn annotations(name: &str) -> Option<ToolAnnotations> {
    let [read_only, destructive, idempotent] = match name {
        "godot_get_version"
        | "godot_get_project_info"
        | "godot_get_run_status"
        | "godot_get_scene_tree"
        | "godot_get_open_scenes"
        | "godot_get_node_properties"
        | "godot_validate_script"
        | "godot_list_signals"
        | "godot_get_project_settings"
        | "godot_get_input_map"
        | "godot_read_script"
        | "godot_run_output"
        | "godot_game_tree"
        | "godot_game_get"
        | "godot_game_signals"
        | "godot_game_observe"
        | "godot_game_snapshot"
        | "godot_game_screenshot"
        | "godot_debug_state"
        | "godot_code_diagnostics"
        | "godot_code_complete"
        | "godot_code_symbols"
        | "godot_code_definition"
        | "godot_code_references" => [true, false, true],
        "godot_capture_viewport"
        | "godot_resave_resources"
        | "godot_create_project"
        | "godot_create_scene"
        | "godot_connect_signal"
        | "godot_create_resource"
        | "godot_game_input_release"
        | "godot_game_wait"
        | "godot_debug_breakpoints" => [false, false, true],
        "godot_launch_editor"
        | "godot_run_project"
        | "godot_add_node"
        | "godot_game_input"
        | "godot_game_physics"
        | "godot_game_scenario"
        | "godot_game_record"
        | "godot_debug_control"
        | "godot_note_feedback"
        | "godot_draft_feedback"
        | "godot_submit_feedback" => [false, false, false],
        "godot_stop_project"
        | "godot_remove_node"
        | "godot_set_node_properties"
        | "godot_open_scene"
        | "godot_save_scene"
        | "godot_create_script"
        | "godot_check_voxel_model"
        | "godot_attach_script"
        | "godot_set_project_setting"
        | "godot_edit_input_action"
        | "godot_delete_file"
        | "godot_game_set" => [false, true, true],
        "godot_run_script" | "godot_edit_script" | "godot_game_call" => [false, true, false],
        _ => return None,
    };
    Some(ToolAnnotations {
        read_only_hint: read_only,
        destructive_hint: destructive,
        idempotent_hint: idempotent,
        open_world_hint: name == "godot_submit_feedback",
    })
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
            "Opens a scene file in the Godot Editor by its res:// path. An absolute path inside the project is converted, including both macOS spellings of /var and /private/var. Reloading an already-open scene from disk drops that scene's undo history. The answer lists in load_errors what Godot reported while loading (for example a node renamed because its name clashed with a child of an instanced scene), names the editor dialog that shows them, and says in unsaved whether the load left the scene with unsaved changes.",
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
            "Saves the current open scene in Godot Editor. The editor saves its open script tabs with it: a script with unsaved typing is written and listed in scripts_saved, while the editor's convert-indent and trim-on-save settings are held off so a clean script is not rewritten. Returns scene_path.",
            json!({}),
            &[],
        ),
        tool(
            "godot_create_script",
            "Creates a new GDScript file with a standard header inside a Godot project. Refuses to overwrite an existing file unless overwrite is true. When the editor has the script open in a tab, an overwrite is refused while the tab has unsaved changes, and otherwise reloads the tab with the new text. script_path is the same parameter the other script tools use; file_path is its older name.",
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
        model_chain::definition(),
        tool(
            "godot_resave_resources",
            "Refreshes assets and resources after they were changed externally. With the editor connected, importable files the editor already knows are re-imported synchronously and other files are re-scanned. An open script tab without unsaved changes whose text differs from its file is reloaded (listed under reloaded_tabs); one with unsaved changes is kept and listed under unsaved_tabs. A path the editor has never scanned is updated and, if needed, scanned first; it is listed under reimported only after a .import file exists, and otherwise under not_scanned (load() would fail with no loader). Without paths, rescans the whole project in the background. wait true holds the reply until scanning and importing finish (up to 60s) and reports how many files were imported; timed_out means they may not be loadable yet. Without the editor, runs `godot --headless --path <project> --import` and reports the sources whose .import sidecar appeared or changed.",
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
    tools.extend(input_map::catalog());
    tools.extend(files::catalog());
    tools.extend(scripts::catalog());
    tools.extend(game::catalog());
    tools.extend(code::catalog());
    tools.extend(feedback::catalog(feedback_mode));
    tools
}
