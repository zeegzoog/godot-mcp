//! Managed Godot process runs.
//!
//! A run listens as the game's remote debugger, injects the runtime autoload through
//! `override.cfg` (never `project.godot`), and restores that file when the process exits.
//! The runtime script is copied into the project only when the addon does not already
//! contain it, and those copied files are removed when the run ends.

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
    planted: Vec<PathBuf>,
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
        let planted = ensure_runtime(project)?;
        let override_state = inject_override(project, planted)?;
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
        let id = Uuid::new_v4().simple().to_string();
        let id = id[..8].to_string();
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

fn ensure_runtime(project: &Path) -> Result<Vec<PathBuf>> {
    let mut planted = Vec::new();
    for (relative, contents) in RUNTIME_FILES {
        let path = project.join(relative);
        if path.is_file() {
            continue;
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, contents)?;
        planted.push(path);
    }
    Ok(planted)
}

fn remove_planted(files: &[PathBuf]) {
    for path in files {
        let _ = std::fs::remove_file(path);
        let mut current = path.parent().map(Path::to_path_buf);
        while let Some(dir) = current {
            if dir.join("project.godot").is_file() || std::fs::remove_dir(&dir).is_err() {
                break;
            }
            current = dir.parent().map(Path::to_path_buf);
        }
    }
}

struct PlantedGuard(Vec<PathBuf>);

impl Drop for PlantedGuard {
    fn drop(&mut self) {
        remove_planted(&self.0);
    }
}

fn inject_override(project: &Path, planted: Vec<PathBuf>) -> Result<OverrideState> {
    let mut planted = PlantedGuard(planted);
    let state = write_override(project, &planted.0)?;
    planted.0.clear();
    Ok(state)
}

fn write_override(project: &Path, planted: &[PathBuf]) -> Result<OverrideState> {
    let path = project.join("override.cfg");
    let backup_path = project.join(".godot/godot_mcp/override.cfg.user-backup");
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
        "{MARKER}\n[autoload]\n\nGodotMcpRuntime=\"res://addons/godot_mcp/runtime/mcp_runtime.gd\"\n"
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
        planted: planted.to_vec(),
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
    remove_planted(&state.planted);
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
    use super::{ensure_runtime, inject_override, restore_override};

    #[test]
    fn a_run_removes_only_the_runtime_files_it_created() {
        let project = tempfile::tempdir().expect("project");
        std::fs::write(project.path().join("project.godot"), "config_version=5\n").unwrap();
        let kept = project
            .path()
            .join("addons/godot_mcp/runtime/mcp_runtime.gd");
        std::fs::create_dir_all(kept.parent().unwrap()).unwrap();
        std::fs::write(&kept, "extends Node\n").unwrap();

        let planted = ensure_runtime(project.path()).unwrap();
        assert!(planted.iter().all(|path| path != &kept));
        assert!(
            project
                .path()
                .join("addons/godot_mcp/runtime/ops/rt_input.gd")
                .is_file()
        );

        let state = inject_override(project.path(), planted).unwrap();
        assert!(
            std::fs::read_to_string(project.path().join("override.cfg"))
                .unwrap()
                .contains("temporary runtime autoload")
        );
        restore_override(&state);

        assert!(!project.path().join("override.cfg").exists());
        assert!(kept.is_file(), "a file the project already had was removed");
        assert!(!project.path().join("addons/godot_mcp/runtime/ops").exists());

        let fresh = tempfile::tempdir().expect("fresh project");
        std::fs::write(fresh.path().join("project.godot"), "config_version=5\n").unwrap();
        let planted = ensure_runtime(fresh.path()).unwrap();
        let state = inject_override(fresh.path(), planted).unwrap();
        restore_override(&state);
        assert!(
            !fresh.path().join("addons").exists(),
            "a project without the addon must not keep the copied runtime"
        );
    }
}
