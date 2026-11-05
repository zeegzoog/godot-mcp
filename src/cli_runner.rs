use crate::error::{McpError, Result};
use crate::progress::{self, ProgressSink};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, Command};
use tokio::task::JoinHandle;
use tokio::time::timeout;

const VERSION_TIMEOUT: Duration = Duration::from_secs(10);
const VALIDATE_TIMEOUT: Duration = Duration::from_secs(30);
const CLASS_CHECK_TIMEOUT: Duration = Duration::from_secs(30);
const SCRIPT_OUTPUT_CAP: usize = 64 * 1024;
/// Where a `godot_run_script` stream that outgrew [`SCRIPT_OUTPUT_CAP`] is kept whole, inside the
/// project's cache folder. The next run replaces the files.
const SCRIPT_LOG_DIR: &str = ".godot/godot-mcp";
const SCRIPT_STDOUT_LOG: &str = "script-run.stdout.log";
const SCRIPT_STDERR_LOG: &str = "script-run.stderr.log";
/// A script printing without end must not fill the disk: its log stops here.
const SCRIPT_LOG_CAP: u64 = 256 * 1024 * 1024;
/// Output reaches the MCP client at most this often; the answer holds every line anyway.
const PROGRESS_OUTPUT_INTERVAL: Duration = Duration::from_millis(250);
/// Without new output, how often the client still hears that the script is running.
const PROGRESS_HEARTBEAT: Duration = Duration::from_secs(5);
const PROGRESS_LINE_CHARS: usize = 500;
/// How long to keep reading after the process tree was killed. A grandchild that
/// survived the kill must not hold the tool open.
const READER_DRAIN: Duration = Duration::from_secs(2);
/// After a `SCRIPT ERROR`, this much silence means the main loop is idle and the
/// process is not going to quit on its own. A script error in `_init` leaves Godot running.
const SCRIPT_ERROR_IDLE: Duration = Duration::from_millis(1500);
const IMPORT_TIMEOUT: Duration = Duration::from_secs(120);
const PRESETS_TIMEOUT: Duration = Duration::from_secs(30);

/// One `SCRIPT ERROR` block from a headless script run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScriptErrorLoc {
    pub message: String,
    pub file: String,
    pub line: u32,
}

/// What `godot --headless -s` printed, and whether the timeout killed it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScriptRun {
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    /// A `SCRIPT ERROR` was printed and the process then sat idle, so it was stopped
    /// instead of waiting out the timeout.
    pub stopped_idle: bool,
    pub stdout: String,
    pub stderr: String,
    pub script_errors: Vec<ScriptErrorLoc>,
    /// All the bytes each stream carried; `stdout` and `stderr` hold the last 64 KiB of them.
    pub stdout_bytes: u64,
    pub stderr_bytes: u64,
    /// The file holding a stream that outgrew what the answer holds.
    pub stdout_log: Option<PathBuf>,
    pub stderr_log: Option<PathBuf>,
    /// Why a stream that outgrew the answer could not be kept whole.
    pub log_error: Option<String>,
    /// The project turned off flushing `print()`, so a killed process loses what Godot still held.
    pub stdout_buffered: bool,
}

/// What the world map process left: its run, the answer JSON and the PNG, when they were written.
#[derive(Debug)]
pub struct WorldMapRun {
    pub run: ScriptRun,
    pub answer: Option<String>,
    pub image: Option<Vec<u8>>,
    /// The map as a JPEG at most 1024 pixels on a side, small enough to return inline.
    pub preview: Option<Vec<u8>>,
}

/// What `godot --headless --path <project> --import` printed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportRun {
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub stdout: String,
    pub stderr: String,
}

/// What a bounded headless Godot run printed: an import or an export.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeadlessRun {
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub stdout: String,
    pub stderr: String,
    pub duration: Duration,
}

/// The kind of export `godot --export-*` makes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExportMode {
    /// Project data only, as a `.pck` or `.zip`: `--export-pack`.
    Pack,
    /// The game with a release export template: `--export-release`.
    Release,
    /// The game with a debug export template: `--export-debug`.
    Debug,
}

impl ExportMode {
    pub fn flag(self) -> &'static str {
        match self {
            Self::Pack => "--export-pack",
            Self::Release => "--export-release",
            Self::Debug => "--export-debug",
        }
    }
}

/// What the engine can create under a built-in class name (CA-11).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClassKind {
    /// A `Node` class that can be instantiated: a valid scene root.
    Scene,
    /// A `Resource` class that can be instantiated: a valid resource class.
    Resource,
    /// Anything else, including a name the engine does not know.
    Other,
}

/// The script the class check runs. Fixed text: the class name reaches it only as a user
/// argument after `--`, never inside the source.
const CLASS_CHECK_SCRIPT: &str = "extends SceneTree

func _init() -> void:
	var args: PackedStringArray = OS.get_cmdline_user_args()
	var name: String = args[0] if args.size() > 0 else \"\"
	var kind: String = \"no\"
	if name != \"\" and ClassDB.class_exists(name) and ClassDB.can_instantiate(name):
		if ClassDB.is_parent_class(name, \"Node\"):
			kind = \"scene\"
		elif ClassDB.is_parent_class(name, \"Resource\"):
			kind = \"resource\"
	print(\"GODOT_MCP_CLASS=\", kind)
	quit()
";

/// The script that reads and changes `export_presets.cfg` with Godot's own `ConfigFile`, as the
/// editor's Export dialog stores it. Fixed text: the request reaches it as `res://request.json`
/// of its throw-away project, never inside the source or on the command line.
const EXPORT_PRESETS_SCRIPT: &str = r#"extends SceneTree

const STRING_FIELDS: Array[String] = ["export_path", "include_filter", "exclude_filter", "custom_features"]


func _init() -> void:
	print("GODOT_MCP_PRESETS=", JSON.stringify(_answer()))
	quit()


func _answer() -> Dictionary:
	var request: Variant = JSON.parse_string(FileAccess.get_file_as_string("res://request.json"))
	if typeof(request) != TYPE_DICTIONARY:
		return {"error": "BAD_REQUEST", "message": "the request could not be read"}
	var file: String = str(request.get("file", ""))
	var config: ConfigFile = ConfigFile.new()
	var exists: bool = FileAccess.file_exists(file)
	if exists:
		var loaded: Error = config.load(file)
		if loaded != OK:
			return {"error": "UNREADABLE", "message": error_string(loaded)}
	if str(request.get("action", "")) == "list":
		var presets: Array = []
		for section: String in _preset_sections(config):
			presets.append(_describe(config, section))
		return {"file_exists": exists, "presets": presets}
	return _change(config, file, request)


# The sections Godot loads: preset.0, preset.1, ... up to the first missing number.
func _preset_sections(config: ConfigFile) -> Array[String]:
	var sections: Array[String] = []
	while config.has_section("preset.%d" % sections.size()):
		sections.append("preset.%d" % sections.size())
	return sections


func _describe(config: ConfigFile, section: String) -> Dictionary:
	var preset: Dictionary = {"index": section.get_slice(".", 1).to_int()}
	for key: String in config.get_section_keys(section):
		preset[key] = _plain(config.get_value(section, key))
	var options: Dictionary = {}
	var options_section: String = section + ".options"
	if config.has_section(options_section):
		for key: String in config.get_section_keys(options_section):
			options[key] = _plain(config.get_value(options_section, key))
	preset["options"] = options
	return preset


func _plain(value: Variant) -> Variant:
	match typeof(value):
		TYPE_NIL, TYPE_BOOL, TYPE_INT, TYPE_FLOAT, TYPE_STRING:
			return value
		TYPE_STRING_NAME:
			return str(value)
		TYPE_PACKED_STRING_ARRAY:
			return Array(value)
	return var_to_str(value)


func _change(config: ConfigFile, file: String, request: Dictionary) -> Dictionary:
	var name: String = str(request.get("name", ""))
	var sections: Array[String] = _preset_sections(config)
	var section: String = ""
	var names: Array = []
	for candidate: String in sections:
		var candidate_name: String = str(config.get_value(candidate, "name", ""))
		names.append(candidate_name)
		if candidate_name == name and section == "":
			section = candidate
	var platform: String = str(request.get("platform", ""))
	var created: bool = false
	if section == "":
		if not bool(request.get("create", false)):
			return {"error": "NOT_FOUND", "names": names}
		section = "preset.%d" % sections.size()
		config.set_value(section, "name", name)
		config.set_value(section, "platform", platform)
		config.set_value(section, "dedicated_server", false)
		config.set_value(section, "custom_features", "")
		config.set_value(section, "export_filter", "all_resources")
		config.set_value(section, "include_filter", "")
		config.set_value(section, "exclude_filter", "")
		config.set_value(section, "export_path", "")
		created = true
	elif platform != "" and platform != str(config.get_value(section, "platform", "")):
		return {"error": "PLATFORM", "platform": str(config.get_value(section, "platform", ""))}
	var changed: Array = []
	for field: String in STRING_FIELDS:
		if request.has(field):
			var text: String = str(request[field])
			if created or str(config.get_value(section, field, "")) != text:
				changed.append(field)
			config.set_value(section, field, text)
	var new_options: Array = []
	var options: Variant = request.get("options", {})
	if typeof(options) == TYPE_DICTIONARY:
		var options_section: String = section + ".options"
		for key: Variant in options:
			var had: bool = config.has_section_key(options_section, key)
			var old: Variant = config.get_value(options_section, key) if had else null
			var value: Variant = _typed(options[key], old)
			if had and typeof(value) != typeof(old):
				return {"error": "TYPE", "option": key, "expected": type_string(typeof(old))}
			if not had:
				new_options.append(key)
			if not had or value != old:
				changed.append("options/" + str(key))
			config.set_value(options_section, key, value)
	var saved: Error = config.save(file)
	if saved != OK:
		return {"error": "UNWRITABLE", "message": error_string(saved)}
	return {"created": created, "changed": changed, "new_options": new_options, "preset": _describe(config, section)}


# JSON has one number type: a whole number is stored as an int unless the option holds a float.
func _typed(value: Variant, old: Variant) -> Variant:
	if typeof(value) == TYPE_FLOAT:
		if typeof(old) == TYPE_FLOAT:
			return value
		if value == floorf(value) and absf(value) < 9.0e15:
			return int(value)
	return value
"#;

/// The script that writes uids back into binary resources. Fixed text: the pairs come from the
/// JSON file named by its first user argument.
const SET_BINARY_UIDS_SCRIPT: &str = r#"extends SceneTree


func _init() -> void:
	var args: PackedStringArray = OS.get_cmdline_user_args()
	var request: Variant = JSON.parse_string(FileAccess.get_file_as_string(args[0] if args.size() > 0 else ""))
	if typeof(request) == TYPE_ARRAY:
		for item: Variant in request:
			if typeof(item) != TYPE_DICTIONARY:
				continue
			var path: String = str(item.get("path", ""))
			var id: int = ResourceUID.text_to_id(str(item.get("uid", "")))
			if path.begins_with("res://") and id != ResourceUID.INVALID_ID:
				print("GODOT_MCP_UID ", path, " ", error_string(ResourceSaver.set_uid(path, id)))
	print("GODOT_MCP_UIDS_DONE")
	quit()
"#;

/// The main script of an import parameter call. Fixed text: the request reaches it as
/// `res://request.json`, and the work is done by the addon's `editor/import_ops.gd`, copied next
/// to it as `res://import_ops.gd`.
const IMPORT_PARAMS_SCRIPT: &str = r#"extends SceneTree


func _init() -> void:
	print("GODOT_MCP_IMPORT=", JSON.stringify(_answer()))
	quit()


func _answer() -> Dictionary:
	var request: Variant = JSON.parse_string(FileAccess.get_file_as_string("res://request.json"))
	if typeof(request) != TYPE_DICTIONARY:
		return {"error": "BAD_REQUEST", "message": "the request could not be read"}
	var ops: GDScript = load("res://import_ops.gd")
	var file: String = str(request.get("file", ""))
	if str(request.get("action", "")) == "get":
		return ops.call("describe", file)
	return ops.call("change", file, request)
"#;

const IMPORT_OPS_SOURCE: &str = include_str!("../addons/godot_mcp/editor/import_ops.gd");

/// What the engine knows about a project setting, from [`CliRunner::setting_type`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SettingType {
    /// The Variant type name, as `type_string` gives it (`Color`, `int`, `String`, ...); `None`
    /// when the engine defines no such setting and `project.godot` holds none.
    pub type_name: Option<String>,
    /// The value converted to that type, as `project.godot` stores it. Only given for a type
    /// that is not a string, number or boolean, and only when the value could be converted.
    pub text: Option<String>,
}

/// The script the setting type check runs. Fixed text: the request reaches it as
/// `res://request.json` of its throw-away project, never inside the source or on the command
/// line. A string becomes a value of another type only as an HTML color for a Color, or as the
/// text Godot writes for that type (`Vector2i(1152, 648)`); never as an Object.
const SETTING_TYPE_SCRIPT: &str = r#"extends SceneTree

const PLAIN_TYPES: Array[int] = [TYPE_STRING, TYPE_BOOL, TYPE_INT, TYPE_FLOAT]


func _init() -> void:
	print("GODOT_MCP_SETTING=", JSON.stringify(_answer()))
	quit()


func _answer() -> Dictionary:
	var request: Variant = JSON.parse_string(FileAccess.get_file_as_string("res://request.json"))
	if typeof(request) != TYPE_DICTIONARY:
		return {"error": "the request could not be read"}
	var key: String = str(request.get("key", ""))
	var value: Variant = request.get("value")
	var current: Variant = null
	if request.has("stored"):
		var stored: String = str(request["stored"]).strip_edges()
		if stored.begins_with("Object("):
			return {"type": "Object"}
		current = str_to_var(stored)
		if current == null:
			return {"error": "the value stored in project.godot could not be read"}
	else:
		var defined: String = _defined_key(key)
		if defined.is_empty():
			return {}
		current = ProjectSettings.get_setting(defined)
	var type: int = typeof(current)
	var answer: Dictionary = {"type": type_string(type)}
	if type in PLAIN_TYPES:
		return answer
	var converted: Variant = _convert(value, type)
	if converted != null:
		answer["text"] = var_to_str(converted)
	return answer


## The setting the engine defines for key; a feature override (`name.mobile`) has the type of
## its base setting.
func _defined_key(key: String) -> String:
	if ProjectSettings.has_setting(key):
		return key
	var last: String = key.get_slice("/", key.get_slice_count("/") - 1)
	if last.contains("."):
		var base: String = key.substr(0, key.rfind("."))
		if ProjectSettings.has_setting(base):
			return base
	return ""


func _convert(value: Variant, type: int) -> Variant:
	if not value is String:
		return null
	var text: String = (value as String).strip_edges()
	match type:
		TYPE_COLOR:
			var missing: Color = Color(-1, -1, -1, -1)
			var color: Color = Color.from_string(text, missing)
			if color != missing:
				return color
		TYPE_STRING_NAME:
			return StringName(value)
		TYPE_NODE_PATH:
			return NodePath(value)
	if type == TYPE_OBJECT or not text.begins_with(type_string(type) + "("):
		return null
	var parsed: Variant = str_to_var(text)
	if typeof(parsed) == type:
		return parsed
	return null
"#;

const SHADER_CHECK_TIMEOUT: Duration = Duration::from_secs(30);
const SHADER_CHECK_MARKER: &str = "GODOT_MCP_SHADER=";

/// The script the shader check runs. Fixed text: the shader path reaches it only as a user
/// argument after `--`, never inside the source. Loading the shader runs the preprocessor
/// (includes, defines); asking for its RID hands the result to the rendering server, whose
/// headless (dummy) renderer compiles it with the engine's shader compiler. That compiler
/// reports an error as a shader error with its file and line, which the logger collects.
const SHADER_CHECK_SCRIPT: &str = "extends SceneTree

class Collector extends Logger:
	var errors: Array = []
	var other: Array = []
	var mutex: Mutex = Mutex.new()

	func _log_error(_function: String, file: String, line: int, code: String, rationale: String, _editor_notify: bool, error_type: int, _script_backtraces: Array[ScriptBacktrace]) -> void:
		mutex.lock()
		if error_type == ERROR_TYPE_SHADER:
			errors.append({\"file\": file, \"line\": line, \"message\": code})
		elif error_type != ERROR_TYPE_WARNING and rationale != \"Shader compilation failed.\":
			other.append(rationale if not rationale.is_empty() else code)
		mutex.unlock()

	func _log_message(_message: String, _error: bool) -> void:
		pass

func _init() -> void:
	var args: PackedStringArray = OS.get_cmdline_user_args()
	var path: String = args[0] if args.size() > 0 else \"\"
	var collector: Collector = Collector.new()
	OS.add_logger(collector)
	var shader: Shader = null
	if path.begins_with(\"res://\"):
		shader = ResourceLoader.load(path, \"Shader\", ResourceLoader.CACHE_MODE_IGNORE) as Shader
	if shader != null:
		shader.get_rid()
	OS.remove_logger(collector)
	print(\"GODOT_MCP_SHADER=\", JSON.stringify({\"loaded\": shader != null, \"errors\": collector.errors, \"other\": collector.other}))
	quit()
";

#[derive(Clone, Debug)]
pub struct CliRunner {
    godot_binary: PathBuf,
    /// The bridge port of the server, given to the editors it launches.
    bridge_port: Option<u16>,
}

impl CliRunner {
    pub fn new(godot_binary: PathBuf) -> Self {
        Self {
            godot_binary,
            bridge_port: None,
        }
    }

    /// Launched editors listen on `port` (as `GODOT_DEBUG_PORT`), so the server reaches the
    /// editor it launched even when that editor's project would derive another port.
    pub fn with_bridge_port(mut self, port: u16) -> Self {
        self.bridge_port = Some(port);
        self
    }

    /// The Godot executable this runner was configured with.
    pub fn binary(&self) -> &Path {
        &self.godot_binary
    }

    /// Retrieve engine version directly via CLI
    pub async fn get_version(&self) -> Result<String> {
        let mut command = Command::new(&self.godot_binary);
        command
            .arg("--version")
            .stdin(Stdio::null())
            .kill_on_drop(true);
        let output = timeout(VERSION_TIMEOUT, command.output())
            .await
            .map_err(|_| {
                McpError::Process(format!(
                    "'{} --version' did not finish within {}s",
                    self.godot_binary.display(),
                    VERSION_TIMEOUT.as_secs()
                ))
            })?
            .map_err(|e| {
                McpError::Process(format!(
                    "Failed to invoke Godot executable at '{}': {e}",
                    self.godot_binary.display()
                ))
            })?;

        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
        } else {
            Err(McpError::Process(format!(
                "Godot --version exited with error: {}",
                String::from_utf8_lossy(&output.stderr)
            )))
        }
    }

    /// Asks the engine whether `class` is a built-in class it can create, and of which kind.
    ///
    /// One bounded call of the Godot executable in a throw-away project of its own: the fixed
    /// script and an empty `project.godot` are written to a new directory under the system's
    /// temporary folder, so no project file is read or written and no project code runs. The class
    /// name must be an identifier and travels as a user argument.
    pub async fn class_kind(&self, class: &str) -> Result<ClassKind> {
        if !crate::paths::is_valid_identifier(class) {
            return Err(McpError::Validation(format!(
                "{class:?} is not a class name: use letters, digits and underscores only"
            )));
        }
        let dir = create_scratch_dir("class-check")?;
        let outcome = self.run_class_check(&dir, class).await;
        let _ = std::fs::remove_dir_all(&dir);
        outcome
    }

    async fn run_class_check(&self, dir: &Path, class: &str) -> Result<ClassKind> {
        std::fs::write(dir.join("project.godot"), "config_version=5\n")?;
        let script = dir.join("class_check.gd");
        std::fs::write(&script, CLASS_CHECK_SCRIPT)?;
        let mut command = Command::new(&self.godot_binary);
        command
            .arg("--headless")
            .arg("--path")
            .arg(dir)
            .arg("--script")
            .arg(&script)
            .arg("--")
            .arg(class)
            .stdin(Stdio::null())
            .kill_on_drop(true);
        let output = timeout(CLASS_CHECK_TIMEOUT, command.output())
            .await
            .map_err(|_| {
                McpError::Process(format!(
                    "Godot did not answer the class check within {}s",
                    CLASS_CHECK_TIMEOUT.as_secs()
                ))
            })?
            .map_err(|e| {
                McpError::Process(format!(
                    "Failed to run the Godot executable at '{}' to check the class: {e}",
                    self.godot_binary.display()
                ))
            })?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        match stdout
            .lines()
            .find_map(|line| line.trim().strip_prefix("GODOT_MCP_CLASS="))
            .map(str::trim)
        {
            Some("scene") => Ok(ClassKind::Scene),
            Some("resource") => Ok(ClassKind::Resource),
            Some("no") => Ok(ClassKind::Other),
            _ => Err(McpError::Process(format!(
                "Godot gave no answer to the class check (exit {})",
                output.status
            ))),
        }
    }

    /// The type a project setting has, and `value` converted to it when it is not a string,
    /// number or boolean (see [`SettingType`]).
    ///
    /// `stored` is the text `project.godot` already holds for the key; without it the engine's
    /// own definition of the setting is used. One bounded call of the Godot executable in a
    /// throw-away project of its own, so no project file is read and no project code runs; the
    /// request travels as a JSON file, never on the command line.
    pub async fn setting_type(
        &self,
        key: &str,
        value: &serde_json::Value,
        stored: Option<&str>,
    ) -> Result<SettingType> {
        let mut request = serde_json::json!({"key": key, "value": value});
        if let Some(stored) = stored {
            request["stored"] = serde_json::Value::String(stored.to_string());
        }
        let dir = create_scratch_dir("setting-type")?;
        let outcome = self.run_setting_type(&dir, &request).await;
        let _ = std::fs::remove_dir_all(&dir);
        outcome
    }

    /// Compiles the shader at `res_path` with the engine's shader compiler in a headless Godot
    /// on the project, and returns what the check script printed after its marker: a JSON
    /// object `{loaded, errors: [{file, line, message}], other: [message]}`.
    pub async fn check_shader(&self, project_path: &Path, res_path: &str) -> Result<String> {
        let dir = create_scratch_dir("shader-check")?;
        let outcome = self.run_shader_check(&dir, project_path, res_path).await;
        let _ = std::fs::remove_dir_all(&dir);
        outcome
    }

    async fn run_setting_type(
        &self,
        dir: &Path,
        request: &serde_json::Value,
    ) -> Result<SettingType> {
        std::fs::write(dir.join("project.godot"), "config_version=5\n")?;
        std::fs::write(dir.join("request.json"), request.to_string())?;
        let script = dir.join("setting_type.gd");
        std::fs::write(&script, SETTING_TYPE_SCRIPT)?;
        let mut command = Command::new(&self.godot_binary);
        command
            .arg("--headless")
            .arg("--path")
            .arg(dir)
            .arg("--script")
            .arg(&script)
            .stdin(Stdio::null())
            .kill_on_drop(true);
        let output = timeout(CLASS_CHECK_TIMEOUT, command.output())
            .await
            .map_err(|_| {
                McpError::Process(format!(
                    "Godot did not answer the setting type check within {}s",
                    CLASS_CHECK_TIMEOUT.as_secs()
                ))
            })?
            .map_err(|e| {
                McpError::Process(format!(
                    "Failed to run the Godot executable at '{}' to check the setting type: {e}",
                    self.godot_binary.display()
                ))
            })?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let answer: serde_json::Value = stdout
            .lines()
            .find_map(|line| line.trim().strip_prefix("GODOT_MCP_SETTING="))
            .and_then(|answer| serde_json::from_str(answer).ok())
            .ok_or_else(|| {
                McpError::Process(format!(
                    "Godot gave no answer to the setting type check (exit {}): {}",
                    output.status,
                    strip_banner(&String::from_utf8_lossy(&output.stderr))
                ))
            })?;
        if let Some(problem) = answer.get("error").and_then(|e| e.as_str()) {
            return Err(McpError::Validation(problem.to_string()));
        }
        let field = |name: &str| {
            answer
                .get(name)
                .and_then(|v| v.as_str())
                .map(str::to_string)
        };
        Ok(SettingType {
            type_name: field("type"),
            text: field("text"),
        })
    }

    async fn run_shader_check(
        &self,
        dir: &Path,
        project_path: &Path,
        res_path: &str,
    ) -> Result<String> {
        let script = dir.join("shader_check.gd");
        std::fs::write(&script, SHADER_CHECK_SCRIPT)?;
        let run = self
            .run_script_with(
                &["--headless"],
                project_path,
                &script,
                &[res_path.to_string()],
                SHADER_CHECK_TIMEOUT.as_secs(),
                None,
            )
            .await?;
        if let Some(answer) = run
            .stdout
            .lines()
            .find_map(|line| line.trim().strip_prefix(SHADER_CHECK_MARKER))
        {
            return Ok(answer.trim().to_string());
        }
        let mut detail = if run.timed_out {
            format!(
                "Godot did not finish the shader check within {}s",
                SHADER_CHECK_TIMEOUT.as_secs()
            )
        } else {
            format!(
                "Godot gave no answer to the shader check (exit {})",
                run.exit_code
                    .map_or_else(|| "unknown".to_string(), |code| code.to_string())
            )
        };
        let output = format!("{}\n{}", run.stdout.trim(), run.stderr.trim());
        let output = output.trim();
        if !output.is_empty() {
            let start = output
                .char_indices()
                .rev()
                .nth(1999)
                .map_or(0, |(index, _)| index);
            detail.push_str(&format!("; its output ends with:\n{}", &output[start..]));
        }
        Err(McpError::Process(detail))
    }

    /// Launch Godot Editor for a designated project directory.
    ///
    /// The handle is returned so the server can tell when the editor exits; dropping it neither
    /// stops nor waits for the editor.
    pub async fn launch_editor(&self, project_path: &Path) -> Result<Child> {
        let mut cmd = Command::new(&self.godot_binary);
        cmd.arg("--editor").arg("--path").arg(project_path);
        if let Some(port) = self.bridge_port {
            cmd.env("GODOT_DEBUG_PORT", port.to_string());
        }
        spawn_detached(&mut cmd, "Godot Editor")
    }

    /// Run the game project or specific scene.
    ///
    /// The game's own output must never reach this process's stdout, which carries the MCP
    /// JSON-RPC stream, so all standard streams are detached.
    pub async fn run_project(&self, project_path: &Path, scene: Option<&str>) -> Result<u32> {
        let mut cmd = Command::new(&self.godot_binary);
        cmd.arg("--path").arg(project_path);
        if let Some(scene_file) = scene {
            cmd.arg(scene_file);
        }
        let child = spawn_detached(&mut cmd, "Godot project")?;
        process_id(&child, "Godot project")
    }

    /// Runs a `SceneTree` script headless: `godot --headless --path <project> -s <script> [-- args]`.
    ///
    /// On timeout the process group is killed. Stdout and stderr are captured and never inherited,
    /// so they cannot corrupt the MCP stream. `user_args` are placed after `--`. `fixed_fps` adds
    /// `--fixed-fps <fps>`: frames no longer wait for the clock, each one advancing `1/fps` of game
    /// time. A stream that outgrows the answer is kept whole in the project's cache folder.
    pub async fn run_script(
        &self,
        project_path: &Path,
        script_path: &Path,
        user_args: &[String],
        timeout_seconds: u64,
        fixed_fps: Option<u32>,
    ) -> Result<ScriptRun> {
        let log_dir = project_path.join(SCRIPT_LOG_DIR);
        for name in [SCRIPT_STDOUT_LOG, SCRIPT_STDERR_LOG] {
            let _ = std::fs::remove_file(log_dir.join(name));
        }
        let fixed_fps = fixed_fps.map(|fps| fps.to_string());
        let mut options = vec!["--headless"];
        if let Some(fps) = &fixed_fps {
            options.extend(["--fixed-fps", fps]);
        }
        let mut run = self
            .run_script_with(
                &options,
                project_path,
                script_path,
                user_args,
                timeout_seconds,
                Some(&log_dir),
            )
            .await?;
        run.stdout_buffered = project_buffers_stdout(project_path);
        Ok(run)
    }

    /// Runs the asset review script on the project in a small window, so the rendering server
    /// draws (a headless Godot has no renderer), with the audio muted: `godot --path <project>
    /// --resolution 64x64 --audio-driver Dummy -s <script> -- <request>`. The request JSON and the
    /// script live in a scratch directory outside the project, removed afterwards; the PNG the
    /// script saved there (`output` in the request) is returned with the run.
    pub async fn review_assets(
        &self,
        project_path: &Path,
        script: &str,
        mut request: serde_json::Value,
        timeout_seconds: u64,
    ) -> Result<(ScriptRun, Option<Vec<u8>>)> {
        let dir = create_scratch_dir("review")?;
        let image = dir.join("review.png");
        request["output"] = serde_json::json!(image.to_string_lossy());
        let outcome = async {
            let request_file = dir.join("request.json");
            std::fs::write(&request_file, request.to_string())?;
            let script_file = dir.join("review_assets.gd");
            std::fs::write(&script_file, script)?;
            let run = self
                .run_script_with(
                    &["--resolution", "64x64", "--audio-driver", "Dummy"],
                    project_path,
                    &script_file,
                    &[request_file.to_string_lossy().into_owned()],
                    timeout_seconds,
                    None,
                )
                .await?;
            Ok::<_, McpError>((run, std::fs::read(&image).ok()))
        }
        .await;
        let _ = std::fs::remove_dir_all(&dir);
        outcome
    }

    /// `godot <options> --path <project> -s <script> [-- args]`, as [`Self::run_script`] describes.
    async fn run_script_with(
        &self,
        options: &[&str],
        project_path: &Path,
        script_path: &Path,
        user_args: &[String],
        timeout_seconds: u64,
        log_dir: Option<&Path>,
    ) -> Result<ScriptRun> {
        let mut cmd = Command::new(&self.godot_binary);
        cmd.args(options)
            .arg("--path")
            .arg(project_path)
            .arg("-s")
            .arg(script_path);
        if !user_args.is_empty() {
            cmd.arg("--");
            for argument in user_args {
                cmd.arg(argument);
            }
        }
        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(unix)]
        cmd.process_group(0);
        #[cfg(windows)]
        {
            const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
            cmd.creation_flags(CREATE_NEW_PROCESS_GROUP);
        }

        let mut child = cmd.spawn().map_err(|err| {
            McpError::Process(format!(
                "failed to spawn Godot at '{}': {err}",
                self.godot_binary.display()
            ))
        })?;
        let pid = child.id().unwrap_or(0);
        let stdout_buf = Arc::new(Mutex::new(Vec::new()));
        let stderr_buf = Arc::new(Mutex::new(Vec::new()));
        let stdout_stream: Arc<Mutex<StreamRecord>> = Arc::default();
        let stderr_stream: Arc<Mutex<StreamRecord>> = Arc::default();
        let stdout_task = tokio::spawn(read_stream(
            child.stdout.take(),
            Arc::clone(&stdout_buf),
            Arc::clone(&stdout_stream),
            log_dir.map(|dir| (dir.to_path_buf(), SCRIPT_STDOUT_LOG)),
        ));
        let stderr_task = tokio::spawn(read_stream(
            child.stderr.take(),
            Arc::clone(&stderr_buf),
            Arc::clone(&stderr_stream),
            log_dir.map(|dir| (dir.to_path_buf(), SCRIPT_STDERR_LOG)),
        ));
        let mut progress = progress::current().map(ScriptProgress::new);

        let deadline = tokio::time::Instant::now() + Duration::from_secs(timeout_seconds);
        let mut quiet_since: Option<tokio::time::Instant> = None;
        let mut last_bytes = 0u64;
        let mut timed_out = false;
        let mut stopped_idle = false;
        let mut status = None;
        let mut wait_error = None;
        loop {
            let now = tokio::time::Instant::now();
            if now >= deadline {
                kill_process_tree(pid);
                let _ = child.start_kill();
                let _ = timeout(Duration::from_secs(2), child.wait()).await;
                timed_out = true;
                break;
            }
            let slice = Duration::from_millis(50).min(deadline - now);
            match timeout(slice, child.wait()).await {
                Ok(Ok(code)) => {
                    status = Some(code);
                    break;
                }
                Ok(Err(err)) => {
                    kill_process_tree(pid);
                    let _ = child.start_kill();
                    wait_error = Some(err);
                    timed_out = true;
                    break;
                }
                Err(_elapsed) => {
                    let (stdout_bytes, stderr_bytes) =
                        (stream_bytes(&stdout_stream), stream_bytes(&stderr_stream));
                    if let Some(progress) = &mut progress {
                        progress.tick(
                            (stdout_bytes, stdout_buf.as_ref()),
                            (stderr_bytes, stderr_buf.as_ref()),
                            timeout_seconds,
                        );
                    }
                    let bytes = stdout_bytes + stderr_bytes;
                    if bytes != last_bytes {
                        last_bytes = bytes;
                        quiet_since = None;
                        continue;
                    }
                    let since = *quiet_since.get_or_insert_with(tokio::time::Instant::now);
                    if since.elapsed() < SCRIPT_ERROR_IDLE {
                        continue;
                    }
                    let stdout_now = text_of(peek_tail(stdout_buf.as_ref()));
                    let stderr_now = text_of(peek_tail(stderr_buf.as_ref()));
                    if script_errors_in(&stdout_now).is_empty()
                        && script_errors_in(&stderr_now).is_empty()
                    {
                        continue;
                    }
                    kill_process_tree(pid);
                    let _ = child.start_kill();
                    let _ = timeout(Duration::from_secs(2), child.wait()).await;
                    stopped_idle = true;
                    break;
                }
            }
        }
        let (stdout, stderr) = finish_readers(
            stdout_task,
            stderr_task,
            stdout_buf.as_ref(),
            stderr_buf.as_ref(),
            timed_out || stopped_idle,
        )
        .await;
        if let Some(err) = wait_error {
            return Err(McpError::Process(format!(
                "failed to wait for Godot: {err}"
            )));
        }
        let mut script_errors = script_errors_in(&stdout);
        script_errors.extend(script_errors_in(&stderr));
        let stdout_stream = take_stream(&stdout_stream);
        let stderr_stream = take_stream(&stderr_stream);
        Ok(ScriptRun {
            exit_code: status.and_then(|status| status.code()),
            timed_out,
            stopped_idle,
            stdout,
            stderr,
            script_errors,
            stdout_bytes: stdout_stream.bytes,
            stderr_bytes: stderr_stream.bytes,
            stdout_log: stdout_stream.log,
            stderr_log: stderr_stream.log,
            log_error: stdout_stream.log_error.or(stderr_stream.log_error),
            stdout_buffered: false,
        })
    }

    /// Runs the world map script on the project in a small window, so the rendering server draws
    /// (a headless Godot has no renderer), with the audio muted: `godot --path <project>
    /// --resolution 64x64 --audio-driver Dummy -s <script> -- <request>`. The request JSON, the
    /// script, the PNG (`output` in the request), its JPEG preview (`preview`) and the answer JSON
    /// (`answer`) live in a scratch
    /// directory outside the project, removed afterwards. The answer is a file rather than a
    /// stdout line because a summary of a few hundred scenes outgrows the captured output.
    pub async fn world_map(
        &self,
        project_path: &Path,
        script: &str,
        mut request: serde_json::Value,
        timeout_seconds: u64,
    ) -> Result<WorldMapRun> {
        let dir = create_scratch_dir("world-map")?;
        let image = dir.join("map.png");
        let answer = dir.join("answer.json");
        let preview = dir.join("preview.jpg");
        request["preview"] = serde_json::json!(preview.to_string_lossy());
        request["output"] = serde_json::json!(image.to_string_lossy());
        request["answer"] = serde_json::json!(answer.to_string_lossy());
        let outcome = async {
            let request_file = dir.join("request.json");
            std::fs::write(&request_file, request.to_string())?;
            let script_file = dir.join("world_map.gd");
            std::fs::write(&script_file, script)?;
            let run = self
                .run_script_with(
                    &["--resolution", "64x64", "--audio-driver", "Dummy"],
                    project_path,
                    &script_file,
                    &[request_file.to_string_lossy().into_owned()],
                    timeout_seconds,
                    None,
                )
                .await?;
            Ok::<_, McpError>(WorldMapRun {
                run,
                answer: std::fs::read_to_string(&answer).ok(),
                image: std::fs::read(&image).ok(),
                preview: std::fs::read(&preview).ok(),
            })
        }
        .await;
        let _ = std::fs::remove_dir_all(&dir);
        outcome
    }

    /// Imports every new or changed resource: `godot --headless --path <project> --import`.
    ///
    /// The process exits when the import finishes, so the caller can tell which sidecars
    /// appeared. On timeout the process is killed.
    pub async fn import_project(&self, project_path: &Path) -> Result<ImportRun> {
        let mut command = Command::new(&self.godot_binary);
        command
            .arg("--headless")
            .arg("--path")
            .arg(project_path)
            .arg("--import")
            .stdin(Stdio::null())
            .kill_on_drop(true);
        match timeout(IMPORT_TIMEOUT, command.output()).await {
            Ok(Ok(output)) => Ok(ImportRun {
                exit_code: output.status.code(),
                timed_out: false,
                stdout: strip_banner(&String::from_utf8_lossy(&output.stdout)),
                stderr: strip_banner(&String::from_utf8_lossy(&output.stderr)),
            }),
            Ok(Err(err)) => Err(McpError::Process(format!(
                "failed to run `godot --headless --path {} --import` ({}): {err}",
                project_path.display(),
                self.godot_binary.display()
            ))),
            Err(_elapsed) => Err(McpError::Process(format!(
                "`godot --headless --path {} --import` did not finish within {}s",
                project_path.display(),
                IMPORT_TIMEOUT.as_secs()
            ))),
        }
    }

    /// Lists or changes the export presets of a project with Godot's own `ConfigFile`.
    ///
    /// `request` names the `export_presets.cfg` (`file`) and the `action` (`list` or `set`). It
    /// is written as JSON next to a fixed script in a throw-away project, so no project code runs
    /// and nothing the caller sent reaches the command line. Returns the script's JSON answer.
    pub async fn export_presets(&self, request: &serde_json::Value) -> Result<serde_json::Value> {
        self.config_script(
            "export-presets",
            &[("export_presets.gd", EXPORT_PRESETS_SCRIPT)],
            request,
            "GODOT_MCP_PRESETS=",
            "the export presets",
        )
        .await
    }

    /// Reads (`action` `get`) or changes (`set`) the `[params]` of a `.import` file (`file`) with
    /// Godot's own `ConfigFile`, through the addon's `editor/import_ops.gd`, the code the editor
    /// bridge uses. Same throw-away project as [`Self::export_presets`]; `copy_from` names another
    /// `.import` file and `params` the options to set. Returns the script's JSON answer.
    pub async fn import_params(&self, request: &serde_json::Value) -> Result<serde_json::Value> {
        self.config_script(
            "import-params",
            &[
                ("import_params.gd", IMPORT_PARAMS_SCRIPT),
                ("import_ops.gd", IMPORT_OPS_SOURCE),
            ],
            request,
            "GODOT_MCP_IMPORT=",
            "the import parameters",
        )
        .await
    }

    /// Runs the first of `files` as the main script of a new throw-away project holding `files`
    /// and `request` as `res://request.json`, and parses the JSON it prints after `marker`.
    async fn config_script(
        &self,
        label: &str,
        files: &[(&str, &str)],
        request: &serde_json::Value,
        marker: &str,
        what: &str,
    ) -> Result<serde_json::Value> {
        let dir = create_scratch_dir(label)?;
        let outcome = self
            .run_config_script(&dir, files, request, marker, what)
            .await;
        let _ = std::fs::remove_dir_all(&dir);
        outcome
    }

    async fn run_config_script(
        &self,
        dir: &Path,
        files: &[(&str, &str)],
        request: &serde_json::Value,
        marker: &str,
        what: &str,
    ) -> Result<serde_json::Value> {
        std::fs::write(dir.join("project.godot"), "config_version=5\n")?;
        std::fs::write(dir.join("request.json"), request.to_string())?;
        for (name, text) in files {
            std::fs::write(dir.join(name), text)?;
        }
        let script = dir.join(files.first().map_or("main.gd", |(name, _)| *name));
        let mut command = Command::new(&self.godot_binary);
        command
            .arg("--headless")
            .arg("--path")
            .arg(dir)
            .arg("--script")
            .arg(&script)
            .stdin(Stdio::null())
            .kill_on_drop(true);
        let output = timeout(PRESETS_TIMEOUT, command.output())
            .await
            .map_err(|_| {
                McpError::Process(format!(
                    "Godot did not read {what} within {}s",
                    PRESETS_TIMEOUT.as_secs()
                ))
            })?
            .map_err(|e| {
                McpError::Process(format!(
                    "Failed to run the Godot executable at '{}' to read {what}: {e}",
                    self.godot_binary.display()
                ))
            })?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        stdout
            .lines()
            .find_map(|line| line.trim().strip_prefix(marker))
            .and_then(|answer| serde_json::from_str(answer).ok())
            .ok_or_else(|| {
                McpError::Process(format!(
                    "Godot gave no answer about {what} (exit {}): {}",
                    output.status,
                    strip_banner(&String::from_utf8_lossy(&output.stderr))
                ))
            })
    }

    /// Writes each `(res path, uid://)` pair into the header of that binary scene or resource of
    /// `project` with `ResourceSaver.set_uid`, which rewrites a compressed file as well. The pairs
    /// reach a fixed script outside the project as a JSON file, never on the command line. The
    /// caller reads the headers back to see which ones took.
    pub async fn set_binary_uids(&self, project: &Path, uids: &[(String, String)]) -> Result<()> {
        let dir = create_scratch_dir("binary-uids")?;
        let outcome = self.run_set_binary_uids(project, &dir, uids).await;
        let _ = std::fs::remove_dir_all(&dir);
        outcome
    }

    async fn run_set_binary_uids(
        &self,
        project: &Path,
        dir: &Path,
        uids: &[(String, String)],
    ) -> Result<()> {
        let request: Vec<serde_json::Value> = uids
            .iter()
            .map(|(path, uid)| serde_json::json!({"path": path, "uid": uid}))
            .collect();
        let request_file = dir.join("request.json");
        std::fs::write(&request_file, serde_json::Value::from(request).to_string())?;
        let script = dir.join("set_binary_uids.gd");
        std::fs::write(&script, SET_BINARY_UIDS_SCRIPT)?;
        let mut command = Command::new(&self.godot_binary);
        command
            .arg("--headless")
            .arg("--path")
            .arg(project)
            .arg("--script")
            .arg(&script)
            .arg("--")
            .arg(&request_file)
            .stdin(Stdio::null())
            .kill_on_drop(true);
        let output = timeout(PRESETS_TIMEOUT, command.output())
            .await
            .map_err(|_| {
                McpError::Process(format!(
                    "Godot did not write the binary resource uids within {}s",
                    PRESETS_TIMEOUT.as_secs()
                ))
            })?
            .map_err(|e| {
                McpError::Process(format!(
                    "Failed to run the Godot executable at '{}' to write binary resource uids: {e}",
                    self.godot_binary.display()
                ))
            })?;
        if String::from_utf8_lossy(&output.stdout).contains("GODOT_MCP_UIDS_DONE") {
            Ok(())
        } else {
            Err(McpError::Process(format!(
                "Godot did not finish writing the binary resource uids (exit {}): {}",
                output.status,
                strip_banner(&String::from_utf8_lossy(&output.stderr))
            )))
        }
    }

    /// `godot --headless --path <project> --import`, bounded by `limit`, keeping the last output.
    pub async fn import_project_within(
        &self,
        project_path: &Path,
        limit: Duration,
    ) -> Result<HeadlessRun> {
        let mut command = Command::new(&self.godot_binary);
        command
            .arg("--headless")
            .arg("--path")
            .arg(project_path)
            .arg("--import");
        self.run_bounded(command, limit).await
    }

    /// `godot --headless --path <project> --export-<mode> <preset> <output>`, bounded by `limit`.
    ///
    /// The caller checks that `preset` cannot be read as an engine option and passes an absolute
    /// `output`, so neither starts with `-`.
    pub async fn export_project(
        &self,
        project_path: &Path,
        mode: ExportMode,
        preset: &str,
        output: &Path,
        limit: Duration,
    ) -> Result<HeadlessRun> {
        let mut command = Command::new(&self.godot_binary);
        command
            .arg("--headless")
            .arg("--path")
            .arg(project_path)
            .arg(mode.flag())
            .arg(preset)
            .arg(output);
        self.run_bounded(command, limit).await
    }

    /// Runs `command` with piped output, keeping the last [`SCRIPT_OUTPUT_CAP`] bytes of each
    /// stream. After `limit` the process tree is killed and the run reports `timed_out`.
    async fn run_bounded(&self, mut command: Command, limit: Duration) -> Result<HeadlessRun> {
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(unix)]
        command.process_group(0);
        #[cfg(windows)]
        {
            const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
            command.creation_flags(CREATE_NEW_PROCESS_GROUP);
        }
        let started = tokio::time::Instant::now();
        let mut child = command.spawn().map_err(|err| {
            McpError::Process(format!(
                "failed to spawn Godot at '{}': {err}",
                self.godot_binary.display()
            ))
        })?;
        let pid = child.id().unwrap_or(0);
        let stdout_buf = Arc::new(Mutex::new(Vec::new()));
        let stderr_buf = Arc::new(Mutex::new(Vec::new()));
        let stdout_task = tokio::spawn(read_tail(
            child.stdout.take(),
            SCRIPT_OUTPUT_CAP,
            Arc::clone(&stdout_buf),
        ));
        let stderr_task = tokio::spawn(read_tail(
            child.stderr.take(),
            SCRIPT_OUTPUT_CAP,
            Arc::clone(&stderr_buf),
        ));
        let (status, timed_out) = match timeout(limit, child.wait()).await {
            Ok(Ok(status)) => (Some(status), false),
            Ok(Err(err)) => {
                kill_process_tree(pid);
                return Err(McpError::Process(format!(
                    "failed to wait for Godot: {err}"
                )));
            }
            Err(_elapsed) => {
                kill_process_tree(pid);
                let _ = child.start_kill();
                let _ = timeout(Duration::from_secs(2), child.wait()).await;
                (None, true)
            }
        };
        let (stdout, stderr) = finish_readers(
            stdout_task,
            stderr_task,
            stdout_buf.as_ref(),
            stderr_buf.as_ref(),
            timed_out,
        )
        .await;
        Ok(HeadlessRun {
            exit_code: status.and_then(|status| status.code()),
            timed_out,
            stdout,
            stderr,
            duration: started.elapsed(),
        })
    }

    /// What a clean `godot --headless --check-only` means: parse errors only.
    pub const HEADLESS_PARSE_CLEAN: &str = "No parse errors (headless check). Analyzer warnings were not checked: no language server is running; use godot_code_diagnostics, which starts one.";

    /// Validate a GDScript file syntax using headless Godot check
    pub async fn validate_script(
        &self,
        project_path: Option<&Path>,
        script_path: &Path,
    ) -> Result<String> {
        let mut cmd = Command::new(&self.godot_binary);
        cmd.arg("--headless").arg("--check-only");

        if let Some(proj) = project_path {
            cmd.arg("--path").arg(proj);
        }

        cmd.arg("--script")
            .arg(script_path)
            .stdin(Stdio::null())
            .kill_on_drop(true);

        let output = timeout(VALIDATE_TIMEOUT, cmd.output())
            .await
            .map_err(|_| {
                McpError::Process(format!(
                    "Godot script validation did not finish within {}s",
                    VALIDATE_TIMEOUT.as_secs()
                ))
            })?
            .map_err(|e| {
                McpError::Process(format!("Failed to run Godot script validation: {e}"))
            })?;

        let stdout = strip_banner(&String::from_utf8_lossy(&output.stdout));
        let stderr = strip_banner(&String::from_utf8_lossy(&output.stderr));

        if output.status.success() && stderr.is_empty() {
            Ok(Self::HEADLESS_PARSE_CLEAN.into())
        } else {
            let mut report = Vec::new();
            if !stdout.is_empty() {
                report.push(format!("Stdout: {stdout}"));
            }
            if !stderr.is_empty() {
                report.push(format!("Stderr: {stderr}"));
            }
            if report.is_empty() {
                report.push(format!("Godot exited with {}", output.status));
            }
            Err(McpError::Validation(report.join("\n")))
        }
    }
}

/// Creates a new, empty directory for one engine call; it is removed by the caller.
fn create_scratch_dir(purpose: &str) -> Result<PathBuf> {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    for attempt in 0..16u32 {
        let dir = std::env::temp_dir().join(format!(
            "godot-mcp-{purpose}-{}-{nanos:x}-{attempt:x}",
            std::process::id()
        ));
        match std::fs::create_dir(&dir) {
            Ok(()) => return Ok(dir),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.into()),
        }
    }
    Err(McpError::Process(
        "no free name for a temporary directory".into(),
    ))
}

fn spawn_detached(command: &mut Command, what: &str) -> Result<Child> {
    // Ctrl+C in the MCP client's terminal reaches every process attached to its console
    // (Windows) or in its foreground process group (Unix), which would close the editor with
    // unsaved work. No console and a process group of its own keep the child out of reach.
    #[cfg(windows)]
    {
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    }
    #[cfg(unix)]
    command.process_group(0);
    #[cfg(windows)]
    make_std_handles_private();

    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| McpError::Process(format!("Failed to spawn {what} process: {e}")))
}

pub(crate) fn process_id(child: &Child, what: &str) -> Result<u32> {
    child
        .id()
        .ok_or_else(|| McpError::Process(format!("Failed to obtain {what} process ID")))
}

/// Windows lets a spawned process inherit every inheritable handle of its parent, whatever it
/// is given as its own standard streams. The server's stdin, stdout and stderr are pipes of the
/// MCP client, so a launched editor would keep them open for as long as it runs, and a client
/// that waits for the server's stderr to close would wait for the editor instead.
#[cfg(windows)]
pub(crate) fn make_std_handles_private() {
    use windows_sys::Win32::Foundation::SetHandleInformation;
    use windows_sys::Win32::Foundation::{HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Console::{
        GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
    };

    for which in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
        // SAFETY: both calls only read or flag handles owned by this process. A missing or
        // invalid handle makes `SetHandleInformation` fail, which is harmless here: the
        // handle is then simply not protected, as before.
        unsafe {
            let handle = GetStdHandle(which);
            if !handle.is_null() && handle != INVALID_HANDLE_VALUE {
                SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0);
            }
        }
    }
}

/// Captured output with the engine's start-up banner removed.
fn text_of(bytes: Vec<u8>) -> String {
    strip_banner(&String::from_utf8_lossy(&bytes))
}

async fn read_tail<R>(pipe: Option<R>, cap: usize, into: Arc<Mutex<Vec<u8>>>)
where
    R: AsyncRead + Unpin,
{
    let Some(mut pipe) = pipe else {
        return;
    };
    let mut chunk = [0u8; 8192];
    loop {
        let n = match pipe.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        let mut buf = match into.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        buf.extend_from_slice(&chunk[..n]);
        if buf.len() > cap {
            let drop_n = buf.len() - cap;
            buf.drain(..drop_n);
        }
    }
}

/// What a script's stream carried beyond the tail kept in memory.
#[derive(Debug, Default)]
struct StreamRecord {
    bytes: u64,
    log: Option<PathBuf>,
    log_error: Option<String>,
}

fn stream_bytes(record: &Mutex<StreamRecord>) -> u64 {
    lock_ignoring_poison(record).bytes
}

fn take_stream(record: &Mutex<StreamRecord>) -> StreamRecord {
    std::mem::take(&mut *lock_ignoring_poison(record))
}

fn lock_ignoring_poison<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// Reads a script's stream as it is written, keeping the last [`SCRIPT_OUTPUT_CAP`] bytes in
/// memory. Once the stream outgrows them and `log` names a directory and a file, everything it
/// carried goes there, up to [`SCRIPT_LOG_CAP`]: an ordinary run writes no file.
async fn read_stream<R>(
    pipe: Option<R>,
    into: Arc<Mutex<Vec<u8>>>,
    record: Arc<Mutex<StreamRecord>>,
    log: Option<(PathBuf, &'static str)>,
) where
    R: AsyncRead + Unpin,
{
    let Some(mut pipe) = pipe else {
        return;
    };
    let mut chunk = [0u8; 8192];
    let mut file: Option<tokio::fs::File> = None;
    let mut logged = 0u64;
    let mut logging_failed = log.is_none();
    loop {
        let n = match pipe.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        // The bytes the log has not seen yet: the whole stream the moment it first overflows.
        let first_part = {
            let mut buf = lock_ignoring_poison(&into);
            buf.extend_from_slice(&chunk[..n]);
            let overflow = buf.len().saturating_sub(SCRIPT_OUTPUT_CAP);
            let first_part =
                (overflow > 0 && file.is_none() && !logging_failed).then(|| buf.clone());
            buf.drain(..overflow);
            first_part
        };
        lock_ignoring_poison(&record).bytes += n as u64;
        if logging_failed {
            continue;
        }
        let written = match (first_part, &log, &mut file) {
            (Some(first_part), Some((dir, name)), _) => {
                let path = dir.join(name);
                match open_log(dir, &path).await {
                    Ok(mut opened) => {
                        let result = write_log(&mut opened, &first_part, &mut logged).await;
                        file = Some(opened);
                        lock_ignoring_poison(&record).log = Some(path);
                        result
                    }
                    Err(err) => Err(err),
                }
            }
            (None, _, Some(open)) => write_log(open, &chunk[..n], &mut logged).await,
            _ => Ok(()),
        };
        if let Err(err) = written {
            logging_failed = true;
            lock_ignoring_poison(&record).log_error =
                Some(format!("could not write the output log: {err}"));
        }
    }
}

async fn open_log(dir: &Path, path: &Path) -> std::io::Result<tokio::fs::File> {
    tokio::fs::create_dir_all(dir).await?;
    tokio::fs::File::create(path).await
}

async fn write_log(
    file: &mut tokio::fs::File,
    bytes: &[u8],
    logged: &mut u64,
) -> std::io::Result<()> {
    let room = SCRIPT_LOG_CAP.saturating_sub(*logged);
    let take = bytes.len().min(usize::try_from(room).unwrap_or(usize::MAX));
    if take == 0 {
        return Ok(());
    }
    file.write_all(&bytes[..take]).await?;
    // Flushed chunk by chunk: a kill may abort this reader at any moment after.
    file.flush().await?;
    *logged += take as u64;
    Ok(())
}

/// Whether the project keeps Godot from flushing stdout on every `print()` in a debug build, the
/// kind of binary that runs scripts: `application/run/flush_stdout_on_print.debug=false`.
/// `run/flush_stdout_on_print` alone does not, because a debug build reads the `.debug` value,
/// which defaults to true. Godot has no command-line flag that overrides it.
fn project_buffers_stdout(project_path: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(project_path.join("project.godot")) else {
        return false;
    };
    let mut in_application = false;
    for line in text.lines().map(str::trim) {
        if line.starts_with('[') {
            in_application = line == "[application]";
        } else if in_application
            && let Some(value) = line.strip_prefix("run/flush_stdout_on_print.debug=")
        {
            return value.trim() == "false";
        }
    }
    false
}

/// Tells the MCP client how a script is doing: its newest output line, or that it is still running.
struct ScriptProgress {
    sink: Arc<ProgressSink>,
    started: tokio::time::Instant,
    last_sent: tokio::time::Instant,
    last_stdout: u64,
    last_stderr: u64,
}

impl ScriptProgress {
    fn new(sink: Arc<ProgressSink>) -> Self {
        let now = tokio::time::Instant::now();
        Self {
            sink,
            started: now,
            last_sent: now,
            last_stdout: 0,
            last_stderr: 0,
        }
    }

    fn tick(
        &mut self,
        (stdout_bytes, stdout): (u64, &Mutex<Vec<u8>>),
        (stderr_bytes, stderr): (u64, &Mutex<Vec<u8>>),
        timeout_seconds: u64,
    ) {
        let since_last = self.last_sent.elapsed();
        if since_last < PROGRESS_OUTPUT_INTERVAL {
            return;
        }
        let line = if stderr_bytes != self.last_stderr {
            last_line(stderr)
        } else if stdout_bytes != self.last_stdout {
            last_line(stdout)
        } else {
            None
        };
        self.last_stdout = stdout_bytes;
        self.last_stderr = stderr_bytes;
        if line.is_none() && since_last < PROGRESS_HEARTBEAT {
            return;
        }
        let elapsed = self.started.elapsed();
        let message = line.unwrap_or_else(|| {
            format!(
                "running for {}s of at most {timeout_seconds}s",
                elapsed.as_secs()
            )
        });
        self.sink.send(elapsed.as_secs_f64(), &message);
        self.last_sent = tokio::time::Instant::now();
    }
}

/// The last whole line that is not blank, cut to [`PROGRESS_LINE_CHARS`].
fn last_line(buf: &Mutex<Vec<u8>>) -> Option<String> {
    let bytes = peek_tail(buf);
    let whole = &bytes[..bytes.iter().rposition(|byte| *byte == b'\n')?];
    let text = String::from_utf8_lossy(whole);
    let line = text.lines().rev().find(|line| !line.trim().is_empty())?;
    Some(line.trim_end().chars().take(PROGRESS_LINE_CHARS).collect())
}

/// Waits for both readers. After a kill, gives up after [`READER_DRAIN`] and aborts a
/// reader that is still blocked, keeping whatever it already stored.
async fn finish_readers(
    stdout_task: JoinHandle<()>,
    stderr_task: JoinHandle<()>,
    stdout_buf: &Mutex<Vec<u8>>,
    stderr_buf: &Mutex<Vec<u8>>,
    bound: bool,
) -> (String, String) {
    let stdout_abort = stdout_task.abort_handle();
    let stderr_abort = stderr_task.abort_handle();
    let joined = async {
        let _ = stdout_task.await;
        let _ = stderr_task.await;
    };
    let finished = if bound {
        timeout(READER_DRAIN, joined).await.is_ok()
    } else {
        joined.await;
        true
    };
    if !finished {
        stdout_abort.abort();
        stderr_abort.abort();
    }
    (
        text_of(take_tail(stdout_buf)),
        text_of(take_tail(stderr_buf)),
    )
}

fn peek_tail(buf: &Mutex<Vec<u8>>) -> Vec<u8> {
    match buf.lock() {
        Ok(guard) => guard.clone(),
        Err(poisoned) => poisoned.into_inner().clone(),
    }
}

fn take_tail(buf: &Mutex<Vec<u8>>) -> Vec<u8> {
    let mut guard = match buf.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    std::mem::take(&mut *guard)
}

/// The `ERROR:` line Godot printed when it could not load the `-s` script `script_res`
/// (`res://...`): a base class other than `SceneTree` or `MainLoop`, a parse error, or a file
/// it could not open. Godot 4.8 still exits with code 0, so the exit code does not tell.
pub fn script_load_failure(text: &str, script_res: &str) -> Option<String> {
    text.lines().find_map(|line| {
        let message = line.trim().strip_prefix("ERROR:")?.trim();
        (message.contains("as it doesn't inherit from SceneTree or MainLoop")
            || message.starts_with("Can't load script:")
            || failed_to_load(message, script_res))
        .then(|| message.to_string())
    })
}

/// `Failed to load script "<path>"` for the run script. Godot names it by `res://` path, or by
/// the absolute path it was given when that path does not start with the project's real path
/// (`/var/...` for a project in `/private/var/...`); other scripts are always `res://` paths.
fn failed_to_load(message: &str, script_res: &str) -> bool {
    let Some(rest) = message.strip_prefix("Failed to load script \"") else {
        return false;
    };
    let Some((path, _)) = rest.split_once('"') else {
        return false;
    };
    if path == script_res {
        return true;
    }
    let relative = script_res.strip_prefix("res://").unwrap_or(script_res);
    !path.starts_with("res://") && path.replace('\\', "/").ends_with(&format!("/{relative}"))
}

/// `SCRIPT ERROR` lines and the `at: ... (file:line)` that follows them.
pub fn script_errors_in(text: &str) -> Vec<ScriptErrorLoc> {
    let lines: Vec<&str> = text.lines().collect();
    let mut errors = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        let Some(message) = line.trim().strip_prefix("SCRIPT ERROR:") else {
            continue;
        };
        let (file, line_no) = lines
            .get(index + 1)
            .and_then(|next| error_location(next))
            .unwrap_or_default();
        errors.push(ScriptErrorLoc {
            message: message.trim().to_string(),
            file,
            line: line_no,
        });
    }
    errors
}

fn error_location(line: &str) -> Option<(String, u32)> {
    let trimmed = line.trim();
    if !trimmed.contains("at:") {
        return None;
    }
    let open = trimmed.rfind('(')?;
    let close = trimmed.rfind(')')?;
    if close <= open {
        return None;
    }
    let inside = &trimmed[open + 1..close];
    let (file, line_no) = inside.rsplit_once(':')?;
    let line_no = line_no.parse().ok()?;
    if file.is_empty() || line_no == 0 {
        return None;
    }
    Some((file.to_string(), line_no))
}

fn kill_process_tree(pid: u32) {
    if pid == 0 {
        return;
    }
    #[cfg(unix)]
    {
        // The child is its own process group (`process_group(0)`), so the id to signal is
        // `-<pid>`. procps `kill` on Linux parses a leading `-` as an option, and
        // `kill -KILL -<pid>` never reaches the group; a grandchild then keeps the stdout
        // pipe open. `-s KILL --` ends options on both procps and BSD kill. Windows uses
        // taskkill /T below.
        let _ = std::process::Command::new("kill")
            .args(["-s", "KILL", "--", &format!("-{pid}")])
            .status();
    }
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("taskkill")
            .args(["/F", "/T", "/PID", &pid.to_string()])
            .status();
    }
}

/// Drops the engine's start-up banner ("Godot Engine v4.x ...") from captured output.
fn strip_banner(text: &str) -> String {
    text.lines()
        .filter(|line| !line.starts_with("Godot Engine v"))
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::{script_errors_in, strip_banner};

    #[test]
    fn banner_is_removed_and_the_rest_is_kept() {
        let raw = "Godot Engine v4.8.dev7.official - https://godotengine.org\n\nSCRIPT ERROR: x\n";
        assert_eq!(strip_banner(raw), "SCRIPT ERROR: x");
        assert_eq!(strip_banner("Godot Engine v4.5\n"), "");
    }

    #[test]
    fn script_error_blocks_keep_the_file_and_line() {
        let text = "\
Godot Engine v4.8.beta1\n\
SCRIPT ERROR: Parse Error: Unexpected identifier.\n\
          at: GDScript::reload (res://scripts/boom.gd:2)\n\
SCRIPT ERROR: Assertion failed: boom\n\
   at: _initialize (res://scripts/hang.gd:4)\n";
        let errors = script_errors_in(&strip_banner(text));
        assert_eq!(errors.len(), 2);
        assert_eq!(errors[0].file, "res://scripts/boom.gd");
        assert_eq!(errors[0].line, 2);
        assert!(errors[0].message.contains("Unexpected identifier"));
        assert_eq!(errors[1].file, "res://scripts/hang.gd");
        assert_eq!(errors[1].line, 4);
        assert!(errors[1].message.contains("boom"));
    }

    /// Ctrl+C in the MCP client's terminal signals its whole process group; a launched editor
    /// must not be part of it.
    #[cfg(unix)]
    #[tokio::test]
    async fn launched_processes_get_their_own_process_group() {
        use super::CliRunner;
        use std::os::unix::fs::PermissionsExt;
        use std::time::Duration;

        fn process_group_of(pid: &str) -> String {
            let output = std::process::Command::new("ps")
                .args(["-o", "pgid=", "-p", pid])
                .output()
                .expect("run ps");
            String::from_utf8_lossy(&output.stdout).trim().to_string()
        }

        let dir = tempfile::tempdir().expect("temp dir");
        let report = dir.path().join("pgid.txt");
        let script = dir.path().join("fake_godot.sh");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\nps -o pgid= -p $$ > '{0}.tmp' && mv '{0}.tmp' '{0}'\n",
                report.display()
            ),
        )
        .expect("write script");
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).expect("chmod");

        let child = CliRunner::new(script)
            .launch_editor(dir.path())
            .await
            .expect("spawn");
        let pid = child.id().expect("pid");

        for _ in 0..100 {
            if report.exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let child_group = std::fs::read_to_string(&report).expect("child reported its group");
        assert_eq!(child_group.trim(), pid.to_string());
        assert_ne!(
            child_group.trim(),
            process_group_of(&std::process::id().to_string())
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_launched_editor_is_given_the_servers_bridge_port() {
        use super::CliRunner;
        use std::os::unix::fs::PermissionsExt;
        use std::time::Duration;

        let dir = tempfile::tempdir().expect("temp dir");
        let report = dir.path().join("port.txt");
        let script = dir.path().join("fake_godot.sh");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\necho \"$GODOT_DEBUG_PORT\" > '{0}.tmp' && mv '{0}.tmp' '{0}'\n",
                report.display()
            ),
        )
        .expect("write script");
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).expect("chmod");

        CliRunner::new(script)
            .with_bridge_port(23456)
            .launch_editor(dir.path())
            .await
            .expect("spawn");

        for _ in 0..100 {
            if report.exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let port = std::fs::read_to_string(&report).expect("child reported its port");
        assert_eq!(port.trim(), "23456");
    }
}
