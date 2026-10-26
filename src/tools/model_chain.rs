//! Export a `.vox` through Froxel, import the `.glb` into a Godot project, and
//! report whether Godot wrote an `.import` sidecar whose cache matches the new
//! file. The picture, when asked
//! for, is Froxel's path-traced PNG. A Godot window capture stays on
//! `godot_capture_viewport` or `godot_game_screenshot`.

use super::{ToolManager, tool};
use crate::godot::import_current;
use crate::paths::{res_display, resolve_in_project};
use crate::protocol::{Tool, ToolResult};
use serde_json::{Value, json};
use std::collections::HashSet;
use std::collections::hash_map::DefaultHasher;
use std::ffi::OsStr;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

const EXPORT_WAIT: Duration = Duration::from_secs(60);

pub fn definition() -> Tool {
    tool(
        "godot_check_voxel_model",
        "Exports a .vox through `froxel mcp` (FROXEL_BIN, or `froxel` on PATH), writes the .glb inside the Godot project, imports it, and checks the result. The Froxel reply's structure lists nodes, meshes, skins, animations, and warnings (an empty mesh, a skin with no joints, a missing or repeated node name, an animation with no channels). The import goes through the connected editor when it has this project open, and is a headless import otherwise. The Godot check is the .import sidecar: without it the model would not load (missing_imports), and a changed .glb whose cache is older than the file was not imported again (stale_imports). A failed export is reported under export and is not imported. preview true also asks Froxel to path-trace a PNG next to the .glb (128², 4 samples). This does not open a Godot scene and does not take a Godot screenshot; use godot_capture_viewport or godot_game_screenshot for that. The .vox is read only. 1 to 16 models can be passed instead of one vox, each with its own dest.",
        json!({
            "project_path": {"type": "string", "description": "Directory containing project.godot. Optional when the server was started in the project."},
            "vox": {"type": "string", "description": "Absolute path of one .vox file. Use models instead to export several."},
            "dest": {"type": "string", "description": "res:// path of the .glb to write, inside the project. Required with vox."},
            "models": {
                "type": "array",
                "maxItems": 16,
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
            "preview": {"type": "boolean", "description": "Path-trace each model to a PNG beside its .glb. Default false."}
        }),
        &[],
    )
}

pub async fn call(tools: &ToolManager, args: &Value) -> ToolResult {
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
                    "Set FROXEL_BIN to the froxel binary",
                ],
            );
        }
    };
    // An identical re-export is not imported again (Godot compares the source hash), so its
    // older cache still matches; a changed .glb must get a cache newer than itself.
    let previous: Vec<Option<u64>> = jobs.iter().map(|job| content_hash(&job.dest)).collect();
    let mut models = Vec::new();
    for job in &jobs {
        if let Some(parent) = job.dest.parent() {
            if let Err(error) = std::fs::create_dir_all(parent) {
                return ToolResult::error(
                    format!("Could not create {}: {error}", parent.display()),
                    &["Check that the project directory is writable"],
                );
            }
        }
        models.push(json!({
            "source": job.vox,
            "path": job.dest,
        }));
    }
    let exported = match froxel_export(&models).await {
        Ok(value) => value,
        Err(message) => {
            return ToolResult::error(
                message,
                &[
                    "Set FROXEL_BIN to the froxel binary, or put `froxel` on PATH",
                    "The export is `froxel mcp` tool export_models",
                ],
            );
        }
    };
    let exported_models = exported.get("models").and_then(Value::as_array);
    let successful: Vec<(usize, &Job)> = jobs
        .iter()
        .enumerate()
        .filter(|(index, _)| {
            let result = exported_models.and_then(|models| models.get(*index));
            result
                .and_then(|model| model.get("ok"))
                .and_then(Value::as_bool)
                == Some(true)
        })
        .collect();
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
    let mut previews = Vec::new();
    if preview {
        for (_, job) in &successful {
            if let Some(png) = &job.preview {
                match froxel_render(&job.vox, png).await {
                    Ok(()) => previews.push(json!({"vox": job.vox, "png": png, "ok": true})),
                    Err(error) => previews
                        .push(json!({"vox": job.vox, "png": png, "ok": false, "error": error})),
                }
            }
        }
    }
    let mut missing = Vec::new();
    let mut stale = Vec::new();
    for (index, job) in &successful {
        let dest = res_display(&project, &job.dest);
        match import_current(&project, &job.dest) {
            None => missing.push(format!("{dest}.import")),
            Some(false) if content_hash(&job.dest) != previous[*index] => stale.push(dest),
            Some(_) => {}
        }
    }
    let structure_ok = exported.get("ok").and_then(Value::as_bool) == Some(true);
    let previews_ok = previews
        .iter()
        .all(|preview| preview.get("ok").and_then(Value::as_bool) != Some(false));
    let ok = structure_ok && import_ok && missing.is_empty() && stale.is_empty() && previews_ok;
    let body = json!({
        "ok": ok,
        "export": exported,
        "import": import_body,
        "missing_imports": missing,
        "stale_imports": stale,
        "previews": previews,
        "screenshot": "This check does not capture a Godot window. preview path-traces the voxel model. godot_capture_viewport and godot_game_screenshot capture a window.",
    });
    let text = serde_json::to_string_pretty(&body).unwrap_or_else(|_| body.to_string());
    ToolResult {
        content: vec![crate::protocol::ToolContent::Text { text }],
        is_error: !ok,
    }
}

fn content_hash(path: &Path) -> Option<u64> {
    let bytes = std::fs::read(path).ok()?;
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
        if models.is_empty() || models.len() > 16 {
            return Err("models must contain 1 to 16 {vox, dest} pairs.".into());
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
    let vox_path = PathBuf::from(vox);
    if !vox_path.is_absolute() {
        return Err(format!("models[{index}].vox must be an absolute path."));
    }
    if !vox_path.is_file() {
        return Err(format!("models[{index}].vox was not found at {vox}."));
    }
    let dest_path = resolve_in_project(project, dest)
        .map_err(|error| format!("models[{index}].dest: {error}"))?;
    let preview_path = if preview {
        let png = dest_path.with_extension("png");
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

async fn froxel_export(models: &[Value]) -> Result<Value, String> {
    let reply = froxel_call("export_models", json!({"models": models}), EXPORT_WAIT).await?;
    serde_json::from_str(&reply)
        .map_err(|error| format!("Froxel export was not JSON: {error}: {reply}"))
}

async fn froxel_render(vox: &Path, png: &Path) -> Result<(), String> {
    froxel_script(
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
        EXPORT_WAIT,
    )
    .await?;
    if png.is_file() {
        Ok(())
    } else {
        Err(format!("Froxel did not write {}", png.display()))
    }
}

async fn froxel_call(name: &str, arguments: Value, wait: Duration) -> Result<String, String> {
    froxel_script(&[(name, arguments)], wait)
        .await
        .map(|replies| replies.into_iter().next().unwrap_or_default())
}

async fn froxel_script(calls: &[(&str, Value)], wait: Duration) -> Result<Vec<String>, String> {
    let bin = std::env::var_os("FROXEL_BIN").unwrap_or_else(|| "froxel".into());
    froxel_script_with(&bin, calls, wait).await
}

async fn froxel_script_with(
    bin: &OsStr,
    calls: &[(&str, Value)],
    wait: Duration,
) -> Result<Vec<String>, String> {
    let operation = async {
        let mut child = Command::new(bin)
            .arg("mcp")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|error| format!("Could not start {} mcp: {error}", bin.to_string_lossy()))?;
        let mut stdin = child.stdin.take().ok_or("froxel mcp stdin was closed")?;
        let stdout = child.stdout.take().ok_or("froxel mcp stdout was closed")?;
        let mut lines = BufReader::new(stdout).lines();
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
                return Err(text.to_string());
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

async fn read_reply(
    lines: &mut tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
    id: u64,
) -> Result<Value, String> {
    while let Some(line) = lines
        .next_line()
        .await
        .map_err(|error| format!("Reading Froxel: {error}"))?
    {
        let reply: Value = serde_json::from_str(&line)
            .map_err(|error| format!("Invalid Froxel JSON-RPC: {error}"))?;
        if reply.get("id").and_then(Value::as_u64) != Some(id) {
            continue;
        }
        if let Some(error) = reply.get("error") {
            return Err(format!("Froxel JSON-RPC error: {error}"));
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
            crate::protocol::ToolContent::Text { text } => Some(text.as_str()),
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
    #[cfg(unix)]
    #[test]
    fn preview_paths_are_validated_before_export() {
        let dir = project();
        let outside = tempfile::tempdir().unwrap();
        let sentinel = outside.path().join("sentinel");
        std::fs::write(&sentinel, b"unchanged").unwrap();
        std::os::unix::fs::symlink(&sentinel, dir.path().join("model.png")).unwrap();
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
        for extension in ["glb", "png"] {
            let dir = project();
            let outside = tempfile::tempdir().unwrap();
            let target = outside.path().join("not-created");
            std::os::unix::fs::symlink(&target, dir.path().join(format!("model.{extension}")))
                .unwrap();
            let jobs = jobs_of(
                &json!({"vox":dir.path().join("source.vox"),"dest":"res://model.glb","preview":true}),
                dir.path(),
            );
            assert!(jobs.is_err());
            assert!(!target.exists());
        }
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn the_deadline_includes_child_exit_after_all_replies() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("slow-froxel");
        std::fs::write(&bin, r#"#!/bin/sh
while IFS= read -r line; do
 case "$line" in
  *initialize*notifications*) ;;
  *notifications/initialized*) ;;
  *initialize*) printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{}}' ;;
  *tools/call*) printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{"content":[{"type":"text","text":"done"}]}}' ;;
 esac
done
exec sleep 3
"#).unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
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
}
