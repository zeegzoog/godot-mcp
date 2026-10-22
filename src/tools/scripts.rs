//! Reading and editing existing GDScript files.
//!
//! The editor path goes through the bridge, so the answer reflects what the script editor
//! holds, including text that is not saved yet. Only when no editor is connected do the tools
//! fall back to the file itself.

use super::{EditorChoice, ToolManager, str_arg};
use crate::error::McpError;
use crate::paths::resolve_in_project;
use crate::protocol::{Tool, ToolResult};
use serde_json::{Value, json};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use tokio::io::AsyncWriteExt;

/// Scripts larger than this are not handled by the script tools.
const MAX_BYTES: u64 = 1_048_576;

pub fn handles(name: &str) -> bool {
    matches!(name, "godot_read_script" | "godot_edit_script")
}

pub fn catalog() -> Vec<Tool> {
    vec![
        super::tool(
            "godot_read_script",
            "Reads an existing .gd script as the editor would save it: the editor's unsaved text when the script is open with unsaved changes (dirty: true, source: editor), otherwise the file. Returns {path, text, source, dirty, version}; the version label identifies the returned text. Without an editor it reads the file.",
            json!({
                "script_path": {"type": "string", "description": "res:// path to a .gd file in the project."},
                "project_path": {"type": "string"}
            }),
            &["script_path"],
        ),
        super::tool(
            "godot_edit_script",
            "Replaces one fragment of an existing .gd script: old_text must occur exactly once (any other count is refused with the count), and new_text takes its place. With the editor connected the change is one undoable editor action and the script is saved; it is refused while the script has unsaved changes in the editor. Without an editor the file is replaced as a whole. Pass the version from godot_read_script to refuse the edit if the script changed since. Never creates a file (use godot_create_script); run godot_validate_script after an edit. The text is data to this tool, but a script is code: Godot runs a @tool script in the editor as soon as it is saved.",
            json!({
                "script_path": {"type": "string", "description": "res:// path to an existing .gd file."},
                "old_text": {"type": "string", "description": "The exact fragment to replace; it must occur once."},
                "new_text": {"type": "string", "description": "The replacement text."},
                "version": {"type": "string", "description": "Optional version label from godot_read_script."},
                "project_path": {"type": "string"}
            }),
            &["script_path", "old_text", "new_text"],
        ),
    ]
}

pub async fn call(tools: &ToolManager, name: &str, args: Value) -> ToolResult {
    match name {
        "godot_read_script" => read(tools, &args).await,
        "godot_edit_script" => edit(tools, &args).await,
        _ => ToolResult::error(format!("Tool '{name}' has no handler"), &[]),
    }
}

async fn read(tools: &ToolManager, args: &Value) -> ToolResult {
    let choice = match tools.editor_choice(args, false).await {
        Ok(choice) => choice,
        Err(result) => return result,
    };
    if matches!(choice, EditorChoice::Host) {
        return read_file(tools, args).await;
    }
    match tools.bridge.send_command("read_script", args.clone()).await {
        Ok(value) => ToolResult::text(
            serde_json::to_string_pretty(&choice.name_project(value)).unwrap_or_default(),
        ),
        Err(McpError::BridgeUnavailable(_)) => read_file(tools, args).await,
        Err(error) => super::bridge_error_result(&error),
    }
}

async fn read_file(tools: &ToolManager, args: &Value) -> ToolResult {
    let script = match load(tools, args).await {
        Ok(script) => script,
        Err(result) => return result,
    };
    ToolResult::text(
        serde_json::to_string_pretty(&json!({
            "path": script.res_path,
            "text": script.text,
            "source": "file",
            "dirty": false,
            "editor_consulted": false,
            "version": label(&script.text),
            "project_dir": script.project.display().to_string(),
        }))
        .unwrap_or_default(),
    )
}

async fn edit(tools: &ToolManager, args: &Value) -> ToolResult {
    let choice = match tools.editor_choice(args, true).await {
        Ok(choice) => choice,
        Err(result) => return result,
    };
    if matches!(choice, EditorChoice::Host) {
        return edit_file(tools, args).await;
    }
    match tools.bridge.send_command("edit_script", args.clone()).await {
        Ok(value) => ToolResult::text(
            serde_json::to_string_pretty(&choice.name_project(value)).unwrap_or_default(),
        ),
        Err(McpError::BridgeUnavailable(_)) => edit_file(tools, args).await,
        Err(error) => super::bridge_error_result(&error),
    }
}

/// The edit without an editor: the same checks as the addon, then the whole file is replaced
/// through a temporary file so no half-written script is ever seen.
async fn edit_file(tools: &ToolManager, args: &Value) -> ToolResult {
    let (Some(old_text), Some(new_text)) = (str_arg(args, "old_text"), str_arg(args, "new_text"))
    else {
        return ToolResult::error(
            "Missing old_text or new_text",
            &["Pass the fragment to replace as old_text and its replacement as new_text"],
        );
    };
    if old_text.is_empty() {
        return ToolResult::error(
            "old_text must not be empty",
            &["Pass the exact fragment to replace"],
        );
    }
    let old_lf = old_text.replace("\r\n", "\n");
    let new_lf = new_text.replace("\r\n", "\n");
    if old_lf == new_lf {
        return ToolResult::error(
            "old_text and new_text are identical, so nothing would change",
            &["Pass the text as it should read after the edit in new_text"],
        );
    }
    let script = match load(tools, args).await {
        Ok(script) => script,
        Err(result) => return result,
    };
    if let Some(version) = str_arg(args, "version").filter(|version| !version.is_empty()) {
        let current = label(&script.text);
        if version != current {
            return ToolResult::error(
                format!("The script changed since it was read (version {version}, now {current})"),
                &["Read it again with godot_read_script and repeat the edit"],
            );
        }
    }
    let (count, first) = occurrences(&script.text, &old_lf);
    let Some(at) = first.filter(|_| count == 1) else {
        return ToolResult::error(
            format!(
                "old_text was found {count} times in {}; it must occur exactly once",
                script.res_path
            ),
            &["Include more surrounding text so old_text identifies one place"],
        );
    };
    let edited = format!(
        "{}{new_lf}{}",
        &script.text[..at],
        &script.text[at + old_lf.len()..]
    );
    if let Err(result) = replace_file(&script, &edited).await {
        return result;
    }
    ToolResult::text(
        serde_json::to_string_pretty(&json!({
            "path": script.res_path,
            "replaced": 1,
            "version": label(&edited),
            "source": "file",
            "editor_consulted": false,
            "project_dir": script.project.display().to_string(),
        }))
        .unwrap_or_default(),
    )
}

/// How often `fragment` occurs in `text`, counting overlapping occurrences (as the addon
/// does), and where the first one starts.
fn occurrences(text: &str, fragment: &str) -> (usize, Option<usize>) {
    let mut count = 0;
    let mut first = None;
    let mut from = 0;
    while let Some(relative) = text[from..].find(fragment) {
        let at = from + relative;
        first.get_or_insert(at);
        count += 1;
        from = at + text[at..].chars().next().map_or(1, char::len_utf8);
    }
    (count, first)
}

/// Writes `edited` (with `\n` line endings) over the script, in the line ending and with the
/// byte order mark the file had, through a temporary file in the same directory.
async fn replace_file(script: &Script, edited: &str) -> Result<(), ToolResult> {
    // Replacing a symbolic link would swap the link for a regular file; the edit leaves it alone.
    if tokio::fs::symlink_metadata(&script.file)
        .await
        .is_ok_and(|metadata| metadata.file_type().is_symlink())
    {
        return Err(ToolResult::error(
            format!(
                "{} is a symbolic link, so it is not replaced",
                script.res_path
            ),
            &["Edit the file the link points to, or replace the link with the script itself"],
        ));
    }
    let mut body = String::new();
    if script.bom {
        body.push('\u{feff}');
    }
    if script.crlf {
        body.push_str(&edited.replace('\n', "\r\n"));
    } else {
        body.push_str(edited);
    }
    let name = script
        .file
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let failure = |error: std::io::Error| {
        ToolResult::error(
            format!("Could not write {}: {error}", script.res_path),
            &["Check that the file and its directory are writable; the script is unchanged"],
        )
    };
    let (temporary, mut file) = create_temporary(&script.file, &name)
        .await
        .map_err(failure)?;
    let written = async {
        file.write_all(body.as_bytes()).await?;
        file.flush().await
    }
    .await;
    drop(file);
    if let Err(error) = written {
        let _ = tokio::fs::remove_file(&temporary).await;
        return Err(failure(error));
    }
    if let Err(error) = tokio::fs::rename(&temporary, &script.file).await {
        let _ = tokio::fs::remove_file(&temporary).await;
        return Err(failure(error));
    }
    Ok(())
}

/// Creates the temporary file next to `target` under a name nobody can predict, and only if
/// nothing exists at that name (`create_new` fails on an existing file or symlink instead of
/// following it), so a planted link can never redirect the write out of the project.
async fn create_temporary(
    target: &Path,
    name: &str,
) -> std::io::Result<(PathBuf, tokio::fs::File)> {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    for attempt in 0..16u32 {
        let suffix = format!("{:x}{:x}{attempt:x}", std::process::id(), nanos);
        let path = target.with_file_name(format!(".{name}.{suffix}.godot-mcp-edit.tmp"));
        match tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .await
        {
            Ok(file) => return Ok((path, file)),
            Err(error) if error.kind() == ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(std::io::Error::new(
        ErrorKind::AlreadyExists,
        "no free temporary file name",
    ))
}

/// A script read from disk: its text with `\n` line endings and the facts needed to write it
/// back the way it was.
struct Script {
    res_path: String,
    project: PathBuf,
    file: PathBuf,
    text: String,
    crlf: bool,
    bom: bool,
}

/// The version label of a text: the 64-bit FNV-1a hash of its UTF-8 bytes with `\r\n` read
/// as `\n`, as 16 lowercase hex digits. The addon computes the same label in GDScript.
pub fn label(text: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.replace("\r\n", "\n").bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// Finds the script named by `script_path` in the host project and reads it, or explains why
/// it cannot be used.
async fn load(tools: &ToolManager, args: &Value) -> Result<Script, ToolResult> {
    let Some(requested) = str_arg(args, "script_path") else {
        return Err(ToolResult::error(
            "Missing script_path",
            &["Pass a res:// path to a .gd script"],
        ));
    };
    let project = tools.host_project(args)?;
    let file = resolve_in_project(&project, requested).map_err(|problem| {
        ToolResult::error(
            problem,
            &["Use a res:// path or a path relative to the project directory"],
        )
    })?;
    let res_path = res_path_of(&project, &file, requested);
    if !res_path.ends_with(".gd") {
        return Err(ToolResult::error(
            format!("Only .gd scripts can be read or edited: {res_path}"),
            &["Pass the path of a GDScript file"],
        ));
    }
    let size = match tokio::fs::metadata(&file).await {
        Ok(metadata) => metadata.len(),
        Err(error) if error.kind() == ErrorKind::NotFound => {
            return Err(ToolResult::error(
                format!("Script file does not exist: {res_path}"),
                &["Create it with godot_create_script"],
            ));
        }
        Err(error) => {
            return Err(ToolResult::error(
                format!("Cannot read {res_path}: {error}"),
                &["Check that the file is readable"],
            ));
        }
    };
    if size > MAX_BYTES {
        return Err(ToolResult::error(
            format!("{res_path} is larger than 1 MiB, which the script tools do not handle"),
            &["Edit it with your own file tools"],
        ));
    }
    let bytes = tokio::fs::read(&file).await.map_err(|error| {
        ToolResult::error(
            format!("Cannot read {res_path}: {error}"),
            &["Check that the file is readable"],
        )
    })?;
    let Ok(raw) = String::from_utf8(bytes) else {
        return Err(ToolResult::error(
            format!("{res_path} is not valid UTF-8, so it is not read as text"),
            &["Convert the file to UTF-8 first"],
        ));
    };
    let bom = raw.starts_with('\u{feff}');
    let body = raw.strip_prefix('\u{feff}').unwrap_or(&raw);
    Ok(Script {
        res_path,
        project,
        file,
        text: body.replace("\r\n", "\n"),
        crlf: body.contains("\r\n"),
        bom,
    })
}

/// The `res://` spelling of a path inside the project, for answers.
fn res_path_of(project: &Path, file: &Path, requested: &str) -> String {
    let normalized = requested.trim().replace('\\', "/");
    if let Some(rest) = normalized.strip_prefix("res://") {
        return format!("res://{rest}");
    }
    let relative: Option<PathBuf> = file.strip_prefix(project).ok().map(Path::to_path_buf);
    match relative {
        Some(relative) => format!("res://{}", relative.to_string_lossy().replace('\\', "/")),
        None => format!("res://{}", normalized.trim_start_matches('/')),
    }
}
