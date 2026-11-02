//! Creating, reading, editing and checking text shaders (`.gdshader`, `.gdshaderinc`).
//!
//! Like the script tools, the editor path goes through the bridge, so an open shader tab and
//! the shader the editor holds in memory stay in step with the file. Only when no editor is
//! connected do the tools fall back to the file itself. The check compiles the saved file with
//! the engine's shader compiler in a headless Godot.

use super::addon_update::{file_fallback_note, unknown_command, with_fields};
use super::scripts::{
    MAX_BYTES, TextFile, TextKind, edit_file, label, locate, locate_in, read_file, replace_file,
};
use super::{EditorChoice, ToolManager, bridge_error_result, str_arg};
use crate::error::McpError;
use crate::protocol::{Tool, ToolResult};
use serde::Deserialize;
use serde_json::{Value, json};
use std::io::ErrorKind;
use std::path::Path;
use tokio::io::AsyncWriteExt;

const SHADER: TextKind = TextKind {
    param: "shader_path",
    extensions: &[".gdshader", ".gdshaderinc"],
    noun: "shader",
    noun_title: "Shader",
    what: "a .gdshader or .gdshaderinc file",
    wrong_kind: "Only .gdshader and .gdshaderinc files are handled by the shader tools",
    wrong_kind_hint: "Pass the path of a text shader or shader include; a visual shader is a .tres resource",
    read_tool: "godot_read_shader",
    create_tool: "godot_create_shader",
};

/// What a clean check covered, for the answer and the tool description.
const CHECKED: &str = "The saved file was preprocessed (#include, #define, #if) and compiled by Godot's shader compiler for its shader_type: syntax, types, unknown identifiers, built-ins and render modes of that type, and the includes it pulls in. Not checked: the GPU driver's own compile of the generated code, and warnings the shader editor shows (such as unused uniforms).";

pub fn handles(name: &str) -> bool {
    matches!(
        name,
        "godot_create_shader" | "godot_read_shader" | "godot_edit_shader" | "godot_validate_shader"
    )
}

pub fn catalog() -> Vec<Tool> {
    vec![
        super::tool(
            "godot_create_shader",
            "Creates a text shader (.gdshader) or shader include (.gdshaderinc) inside the project with the given code, written as it is (tabs and line endings kept). Refuses an existing file unless overwrite: true. With the editor connected the editor writes it, so the shader it holds in memory (and every material using it) gets the new code and an open shader tab is reloaded; an overwrite is refused while that tab has unsaved changes. A new file gets a .uid file as the editor would give it; an existing file keeps its .uid. Without an editor the file is written directly. Returns {path, created, version, uid}. Run godot_validate_shader afterwards: this tool does not compile the code.",
            json!({
                "shader_path": {"type": "string", "description": "res:// path ending in .gdshader or .gdshaderinc."},
                "code": {"type": "string", "description": "The whole shader source, for example \"shader_type spatial;\\n\\nvoid fragment() {\\n\\tALBEDO = vec3(1.0);\\n}\\n\". At most 1 MiB."},
                "overwrite": {"type": "boolean", "description": "Replace an existing file. Default false."},
                "project_path": {"type": "string"}
            }),
            &["shader_path", "code"],
        ),
        super::tool(
            "godot_read_shader",
            "Reads an existing .gdshader or .gdshaderinc as the editor would save it: the shader editor's unsaved text when the shader is open with unsaved changes (dirty: true, source: editor), otherwise the file. Returns {path, text, source, dirty, version}; the version label identifies the returned text. stale_tab: true means the shader is open in a tab without unsaved changes that still shows older text than the file; godot_edit_shader refuses until the tab shows the file. Without an editor it reads the file.",
            json!({
                "shader_path": {"type": "string", "description": "res:// path to a .gdshader or .gdshaderinc file in the project."},
                "project_path": {"type": "string"}
            }),
            &["shader_path"],
        ),
        super::tool(
            "godot_edit_shader",
            "Replaces one fragment of an existing .gdshader or .gdshaderinc: old_text must occur exactly once (any other count is refused with the count), and new_text takes its place. With the editor connected the shader is opened in the shader editor if it is not, the change is one undo step in its tab, the shader is saved, and the editor's shader and the materials using it get the new code; it is refused while the shader has unsaved changes in the editor. Without an editor the file is replaced as a whole. Pass the version from godot_read_shader to refuse the edit if the shader changed since. Never creates a file (use godot_create_shader); run godot_validate_shader after an edit.",
            json!({
                "shader_path": {"type": "string", "description": "res:// path to an existing .gdshader or .gdshaderinc file."},
                "old_text": {"type": "string", "description": "The exact fragment to replace; it must occur once."},
                "new_text": {"type": "string", "description": "The replacement text."},
                "version": {"type": "string", "description": "Optional version label from godot_read_shader."},
                "project_path": {"type": "string"}
            }),
            &["shader_path", "old_text", "new_text"],
        ),
        super::tool(
            "godot_validate_shader",
            &format!(
                "Compiles a saved .gdshader with Godot's own shader compiler in a headless Godot (needs GODOT_PATH) and lists its errors with file and line; an error inside an included file names that file. {CHECKED} Unsaved text in the shader editor is not checked: save it first. A .gdshaderinc is not compiled on its own (it has no shader_type); validate a shader that includes it. A preprocessor failure (a missing include, an #if without #endif) is reported by Godot at the directive's line as an unknown '#' character."
            ),
            json!({
                "shader_path": {"type": "string", "description": "res:// path to a .gdshader file in the project."},
                "project_path": {"type": "string"}
            }),
            &["shader_path"],
        ),
    ]
}

pub async fn call(tools: &ToolManager, name: &str, args: Value) -> ToolResult {
    match name {
        "godot_create_shader" => create(tools, &args).await,
        "godot_read_shader" => read(tools, &args).await,
        "godot_edit_shader" => edit(tools, &args).await,
        "godot_validate_shader" => validate(tools, &args).await,
        _ => ToolResult::error(format!("Tool '{name}' has no handler"), &[]),
    }
}

async fn read(tools: &ToolManager, args: &Value) -> ToolResult {
    let choice = match tools.editor_choice(args, false).await {
        Ok(choice) => choice,
        Err(result) => return result,
    };
    if matches!(choice, EditorChoice::Host) {
        return read_file(tools, args, &SHADER).await;
    }
    match tools.bridge.send_command("read_shader", args.clone()).await {
        Ok(value) => ToolResult::text(
            serde_json::to_string_pretty(&choice.name_project(value)).unwrap_or_default(),
        ),
        Err(McpError::BridgeUnavailable(_)) => read_file(tools, args, &SHADER).await,
        Err(error) => match unknown_command(&error) {
            // An addon older than the server: the saved file is what can be read.
            Some(command) => {
                let note = file_fallback_note(
                    tools,
                    &command,
                    "the shader was read from its file: changes not saved in the editor are not in it",
                )
                .await;
                with_fields(read_file(tools, args, &SHADER).await, &note)
            }
            None => bridge_error_result(&error),
        },
    }
}

async fn edit(tools: &ToolManager, args: &Value) -> ToolResult {
    let choice = match tools.editor_choice(args, true).await {
        Ok(choice) => choice,
        Err(result) => return result,
    };
    if matches!(choice, EditorChoice::Host) {
        return edit_file(tools, args, &SHADER).await;
    }
    match tools.bridge.send_command("edit_shader", args.clone()).await {
        Ok(value) => ToolResult::text(
            serde_json::to_string_pretty(&choice.name_project(value)).unwrap_or_default(),
        ),
        Err(McpError::BridgeUnavailable(_)) => edit_file(tools, args, &SHADER).await,
        Err(error) => match unknown_command(&error) {
            // An addon older than the server: the file is edited, against the version read from it.
            Some(command) => {
                let note = file_fallback_note(
                    tools,
                    &command,
                    "the shader file was edited on disk, checked against the version read from the file; this addon cannot tell whether the shader is open in the editor, and if it is, Godot asks there whether to reload it",
                )
                .await;
                with_fields(edit_file(tools, args, &SHADER).await, &note)
            }
            None => bridge_error_result(&error),
        },
    }
}

async fn create(tools: &ToolManager, args: &Value) -> ToolResult {
    let Some(code) = str_arg(args, "code") else {
        return ToolResult::error("Missing code", &["Pass the whole shader source as code"]);
    };
    if code.len() as u64 > MAX_BYTES {
        return ToolResult::error(
            "code is larger than 1 MiB, which the shader tools do not handle",
            &["Split the shader into a shader and .gdshaderinc includes"],
        );
    }
    let choice = match tools.editor_choice(args, true).await {
        Ok(choice) => choice,
        Err(result) => return result,
    };
    if matches!(choice, EditorChoice::Host) {
        return create_file(tools, args, code).await;
    }
    match tools
        .bridge
        .send_command("create_shader", args.clone())
        .await
    {
        Ok(value) => ToolResult::text(
            serde_json::to_string_pretty(&choice.name_project(value)).unwrap_or_default(),
        ),
        Err(McpError::BridgeUnavailable(_)) => create_file(tools, args, code).await,
        Err(error) => bridge_error_result(&error),
    }
}

/// The create without an editor: the file is written as given, an existing one only with
/// `overwrite` and through a temporary file, and a new one gets a `.uid` file.
async fn create_file(tools: &ToolManager, args: &Value, code: &str) -> ToolResult {
    let (project, file, res_path) = match locate(tools, args, &SHADER) {
        Ok(located) => located,
        Err(result) => return result,
    };
    let overwrite = args
        .get("overwrite")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let existing = match tokio::fs::symlink_metadata(&file).await {
        Ok(metadata) => Some(metadata),
        Err(error) if error.kind() == ErrorKind::NotFound => None,
        Err(error) => {
            return ToolResult::error(
                format!("Cannot inspect {res_path}: {error}"),
                &["Check that the project directory is readable"],
            );
        }
    };
    if let Some(metadata) = &existing {
        if !overwrite {
            return ToolResult::error(
                format!("File already exists: {res_path}"),
                &[
                    "Pass overwrite: true to replace it, or choose another file name",
                    "Or change part of it with godot_read_shader and godot_edit_shader",
                ],
            );
        }
        if !metadata.is_file() && !metadata.file_type().is_symlink() {
            return ToolResult::error(
                format!("{res_path} is not a file, so it is not replaced"),
                &["Choose another path"],
            );
        }
    }
    if let Some(parent) = file.parent() {
        if let Err(error) = tokio::fs::create_dir_all(parent).await {
            return ToolResult::error(
                format!("Could not create the folder for {res_path}: {error}"),
                &["Check that the project directory is writable"],
            );
        }
    }

    if existing.is_some() {
        let target = TextFile {
            res_path: res_path.clone(),
            project: project.clone(),
            file: file.clone(),
            text: String::new(),
            crlf: false,
            bom: false,
        };
        if let Err(result) = replace_file(&target, code, SHADER.noun).await {
            return result;
        }
    } else if let Err(error) = write_new(&file, code).await {
        return if error.kind() == ErrorKind::AlreadyExists {
            ToolResult::error(
                format!("File already exists: {res_path}"),
                &["Pass overwrite: true to replace it, or choose another file name"],
            )
        } else {
            ToolResult::error(
                format!("Could not write {res_path}: {error}"),
                &["Check that the project directory is writable"],
            )
        };
    }

    let uid = match ensure_uid_file(&file).await {
        Ok(uid) => uid,
        Err(error) => {
            return ToolResult::error(
                format!("{res_path} was written, but its .uid file could not be: {error}"),
                &[
                    "Open the project in the Godot editor, which gives the file a .uid when it scans it",
                ],
            );
        }
    };
    ToolResult::text(
        serde_json::to_string_pretty(&json!({
            "path": res_path,
            "created": existing.is_none(),
            "version": label(code),
            "uid": uid,
            "source": "file",
            "editor_consulted": false,
            "project_dir": project.display().to_string(),
        }))
        .unwrap_or_default(),
    )
}

/// Writes a file that must not exist yet (`create_new` neither replaces a file nor follows a
/// link planted at the name).
async fn write_new(file: &Path, code: &str) -> std::io::Result<()> {
    let mut handle = tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(file)
        .await?;
    handle.write_all(code.as_bytes()).await?;
    // tokio's File writes on a worker thread and does not flush when dropped.
    handle.flush().await
}

/// The uid in the file's `.uid` file, writing one first when there is none: `uid://...` and a
/// newline, as the editor writes it for a new file it finds.
async fn ensure_uid_file(file: &Path) -> std::io::Result<String> {
    let mut name = file.as_os_str().to_owned();
    name.push(".uid");
    let uid_file = std::path::PathBuf::from(name);
    match tokio::fs::read_to_string(&uid_file).await {
        Ok(text) => return Ok(text.trim().to_string()),
        Err(error) if error.kind() != ErrorKind::NotFound => return Err(error),
        Err(_) => {}
    }
    let uid = uid_text(random_uid());
    write_new(&uid_file, &format!("{uid}\n")).await?;
    Ok(uid)
}

/// A new resource id: 63 random bits, as Godot draws them.
fn random_uid() -> u64 {
    // The low 64 bits of a version 4 UUID: 62 random bits plus two fixed variant bits, of
    // which the top one is cleared like Godot clears the sign bit.
    (uuid::Uuid::new_v4().as_u128() as u64) & 0x7fff_ffff_ffff_ffff
}

/// Godot's text form of a resource id: `uid://` and the id in base 34 (`a`..`y`, `0`..`8`;
/// the engine never uses `z` and `9`).
pub fn uid_text(mut id: u64) -> String {
    const DIGITS: &[u8; 34] = b"abcdefghijklmnopqrstuvwxy012345678";
    let mut digits = Vec::new();
    loop {
        digits.push(DIGITS[(id % 34) as usize]);
        id /= 34;
        if id == 0 {
            break;
        }
    }
    digits.reverse();
    format!("uid://{}", String::from_utf8_lossy(&digits))
}

#[derive(Debug, Deserialize)]
struct ShaderCheck {
    loaded: bool,
    #[serde(default)]
    errors: Vec<CompileError>,
    #[serde(default)]
    other: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct CompileError {
    #[serde(default)]
    file: String,
    #[serde(default)]
    line: i64,
    #[serde(default)]
    message: String,
}

async fn validate(tools: &ToolManager, args: &Value) -> ToolResult {
    let project = match tools.resolve_project(args).await {
        Ok(project) => project,
        Err(result) => return result,
    };
    let (file, res_path) = match locate_in(&project.dir, args, &SHADER) {
        Ok(located) => located,
        Err(result) => return result,
    };
    if !file.is_file() {
        return ToolResult::error(
            format!("Shader file does not exist: {res_path}"),
            &["Create it with godot_create_shader"],
        );
    }
    if res_path.ends_with(".gdshaderinc") {
        return ToolResult::error(
            format!(
                "{res_path} is a shader include, which Godot compiles only as part of a shader that includes it"
            ),
            &["Validate a .gdshader that has #include \"...\" of this file"],
        );
    }
    let answer = match tools.cli.check_shader(&project.dir, &res_path).await {
        Ok(answer) => answer,
        Err(error) => {
            return ToolResult::error(
                format!("The shader could not be checked: {error}"),
                &[
                    "Set GODOT_PATH to a working Godot executable of the project's version",
                    "Retry once; note it with godot_note_feedback if it repeats",
                ],
            );
        }
    };
    let check: ShaderCheck = match serde_json::from_str(&answer) {
        Ok(check) => check,
        Err(error) => {
            return ToolResult::error(
                format!("Godot's answer to the shader check could not be read ({error}): {answer}"),
                &["Retry once; note it with godot_note_feedback if it repeats"],
            );
        }
    };
    check_result(&res_path, &check)
}

/// The tool result for the answer of the check script.
fn check_result(res_path: &str, check: &ShaderCheck) -> ToolResult {
    if !check.loaded {
        let mut message = format!("Godot could not load {res_path} as a shader");
        if !check.other.is_empty() {
            message.push_str(&format!(": {}", check.other.join("; ")));
        }
        return ToolResult::error(
            message,
            &["Check that the file is a text shader saved as UTF-8, then validate again"],
        );
    }
    if check.errors.is_empty() && check.other.is_empty() {
        return ToolResult::text(
            serde_json::to_string_pretty(&json!({
                "path": res_path,
                "valid": true,
                "checked": CHECKED,
            }))
            .unwrap_or_default(),
        );
    }
    let mut lines = Vec::new();
    for error in &check.errors {
        let file = if error.file.is_empty() {
            res_path
        } else {
            &error.file
        };
        let mut line = format!("{file}:{}: {}", error.line, error.message);
        if error.message.contains("Unknown character #35: '#'") {
            line.push_str(
                " (a preprocessor directive on this line failed: check the #include path and that every #if has its #endif)",
            );
        }
        lines.push(line);
    }
    for message in &check.other {
        if message.contains("not supported in Dummy renderer") {
            lines.push(format!(
                "{res_path}: the shader has no supported shader_type line ({message})"
            ));
        } else {
            lines.push(format!("{res_path}: {message}"));
        }
    }
    ToolResult::error(
        format!(
            "{res_path} does not compile:\n{}",
            lines
                .iter()
                .map(|line| format!("- {line}"))
                .collect::<Vec<_>>()
                .join("\n")
        ),
        &[
            "Read the shader with godot_read_shader, fix the line named above with godot_edit_shader, then validate again",
            "Begin a shader with shader_type spatial;, canvas_item;, particles;, sky; or fog;",
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uid_text_spells_ids_as_godot_does() {
        // The engine's own comment names the largest id: 0x7FFFFFFFFFFFFFFF is uid://d4n4ub6itg400.
        assert_eq!(uid_text(0x7fff_ffff_ffff_ffff), "uid://d4n4ub6itg400");
        assert_eq!(uid_text(0), "uid://a");
        assert_eq!(uid_text(33), "uid://8");
        assert_eq!(uid_text(34), "uid://ba");
        let random = uid_text(random_uid());
        assert!(random.starts_with("uid://"), "{random}");
        assert!(
            !random[6..].contains(['z', '9']),
            "Godot never writes z or 9: {random}"
        );
    }

    fn text_of(result: &ToolResult) -> String {
        match result.content.first() {
            Some(crate::protocol::ToolContent::Text { text }) => text.clone(),
            _ => String::new(),
        }
    }

    #[test]
    fn a_check_with_errors_names_each_file_and_line() {
        let check: ShaderCheck = serde_json::from_str(
            r#"{"loaded": true, "errors": [
                {"file": "", "line": 3, "message": "Unknown identifier in expression: 'foo'."},
                {"file": "res://inc.gdshaderinc", "line": 1, "message": "Expected a ';' after 'return'."},
                {"file": "", "line": 2, "message": "Tokenizer: Unknown character #35: '#'"}
            ], "other": ["Shader type  not supported in Dummy renderer."]}"#,
        )
        .expect("check json");
        let result = check_result("res://water.gdshader", &check);
        assert!(result.is_error);
        let text = text_of(&result);
        assert!(
            text.contains("res://water.gdshader:3: Unknown identifier in expression: 'foo'."),
            "{text}"
        );
        assert!(
            text.contains("res://inc.gdshaderinc:1: Expected a ';'"),
            "{text}"
        );
        assert!(text.contains("#include path"), "{text}");
        assert!(text.contains("no supported shader_type"), "{text}");
        assert!(text.contains("godot_edit_shader"), "{text}");
    }

    #[test]
    fn a_clean_check_says_what_was_checked() {
        let check: ShaderCheck =
            serde_json::from_str(r#"{"loaded": true, "errors": [], "other": []}"#)
                .expect("check json");
        let result = check_result("res://water.gdshader", &check);
        assert!(!result.is_error);
        let answer: Value = serde_json::from_str(&text_of(&result)).expect("answer json");
        assert_eq!(answer["valid"], true);
        assert!(
            answer["checked"]
                .as_str()
                .unwrap_or_default()
                .contains("Not checked")
        );
        let unloaded: ShaderCheck =
            serde_json::from_str(r#"{"loaded": false}"#).expect("check json");
        assert!(check_result("res://water.gdshader", &unloaded).is_error);
    }
}
