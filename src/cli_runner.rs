use crate::error::{McpError, Result};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;
use tokio::task::JoinHandle;
use tokio::time::timeout;

const VERSION_TIMEOUT: Duration = Duration::from_secs(10);
const VALIDATE_TIMEOUT: Duration = Duration::from_secs(30);
const CLASS_CHECK_TIMEOUT: Duration = Duration::from_secs(30);
const SCRIPT_OUTPUT_CAP: usize = 64 * 1024;
/// How long to keep reading after the process tree was killed. A grandchild that
/// survived the kill must not hold the tool open.
const READER_DRAIN: Duration = Duration::from_secs(2);
/// After a `SCRIPT ERROR`, this much silence means the main loop is idle and the
/// process is not going to quit on its own. A script error in `_init` leaves Godot running.
const SCRIPT_ERROR_IDLE: Duration = Duration::from_millis(1500);
const IMPORT_TIMEOUT: Duration = Duration::from_secs(120);

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
}

/// What `godot --headless --path <project> --import` printed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportRun {
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub stdout: String,
    pub stderr: String,
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

#[derive(Clone, Debug)]
pub struct CliRunner {
    godot_binary: PathBuf,
}

impl CliRunner {
    pub fn new(godot_binary: PathBuf) -> Self {
        Self { godot_binary }
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
        let dir = create_scratch_dir()?;
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

    /// Launch Godot Editor for a designated project directory
    pub async fn launch_editor(&self, project_path: &Path) -> Result<u32> {
        let mut cmd = Command::new(&self.godot_binary);
        cmd.arg("--editor").arg("--path").arg(project_path);
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
        spawn_detached(&mut cmd, "Godot project")
    }

    /// Runs a `SceneTree` script headless: `godot --headless --path <project> -s <script> [-- args]`.
    ///
    /// On timeout the process group is killed. Stdout and stderr are captured and never inherited,
    /// so they cannot corrupt the MCP stream. `user_args` are placed after `--`.
    pub async fn run_script(
        &self,
        project_path: &Path,
        script_path: &Path,
        user_args: &[String],
        timeout_seconds: u64,
    ) -> Result<ScriptRun> {
        let mut cmd = Command::new(&self.godot_binary);
        cmd.arg("--headless")
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

        let deadline = tokio::time::Instant::now() + Duration::from_secs(timeout_seconds);
        let mut quiet_since: Option<tokio::time::Instant> = None;
        let mut last_len = 0usize;
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
                    let len = buf_len(stdout_buf.as_ref()) + buf_len(stderr_buf.as_ref());
                    if len != last_len {
                        last_len = len;
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
        Ok(ScriptRun {
            exit_code: status.and_then(|status| status.code()),
            timed_out,
            stopped_idle,
            stdout,
            stderr,
            script_errors,
        })
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
fn create_scratch_dir() -> Result<PathBuf> {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    for attempt in 0..16u32 {
        let dir = std::env::temp_dir().join(format!(
            "godot-mcp-class-check-{}-{nanos:x}-{attempt:x}",
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

fn spawn_detached(command: &mut Command, what: &str) -> Result<u32> {
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

    let child = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| McpError::Process(format!("Failed to spawn {what} process: {e}")))?;
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

fn buf_len(buf: &Mutex<Vec<u8>>) -> usize {
    match buf.lock() {
        Ok(guard) => guard.len(),
        Err(poisoned) => poisoned.into_inner().len(),
    }
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

        let pid = CliRunner::new(script)
            .launch_editor(dir.path())
            .await
            .expect("spawn");

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
}
