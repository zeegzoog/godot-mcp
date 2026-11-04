//! Tools that run the Godot binary without the editor: script validation, one-off scripts,
//! resource resaving and the headless import.

use super::exit_leaks::{EXIT_LEAK_HINT, collapse_exit_leaks};
use super::main_loop::main_loop_problem;
use super::run::StaleImports;
use super::{ToolManager, bridge_error_result, code, missing_argument, str_arg, user_args_of};
use crate::cli_runner::{ScriptErrorLoc, ScriptRun, script_load_failure};
use crate::error::McpError;
use crate::godot::{
    IMPORTED_EXTENSIONS, ResourceDamage, ResourceRepair, TextResourceSnapshot, changed_imports,
    force_reimport, import_fingerprints, import_kind, stale_reason,
};
use crate::paths::{res_display, resolve_in_project, same_directory};
use crate::protocol::ToolResult;
use serde_json::{Value, json};
use std::collections::BTreeMap;
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
    let paths = path_examples(damage.iter().map(|item| item.path.as_str()));
    let warning = format!(
        "WARNING: the script rewrote {paths} without the editor, and the server could not repair them: resource_damage lists the uid:// references still missing and the shader parameters still written as null. Restore the files from version control, or put the listed uid=\"...\" back (for a .scn or .res, with ResourceSaver.set_uid in a godot_run_script) and delete the null shader_parameter lines; make sure the files are writable."
    );
    (entries, Some(warning))
}

/// What the run's answer says about the files the server repaired after the script.
struct RepairReport {
    repairs: Vec<ResourceRepair>,
    /// Every scene, resource, script and shader the script wrote, repaired or not.
    written: Vec<String>,
    /// The connected editor's answer to refreshing the written files, when it holds the project.
    editor: Option<Value>,
    /// Why the connected editor could not refresh them.
    editor_error: Option<String>,
    /// Why Godot could not write uids back into binary scenes and resources.
    binary_error: Option<String>,
}

impl RepairReport {
    /// The first `REPAIR_LIST_LIMIT` repairs, each with counts instead of the restored references:
    /// a script that saves a whole kit repairs hundreds of files, and the answer must stay readable.
    fn entries(&self) -> Vec<Value> {
        self.repairs
            .iter()
            .take(REPAIR_LIST_LIMIT)
            .map(|item| {
                json!({
                    "path": item.path,
                    "uid": item.uid,
                    "new_uid": item.new_uid,
                    "ext_resource_uids_restored": item.ext_resource_uids.len(),
                    "removed_null_shader_parameters": item.removed_null_shader_parameters,
                })
            })
            .collect()
    }

    /// The editor's refresh answer with each list cut to `REPAIR_LIST_LIMIT` paths and
    /// `<list>_total` beside a cut one.
    fn editor_refresh(&self) -> Option<Value> {
        let mut answer = self.editor.clone()?;
        if let Some(fields) = answer.as_object_mut() {
            let mut totals = Vec::new();
            for (key, value) in fields.iter_mut() {
                if let Some(items) = value.as_array_mut()
                    && items.len() > REPAIR_LIST_LIMIT
                {
                    totals.push((format!("{key}_total"), items.len()));
                    items.truncate(REPAIR_LIST_LIMIT);
                }
            }
            for (key, total) in totals {
                fields.insert(key, json!(total));
            }
        }
        Some(answer)
    }

    fn sentence(&self) -> Option<String> {
        let editor = match (&self.editor, &self.editor_error) {
            (Some(_), _) => Some(
                "The connected editor refreshed the files the script wrote and reloaded the shaders, scripts and resources it had loaded (editor_refresh); reload any scene it has open.",
            ),
            (None, Some(_)) => Some(
                "The connected editor could not refresh the files the script wrote (editor_refresh_error), so it may still use their old contents; call godot_resave_resources with those paths.",
            ),
            (None, None) => None,
        };
        let repaired = (!self.repairs.is_empty()).then(|| {
            let total = self.repairs.len();
            let files = if total == 1 {
                format!("1 file, {}", self.repairs[0].path)
            } else {
                format!(
                    "{total} files, among them {}",
                    path_examples(self.repairs.iter().map(|item| item.path.as_str()))
                )
            };
            let uids = self.repairs.iter().filter(|item| item.uid.is_some()).count();
            let new_uids = self.repairs.iter().filter(|item| item.new_uid).count();
            let ext_uids: usize = self
                .repairs
                .iter()
                .map(|item| item.ext_resource_uids.len())
                .sum();
            let nulls: usize = self
                .repairs
                .iter()
                .map(|item| item.removed_null_shader_parameters.len())
                .sum();
            let mut counts = Vec::new();
            if uids > 0 {
                let new = if new_uids > 0 {
                    format!(" ({new_uids} new)")
                } else {
                    String::new()
                };
                counts.push(format!("{} written back{new}", plural(uids, "uid")));
            }
            if ext_uids > 0 {
                counts.push(format!(
                    "{} restored",
                    plural(ext_uids, "ext_resource uid")
                ));
            }
            if nulls > 0 {
                counts.push(format!(
                    "{} removed",
                    plural(nulls, "null shader parameter")
                ));
            }
            let counts = counts.join(", ");
            let listed = if total > REPAIR_LIST_LIMIT {
                format!(
                    "resource_repairs lists the first {REPAIR_LIST_LIMIT} of {total} (resource_repairs_total)"
                )
            } else {
                "resource_repairs lists them".to_string()
            };
            format!(
                "Headless Godot saves without uids and writes unset shader uniforms as null, so the server repaired {files}: {counts}; {listed}."
            )
        });
        match (repaired, editor) {
            (Some(repaired), Some(editor)) => Some(format!("{repaired} {editor}")),
            (Some(repaired), None) => Some(repaired),
            (None, Some(editor)) => Some(editor.to_string()),
            (None, None) => None,
        }
    }
}

/// How many repaired files, or stale imports, the run's answer lists one by one.
const REPAIR_LIST_LIMIT: usize = 20;
/// How many paths a summary sentence names before it counts the rest.
const EXAMPLE_PATHS: usize = 3;

/// "a, b, c and 12 more" for a summary sentence.
fn path_examples<'a>(paths: impl ExactSizeIterator<Item = &'a str>) -> String {
    let total = paths.len();
    let named = paths.take(EXAMPLE_PATHS).collect::<Vec<_>>().join(", ");
    if total > EXAMPLE_PATHS {
        format!("{named} and {} more", total - EXAMPLE_PATHS)
    } else {
        named
    }
}

fn plural(count: usize, noun: &str) -> String {
    if count == 1 {
        format!("1 {noun}")
    } else {
        format!("{count} {noun}s")
    }
}

fn script_run_result(
    report: &ScriptRun,
    script_res: &str,
    timeout_seconds: u64,
    damage: &[ResourceDamage],
    repair: &RepairReport,
    imports: Option<&StaleImports>,
) -> ToolResult {
    let exit = report
        .exit_code
        .map(|code| code.to_string())
        .unwrap_or_else(|| "none".into());
    // Godot repeats its shutdown leak reports hundreds of times, burying the errors that matter.
    let mut exit_leaks = BTreeMap::new();
    let stdout = collapse_exit_leaks(&report.stdout, &mut exit_leaks);
    let stderr = collapse_exit_leaks(&report.stderr, &mut exit_leaks);
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
            last_output_lines(&stdout, &stderr, 20)
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
    let import_sentence = imports.map(|stale| stale.sentence("script"));
    let import_warning = imports
        .filter(|stale| !stale.reimported())
        .and(import_sentence.clone());
    let summary = match &import_sentence {
        Some(sentence) => format!("{summary} {sentence}"),
        None => summary,
    };
    let body = json!({
        "summary": summary,
        "exit_code": report.exit_code,
        "timed_out": report.timed_out,
        "stopped_idle": report.stopped_idle,
        "load_error": load_error,
        "stdout": stdout,
        "stderr": stderr,
        "exit_leaks": (!exit_leaks.is_empty()).then_some(&exit_leaks),
        "hint": (!exit_leaks.is_empty()).then_some(EXIT_LEAK_HINT),
        "script_errors": script_errors.iter().map(|error| json!({
            "message": error.message,
            "file": error.file,
            "line": error.line,
        })).collect::<Vec<_>>(),
        "resource_repairs": repair.entries(),
        "resource_repairs_total": repair.repairs.len(),
        "editor_refresh": repair.editor_refresh(),
        "editor_refresh_error": repair.editor_error,
        "binary_uid_error": repair.binary_error,
        "resource_damage": resource_damage,
        "stale_imports": imports.map(|stale| json!({
            "paths": stale.paths.iter().take(REPAIR_LIST_LIMIT).collect::<Vec<_>>(),
            "total": stale.paths.len(),
            "reimported": stale.reimported(),
        })),
        "import_warning": import_warning,
    });
    let mut text = serde_json::to_string_pretty(&body).unwrap_or(summary);
    if let Some(warning) = import_warning {
        text = format!("{warning}\n{text}");
    }
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

/// Each imported `res://` path with its kind: the importer (`scene`, `texture`...) and the
/// resource class it imports as, read from the sidecar.
fn imported_with_kind(project: &Path, paths: &[String]) -> Vec<Value> {
    paths
        .iter()
        .map(|path| {
            let kind = resolve_in_project(project, path)
                .ok()
                .and_then(|file| import_kind(&file));
            let (importer, class) = kind.unwrap_or_default();
            json!({
                "path": path,
                "kind": (!importer.is_empty()).then_some(importer),
                "type": class,
            })
        })
        .collect()
}

/// The requested `res://` paths whose imported data still does not match their bytes, with why.
fn still_stale(project: &Path, paths: &[String]) -> Vec<Value> {
    paths
        .iter()
        .filter_map(|path| {
            let file = resolve_in_project(project, path).ok()?;
            let (reason, detail) = stale_reason(project, &file)?;
            Some(json!({"path": path, "reason": reason, "detail": detail}))
        })
        .collect()
}

const STILL_STALE_HINTS: &[&str] = &[
    "still_stale lists the requested paths whose .godot/imported data is older than the source, with the reason",
    "Call godot_resave_resources again with those paths and force: true, which makes Godot reimport them whatever its cache says",
];

const STILL_STALE_FORCED_HINTS: &[&str] = &[
    "still_stale lists the requested paths whose .godot/imported data is older than the source, with the reason",
    "The import was forced and still did not match: read the import errors in the output, and check that nothing rewrote the source during the import",
];

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
        if let Some(force) = args.get("force") {
            if !force.is_null() && !force.is_boolean() {
                return ToolResult::error(
                    "force must be a boolean",
                    &["Use force: true with paths to reimport them whatever Godot's cache says"],
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
                self.editor_resave_result(&args, result).await
            }
            Err(McpError::BridgeUnavailable(_)) => self.import_headless(&args).await,
            Err(error) => bridge_error_result(&error),
        }
    }

    /// The editor's answer with the kind of each reimported file under imported, and the
    /// requested files whose imported data still does not match under still_stale, when the
    /// editor's project is on this machine. The editor's reimport_files always imports, so force
    /// changes nothing there.
    async fn editor_resave_result(&self, args: &Value, mut result: Value) -> ToolResult {
        let project = self
            .resolve_project(args)
            .await
            .ok()
            .map(|project| project.dir)
            .filter(|dir| dir.is_dir());
        let mut stale = Vec::new();
        if let (Some(project), Some(answer)) = (project, result.as_object_mut()) {
            let reimported: Vec<String> = answer
                .get("reimported")
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|item| item.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            if answer.contains_key("reimported") {
                answer.insert(
                    "imported".into(),
                    Value::Array(imported_with_kind(&project, &reimported)),
                );
                stale = still_stale(&project, &reimported);
                answer.insert("still_stale".into(), Value::Array(stale.clone()));
            }
        }
        let text = serde_json::to_string_pretty(&result).unwrap_or_default();
        if stale.is_empty() {
            ToolResult::text(text)
        } else {
            ToolResult::error(text, STILL_STALE_FORCED_HINTS)
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
        let force = args.get("force").and_then(Value::as_bool) == Some(true);
        let mut forced = Vec::new();
        if force {
            for path in &wanted {
                let Ok(file) = resolve_in_project(&project, path) else {
                    continue;
                };
                match force_reimport(&project, &file) {
                    Ok(true) => forced.push(path.clone()),
                    Ok(false) => {}
                    Err(error) => {
                        return ToolResult::error(
                            format!("Could not prepare {path} for a forced reimport: {error}"),
                            &[
                                "Check that the .import file and .godot/imported are writable",
                                "Or open the project in the editor and reimport the file from the Import dock",
                            ],
                        );
                    }
                }
            }
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
        let stale = still_stale(&project, &wanted);
        let body = json!({
            "mode": "headless_import",
            "command": command,
            "imported": imported_with_kind(&project, &imported),
            "imported_count": imported.len(),
            "still_stale": stale,
            "forced": forced,
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
        if !stale.is_empty() {
            let hints = if force {
                STILL_STALE_FORCED_HINTS
            } else {
                STILL_STALE_HINTS
            };
            return ToolResult::error(text, hints);
        }
        ToolResult::text(text)
    }

    /// Repairs the text scenes and resources the script rewrote, then has the editor refresh
    /// every scene, resource, script and shader the script wrote when it holds the same project,
    /// so its file system, uid cache and loaded resources follow the files: a material that uses
    /// a rewritten shader sees its new uniforms and defaults before the next scene save.
    async fn repair_resources(
        &self,
        project: &Path,
        snapshot: &TextResourceSnapshot,
    ) -> RepairReport {
        // Binary files first, so a rewritten text scene that refers to one finds its uid again.
        let plan = snapshot.binary_uid_plan(project);
        let mut binary_error = None;
        let mut repairs = Vec::new();
        if !plan.is_empty() {
            let uids: Vec<(String, String)> = plan
                .iter()
                .map(|fix| (fix.path.clone(), fix.uid.clone()))
                .collect();
            if let Err(error) = self.cli.set_binary_uids(project, &uids).await {
                binary_error = Some(error.to_string());
            }
            repairs = snapshot.binary_repairs(project, &plan);
        }
        repairs.extend(snapshot.repair_since(project));
        repairs.sort_by(|a, b| a.path.cmp(&b.path));
        let mut report = RepairReport {
            repairs,
            written: snapshot.written_since(project),
            editor: None,
            editor_error: None,
            binary_error,
        };
        if report.written.is_empty() {
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
        let bridge = self.bridge.clone().with_timeout(Self::RESAVE_BRIDGE_WAIT);
        match bridge
            .send_command("resave_resources", json!({"paths": report.written}))
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
        // A headless script loads the .godot/imported cache as it is, so stale imports are
        // reimported (or reported) before it starts, and before the snapshot, so a reimport is
        // not taken for the script's own writes.
        let imports = self.check_imports(&project.dir).await;
        let snapshot = TextResourceSnapshot::take(&project.dir);
        match self
            .cli
            .run_script(&project.dir, &file, &user_args, timeout_seconds)
            .await
        {
            Ok(report) => {
                let repair = self.repair_resources(&project.dir, &snapshot).await;
                let damage = snapshot.damage_since(&project.dir);
                script_run_result(
                    &report,
                    &script_res,
                    timeout_seconds,
                    &damage,
                    &repair,
                    imports.as_ref(),
                )
            }
            Err(error) => ToolResult::error(
                format!("Failed to run the script: {error}"),
                &["Verify GODOT_PATH points to a working Godot executable"],
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editor_refresh_cuts_long_lists_and_names_their_totals() {
        let paths: Vec<String> = (0..30)
            .map(|index| format!("res://kit/{index}.tscn"))
            .collect();
        let report = RepairReport {
            repairs: Vec::new(),
            written: paths.clone(),
            editor: Some(json!({
                "mode": "refresh",
                "refreshed": paths,
                "not_scanned": ["res://kit/odd.bin"],
            })),
            editor_error: None,
            binary_error: None,
        };
        let answer = report.editor_refresh().expect("answer");
        assert_eq!(answer["mode"], "refresh");
        assert_eq!(answer["refreshed"].as_array().map(Vec::len), Some(20));
        assert_eq!(answer["refreshed"][0], "res://kit/0.tscn");
        assert_eq!(answer["refreshed_total"], 30);
        assert_eq!(answer["not_scanned"], json!(["res://kit/odd.bin"]));
        assert!(answer.get("not_scanned_total").is_none(), "{answer}");
    }

    #[test]
    fn path_examples_names_three_and_counts_the_rest() {
        assert_eq!(path_examples(["res://a"].into_iter()), "res://a");
        assert_eq!(
            path_examples(["res://a", "res://b", "res://c", "res://d", "res://e"].into_iter()),
            "res://a, res://b, res://c and 2 more"
        );
    }
}
