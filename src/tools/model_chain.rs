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
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{ChildStdout, Command};
use tokio::time::Instant;

const EXPORT_WAIT: Duration = Duration::from_secs(60);
/// The longest line Froxel may send. A reply to 16 models is a few kilobytes.
const MAX_LINE_BYTES: usize = 4 * 1024 * 1024;
/// How much of Froxel's text an error message repeats.
const MAX_ECHO_CHARS: usize = 2000;
const MAX_MODELS: usize = 16;

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
    let jobs = match jobs_of(args, &project) {
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
    for (index, job) in &successful {
        let dest = res_display(&project, &job.dest);
        match import_current(&project, &job.dest) {
            None => missing.push(format!("{dest}.import")),
            Some(false) if content_hash(&job.dest).await != previous[*index] => stale.push(dest),
            Some(_) => {}
        }
    }
    let structure_ok = exported.get("ok").and_then(Value::as_bool) == Some(true);
    let previews_ok = previews
        .iter()
        .all(|preview| preview.get("ok").and_then(Value::as_bool) != Some(false));
    let ok = structure_ok
        && import_ok
        && missing.is_empty()
        && stale.is_empty()
        && missing_exports.is_empty()
        && previews_ok;
    let body = json!({
        "ok": ok,
        "export": exported,
        "import": import_body,
        "missing_exports": missing_exports,
        "missing_imports": missing,
        "stale_imports": stale,
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
        .map_err(|message| ToolResult::error(message.clone(), export_hints(&message)))?;
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

/// The recovery steps that fit what went wrong with Froxel.
fn export_hints(message: &str) -> &'static [&'static str] {
    if message.contains("Could not start") {
        &[
            "Set FROXEL_BIN to the Froxel executable, or put `froxel` on PATH",
            "Froxel needs a build that supports the export_models tool",
        ]
    } else if message.contains("deadline") {
        &[
            "Export fewer models in one call",
            "Run `froxel mcp` by hand to see whether it answers",
        ]
    } else {
        &[
            "Run `froxel mcp` by hand and call export_models to see its reply",
            "Check that the .vox files are valid and that Froxel supports export_models",
        ]
    }
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

/// A network share (`\\host\share`, `//host/share`) or a device path (`\\?\`): looking at it
/// would reach out to another machine or bypass the path rules.
fn is_remote_or_device_path(path: &str) -> bool {
    path.starts_with("\\\\") || path.starts_with("//")
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
    if res_display(project, &dest_path)
        .split('/')
        .any(|part| part == ".godot")
    {
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

async fn froxel_export(bin: &OsStr, models: &[Value]) -> Result<Value, String> {
    let reply = froxel_call(bin, "export_models", json!({"models": models}), EXPORT_WAIT).await?;
    serde_json::from_str(&reply).map_err(|error| {
        format!(
            "Froxel export was not JSON: {error}: {}",
            clip(&reply, MAX_ECHO_CHARS)
        )
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
    .await?;
    if png.is_file() {
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
) -> Result<String, String> {
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
) -> Result<Vec<String>, String> {
    let operation = async {
        let mut child = froxel_command(bin)
            .spawn()
            .map_err(|error| format!("Could not start {} mcp: {error}", bin.to_string_lossy()))?;
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
                return Err(clip(text, MAX_ECHO_CHARS));
            }
            replies.push(text.to_string());
        }
        drop(stdin);
        let status = child
            .wait()
            .await
            .map_err(|error| format!("Waiting for Froxel: {error}"))?;
        if !status.success() {
            return Err(format!("Froxel exited with {status}"));
        }
        Ok(replies)
    };
    tokio::time::timeout(wait, operation).await.map_err(|_| {
        format!(
            "Froxel MCP exceeded its {:.3}-second deadline, including process exit.",
            wait.as_secs_f64()
        )
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
async fn read_line(reader: &mut BufReader<ChildStdout>) -> Result<Option<String>, String> {
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

async fn read_reply(reader: &mut BufReader<ChildStdout>, id: u64) -> Result<Value, String> {
    while let Some(line) = read_line(reader).await? {
        let reply: Value = serde_json::from_str(&line)
            .map_err(|error| format!("Invalid Froxel JSON-RPC: {error}"))?;
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
        assert!(
            refused(json!({"vox":"\\\\host\\share\\a.vox","dest":"res://a.glb"}))
                .contains("network")
        );
        assert!(
            refused(json!({"vox":"//host/share/a.vox","dest":"res://a.glb"})).contains("network")
        );
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
        assert!(export_hints("Could not start froxel mcp: not found")[0].contains("FROXEL_BIN"));
        assert!(export_hints("Froxel MCP exceeded its deadline")[0].contains("fewer"));
        assert!(export_hints("Froxel exited with 1")[0].contains("by hand"));
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
        use std::os::unix::fs::PermissionsExt;
        let bin = dir.join(name);
        std::fs::write(&bin, body).unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        bin
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
        let error = froxel_script_with(
            bin.as_os_str(),
            &[("export_models", json!({}))],
            Duration::from_millis(150),
        )
        .await
        .unwrap_err();
        assert!(error.contains("deadline"), "{error}");
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
        let error = froxel_script_with(
            bin.as_os_str(),
            &[("export_models", json!({}))],
            Duration::from_secs(20),
        )
        .await
        .unwrap_err();
        assert!(error.contains("longer than"), "{error}");
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
        assert!(
            export_hints(&missing)[0].contains("FROXEL_BIN"),
            "{missing}"
        );
        let dir = tempfile::tempdir().unwrap();
        let bin = stub(dir.path(), "failing-froxel", "#!/bin/sh\nexit 3\n");
        let failed = froxel_script_with(
            bin.as_os_str(),
            &[("export_models", json!({}))],
            Duration::from_secs(5),
        )
        .await
        .unwrap_err();
        assert!(!export_hints(&failed)[0].contains("FROXEL_BIN"), "{failed}");
    }
}
