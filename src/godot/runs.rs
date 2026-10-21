//! Managed Godot process runs.
//!
//! A run listens as the game's remote debugger, injects the runtime autoload through
//! `override.cfg` (never `project.godot`), and restores that file when the process exits.
//! The runtime scripts come from the project's own addon when it has all of them. Otherwise
//! they are copied to a private folder under `.godot/` (which the editor does not scan and
//! version control ignores), and that copy is removed when the run ends.

use crate::error::{McpError, Result};
use crate::godot::debug_ctl::DebugCtl;
use crate::godot::debug_session::{DebugSession, GameMessage};
use crate::util::lock;
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::broadcast;
use uuid::Uuid;

const MARKER: &str = "; godot_mcp: temporary runtime autoload (removed automatically)";
const MAX_LOGS: usize = 2_000;
static NEXT_REQUEST: AtomicI64 = AtomicI64::new(1);

const RUNTIME_FILES: &[(&str, &str)] = &[
    (
        "addons/godot_mcp/runtime/mcp_runtime.gd",
        include_str!("../../addons/godot_mcp/runtime/mcp_runtime.gd"),
    ),
    (
        "addons/godot_mcp/runtime/ops/rt_result.gd",
        include_str!("../../addons/godot_mcp/runtime/ops/rt_result.gd"),
    ),
    (
        "addons/godot_mcp/runtime/ops/rt_live.gd",
        include_str!("../../addons/godot_mcp/runtime/ops/rt_live.gd"),
    ),
    (
        "addons/godot_mcp/runtime/ops/rt_input.gd",
        include_str!("../../addons/godot_mcp/runtime/ops/rt_input.gd"),
    ),
    (
        "addons/godot_mcp/runtime/ops/rt_view.gd",
        include_str!("../../addons/godot_mcp/runtime/ops/rt_view.gd"),
    ),
    (
        "addons/godot_mcp/runtime/ops/rt_output.gd",
        include_str!("../../addons/godot_mcp/runtime/ops/rt_output.gd"),
    ),
];

/// Where the game loads the runtime from.
struct RuntimeSource {
    /// The `res://` path of the autoload script.
    autoload: String,
    /// The private copy to remove when the run ends; none when the project's addon has it.
    copied: Option<PathBuf>,
}

const ADDON_RUNTIME: &str = "addons/godot_mcp/runtime/";
const COPY_ROOT: &str = ".godot/godot_mcp";
/// A private copy older than this belongs to a run that never cleaned up.
const STALE_COPY_AGE: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Clone, Debug)]
pub struct RunInfo {
    pub id: String,
    pub pid: u32,
    pub project: PathBuf,
}

struct LogLine {
    seq: u64,
    level: String,
    text: String,
    stack: Vec<String>,
}

struct Logs {
    seq: u64,
    lines: VecDeque<LogLine>,
    errors: VecDeque<Value>,
}

struct OverrideState {
    path: PathBuf,
    backup_path: PathBuf,
    previous: Option<Vec<u8>>,
    copied: Option<PathBuf>,
}

pub struct Run {
    id: String,
    project: PathBuf,
    scene: String,
    headless: bool,
    pid: AtomicU32,
    session: Arc<DebugSession>,
    debug: Arc<DebugCtl>,
    logs: Mutex<Logs>,
    child: Mutex<Option<Child>>,
    hello: Mutex<Option<Value>>,
    running: AtomicBool,
    exit_code: Mutex<Option<i32>>,
    override_state: OverrideState,
    restored: AtomicBool,
}

pub struct GameHub {
    godot: PathBuf,
    runs: Mutex<Vec<Arc<Run>>>,
}

impl GameHub {
    pub fn new(godot: PathBuf) -> Self {
        Self {
            godot,
            runs: Mutex::new(Vec::new()),
        }
    }

    pub async fn start(
        &self,
        project: &Path,
        scene: Option<&str>,
        breakpoints: &[String],
        headless: bool,
        break_on_errors: bool,
    ) -> Result<RunInfo> {
        let id = Uuid::new_v4().simple().to_string();
        let id = id[..8].to_string();
        let source = ensure_runtime(project, &id)?;
        let override_state = inject_override(project, &source)?;
        let session = match DebugSession::listen().await {
            Ok(session) => session,
            Err(err) => {
                restore_override(&override_state);
                return Err(err);
            }
        };
        session.set_ignore_error_breaks(!break_on_errors);
        let debug = DebugCtl::new(Arc::clone(&session));
        debug.seed(breakpoints);
        let run = Arc::new(Run {
            id: id.clone(),
            project: project.to_path_buf(),
            scene: scene.unwrap_or("").to_string(),
            headless,
            pid: AtomicU32::new(0),
            session: Arc::clone(&session),
            debug,
            logs: Mutex::new(Logs {
                seq: 0,
                lines: VecDeque::new(),
                errors: VecDeque::new(),
            }),
            child: Mutex::new(None),
            hello: Mutex::new(None),
            running: AtomicBool::new(true),
            exit_code: Mutex::new(None),
            override_state,
            restored: AtomicBool::new(false),
        });
        spawn_log_task(Arc::clone(&run));

        let mut command = Command::new(&self.godot);
        if headless {
            command.arg("--headless");
        }
        command.arg("--path").arg(project);
        command.arg("--remote-debug").arg(session.uri());
        if !breakpoints.is_empty() {
            command.arg("--breakpoints").arg(breakpoints.join(","));
        }
        if let Some(scene) = scene {
            command.arg("--scene").arg(scene);
        }
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(unix)]
        command.process_group(0);
        #[cfg(windows)]
        {
            const DETACHED_PROCESS: u32 = 0x0000_0008;
            const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
            command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
        }

        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(err) => {
                run.restore();
                return Err(McpError::Process(format!(
                    "failed to spawn Godot at '{}': {err}",
                    self.godot.display()
                )));
            }
        };
        let pid = child.id().unwrap_or(0);
        run.pid.store(pid, Ordering::Release);
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        *lock(&run.child) = Some(child);
        spawn_pipe(Arc::clone(&run), stdout, "stdout");
        spawn_pipe(Arc::clone(&run), stderr, "stderr");
        spawn_wait(Arc::clone(&run));

        let mut runs = lock(&self.runs);
        runs.push(Arc::clone(&run));
        if runs.len() > 8 {
            if let Some(old) = runs.first().cloned() {
                if !old.running.load(Ordering::Acquire) {
                    runs.remove(0);
                }
            }
        }
        Ok(RunInfo {
            id,
            pid,
            project: project.to_path_buf(),
        })
    }

    pub fn status(&self) -> Vec<Value> {
        lock(&self.runs).iter().map(|run| run.summary()).collect()
    }

    pub async fn stop(&self, run_id: Option<&str>) -> Result<Value> {
        let run = self.select(run_id)?.ok_or_else(|| {
            McpError::ToolExecution("No managed Godot process is running.".into())
        })?;
        run.kill().await;
        Ok(run.summary())
    }

    pub fn select(&self, run_id: Option<&str>) -> Result<Option<Arc<Run>>> {
        let runs = lock(&self.runs);
        if let Some(id) = run_id {
            if id == "editor" {
                return Ok(None);
            }
            return runs
                .iter()
                .rev()
                .find(|run| run.id == id)
                .cloned()
                .map(Some)
                .ok_or_else(|| McpError::ToolExecution(format!("No run with id '{id}'.")));
        }
        Ok(runs.iter().rev().find(|run| run.is_alive()).cloned())
    }

    pub async fn runtime(
        &self,
        run_id: Option<&str>,
        method: &str,
        params: Value,
        wait: Duration,
    ) -> Result<Value> {
        let run = self.select(run_id)?.ok_or_else(|| {
            McpError::ToolExecution(
                "No managed game is running. Start one with godot_run_project (mode \"process\")."
                    .into(),
            )
        })?;
        run.runtime(method, params, wait).await
    }

    pub fn logs(&self, run_id: Option<&str>, since: u64, level: &str) -> Result<Value> {
        let run = self
            .select(run_id)?
            .ok_or_else(|| McpError::ToolExecution("No managed game to read logs from.".into()))?;
        Ok(run.page_logs(since, level))
    }

    pub fn debug(&self, run_id: Option<&str>) -> Result<Arc<Run>> {
        self.select(run_id)?.ok_or_else(|| {
            McpError::ToolExecution(
                "Breakpoint debugging needs a process run (godot_run_project mode \"process\")."
                    .into(),
            )
        })
    }
}

impl Run {
    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn debug_ctl(&self) -> &DebugCtl {
        &self.debug
    }

    pub fn session(&self) -> &DebugSession {
        &self.session
    }

    fn is_alive(&self) -> bool {
        self.running.load(Ordering::Acquire)
    }

    fn summary(&self) -> Value {
        json!({
            "run_id": self.id,
            "pid": self.pid.load(Ordering::Acquire),
            "project": self.project.display().to_string(),
            "scene": self.scene,
            "headless": self.headless,
            "running": self.is_alive(),
            "debugger_connected": self.session.is_connected(),
            "exit_code": lock(&self.exit_code).clone(),
            "stopped": self.debug.stop().map(|stop| json!({
                "reason": stop.reason,
                "error": stop.error,
                "can_step": stop.can_step,
                "has_stack": stop.has_stack,
            })),
        })
    }

    async fn kill(&self) {
        let child = lock(&self.child).take();
        if let Some(mut child) = child {
            let _ = child.start_kill();
            let _ = child.wait().await;
        }
        self.running.store(false, Ordering::Release);
        self.restore();
    }

    fn restore(&self) {
        if self.restored.swap(true, Ordering::AcqRel) {
            return;
        }
        restore_override(&self.override_state);
    }

    pub fn page_logs(&self, since: u64, level: &str) -> Value {
        let logs = lock(&self.logs);
        let lines: Vec<Value> = logs
            .lines
            .iter()
            .filter(|line| line.seq >= since)
            .filter(|line| match level {
                "error" => line.level == "error",
                "warning" => line.level != "info",
                _ => true,
            })
            .take(200)
            .map(|line| {
                json!({
                    "seq": line.seq,
                    "level": line.level,
                    "text": line.text,
                    "stack": line.stack,
                })
            })
            .collect();
        let next = lines
            .last()
            .and_then(|line| line.get("seq"))
            .and_then(Value::as_u64)
            .map(|seq| seq + 1)
            .unwrap_or(since);
        json!({
            "run_id": self.id,
            "running": self.is_alive(),
            "lines": lines,
            "errors": logs.errors.iter().cloned().collect::<Vec<_>>(),
            "next": next,
        })
    }

    pub async fn runtime(&self, method: &str, params: Value, wait: Duration) -> Result<Value> {
        self.wait_hello(Duration::from_secs(8).min(wait)).await?;
        if !self.session.is_connected() {
            return Err(McpError::Process(
                "the game exited before the runtime answered".into(),
            ));
        }
        let id = NEXT_REQUEST.fetch_add(1, Ordering::Relaxed);
        let mut rx = self.session.subscribe();
        let payload = serde_json::to_string(&json!({
            "id": id,
            "method": method,
            "params": params,
        }))?;
        self.session
            .send("mcp:request", vec![Value::String(payload)], None)?;
        let deadline = tokio::time::Instant::now() + wait;
        loop {
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            if left.is_zero() {
                return Err(McpError::Process(format!(
                    "timed out waiting for the game runtime to answer '{method}'"
                )));
            }
            let message = match tokio::time::timeout(left, rx.recv()).await {
                Ok(Ok(message)) => message,
                Ok(Err(broadcast::error::RecvError::Lagged(_))) => continue,
                Ok(Err(broadcast::error::RecvError::Closed)) => {
                    return Err(McpError::Process("the game disconnected".into()));
                }
                Err(_) => {
                    return Err(McpError::Process(format!(
                        "timed out waiting for the game runtime to answer '{method}'"
                    )));
                }
            };
            if message.message != "mcp:reply" {
                continue;
            }
            let Some(text) = message.data.first().and_then(Value::as_str) else {
                continue;
            };
            let parsed: Value = serde_json::from_str(text)?;
            if !json_id_eq(parsed.get("id"), id) {
                continue;
            }
            if let Some(err) = parsed.get("error") {
                return Err(McpError::ToolExecution(err.to_string()));
            }
            return Ok(parsed.get("result").cloned().unwrap_or(Value::Null));
        }
    }

    async fn wait_hello(&self, wait: Duration) -> Result<Value> {
        let deadline = tokio::time::Instant::now() + wait;
        loop {
            if let Some(hello) = lock(&self.hello).clone() {
                return Ok(hello);
            }
            if !self.is_alive() && self.session.is_closed() {
                return Err(McpError::Process(
                    "the game exited before the runtime said hello. Read godot_run_output.".into(),
                ));
            }
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            if left.is_zero() {
                return Err(McpError::Process(
                    "the game did not load the MCP runtime. Install addons/godot_mcp in the project and start it with godot_run_project."
                        .into(),
                ));
            }
            tokio::time::sleep(Duration::from_millis(50).min(left)).await;
        }
    }

    fn push_line(&self, level: &str, text: &str, stack: Vec<String>) {
        if text.is_empty() {
            return;
        }
        let mut logs = lock(&self.logs);
        let seq = logs.seq;
        logs.seq += 1;
        if level != "info" {
            logs.errors.push_back(json!({
                "seq": seq,
                "level": level,
                "message": text,
                "stack": stack,
            }));
            while logs.errors.len() > 200 {
                logs.errors.pop_front();
            }
        }
        logs.lines.push_back(LogLine {
            seq,
            level: level.into(),
            text: text.chars().take(2_000).collect(),
            stack,
        });
        while logs.lines.len() > MAX_LOGS {
            logs.lines.pop_front();
        }
    }

    fn note_hello(&self, payload: Value) {
        *lock(&self.hello) = Some(payload);
    }

    fn note_exit(&self, code: Option<i32>) {
        self.running.store(false, Ordering::Release);
        *lock(&self.exit_code) = Some(code.unwrap_or(-1));
        self.restore();
    }
}

fn spawn_log_task(run: Arc<Run>) {
    let mut rx = run.session.subscribe();
    tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(message) => ingest(&run, &message),
                Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });
}

pub fn json_id_eq(value: Option<&Value>, id: i64) -> bool {
    let Some(value) = value else {
        return false;
    };
    if value.as_i64() == Some(id) {
        return true;
    }
    // Godot's JSON parser stores every number as a float, so a reply echoes 1.0.
    value
        .as_f64()
        .is_some_and(|number| number == id as f64 && number.fract() == 0.0)
}

fn ingest(run: &Run, message: &GameMessage) {
    match message.message.as_str() {
        "mcp:hello" => {
            if let Some(text) = message.data.first().and_then(Value::as_str) {
                if let Ok(payload) = serde_json::from_str::<Value>(text) {
                    run.note_hello(payload);
                }
            }
        }
        "output" => {
            let strings = message.data.first().and_then(Value::as_array);
            if let Some(strings) = strings {
                for item in strings {
                    if let Some(text) = item.as_str() {
                        let level = if text.contains("ERROR") {
                            "error"
                        } else if text.contains("WARNING") {
                            "warning"
                        } else {
                            "info"
                        };
                        run.push_line(level, text, Vec::new());
                    }
                }
            }
        }
        "error" => {
            if let Some((level, text, stack)) = parse_error(&message.data) {
                run.push_line(&level, &text, stack);
            }
        }
        _ => {}
    }
}

fn parse_error(data: &[Value]) -> Option<(String, String, Vec<String>)> {
    if data.len() < 11 {
        return None;
    }
    let file = data.get(4)?.as_str().unwrap_or("");
    let function = data.get(5)?.as_str().unwrap_or("");
    let line = data.get(6)?.as_i64().unwrap_or(0);
    let error = data.get(7).and_then(Value::as_str).unwrap_or("");
    let detail = data.get(8).and_then(Value::as_str).unwrap_or("");
    let warning = data.get(9).and_then(Value::as_bool).unwrap_or(false);
    let stack_size = data.get(10).and_then(Value::as_i64).unwrap_or(0) / 3;
    let mut stack = Vec::new();
    for index in 0..stack_size {
        let base = 11 + (index as usize) * 3;
        if base + 2 >= data.len() {
            break;
        }
        stack.push(format!(
            "{}:{} in {}()",
            data[base].as_str().unwrap_or(""),
            data[base + 2].as_i64().unwrap_or(0),
            data[base + 1].as_str().unwrap_or("")
        ));
    }
    if stack.is_empty() && !file.is_empty() {
        stack.push(format!("{file}:{line} in {function}()"));
    }
    let text = if !error.is_empty() { error } else { detail };
    Some((
        if warning { "warning" } else { "error" }.into(),
        text.into(),
        stack,
    ))
}

fn spawn_pipe(run: Arc<Run>, pipe: Option<impl AsyncRead + Unpin + Send + 'static>, _name: &str) {
    let Some(pipe) = pipe else {
        return;
    };
    tokio::spawn(async move {
        let mut lines = BufReader::new(pipe).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let level = if line.contains("ERROR") {
                "error"
            } else if line.contains("WARNING") {
                "warning"
            } else {
                "info"
            };
            run.push_line(level, &line, Vec::new());
        }
    });
}

fn spawn_wait(run: Arc<Run>) {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_millis(200)).await;
            let status = {
                let mut child = lock(&run.child);
                match child.as_mut() {
                    Some(child) => child.try_wait(),
                    None => break,
                }
            };
            match status {
                Ok(Some(code)) => {
                    run.note_exit(code.code());
                    break;
                }
                Ok(None) => {}
                Err(_) => {
                    run.note_exit(None);
                    break;
                }
            }
        }
    });
}

fn ensure_runtime(project: &Path, run_id: &str) -> Result<RuntimeSource> {
    if RUNTIME_FILES
        .iter()
        .all(|(relative, _)| project.join(relative).is_file())
    {
        return Ok(RuntimeSource {
            autoload: format!("res://{ADDON_RUNTIME}mcp_runtime.gd"),
            copied: None,
        });
    }
    sweep_old_copies(project);
    let relative_dir = format!("{COPY_ROOT}/run-{run_id}");
    let dir = project.join(&relative_dir);
    let written = RUNTIME_FILES.iter().try_for_each(|(relative, contents)| {
        let name = relative.strip_prefix(ADDON_RUNTIME).unwrap_or(relative);
        let path = dir.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, contents)
    });
    if let Err(err) = written {
        remove_copy(&dir);
        return Err(err.into());
    }
    Ok(RuntimeSource {
        autoload: format!("res://{relative_dir}/mcp_runtime.gd"),
        copied: Some(dir),
    })
}

/// Removes the private copies of runs that never cleaned up, found by the age of their main
/// script. A copy younger than that may belong to a run that is still going.
fn sweep_old_copies(project: &Path) {
    let Ok(entries) = std::fs::read_dir(project.join(COPY_ROOT)) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let is_copy = path.is_dir() && entry.file_name().to_string_lossy().starts_with("run-");
        let age = std::fs::metadata(path.join("mcp_runtime.gd"))
            .or_else(|_| std::fs::metadata(&path))
            .and_then(|meta| meta.modified())
            .ok()
            .and_then(|modified| modified.elapsed().ok());
        if is_copy && age.is_some_and(|age| age > STALE_COPY_AGE) {
            let _ = std::fs::remove_dir_all(&path);
        }
    }
}

fn remove_copy(dir: &Path) {
    let _ = std::fs::remove_dir_all(dir);
    prune_empty_parents(dir);
}

/// Removes the empty folders above `path`, up to the folder with `project.godot`. A folder
/// the project already had stays, because it is not empty.
fn prune_empty_parents(path: &Path) {
    let mut current = path.parent().map(Path::to_path_buf);
    while let Some(dir) = current {
        if dir.join("project.godot").is_file() || std::fs::remove_dir(&dir).is_err() {
            break;
        }
        current = dir.parent().map(Path::to_path_buf);
    }
}

fn inject_override(project: &Path, source: &RuntimeSource) -> Result<OverrideState> {
    write_override(project, source).inspect_err(|_| {
        if let Some(dir) = &source.copied {
            remove_copy(dir);
        }
    })
}

fn write_override(project: &Path, source: &RuntimeSource) -> Result<OverrideState> {
    let path = project.join("override.cfg");
    let backup_path = project.join(COPY_ROOT).join("override.cfg.user-backup");
    if path.is_file() {
        let current = std::fs::read(&path)?;
        if String::from_utf8_lossy(&current).contains(MARKER) {
            if backup_path.is_file() {
                std::fs::rename(&backup_path, &path)?;
            } else {
                std::fs::remove_file(&path)?;
            }
        }
    }
    let previous = if path.is_file() {
        let bytes = std::fs::read(&path)?;
        if let Some(parent) = backup_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&backup_path, &bytes)?;
        Some(bytes)
    } else {
        None
    };
    let autoload = format!(
        "{MARKER}\n[autoload]\n\nGodotMcpRuntime=\"{}\"\n",
        source.autoload
    );
    let body = match &previous {
        Some(bytes) => format!(
            "{}\n\n{autoload}",
            String::from_utf8_lossy(bytes).trim_end()
        ),
        None => autoload,
    };
    std::fs::write(&path, body)?;
    Ok(OverrideState {
        path,
        backup_path,
        previous,
        copied: source.copied.clone(),
    })
}

fn restore_override(state: &OverrideState) {
    match &state.previous {
        Some(bytes) => {
            let _ = std::fs::write(&state.path, bytes);
        }
        None => {
            let _ = std::fs::remove_file(&state.path);
        }
    }
    let _ = std::fs::remove_file(&state.backup_path);
    prune_empty_parents(&state.backup_path);
    if let Some(dir) = &state.copied {
        remove_copy(dir);
    }
}

impl Drop for GameHub {
    fn drop(&mut self) {
        for run in lock(&self.runs).iter() {
            if let Some(child) = lock(&run.child).as_mut() {
                let _ = child.start_kill();
            }
            run.restore();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{RUNTIME_FILES, ensure_runtime, inject_override, restore_override};
    use std::path::Path;
    use std::time::{Duration, SystemTime};

    fn project() -> tempfile::TempDir {
        let project = tempfile::tempdir().expect("project");
        std::fs::write(project.path().join("project.godot"), "config_version=5\n").unwrap();
        project
    }

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn a_project_without_the_addon_gets_a_private_copy_inside_dot_godot() {
        let project = project();
        let source = ensure_runtime(project.path(), "abc12345").unwrap();
        assert_eq!(
            source.autoload,
            "res://.godot/godot_mcp/run-abc12345/mcp_runtime.gd"
        );
        let copy = project.path().join(".godot/godot_mcp/run-abc12345");
        assert!(copy.join("mcp_runtime.gd").is_file());
        assert!(copy.join("ops/rt_input.gd").is_file());
        assert!(
            !project.path().join("addons").exists(),
            "the runtime was copied into the project's addons folder"
        );

        let state = inject_override(project.path(), &source).unwrap();
        let config = std::fs::read_to_string(project.path().join("override.cfg")).unwrap();
        assert!(config.contains("temporary runtime autoload"), "{config}");
        assert!(config.contains(&source.autoload), "{config}");
        restore_override(&state);

        assert!(!project.path().join("override.cfg").exists());
        assert!(
            !project.path().join(".godot").exists(),
            "a project that had no .godot folder was left with one"
        );
    }

    #[test]
    fn a_project_whose_addon_has_the_runtime_is_left_alone() {
        let project = project();
        for (relative, _) in RUNTIME_FILES {
            write(&project.path().join(relative), "extends Node\n");
        }
        let source = ensure_runtime(project.path(), "abc12345").unwrap();
        assert_eq!(
            source.autoload,
            "res://addons/godot_mcp/runtime/mcp_runtime.gd"
        );
        assert!(source.copied.is_none());

        let state = inject_override(project.path(), &source).unwrap();
        restore_override(&state);

        for (relative, _) in RUNTIME_FILES {
            assert!(
                project.path().join(relative).is_file(),
                "{relative} was removed"
            );
        }
        assert!(!project.path().join(".godot").exists());
    }

    #[test]
    fn a_partial_addon_is_neither_completed_nor_changed() {
        let project = project();
        let mine = project
            .path()
            .join("addons/godot_mcp/runtime/mcp_runtime.gd");
        write(&mine, "extends Node\n# the project's own\n");

        let source = ensure_runtime(project.path(), "abc12345").unwrap();
        assert!(source.autoload.contains(".godot/godot_mcp/run-abc12345/"));
        assert_eq!(
            std::fs::read_to_string(&mine).unwrap(),
            "extends Node\n# the project's own\n"
        );
        assert!(
            !project.path().join("addons/godot_mcp/runtime/ops").exists(),
            "the project's addon was completed with runtime files"
        );

        let state = inject_override(project.path(), &source).unwrap();
        restore_override(&state);
        assert!(mine.is_file());
        assert!(!project.path().join(".godot").exists());
    }

    #[test]
    fn a_user_override_comes_back_unchanged_and_the_copy_is_gone() {
        let project = project();
        write(
            &project.path().join("override.cfg"),
            "[display]\n\nwindow/size/viewport_width=640\n",
        );
        let source = ensure_runtime(project.path(), "abc12345").unwrap();
        let state = inject_override(project.path(), &source).unwrap();
        let during = std::fs::read_to_string(project.path().join("override.cfg")).unwrap();
        assert!(during.contains("viewport_width=640"), "{during}");
        assert!(during.contains(&source.autoload), "{during}");
        restore_override(&state);

        assert_eq!(
            std::fs::read_to_string(project.path().join("override.cfg")).unwrap(),
            "[display]\n\nwindow/size/viewport_width=640\n"
        );
        assert!(
            !project
                .path()
                .join(".godot/godot_mcp/run-abc12345")
                .exists()
        );
    }

    #[test]
    fn copies_left_by_a_crashed_run_are_swept_when_a_day_old() {
        let project = project();
        let base = project.path().join(".godot/godot_mcp");
        let old = base.join("run-00000000/mcp_runtime.gd");
        let recent = base.join("run-11111111/mcp_runtime.gd");
        write(&old, "extends Node\n");
        write(&recent, "extends Node\n");
        std::fs::File::options()
            .write(true)
            .open(&old)
            .unwrap()
            .set_modified(SystemTime::now() - Duration::from_secs(3 * 24 * 3600))
            .unwrap();

        let source = ensure_runtime(project.path(), "22222222").unwrap();

        assert!(!base.join("run-00000000").exists(), "an old copy was kept");
        assert!(
            recent.is_file(),
            "a recent copy, maybe of a live run, was removed"
        );
        assert!(base.join("run-22222222/mcp_runtime.gd").is_file());
        let state = inject_override(project.path(), &source).unwrap();
        restore_override(&state);
        assert!(recent.is_file());
    }
}
