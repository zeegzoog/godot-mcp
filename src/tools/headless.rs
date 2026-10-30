//! Tools that run the Godot binary without the editor: script validation, one-off scripts,
//! resource resaving and the headless import.

use super::main_loop::main_loop_problem;
use super::{ToolManager, bridge_error_result, code, missing_argument, str_arg, user_args_of};
use crate::cli_runner::{ScriptErrorLoc, ScriptRun, script_load_failure};
use crate::error::McpError;
use crate::godot::{
    IMPORTED_EXTENSIONS, ResourceDamage, ResourceRepair, TextResourceSnapshot, changed_imports,
    import_fingerprints,
};
use crate::paths::{res_display, resolve_in_project, same_directory};
use crate::protocol::ToolResult;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

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

/// The `resource_damage` entries of a run's answer and a sentence for its summary.
fn damage_report(damage: &[ResourceDamage]) -> (Vec<Value>, Option<String>) {
    let entries = damage
        .iter()
        .map(|item| {
            json!({
                "path": item.path,
                "lost_uid": item.lost_uid,
                "ext_resources_without_uid": item.ext_resources_without_uid.iter().map(|(path, uid)| json!({
                    "path": path,
                    "uid": uid,
                })).collect::<Vec<_>>(),
                "null_shader_parameters": item.null_shader_parameters,
            })
        })
        .collect::<Vec<_>>();
    if damage.is_empty() {
        return (entries, None);
    }
    let paths = damage
        .iter()
        .map(|item| item.path.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let warning = format!(
        "WARNING: the script rewrote {paths} without the editor, and the server could not repair them: resource_damage lists the uid:// references still missing and the shader parameters still written as null. Restore the files from version control, or put the listed uid=\"...\" back and delete the null shader_parameter lines; make sure the files are writable."
    );
    (entries, Some(warning))
}

/// What the run's answer says about the files the server repaired after the script.
struct RepairReport {
    repairs: Vec<ResourceRepair>,
    /// The connected editor's answer to refreshing the repaired files, when it holds the project.
    editor: Option<Value>,
    /// Why the connected editor could not refresh them.
    editor_error: Option<String>,
}

impl RepairReport {
    fn entries(&self) -> Vec<Value> {
        self.repairs
            .iter()
            .map(|item| {
                json!({
                    "path": item.path,
                    "uid": item.uid,
                    "new_uid": item.new_uid,
                    "ext_resource_uids": item.ext_resource_uids.iter().map(|(path, uid)| json!({
                        "path": path,
                        "uid": uid,
                    })).collect::<Vec<_>>(),
                    "removed_null_shader_parameters": item.removed_null_shader_parameters,
                })
            })
            .collect()
    }

    fn sentence(&self) -> Option<String> {
        if self.repairs.is_empty() {
            return None;
        }
        let paths = self
            .repairs
            .iter()
            .map(|item| item.path.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        let editor = match (&self.editor, &self.editor_error) {
            (Some(_), _) => " The connected editor refreshed them; reload any of them it has open.",
            (None, Some(_)) => {
                " The connected editor could not refresh them (editor_refresh_error); rescan the file system there."
            }
            (None, None) => "",
        };
        Some(format!(
            "Headless Godot saves without uids and writes unset shader uniforms as null, so the server repaired {paths}: resource_repairs lists the uids it wrote back and the null shader_parameter lines it removed.{editor}"
        ))
    }
}

fn script_run_result(
    report: &ScriptRun,
    script_res: &str,
    timeout_seconds: u64,
    damage: &[ResourceDamage],
    repair: &RepairReport,
) -> ToolResult {
    let exit = report
        .exit_code
        .map(|code| code.to_string())
        .unwrap_or_else(|| "none".into());
    let load_error = script_load_failure(&report.stderr, script_res)
        .or_else(|| script_load_failure(&report.stdout, script_res));
    let mut script_errors = report.script_errors.clone();
    if let Some(message) = load_error.as_ref().filter(|_| script_errors.is_empty()) {
        script_errors.push(ScriptErrorLoc {
            message: message.clone(),
            file: script_res.to_string(),
            line: 0,
        });
    }
    let summary = if report.stopped_idle {
        let detail = script_errors.first().map_or_else(String::new, |error| {
            format!(" at {}:{}: {}", error.file, error.line, error.message)
        });
        format!(
            "Stopped after a SCRIPT ERROR while the main loop was idle{detail}. The process was still running, so it was stopped without waiting out the {timeout_seconds}s timeout."
        )
    } else if report.timed_out {
        format!(
            "Timed out after {timeout_seconds}s and killed the process tree.\nLast output:\n{}",
            last_output_lines(&report.stdout, &report.stderr, 20)
        )
    } else if let Some(message) = &load_error {
        let detail = script_errors
            .first()
            .filter(|error| error.line > 0)
            .map_or_else(
                || message.clone(),
                |error| format!("{} at {}:{}", error.message, error.file, error.line),
            );
        format!("The script did not run: Godot could not load it. {detail} (exit code {exit}).")
    } else if let Some(error) = script_errors.first() {
        format!(
            "Script error at {}:{}: {} (exit code {exit}).",
            error.file, error.line, error.message
        )
    } else {
        format!("Script finished with exit code {exit}.")
    };
    let (resource_damage, warning) = damage_report(damage);
    let summary = match repair.sentence() {
        Some(sentence) => format!("{summary} {sentence}"),
        None => summary,
    };
    let summary = match &warning {
        Some(warning) => format!("{summary} {warning}"),
        None => summary,
    };
    let body = json!({
        "summary": summary,
        "exit_code": report.exit_code,
        "timed_out": report.timed_out,
        "stopped_idle": report.stopped_idle,
        "load_error": load_error,
        "stdout": report.stdout,
        "stderr": report.stderr,
        "script_errors": script_errors.iter().map(|error| json!({
            "message": error.message,
            "file": error.file,
            "line": error.line,
        })).collect::<Vec<_>>(),
        "resource_repairs": repair.entries(),
        "editor_refresh": repair.editor,
        "editor_refresh_error": repair.editor_error,
        "resource_damage": resource_damage,
    });
    let mut text = serde_json::to_string_pretty(&body).unwrap_or(summary);
    if let Some(warning) = warning {
        text = format!("{warning}\n{text}");
    }
    if report.timed_out {
        ToolResult::error(
            text,
            &[
                "The process tree was killed",
                "Make the script call quit(), or raise timeout_seconds up to 600",
            ],
        )
    } else if load_error.is_some() {
        ToolResult::error(
            text,
            &[
                "The script must parse and extend SceneTree or MainLoop: start it with `extends SceneTree`, do the work in _initialize() and end with quit()",
                "Check the script with godot_validate_script, then run it again",
            ],
        )
    } else if report.exit_code != Some(0) || !script_errors.is_empty() {
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

/// The outcome of godot_validate_script's check.
pub(super) enum ScriptCheck {
    /// The script was checked (or refused before a check); an error result means it failed.
    Checked(ToolResult),
    /// The engine could not be run, so nothing is known about the script.
    NoEngine(ToolResult),
}

impl From<ToolResult> for ScriptCheck {
    fn from(result: ToolResult) -> Self {
        Self::Checked(result)
    }
}

impl ToolManager {
    pub(super) async fn handle_validate_script(&self, args: Value) -> ToolResult {
        match self.check_script(&args).await {
            ScriptCheck::Checked(result) | ScriptCheck::NoEngine(result) => result,
        }
    }

    /// godot_validate_script's check, telling a script that fails apart from an engine that
    /// could not be run to check it.
    pub(super) async fn check_script(&self, args: &Value) -> ScriptCheck {
        let Some(script_path) = str_arg(args, "script_path") else {
            return missing_argument("script_path").into();
        };
        if !script_path.ends_with(".gd") {
            return ToolResult::error(
                format!("Only .gd scripts can be validated: {script_path}"),
                &["Use a path such as res://scripts/player.gd"],
            )
            .into();
        }

        let (project_dir, from_editor) =
            if str_arg(args, "project_path").is_some() || script_path.starts_with("res://") {
                match self.resolve_project(args).await {
                    Ok(project) => (Some(project.dir), project.from_editor),
                    Err(result) => return result.into(),
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
                    )
                    .into();
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
            )
            .into();
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
                Ok(Some(mut value)) => {
                    let mut unregistered = code::unregistered_classes(dir, &file, &value);
                    // A script written outside the editor is unknown to its class list until
                    // the editor rescans; registering the declaring scripts and asking again
                    // tells a stale class list apart from a real error.
                    if !unregistered.is_empty()
                        && external_ok
                        && self.register_scripts(&unregistered).await
                    {
                        if let Ok(Some(again)) = self
                            .lsp
                            .diagnostics_if_reachable(dir, external_ok, &file)
                            .await
                        {
                            value = again;
                            unregistered = code::unregistered_classes(dir, &file, &value);
                        }
                    }
                    let mut report = code::report_diagnostics(script_path, &value);
                    if code::addon_warnings_hidden(dir, &file) {
                        report.text = format!("{}\n{}", report.text, code::ADDON_WARNINGS_HIDDEN);
                    }
                    if report.failed && !unregistered.is_empty() {
                        let scripts: Vec<&str> = unregistered
                            .iter()
                            .map(|class| class.script.as_str())
                            .collect();
                        let refresh = format!(
                            "Register the declaring script with godot_resave_resources (paths: {}), or focus the editor so it rescans, then validate again",
                            scripts.join(", ")
                        );
                        return ScriptCheck::Checked(ToolResult::error(
                            format!(
                                "{}\n{}",
                                report.text,
                                code::unregistered_classes_note(&unregistered)
                            ),
                            &[
                                refresh.as_str(),
                                "godot_run_script compiles scripts with the project's own class list, without the language server",
                                "Fix the other error lines. A warning is listed and does not by itself fail this check",
                            ],
                        ));
                    }
                    return ScriptCheck::Checked(if report.failed {
                        ToolResult::error(
                            report.text,
                            &[
                                "Fix the error lines. A warning is listed and does not by itself fail this check",
                                "godot_code_diagnostics returns the same diagnostics",
                            ],
                        )
                    } else {
                        ToolResult::text(report.text)
                    });
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
            Ok(msg) => ScriptCheck::Checked(match lsp_problem {
                Some(error) => ToolResult::text(format!(
                    "No parse errors (headless check). Analyzer warnings were not checked: the language server was reached but did not answer ({error}); retry, or use godot_code_diagnostics."
                )),
                None => ToolResult::text(msg),
            }),
            // The engine could not be run at all: that is a path problem, not a script problem.
            Err(e @ McpError::Process(_)) => ScriptCheck::NoEngine(ToolResult::error(
                format!("Script validation failed: {e}"),
                &[
                    "Verify GODOT_PATH points to a working Godot executable",
                    "Run the executable with --version to see whether it starts",
                ],
            )),
            Err(e) => ScriptCheck::Checked(ToolResult::error(
                format!("Script validation failed: {e}"),
                &[
                    "Check GDScript syntax line numbers",
                    "Ensure base class and node types exist in the engine",
                ],
            )),
        }
    }

    /// Asks the connected editor to register the scripts that declare `classes`, waiting for its
    /// file system. False when no editor answered, so nothing changed.
    async fn register_scripts(&self, classes: &[code::UnregisteredClass]) -> bool {
        let paths: Vec<&str> = classes.iter().map(|class| class.script.as_str()).collect();
        self.bridge
            .clone()
            .with_timeout(Self::RESAVE_BRIDGE_WAIT)
            .send_command("resave_resources", json!({"paths": paths, "wait": true}))
            .await
            .is_ok()
    }

    /// The editor answers a scan-and-wait within 60s, which is the addon's limit.
    /// This waits a little longer so the addon's own timeout is the one the caller sees.
    const RESAVE_BRIDGE_WAIT: std::time::Duration = std::time::Duration::from_secs(90);

    pub(super) async fn handle_resave_resources(&self, args: Value) -> ToolResult {
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

    /// Repairs the text scenes and resources the script rewrote, then has the editor refresh
    /// them when it holds the same project, so its file system and uid cache follow the files.
    async fn repair_resources(
        &self,
        project: &Path,
        snapshot: &TextResourceSnapshot,
    ) -> RepairReport {
        let repairs = snapshot.repair_since(project);
        let mut report = RepairReport {
            repairs,
            editor: None,
            editor_error: None,
        };
        if report.repairs.is_empty() {
            return report;
        }
        let holds_project = match self
            .bridge
            .send_command("get_project_info", json!({}))
            .await
        {
            Ok(info) => info
                .get("project_dir")
                .and_then(Value::as_str)
                .is_some_and(|dir| same_directory(project, Path::new(dir))),
            Err(_) => false,
        };
        if !holds_project {
            return report;
        }
        let paths = report
            .repairs
            .iter()
            .map(|item| item.path.clone())
            .collect::<Vec<_>>();
        let bridge = self.bridge.clone().with_timeout(Self::RESAVE_BRIDGE_WAIT);
        match bridge
            .send_command("resave_resources", json!({"paths": paths}))
            .await
        {
            Ok(answer) => report.editor = Some(answer),
            Err(error) => report.editor_error = Some(error.to_string()),
        }
        report
    }

    pub(super) async fn handle_run_script(&self, args: Value) -> ToolResult {
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
        let script_res = res_display(&project.dir, &file);
        if let Some(problem) = main_loop_problem(&project.dir, &file) {
            return ToolResult::error(
                problem,
                &[
                    "Start the script with `extends SceneTree`, do the work in _initialize() and end with quit()",
                    "To change a scene or resource with the editor open, use the editor tools instead of a script",
                ],
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
        let snapshot = TextResourceSnapshot::take(&project.dir);
        match self
            .cli
            .run_script(&project.dir, &file, &user_args, timeout_seconds)
            .await
        {
            Ok(report) => {
                let repair = self.repair_resources(&project.dir, &snapshot).await;
                let damage = snapshot.damage_since(&project.dir);
                script_run_result(&report, &script_res, timeout_seconds, &damage, &repair)
            }
            Err(error) => ToolResult::error(
                format!("Failed to run the script: {error}"),
                &["Verify GODOT_PATH points to a working Godot executable"],
            ),
        }
    }
}
