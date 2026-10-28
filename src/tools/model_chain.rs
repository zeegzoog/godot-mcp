//! Export a `.vox` through Froxel, import the `.glb` into a Godot project, and
//! report whether Godot wrote an `.import` sidecar whose cache matches the new
//! file. The picture, when asked
//! for, is Froxel's path-traced PNG. A Godot window capture stays on
//! `godot_capture_viewport` or `godot_game_screenshot`.
//!
//! Froxel is a separate program named by `FROXEL_BIN`, or found as `froxel` on `PATH`; both are
//! trusted like `GODOT_PATH`. It is started as `<bin> mcp` and spoken to over stdio, with the
//! bridge token removed from its environment.

use super::{ToolManager, tool};
use crate::godot::import_current;
use crate::paths::{res_display, resolve_in_project};
use crate::protocol::{Tool, ToolContent, ToolResult};
use serde_json::{Value, json};
use std::collections::HashSet;
use std::collections::hash_map::DefaultHasher;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::time::Instant;

const EXPORT_WAIT: Duration = Duration::from_secs(60);
/// The longest line Froxel may send. A reply to 16 models is a few kilobytes.
const MAX_LINE_BYTES: usize = 1024 * 1024;
/// How much of Froxel's text an error message repeats.
const MAX_ECHO_CHARS: usize = 2000;
const MAX_MODELS: usize = 16;
/// Lines that are not JSON-RPC and are skipped while waiting for a reply.
const MAX_SKIPPED_LINES: usize = 64;

pub fn definition() -> Tool {
    tool(
        "godot_check_voxel_model",
        "Exports a .vox through `froxel mcp` (FROXEL_BIN, or `froxel` on PATH; both are trusted programs), writes the .glb inside the Godot project, imports it, and checks the result. The Froxel reply's structure lists nodes, meshes, skins, animations, and warnings (an empty mesh, a skin with no joints, a missing or repeated node name, an animation with no channels). The import goes through the connected editor when it has this project open, and is a headless import otherwise. The Godot check is the .import sidecar: without it the model would not load (missing_imports), and a changed .glb whose cache is older than the file was not imported again (stale_imports). A model Froxel reports as exported but left no .glb is listed under missing_exports. A failed export is reported under export and is not imported. preview true also asks Froxel to path-trace a PNG named <model>.preview.png next to the .glb (128², 4 samples). This does not open a Godot scene and does not take a Godot screenshot; use godot_capture_viewport or godot_game_screenshot for that. The .vox is read only and may be anywhere on this machine (a network path is refused); the .glb and the preview must be inside the project. 1 to 16 models can be passed instead of one vox, each with its own dest.",
        json!({
            "project_path": {"type": "string", "description": "Directory containing project.godot. Optional when the server was started in the project."},
            "vox": {"type": "string", "description": "Absolute path of one .vox file. Use models instead to export several."},
            "dest": {"type": "string", "description": "res:// path of the .glb to write, inside the project. Required with vox."},
            "models": {
                "type": "array",
                "maxItems": MAX_MODELS,
                "description": "1 to 16 {vox, dest} pairs. dest is a res:// .glb path.",
                "items": {
                    "type": "object",
                    "properties": {
                        "vox": {"type": "string"},
                        "dest": {"type": "string"}
                    },
                    "required": ["vox", "dest"]
                }
            },
            "preview": {"type": "boolean", "description": "Path-trace each model to <model>.preview.png beside its .glb. Default false."}
        }),
        &[],
    )
}

fn froxel_bin() -> OsString {
    std::env::var_os("FROXEL_BIN").unwrap_or_else(|| "froxel".into())
}

pub async fn call(tools: &ToolManager, args: &Value) -> ToolResult {
    call_with(tools, args, &froxel_bin()).await
}

async fn call_with(tools: &ToolManager, args: &Value, bin: &OsStr) -> ToolResult {
    let project = match tools.host_project(args) {
        Ok(project) => project,
        Err(result) => return result,
    };
    let preview = match args.get("preview") {
        None => false,
        Some(Value::Bool(value)) => *value,
        Some(_) => {
            return ToolResult::error(
                "preview must be a boolean.",
                &["Omit preview, or pass true to path-trace a PNG beside each .glb"],
            );
        }
    };
    // The checks look at the disk (links, existing files), so they run off the async workers.
    let checked = {
        let (args, project) = (args.clone(), project.clone());
        tokio::task::spawn_blocking(move || jobs_of(&args, &project)).await
    };
    let Ok(checked) = checked else {
        return ToolResult::error(
            "The path checks did not finish.",
            &["Retry the call; if it fails again, report it with godot_note_feedback"],
        );
    };
    let jobs = match checked {
        Ok(jobs) => jobs,
        Err(message) => {
            return ToolResult::error(
                message,
                &[
                    "Pass vox (an absolute .vox path) and dest (a res:// .glb path inside the project)",
                    "Or pass models: [{vox, dest}], 1 to 16",
                ],
            );
        }
    };
    // An identical re-export is not imported again (Godot compares the source hash), so its
    // older cache still matches; a changed .glb must get a cache newer than itself.
    let mut previous = Vec::with_capacity(jobs.len());
    for job in &jobs {
        previous.push(content_hash(&job.dest).await);
    }
    let exported = match export(bin, &jobs).await {
        Ok(exported) => exported,
        Err(result) => return result,
    };
    let (exported, flags) = exported;
    let mut missing_exports = Vec::new();
    let mut successful: Vec<(usize, &Job)> = Vec::new();
    for (index, job) in jobs.iter().enumerate() {
        if !flags[index] {
            continue;
        }
        if tokio::fs::metadata(&job.dest)
            .await
            .is_ok_and(|meta| meta.is_file())
        {
            successful.push((index, job));
        } else {
            missing_exports.push(res_display(&project, &job.dest));
        }
    }
    let mut import_ok = true;
    let import_body = if successful.is_empty() {
        json!({"skipped": true, "reason": "No models were exported successfully."})
    } else {
        // The resave path imports through a connected editor of this project, and runs a
        // headless import only when no such editor answers.
        let imported = tools
            .handle_resave_resources(json!({
                "project_path": project,
                "paths": successful
                    .iter()
                    .map(|(_, job)| res_display(&project, &job.dest))
                    .collect::<Vec<_>>(),
                "wait": true,
            }))
            .await;
        import_ok = !imported.is_error;
        text_json(&imported)
    };
    let previews = if preview {
        render_previews(bin, &successful).await
    } else {
        Vec::new()
    };
    let mut missing = Vec::new();
    let mut stale = Vec::new();
    let mut unchecked = Vec::new();
    for (index, job) in &successful {
        let dest = res_display(&project, &job.dest);
        let current = {
            let (project, dest) = (project.clone(), job.dest.clone());
            tokio::task::spawn_blocking(move || import_current(&project, &dest)).await
        };
        match current {
            Err(_) => unchecked.push(dest),
            Ok(None) => missing.push(format!("{dest}.import")),
            Ok(Some(false)) if content_hash(&job.dest).await != previous[*index] => {
                stale.push(dest);
            }
            Ok(Some(_)) => {}
        }
    }
    let structure_ok = structure_passed(&exported, &flags);
    let previews_ok = previews
        .iter()
        .all(|preview| preview.get("ok").and_then(Value::as_bool) != Some(false));
    let ok = structure_ok
        && import_ok
        && missing.is_empty()
        && stale.is_empty()
        && missing_exports.is_empty()
        && unchecked.is_empty()
        && previews_ok;
    let body = json!({
        "ok": ok,
        "export": exported,
        "import": import_body,
        "missing_exports": missing_exports,
        "missing_imports": missing,
        "stale_imports": stale,
        "unchecked_imports": unchecked,
        "previews": previews,
        "screenshot": "This check does not capture a Godot window. preview path-traces the voxel model. godot_capture_viewport and godot_game_screenshot capture a window.",
    });
    let text = serde_json::to_string_pretty(&body).unwrap_or_else(|_| body.to_string());
    ToolResult {
        content: vec![ToolContent::Text { text }],
        is_error: !ok,
    }
}

/// Creates the destination folders, asks Froxel to export, and checks that its reply names every
/// model with a boolean `ok`: a reply that omits a model must not look like a pass.
async fn export(bin: &OsStr, jobs: &[Job]) -> Result<(Value, Vec<bool>), ToolResult> {
    let mut models = Vec::new();
    for job in jobs {
        if let Some(parent) = job.dest.parent() {
            if let Err(error) = tokio::fs::create_dir_all(parent).await {
                return Err(ToolResult::error(
                    format!("Could not create {}: {error}", parent.display()),
                    &["Check that the project directory is writable"],
                ));
            }
        }
        models.push(json!({
            "source": job.vox,
            "path": job.dest,
        }));
    }
    let exported = froxel_export(bin, &models)
        .await
        .map_err(|error| ToolResult::error(error.to_string(), export_hints(&error)))?;
    let flags = export_results(&exported, jobs.len()).map_err(|message| {
        ToolResult::error(
            message,
            &[
                "Update Froxel to a build whose export_models reports every model with ok",
                "Run `froxel mcp` by hand and call export_models to see its reply",
            ],
        )
    })?;
    Ok((exported, flags))
}

/// What went wrong while talking to Froxel; the kind picks the recovery steps.
#[derive(Debug)]
enum FroxelError {
    /// The program could not be started.
    Start(String),
    /// The exchange, including the exit, did not finish in time.
    Deadline(String),
    /// Anything else: a bad reply, an error from Froxel, a non-zero exit.
    Failed(String),
}

impl fmt::Display for FroxelError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (Self::Start(text) | Self::Deadline(text) | Self::Failed(text)) = self;
        formatter.write_str(text)
    }
}

impl From<String> for FroxelError {
    fn from(text: String) -> Self {
        Self::Failed(text)
    }
}

impl From<&str> for FroxelError {
    fn from(text: &str) -> Self {
        Self::Failed(text.to_string())
    }
}

/// The recovery steps that fit what went wrong with Froxel.
fn export_hints(error: &FroxelError) -> &'static [&'static str] {
    match error {
        FroxelError::Start(_) => &[
            "Set FROXEL_BIN to the Froxel executable, or put `froxel` on PATH",
            "Froxel needs a build that supports the export_models tool",
        ],
        FroxelError::Deadline(_) => &[
            "Export fewer models in one call",
            "Run `froxel mcp` by hand to see whether it answers",
        ],
        FroxelError::Failed(_) => &[
            "Run `froxel mcp` by hand and call export_models to see its reply",
            "Check that the .vox files are valid and that Froxel supports export_models",
        ],
    }
}

/// Froxel called the export a success and every model in it.
fn structure_passed(exported: &Value, flags: &[bool]) -> bool {
    exported.get("ok").and_then(Value::as_bool) == Some(true) && flags.iter().all(|ok| *ok)
}

/// One boolean per requested model, from Froxel's `models` list.
fn export_results(exported: &Value, expected: usize) -> Result<Vec<bool>, String> {
    let models = exported
        .get("models")
        .and_then(Value::as_array)
        .ok_or("Froxel's reply has no models list.")?;
    if models.len() != expected {
        return Err(format!(
            "Froxel reported {} models for {expected} requested.",
            models.len()
        ));
    }
    models
        .iter()
        .enumerate()
        .map(|(index, model)| {
            model
                .get("ok")
                .and_then(Value::as_bool)
                .ok_or_else(|| format!("Froxel's models[{index}] has no boolean ok."))
        })
        .collect()
}

/// Path-traces each exported model. All previews share one deadline.
async fn render_previews(bin: &OsStr, successful: &[(usize, &Job)]) -> Vec<Value> {
    let deadline = Instant::now() + EXPORT_WAIT;
    let mut previews = Vec::new();
    for (_, job) in successful {
        let Some(png) = &job.preview else {
            continue;
        };
        let left = deadline.saturating_duration_since(Instant::now());
        let outcome = if left.is_zero() {
            Err("The previews used up their 60-second deadline.".to_string())
        } else {
            froxel_render(bin, &job.vox, png, left).await
        };
        previews.push(match outcome {
            Ok(()) => json!({"vox": job.vox, "png": png, "ok": true}),
            Err(error) => json!({"vox": job.vox, "png": png, "ok": false, "error": error}),
        });
    }
    previews
}

async fn content_hash(path: &Path) -> Option<u64> {
    let bytes = tokio::fs::read(path).await.ok()?;
    let mut hasher = DefaultHasher::new();
    bytes.hash(&mut hasher);
    Some(hasher.finish())
}

struct Job {
    vox: PathBuf,
    dest: PathBuf,
    preview: Option<PathBuf>,
}

fn jobs_of(args: &Value, project: &Path) -> Result<Vec<Job>, String> {
    let preview = args
        .get("preview")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let jobs = if let Some(models) = args.get("models") {
        let models = models
            .as_array()
            .ok_or("models must be an array of {vox, dest}.")?;
        if models.is_empty() || models.len() > MAX_MODELS {
            return Err(format!(
                "models must contain 1 to {MAX_MODELS} {{vox, dest}} pairs."
            ));
        }
        if args.get("vox").is_some() || args.get("dest").is_some() {
            return Err("pass either vox and dest, or models, not both.".into());
        }
        models
            .iter()
            .enumerate()
            .map(|(index, model)| {
                one_job(model.get("vox"), model.get("dest"), project, index, preview)
            })
            .collect::<Result<Vec<_>, _>>()?
    } else {
        vec![one_job(
            args.get("vox"),
            args.get("dest"),
            project,
            0,
            preview,
        )?]
    };
    let mut destinations = HashSet::new();
    let mut existing: Vec<&Path> = Vec::new();
    for job in &jobs {
        for path in std::iter::once(&job.dest).chain(job.preview.iter()) {
            let key = destination_key(path)?;
            let mut aliases_existing = false;
            if path.is_file() {
                for previous in &existing {
                    aliases_existing |= same_file::is_same_file(path, previous)
                        .map_err(|error| format!("Cannot compare output files: {error}"))?;
                }
                existing.push(path);
            }
            if !destinations.insert(key) || aliases_existing {
                return Err(format!(
                    "Duplicate output destination: {}. Use distinct names, including previews and case-only aliases.",
                    path.display()
                ));
            }
        }
    }
    Ok(jobs)
}

// Resolve existing links even when the final file does not exist yet. Case folding also
// prevents batches that would overwrite each other on a case-insensitive destination disk.
fn destination_key(path: &Path) -> Result<String, String> {
    let ancestor = path
        .ancestors()
        .find(|part| std::fs::symlink_metadata(part).is_ok())
        .ok_or("No existing output ancestor.")?;
    let mut resolved = ancestor
        .canonicalize()
        .map_err(|error| format!("Cannot resolve output {}: {error}", path.display()))?;
    resolved.push(
        path.strip_prefix(ancestor)
            .map_err(|error| error.to_string())?,
    );
    Ok(resolved.to_string_lossy().replace('\\', "/").to_lowercase())
}

/// Whether `path` runs through `.godot`, Godot's own cache. Windows and macOS ignore the case of
/// a name, and Windows ignores trailing dots and spaces.
fn inside_godot_cache(project: &Path, path: &Path) -> bool {
    let relative = path.strip_prefix(project).unwrap_or(path);
    relative.components().any(|part| {
        part.as_os_str()
            .to_string_lossy()
            .trim_end_matches(['.', ' '])
            .eq_ignore_ascii_case(".godot")
    })
}

/// A network share or a device path that is not a drive: looking at it would reach out to
/// another machine or bypass the path rules. Only Windows has such paths, in several spellings
/// (two leading separators in any mix, or a prefix that is not a drive); `//usr/x.vox` is an
/// ordinary path elsewhere.
#[cfg(windows)]
fn is_remote_or_device_path(path: &str) -> bool {
    use std::path::{Component, Prefix};
    let mut leading = path.chars();
    let separator = |c: Option<char>| matches!(c, Some('/') | Some('\\'));
    if separator(leading.next()) && separator(leading.next()) {
        return true;
    }
    matches!(
        Path::new(path).components().next(),
        Some(Component::Prefix(prefix))
            if !matches!(prefix.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_))
    )
}

#[cfg(not(windows))]
fn is_remote_or_device_path(_path: &str) -> bool {
    false
}

fn one_job(
    vox: Option<&Value>,
    dest: Option<&Value>,
    project: &Path,
    index: usize,
    preview: bool,
) -> Result<Job, String> {
    let vox = vox
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .ok_or_else(|| format!("models[{index}].vox must be a .vox path."))?;
    let dest = dest
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .ok_or_else(|| format!("models[{index}].dest must be a res:// .glb path."))?;
    if !vox.ends_with(".vox") {
        return Err(format!("models[{index}].vox must end in .vox."));
    }
    if !dest.ends_with(".glb") {
        return Err(format!("models[{index}].dest must end in .glb."));
    }
    if is_remote_or_device_path(vox) {
        return Err(format!(
            "models[{index}].vox must be a path on this machine, not a network share or a device path."
        ));
    }
    let vox_path = PathBuf::from(vox);
    if !vox_path.is_absolute() {
        return Err(format!("models[{index}].vox must be an absolute path."));
    }
    if !vox_path.is_file() {
        return Err(format!("models[{index}].vox was not found at {vox}."));
    }
    let dest_path = resolve_in_project(project, dest)
        .map_err(|error| format!("models[{index}].dest: {error}"))?;
    if inside_godot_cache(project, &dest_path) {
        return Err(format!(
            "models[{index}].dest is inside .godot, which is Godot's own cache."
        ));
    }
    // A name of its own, so a texture next to the model is never overwritten.
    let preview_path = if preview {
        let png = dest_path.with_extension("preview.png");
        Some(
            resolve_in_project(project, &png.to_string_lossy())
                .map_err(|error| format!("models[{index}].preview: {error}"))?,
        )
    } else {
        None
    };
    Ok(Job {
        vox: vox_path,
        dest: dest_path,
        preview: preview_path,
    })
}

async fn froxel_export(bin: &OsStr, models: &[Value]) -> Result<Value, FroxelError> {
    let reply = froxel_call(bin, "export_models", json!({"models": models}), EXPORT_WAIT).await?;
    serde_json::from_str(&reply).map_err(|error| {
        FroxelError::Failed(format!(
            "Froxel export was not JSON: {error}: {}",
            clip(&reply, MAX_ECHO_CHARS)
        ))
    })
}

async fn froxel_render(bin: &OsStr, vox: &Path, png: &Path, wait: Duration) -> Result<(), String> {
    // A picture left by an earlier run must not pass for this one.
    match tokio::fs::remove_file(png).await {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("Cannot replace {}: {error}", png.display())),
    }
    froxel_script_with(
        bin,
        &[
            ("open_file", json!({"path": vox})),
            (
                "render",
                json!({
                    "path": png,
                    "width": 128,
                    "height": 128,
                    "samples": 4,
                    "bounces": 1,
                    "denoise": false
                }),
            ),
        ],
        wait,
    )
    .await
    .map_err(|error| error.to_string())?;
    if tokio::fs::metadata(png)
        .await
        .is_ok_and(|meta| meta.is_file())
    {
        Ok(())
    } else {
        Err(format!("Froxel did not write {}", png.display()))
    }
}

async fn froxel_call(
    bin: &OsStr,
    name: &str,
    arguments: Value,
    wait: Duration,
) -> Result<String, FroxelError> {
    froxel_script_with(bin, &[(name, arguments)], wait)
        .await
        .map(|replies| replies.into_iter().next().unwrap_or_default())
}

/// `<bin> mcp` with pipes for stdin and stdout, no stderr, and without the bridge token.
fn froxel_command(bin: &OsStr) -> Command {
    let mut command = Command::new(bin);
    command
        .arg("mcp")
        .env_remove("GODOT_MCP_TOKEN")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    command
}

async fn froxel_script_with(
    bin: &OsStr,
    calls: &[(&str, Value)],
    wait: Duration,
) -> Result<Vec<String>, FroxelError> {
    let operation = async {
        let mut child = froxel_command(bin).spawn().map_err(|error| {
            FroxelError::Start(format!(
                "Could not start {} mcp: {error}",
                bin.to_string_lossy()
            ))
        })?;
        let mut stdin = child.stdin.take().ok_or("froxel mcp stdin was closed")?;
        let stdout = child.stdout.take().ok_or("froxel mcp stdout was closed")?;
        let mut lines = BufReader::new(stdout);
        write_line(&mut stdin, &json!({"jsonrpc":"2.0","id":1,"method":"initialize",
            "params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"godot-mcp","version":env!("CARGO_PKG_VERSION")}}})).await?;
        read_reply(&mut lines, 1).await?;
        write_line(
            &mut stdin,
            &json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
        )
        .await?;
        let mut replies = Vec::new();
        for (offset, (name, arguments)) in calls.iter().enumerate() {
            let id = offset as u64 + 2;
            write_line(
                &mut stdin,
                &json!({"jsonrpc":"2.0","id":id,"method":"tools/call",
                "params":{"name":name,"arguments":arguments}}),
            )
            .await?;
            let result = read_reply(&mut lines, id).await?;
            let text = result
                .get("content")
                .and_then(Value::as_array)
                .and_then(|items| {
                    items
                        .iter()
                        .find_map(|item| item.get("text").and_then(Value::as_str))
                })
                .ok_or("Froxel returned no text content")?;
            if result.get("isError").and_then(Value::as_bool) == Some(true) {
                return Err(FroxelError::Failed(clip(text, MAX_ECHO_CHARS)));
            }
            replies.push(text.to_string());
        }
        drop(stdin);
        let status = child
            .wait()
            .await
            .map_err(|error| FroxelError::Failed(format!("Waiting for Froxel: {error}")))?;
        if !status.success() {
            return Err(FroxelError::Failed(format!("Froxel exited with {status}")));
        }
        Ok(replies)
    };
    tokio::time::timeout(wait, operation).await.map_err(|_| {
        FroxelError::Deadline(format!(
            "Froxel MCP exceeded its {:.3}-second deadline, including process exit.",
            wait.as_secs_f64()
        ))
    })?
}

/// The first `limit` characters of `text`, with a mark when something was cut.
fn clip(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_string();
    }
    let mut clipped: String = text.chars().take(limit).collect();
    clipped.push_str("… (cut)");
    clipped
}

/// One line of Froxel's output, at most `MAX_LINE_BYTES`: a program that never ends its line
/// must not fill the server's memory until the deadline.
async fn read_line<R: AsyncBufRead + Unpin>(reader: &mut R) -> Result<Option<String>, String> {
    let mut line = Vec::new();
    loop {
        let available = reader
            .fill_buf()
            .await
            .map_err(|error| format!("Reading Froxel: {error}"))?;
        if available.is_empty() {
            if line.is_empty() {
                return Ok(None);
            }
            break;
        }
        match available.iter().position(|byte| *byte == b'\n') {
            Some(end) => {
                line.extend_from_slice(&available[..end]);
                reader.consume(end + 1);
                break;
            }
            None => {
                let length = available.len();
                line.extend_from_slice(available);
                reader.consume(length);
            }
        }
        if line.len() > MAX_LINE_BYTES {
            return Err(format!(
                "Froxel sent a line longer than {MAX_LINE_BYTES} bytes."
            ));
        }
    }
    if line.len() > MAX_LINE_BYTES {
        return Err(format!(
            "Froxel sent a line longer than {MAX_LINE_BYTES} bytes."
        ));
    }
    String::from_utf8(line)
        .map(Some)
        .map_err(|_| "Froxel sent output that is not UTF-8.".to_string())
}

async fn read_reply<R: AsyncBufRead + Unpin>(reader: &mut R, id: u64) -> Result<Value, String> {
    let mut skipped = 0_usize;
    let mut last_skipped = String::new();
    while let Some(line) = read_line(reader).await? {
        // A line that is not a JSON message (an empty line, a stray log line) is skipped, but only
        // a few of them: a program that prints text and waits is not Froxel.
        let Ok(reply) = serde_json::from_str::<Value>(&line) else {
            if !line.trim().is_empty() {
                skipped += 1;
                last_skipped = line;
            }
            if skipped > MAX_SKIPPED_LINES {
                return Err(format!(
                    "Froxel sent {skipped} lines that are not JSON-RPC, the last one: {}",
                    clip(&last_skipped, 200)
                ));
            }
            continue;
        };
        if reply.get("id").and_then(Value::as_u64) != Some(id) {
            continue;
        }
        if let Some(error) = reply.get("error") {
            return Err(clip(
                &format!("Froxel JSON-RPC error: {error}"),
                MAX_ECHO_CHARS,
            ));
        }
        return reply
            .get("result")
            .cloned()
            .ok_or("Froxel reply has no result".into());
    }
    Err(format!("Froxel closed before replying to request {id}"))
}

async fn write_line(stdin: &mut tokio::process::ChildStdin, value: &Value) -> Result<(), String> {
    let mut line = serde_json::to_string(value).map_err(|error| error.to_string())?;
    line.push('\n');
    stdin
        .write_all(line.as_bytes())
        .await
        .map_err(|error| format!("writing to froxel mcp: {error}"))
}

fn text_json(result: &ToolResult) -> Value {
    let text = result
        .content
        .iter()
        .find_map(|item| match item {
            ToolContent::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .unwrap_or("");
    serde_json::from_str(text).unwrap_or_else(|_| json!({"text": text}))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn project() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("source.vox"), b"fixture").unwrap();
        dir
    }
    #[test]
    fn duplicate_destinations_are_rejected_before_writes() {
        let dir = project();
        let source = dir.path().join("source.vox");
        let jobs = jobs_of(
            &json!({"models":[
                {"vox":source,"dest":"res://model.glb"},
                {"vox":source,"dest":"res://./MODEL.glb"}
            ]}),
            dir.path(),
        );
        assert!(jobs.err().unwrap().contains("Duplicate"));
        assert!(!dir.path().join("model.glb").exists());
    }
    #[test]
    fn hard_linked_destinations_are_rejected_before_writes() {
        let dir = project();
        let first = dir.path().join("first.glb");
        let second = dir.path().join("second.glb");
        std::fs::write(&first, b"preserved").unwrap();
        std::fs::hard_link(&first, &second).unwrap();
        let jobs = jobs_of(
            &json!({"models":[
                {"vox":dir.path().join("source.vox"),"dest":"res://first.glb"},
                {"vox":dir.path().join("source.vox"),"dest":"res://second.glb"}
            ]}),
            dir.path(),
        );
        assert!(jobs.err().unwrap().contains("Duplicate"));
        assert_eq!(std::fs::read(first).unwrap(), b"preserved");
    }
    #[test]
    fn a_preview_has_a_name_of_its_own_and_leaves_a_texture_alone() {
        let dir = project();
        std::fs::write(dir.path().join("model.png"), b"texture").unwrap();
        let jobs = jobs_of(
            &json!({"vox":dir.path().join("source.vox"),"dest":"res://model.glb","preview":true}),
            dir.path(),
        )
        .unwrap();
        let preview = jobs[0].preview.as_ref().unwrap();
        assert!(preview.ends_with("model.preview.png"), "{preview:?}");
    }
    #[test]
    fn sources_and_destinations_outside_the_rules_are_refused() {
        let dir = project();
        let source = dir.path().join("source.vox");
        let refused = |args: Value| jobs_of(&args, dir.path()).err().expect("refused");
        assert!(refused(json!({"vox":"relative.vox","dest":"res://a.glb"})).contains("absolute"));
        assert!(
            refused(json!({"vox":dir.path().join("a.txt"),"dest":"res://a.glb"})).contains(".vox")
        );
        assert!(refused(json!({"vox":source,"dest":"res://a.png"})).contains(".glb"));
        assert!(refused(json!({"vox":source,"dest":"res://../a.glb"})).contains("dest"));
        assert!(
            refused(json!({"vox":source,"dest":"res://.godot/imported/a.glb"})).contains(".godot")
        );
        let both = json!({"vox":source,"models":[{"vox":source,"dest":"res://b.glb"}]});
        assert!(refused(both).contains("not both"));
        assert!(refused(json!({"models":[]})).contains("1 to 16"));
        let many: Vec<Value> = (0..=MAX_MODELS)
            .map(|n| json!({"vox":source,"dest":format!("res://m{n}.glb")}))
            .collect();
        assert!(refused(json!({"models":many})).contains("1 to 16"));
    }
    #[test]
    fn a_reply_that_leaves_a_model_out_is_not_a_pass() {
        assert!(export_results(&json!({"ok":true}), 1).is_err());
        assert!(export_results(&json!({"ok":true,"models":[]}), 1).is_err());
        assert!(export_results(&json!({"ok":true,"models":[{"ok":true}]}), 2).is_err());
        let missing_flag = export_results(&json!({"ok":true,"models":[{"name":"a"}]}), 1);
        assert!(missing_flag.unwrap_err().contains("models[0]"));
        let mixed = export_results(&json!({"models":[{"ok":true},{"ok":false}]}), 2);
        assert_eq!(mixed.unwrap(), vec![true, false]);
    }
    #[test]
    fn the_recovery_steps_fit_the_failure() {
        let start = FroxelError::Start("Could not start froxel mcp: not found".into());
        assert!(export_hints(&start)[0].contains("FROXEL_BIN"));
        let late = FroxelError::Deadline("Froxel MCP exceeded its deadline".into());
        assert!(export_hints(&late)[0].contains("fewer"));
        // Froxel's own words must not pick the steps.
        let failed = FroxelError::Failed("Could not start the deadline".into());
        assert!(export_hints(&failed)[0].contains("by hand"));
    }
    #[test]
    fn one_failed_model_fails_the_check() {
        let reply = json!({"ok":true,"models":[{"ok":true},{"ok":false}]});
        let flags = export_results(&reply, 2).unwrap();
        assert!(!structure_passed(&reply, &flags));
        let all = json!({"ok":true,"models":[{"ok":true},{"ok":true}]});
        assert!(structure_passed(&all, &export_results(&all, 2).unwrap()));
        let refused = json!({"ok":false,"models":[{"ok":true}]});
        assert!(!structure_passed(&refused, &[true]));
    }
    #[test]
    fn the_godot_cache_is_refused_whatever_the_spelling() {
        let dir = project();
        let source = dir.path().join("source.vox");
        for dest in [
            "res://.godot/a.glb",
            "res://.GODOT/imported/a.glb",
            "res://.Godot./a.glb",
        ] {
            let result = jobs_of(&json!({"vox":source,"dest":dest}), dir.path());
            let message = result.err().unwrap_or_default();
            assert!(message.contains(".godot"), "{dest}: {message:?}");
        }
        assert!(
            jobs_of(
                &json!({"vox":source,"dest":"res://godot/a.glb"}),
                dir.path()
            )
            .is_ok()
        );
    }
    #[cfg(windows)]
    #[test]
    fn network_and_device_paths_are_refused_as_a_source() {
        for vox in [
            "\\\\host\\share\\a.vox",
            "//host/share/a.vox",
            "/\\host\\share\\a.vox",
            "\\/host/share/a.vox",
            "\\\\?\\UNC\\host\\share\\a.vox",
            "\\\\.\\C:\\a.vox",
        ] {
            assert!(is_remote_or_device_path(vox), "{vox}");
        }
        assert!(!is_remote_or_device_path("C:\\models\\a.vox"));
        assert!(is_remote_or_device_path("\\\\?\\C:\\models\\a.vox"));
    }
    #[cfg(windows)]
    #[test]
    fn a_network_source_is_refused_before_anything_is_touched() {
        let dir = project();
        let args = json!({"vox":"//host/share/a.vox","dest":"res://a.glb"});
        let message = jobs_of(&args, dir.path()).err().unwrap_or_default();
        assert!(message.contains("network"), "{message}");
    }
    #[tokio::test]
    async fn text_that_is_not_json_rpc_is_skipped_only_a_few_times() {
        let noise = "banner line\n".repeat(MAX_SKIPPED_LINES + 1);
        let mut reader = BufReader::new(noise.as_bytes());
        let error = read_reply(&mut reader, 2).await.unwrap_err();
        assert!(
            error.contains("not JSON-RPC") && error.contains("banner line"),
            "{error}"
        );
    }
    #[cfg(unix)]
    #[test]
    fn a_path_with_two_leading_slashes_is_an_ordinary_path_on_unix() {
        assert!(!is_remote_or_device_path("//usr/share/a.vox"));
    }
    #[tokio::test]
    async fn a_line_is_read_up_to_the_limit_and_no_further() {
        let mut exact = vec![b'a'; MAX_LINE_BYTES];
        exact.push(b'\n');
        let mut reader = BufReader::new(&exact[..]);
        let line = read_line(&mut reader).await.unwrap().unwrap();
        assert_eq!(line.len(), MAX_LINE_BYTES);
        let long = vec![b'a'; MAX_LINE_BYTES + 1];
        let mut reader = BufReader::new(&long[..]);
        let error = read_line(&mut reader).await.unwrap_err();
        assert!(error.contains("longer than"), "{error}");
        let mut reader = BufReader::new(&b"last words"[..]);
        let line = read_line(&mut reader).await.unwrap().unwrap();
        assert_eq!(line, "last words");
        assert_eq!(read_line(&mut reader).await.unwrap(), None);
    }
    #[tokio::test]
    async fn lines_that_are_not_json_are_skipped_while_waiting_for_a_reply() {
        let input = b"\nnot json\n{\"jsonrpc\":\"2.0\",\"id\":9,\"result\":{}}\n{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"v\":1}}\n";
        let mut reader = BufReader::new(&input[..]);
        let reply = read_reply(&mut reader, 2).await.unwrap();
        assert_eq!(reply["v"], 1);
    }
    #[test]
    fn froxel_does_not_inherit_the_bridge_token() {
        let command = froxel_command(OsStr::new("froxel"));
        let token = command
            .as_std()
            .get_envs()
            .find(|(key, _)| *key == OsStr::new("GODOT_MCP_TOKEN"));
        assert_eq!(token, Some((OsStr::new("GODOT_MCP_TOKEN"), None)));
    }
    #[test]
    fn echoed_text_is_cut() {
        assert_eq!(clip("short", 10), "short");
        let cut = clip(&"x".repeat(50), 10);
        assert!(
            cut.starts_with("xxxxxxxxxx") && cut.ends_with("(cut)"),
            "{cut}"
        );
    }
    #[cfg(unix)]
    #[test]
    fn preview_paths_are_validated_before_export() {
        let dir = project();
        let outside = tempfile::tempdir().unwrap();
        let sentinel = outside.path().join("sentinel");
        std::fs::write(&sentinel, b"unchanged").unwrap();
        std::os::unix::fs::symlink(&sentinel, dir.path().join("model.preview.png")).unwrap();
        let jobs = jobs_of(
            &json!({"vox":dir.path().join("source.vox"),"dest":"res://model.glb","preview":true}),
            dir.path(),
        );
        assert!(jobs.err().unwrap().contains("outside the project"));
        assert_eq!(std::fs::read(sentinel).unwrap(), b"unchanged");
        assert!(!dir.path().join("model.glb").exists());
    }
    #[cfg(unix)]
    #[test]
    fn dangling_output_links_are_rejected_before_export() {
        for name in ["model.glb", "model.preview.png"] {
            let dir = project();
            let outside = tempfile::tempdir().unwrap();
            let target = outside.path().join("not-created");
            std::os::unix::fs::symlink(&target, dir.path().join(name)).unwrap();
            let jobs = jobs_of(
                &json!({"vox":dir.path().join("source.vox"),"dest":"res://model.glb","preview":true}),
                dir.path(),
            );
            assert!(jobs.is_err());
            assert!(!target.exists());
        }
    }
    #[cfg(unix)]
    fn stub(dir: &Path, name: &str, body: &str) -> PathBuf {
        use std::io::Write;
        use std::os::unix::fs::PermissionsExt;
        let bin = dir.join(name);
        // Closed before it runs: executing a file that is still open for writing fails (ETXTBSY).
        let mut file = std::fs::File::create(&bin).unwrap();
        file.write_all(body.as_bytes()).unwrap();
        file.sync_all().unwrap();
        drop(file);
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        bin
    }
    /// Runs a stub. Closing it first is not enough on Linux: a test forking on another thread
    /// holds the write handle until its exec, so a busy start is retried.
    #[cfg(unix)]
    async fn run_stub(
        bin: &Path,
        calls: &[(&str, Value)],
        wait: Duration,
    ) -> Result<Vec<String>, FroxelError> {
        for _ in 0..40 {
            match froxel_script_with(bin.as_os_str(), calls, wait).await {
                Err(FroxelError::Start(text)) if text.contains("os error 26") => {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                other => return other,
            }
        }
        froxel_script_with(bin.as_os_str(), calls, wait).await
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn the_deadline_includes_child_exit_after_all_replies() {
        let dir = tempfile::tempdir().unwrap();
        let bin = stub(
            dir.path(),
            "slow-froxel",
            r#"#!/bin/sh
while IFS= read -r line; do
 case "$line" in
  *initialize*notifications*) ;;
  *notifications/initialized*) ;;
  *initialize*) printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{}}' ;;
  *tools/call*) printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{"content":[{"type":"text","text":"done"}]}}' ;;
 esac
done
exec sleep 3
"#,
        );
        let start = std::time::Instant::now();
        let error = run_stub(
            &bin,
            &[("export_models", json!({}))],
            Duration::from_millis(150),
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("deadline"), "{error}");
        assert!(start.elapsed() < Duration::from_secs(2));
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn an_endless_line_from_froxel_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let bin = stub(
            dir.path(),
            "noisy-froxel",
            "#!/bin/sh\nhead -c 20000000 /dev/zero | tr '\\0' 'a'\nexec sleep 3\n",
        );
        let error = run_stub(
            &bin,
            &[("export_models", json!({}))],
            Duration::from_secs(20),
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("longer than"), "{error}");
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn a_missing_program_names_froxel_bin_and_a_failing_one_does_not() {
        let missing = froxel_script_with(
            OsStr::new("/no/such/froxel"),
            &[("export_models", json!({}))],
            Duration::from_secs(5),
        )
        .await
        .unwrap_err();
        assert!(matches!(missing, FroxelError::Start(_)), "{missing}");
        assert!(
            export_hints(&missing)[0].contains("FROXEL_BIN"),
            "{missing}"
        );
        let dir = tempfile::tempdir().unwrap();
        let bin = stub(dir.path(), "failing-froxel", "#!/bin/sh\nexit 3\n");
        let failed = run_stub(
            &bin,
            &[("export_models", json!({}))],
            Duration::from_secs(5),
        )
        .await
        .unwrap_err();
        assert!(matches!(failed, FroxelError::Failed(_)), "{failed}");
        assert!(failed.to_string().starts_with("Froxel closed"), "{failed}");
        assert!(!export_hints(&failed)[0].contains("FROXEL_BIN"), "{failed}");
    }
}
