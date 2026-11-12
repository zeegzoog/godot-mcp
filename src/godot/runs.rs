//! Managed Godot process runs.
//!
//! A run listens as the game's remote debugger, injects the runtime autoload through
//! `override.cfg` (never `project.godot`), and restores that file when the last run of the
//! project exits (see `LEASE_DIR`).
//! The runtime scripts come from the project's own addon when it has all of them. Otherwise
//! they are copied to a private folder under `.godot/` (which the editor does not scan and
//! version control ignores), and that copy is removed when the run ends.

use crate::error::{McpError, Result};
use crate::godot::debug_ctl::DebugCtl;
use crate::godot::debug_session::{DebugSession, GameMessage};
use crate::util::{lock, process_runs};
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::broadcast;
use uuid::Uuid;

const MARKER: &str = "; godot_mcp: temporary runtime autoload (removed automatically)";
const MAX_LOGS: usize = 2_000;
/// The last stderr lines a run keeps for the report of how it ended.
const STDERR_TAIL_LINES: usize = 20;
const STDERR_TAIL_CHARS: usize = 500;
/// How often a call waiting for the game looks whether the game is still there.
const WATCH_INTERVAL: Duration = Duration::from_millis(250);
/// How long a process that closed its debugger connection may take to exit.
const EXIT_GRACE: Duration = Duration::from_secs(5);
/// How long a report waits for the last output of a process that has ended.
const PIPE_DRAIN: Duration = Duration::from_millis(300);
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
        "addons/godot_mcp/runtime/ops/rt_hold.gd",
        include_str!("../../addons/godot_mcp/runtime/ops/rt_hold.gd"),
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
    (
        "addons/godot_mcp/runtime/ops/rt_record.gd",
        include_str!("../../addons/godot_mcp/runtime/ops/rt_record.gd"),
    ),
    (
        "addons/godot_mcp/runtime/ops/rt_registry.gd",
        include_str!("../../addons/godot_mcp/runtime/ops/rt_registry.gd"),
    ),
    (
        "addons/godot_mcp/runtime/ops/rt_project.gd",
        include_str!("../../addons/godot_mcp/runtime/ops/rt_project.gd"),
    ),
    (
        "addons/godot_mcp/runtime/ops/rt_probe.gd",
        include_str!("../../addons/godot_mcp/runtime/ops/rt_probe.gd"),
    ),
    (
        "addons/godot_mcp/runtime/ops/rt_script.gd",
        include_str!("../../addons/godot_mcp/runtime/ops/rt_script.gd"),
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

/// How a run ended. `code` is what the process exited with, `signal` the signal that ended it.
#[derive(Clone, Copy)]
struct Ended {
    /// `server` when this server stopped it, `game` for anything else.
    by: &'static str,
    /// Seconds since the Unix epoch.
    at: u64,
    code: Option<i32>,
    signal: Option<i32>,
}

impl Ended {
    /// One clause that says what ended the process.
    fn describe(&self) -> String {
        if self.by == "server" {
            return match self.signal {
                Some(signal) => format!(
                    "this server stopped it (godot_stop_project), so it ended with signal {signal} ({})",
                    signal_name(signal)
                ),
                None => format!(
                    "this server stopped it (godot_stop_project), and it exited with code {}",
                    self.code.unwrap_or(-1)
                ),
            };
        }
        match (self.signal, self.code) {
            (Some(signal), _) => format!(
                "signal {signal} ({}) ended the process, {}. This server did not stop it",
                signal_name(signal),
                signal_cause(signal)
            ),
            (None, Some(0)) => "the process exited by itself with code 0: the game quit, or its window was closed. This server did not stop it".to_string(),
            (None, Some(code)) => format!(
                "the process exited by itself with code {code}. This server did not stop it"
            ),
            (None, None) => "the process ended, and its exit status could not be read. This server did not stop it".to_string(),
        }
    }
}

/// The name of a signal, so an agent need not know the numbers of this platform.
fn signal_name(signal: i32) -> &'static str {
    #[cfg(unix)]
    {
        match signal {
            libc::SIGHUP => "SIGHUP",
            libc::SIGINT => "SIGINT",
            libc::SIGQUIT => "SIGQUIT",
            libc::SIGILL => "SIGILL",
            libc::SIGTRAP => "SIGTRAP",
            libc::SIGABRT => "SIGABRT",
            libc::SIGBUS => "SIGBUS",
            libc::SIGFPE => "SIGFPE",
            libc::SIGKILL => "SIGKILL",
            libc::SIGSEGV => "SIGSEGV",
            libc::SIGPIPE => "SIGPIPE",
            libc::SIGTERM => "SIGTERM",
            _ => "another signal",
        }
    }
    #[cfg(not(unix))]
    {
        let _ = signal;
        "a signal"
    }
}

/// What a signal says about the cause. A signal does not name its sender, so this stays general.
fn signal_cause(signal: i32) -> &'static str {
    #[cfg(unix)]
    {
        match signal {
            libc::SIGILL
            | libc::SIGTRAP
            | libc::SIGABRT
            | libc::SIGBUS
            | libc::SIGFPE
            | libc::SIGSEGV => "which is how a crash ends a process",
            libc::SIGPIPE => "because it wrote to a pipe nobody reads",
            _ => "which came from outside the game: a person, another program or the system",
        }
    }
    #[cfg(not(unix))]
    {
        let _ = signal;
        "which came from outside the game"
    }
}

#[cfg(unix)]
fn signal_of(status: &ExitStatus) -> Option<i32> {
    use std::os::unix::process::ExitStatusExt;
    status.signal()
}

#[cfg(not(unix))]
fn signal_of(_status: &ExitStatus) -> Option<i32> {
    None
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

struct OverrideState {
    project: PathBuf,
    path: PathBuf,
    backup_path: PathBuf,
    lease: PathBuf,
    copied: Option<PathBuf>,
}

pub struct Run {
    id: String,
    project: PathBuf,
    scene: String,
    /// Arguments placed after `--`, so the game reads them and Godot does not.
    user_args: Vec<String>,
    headless: bool,
    pid: AtomicU32,
    session: Arc<DebugSession>,
    debug: Arc<DebugCtl>,
    logs: Mutex<Logs>,
    child: Mutex<Option<Child>>,
    hello: Mutex<Option<Value>>,
    running: AtomicBool,
    /// Whether the game last said it is held (or was started held).
    held: AtomicBool,
    /// How the run ended; set once.
    ended: Mutex<Option<Ended>>,
    /// The last lines the process wrote to stderr, which outlast the debugger connection.
    stderr_tail: Mutex<VecDeque<String>>,
    /// How many of the process's output pipes are still being read.
    pipes_open: AtomicU32,
    override_state: OverrideState,
    restored: AtomicBool,
}

pub struct GameHub {
    godot: PathBuf,
    runs: Mutex<Vec<Arc<Run>>>,
}

/// How one managed process run is started. `user_args` are placed after `--`.
pub struct ProcessStart<'a> {
    pub scene: Option<&'a str>,
    pub breakpoints: &'a [String],
    pub headless: bool,
    pub break_on_errors: bool,
    pub held: bool,
    pub user_args: &'a [String],
}

impl GameHub {
    pub fn new(godot: PathBuf) -> Self {
        Self {
            godot,
            runs: Mutex::new(Vec::new()),
        }
    }

    pub async fn start(&self, project: &Path, launch: ProcessStart<'_>) -> Result<RunInfo> {
        let id = Uuid::new_v4().simple().to_string();
        let id = id[..8].to_string();
        let source = ensure_runtime(project, &id)?;
        let override_state = inject_override(project, &source, &id)?;
        let session = match DebugSession::listen().await {
            Ok(session) => session,
            Err(err) => {
                restore_override(&override_state);
                return Err(err);
            }
        };
        let ProcessStart {
            scene,
            breakpoints,
            headless,
            break_on_errors,
            held,
            user_args,
        } = launch;
        session.set_ignore_error_breaks(!break_on_errors);
        let debug = DebugCtl::new(Arc::clone(&session));
        debug.seed(breakpoints);
        let run = Arc::new(Run {
            id: id.clone(),
            project: project.to_path_buf(),
            scene: scene.unwrap_or("").to_string(),
            user_args: user_args.to_vec(),
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
            held: AtomicBool::new(held),
            ended: Mutex::new(None),
            stderr_tail: Mutex::new(VecDeque::new()),
            pipes_open: AtomicU32::new(0),
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
        if held {
            // Exact steps need one frame per tick, so the engine's frame rate is fixed to the tick
            // rate and the runtime is told both. Only integers reach the command line.
            let rate = physics_tick_rate(project);
            command.arg("--fixed-fps").arg(rate.to_string());
            command
                .arg("--")
                .arg(format!("--godot-mcp-fixed-fps={rate}"))
                .arg("--godot-mcp-hold");
        } else if !user_args.is_empty() {
            // Everything after this separator is a game argument. A value that starts with `-`
            // cannot become a Godot engine option.
            command.arg("--");
        }
        for argument in user_args {
            command.arg(argument);
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
        spawn_pipe(Arc::clone(&run), stdout, false);
        spawn_pipe(Arc::clone(&run), stderr, true);
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

    /// Stops every managed run that is still alive and returns the summary of each.
    pub async fn stop_all(&self) -> Vec<Value> {
        let alive: Vec<Arc<Run>> = lock(&self.runs)
            .iter()
            .filter(|run| run.is_alive())
            .cloned()
            .collect();
        let mut stopped = Vec::with_capacity(alive.len());
        for run in alive {
            run.kill().await;
            stopped.push(run.summary());
        }
        stopped
    }

    /// The runs that have ended, newest first, each with how it ended.
    pub fn ended_runs(&self) -> Vec<Value> {
        lock(&self.runs)
            .iter()
            .rev()
            .filter(|run| !run.is_alive())
            .map(|run| run.ended_summary())
            .collect()
    }

    /// The newest run that has ended.
    pub fn last_ended(&self) -> Option<Arc<Run>> {
        lock(&self.runs)
            .iter()
            .rev()
            .find(|run| !run.is_alive())
            .cloned()
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

    /// The logs of the named run, of the live run, or else of the run that ended last: what a run
    /// printed is what explains its end.
    pub fn logs_or_last_ended(
        &self,
        run_id: Option<&str>,
        since: u64,
        level: &str,
    ) -> Result<Value> {
        if run_id.is_none() && self.select(None)?.is_none() {
            if let Some(run) = self.last_ended() {
                return Ok(run.page_logs(since, level));
            }
        }
        self.logs(run_id, since, level)
    }

    pub fn logs(&self, run_id: Option<&str>, since: u64, level: &str) -> Result<Value> {
        let run = self
            .select(run_id)?
            .ok_or_else(|| McpError::ToolExecution("No managed game to read logs from.".into()))?;
        Ok(run.page_logs(since, level))
    }

    pub fn debug(&self, run_id: Option<&str>) -> Result<Arc<Run>> {
        match self.select(run_id)? {
            Some(run) if run.is_alive() => Ok(run),
            Some(run) => Err(McpError::RunEnded(run.ended_report(None))),
            None => Err(match self.last_ended().filter(|_| run_id.is_none()) {
                Some(run) => McpError::RunEnded(run.ended_report(None)),
                None => McpError::ToolExecution(
                    "Breakpoint debugging needs a process run (godot_run_project mode \"process\")."
                        .into(),
                ),
            }),
        }
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

    /// Whether the game last reported that it is held.
    pub fn is_held(&self) -> bool {
        self.held.load(Ordering::Acquire)
    }

    pub fn set_held(&self, held: bool) {
        self.held.store(held, Ordering::Release);
    }

    fn summary(&self) -> Value {
        let ended = *lock(&self.ended);
        json!({
            "run_id": self.id,
            "pid": self.pid.load(Ordering::Acquire),
            "project": self.project.display().to_string(),
            "scene": self.scene,
            "user_args": self.user_args,
            "headless": self.headless,
            "running": self.is_alive(),
            "held": self.is_held(),
            "debugger_connected": self.session.is_connected(),
            "adapter": lock(&self.hello)
                .as_ref()
                .and_then(|hello| hello.get("adapter"))
                .cloned(),
            "exit_code": ended.map(|ended| ended.code.unwrap_or(-1)),
            "signal": ended.and_then(|ended| ended.signal),
            "ended_by": ended.map(|ended| ended.by),
            "ended_at": ended.map(|ended| ended.at),
            "how_ended": ended.map(|ended| ended.describe()),
            "last_stderr": ended.map(|_| self.last_stderr()),
            "stopped": self.debug.stop().map(|stop| json!({
                "reason": stop.reason,
                "error": stop.error,
                "can_step": stop.can_step,
                "has_stack": stop.has_stack,
            })),
        })
    }

    /// What `godot_stop_project` lists about a run that had ended before it was called.
    fn ended_summary(&self) -> Value {
        let ended = *lock(&self.ended);
        json!({
            "run_id": self.id,
            "exit_code": ended.map(|ended| ended.code.unwrap_or(-1)),
            "signal": ended.and_then(|ended| ended.signal),
            "ended_by": ended.map(|ended| ended.by),
            "ended_at": ended.map(|ended| ended.at),
            "how_ended": ended.map(|ended| ended.describe()),
            "last_stderr": self.last_stderr(),
        })
    }

    fn last_stderr(&self) -> Vec<String> {
        lock(&self.stderr_tail).iter().cloned().collect()
    }

    /// Says that this run has ended, how, and what it last wrote to stderr. `during` names the
    /// request that was waiting for the game when it went away.
    pub fn ended_report(&self, during: Option<&str>) -> String {
        let ended = *lock(&self.ended);
        let how = ended.map_or_else(
            || "the process has ended, and how is not known yet".to_string(),
            |ended| ended.describe(),
        );
        let ago = ended.map_or(String::new(), |ended| {
            format!(", {} s ago", unix_now().saturating_sub(ended.at))
        });
        let waiting = during.map_or(String::new(), |method| {
            format!(" No answer to '{method}' came, so what it did is not known.")
        });
        let stderr = self.last_stderr();
        let tail = if stderr.is_empty() {
            "It wrote nothing to stderr.".to_string()
        } else {
            format!("Last stderr lines:\n{}", stderr.join("\n"))
        };
        format!(
            "Managed run {} has ended (PID {}{ago}): {how}.{waiting} {tail}",
            self.id,
            self.pid.load(Ordering::Acquire),
        )
    }

    /// The error of a call that cannot reach the game any more, once the process's last output
    /// has been read. A process that closed the connection and still runs gets a moment to exit.
    async fn gone(&self, during: &str) -> McpError {
        let deadline = tokio::time::Instant::now() + EXIT_GRACE;
        while self.is_alive() && tokio::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        if self.is_alive() {
            return McpError::Process(format!(
                "the game closed its debugger connection during '{during}', and its process is still alive (run {}, PID {}). This server did not stop it",
                self.id,
                self.pid.load(Ordering::Acquire),
            ));
        }
        self.drain_pipes().await;
        McpError::RunEnded(self.ended_report(Some(during)))
    }

    /// Output a process wrote just before it ended may still be on its way.
    async fn drain_pipes(&self) {
        let deadline = tokio::time::Instant::now() + PIPE_DRAIN;
        while self.pipes_open.load(Ordering::Acquire) > 0 && tokio::time::Instant::now() < deadline
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    async fn kill(&self) {
        let child = lock(&self.child).take();
        if let Some(mut child) = child {
            // A process that already exited ended by itself, whether or not the watcher saw it.
            if let Ok(Some(status)) = child.try_wait() {
                self.record_end("game", Some(status));
            } else {
                let _ = child.start_kill();
                if let Ok(status) = child.wait().await {
                    self.record_end("server", Some(status));
                }
            }
        }
        self.running.store(false, Ordering::Release);
        self.restore();
        self.drain_pipes().await;
    }

    /// Records how the process ended, once: the first writer (the watcher for a game that quit by
    /// itself, `kill` for a stop) wins. `exit_code` reports a process without one (killed by a
    /// signal) as -1, and `signal` says which.
    fn record_end(&self, who: &'static str, status: Option<ExitStatus>) {
        let mut ended = lock(&self.ended);
        if ended.is_some() {
            return;
        }
        *ended = Some(Ended {
            by: who,
            at: unix_now(),
            code: status.and_then(|status| status.code()),
            signal: status.as_ref().and_then(signal_of),
        });
    }

    fn restore(&self) {
        if self.restored.swap(true, Ordering::AcqRel) {
            return;
        }
        restore_override(&self.override_state);
    }

    /// The next log sequence number. Lines written after this call have `seq >=` the result.
    pub fn log_cursor(&self) -> u64 {
        lock(&self.logs).seq
    }

    /// Warnings and errors recorded at or after `since`. Info lines are left out, and
    /// lines from before `since` are not included.
    pub fn logs_from(&self, since: u64) -> Value {
        let logs = lock(&self.logs);
        let lines: Vec<Value> = logs
            .lines
            .iter()
            .filter(|line| line.seq >= since && line.level != "info")
            .map(|line| {
                json!({
                    "seq": line.seq,
                    "level": line.level,
                    "text": line.text,
                    "stack": line.stack,
                })
            })
            .collect();
        let errors: Vec<Value> = logs
            .errors
            .iter()
            .filter(|line| line.get("seq").and_then(Value::as_u64).unwrap_or(0) >= since)
            .cloned()
            .collect();
        json!({"lines": lines, "errors": errors})
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
            "how_ended": lock(&self.ended).map(|ended| ended.describe()),
            "lines": lines,
            "errors": logs.errors.iter().cloned().collect::<Vec<_>>(),
            "next": next,
        })
    }

    pub async fn runtime(&self, method: &str, params: Value, wait: Duration) -> Result<Value> {
        self.wait_hello(method, Duration::from_secs(8).min(wait))
            .await?;
        if !self.is_alive() || !self.session.is_connected() {
            return Err(self.gone(method).await);
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
                if self.gone_away() {
                    return Err(self.gone(method).await);
                }
                return Err(self.no_answer(method));
            }
            // A reply that arrived before the game went away is read first: this only looks at
            // the process when nothing is waiting to be read.
            let message = match tokio::time::timeout(left.min(WATCH_INTERVAL), rx.recv()).await {
                Ok(Ok(message)) => message,
                Ok(Err(broadcast::error::RecvError::Lagged(_))) => continue,
                Ok(Err(broadcast::error::RecvError::Closed)) => {
                    return Err(self.gone(method).await);
                }
                Err(_) => {
                    if self.gone_away() {
                        return Err(self.gone(method).await);
                    }
                    continue;
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

    fn gone_away(&self) -> bool {
        !self.is_alive() || self.session.is_closed()
    }

    /// The server only stops waiting at a timeout: it ends neither the call in the game nor the
    /// run, so the game may still be working on the request.
    fn no_answer(&self, method: &str) -> McpError {
        McpError::Process(format!(
            "timed out waiting for the game runtime to answer '{method}'. Run {} is still alive (PID {}): this server did not stop it, and did not end the request in the game",
            self.id,
            self.pid.load(Ordering::Acquire),
        ))
    }

    async fn wait_hello(&self, method: &str, wait: Duration) -> Result<Value> {
        let deadline = tokio::time::Instant::now() + wait;
        loop {
            if let Some(hello) = lock(&self.hello).clone() {
                return Ok(hello);
            }
            if !self.is_alive() {
                return Err(self.gone(method).await);
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

    fn note_exit(&self, status: Option<ExitStatus>) {
        self.record_end("game", status);
        self.running.store(false, Ordering::Release);
        self.restore();
    }
}

const DEFAULT_TICK_RATE: u32 = 60;

/// The project's physics tick rate, as its settings file stores it; the engine's default when the
/// file does not set a usable one.
fn physics_tick_rate(project: &Path) -> u32 {
    let Ok(text) = std::fs::read_to_string(project.join("project.godot")) else {
        return DEFAULT_TICK_RATE;
    };
    let mut in_physics = false;
    for line in text.lines().map(str::trim) {
        if let Some(name) = line
            .strip_prefix('[')
            .and_then(|rest| rest.strip_suffix(']'))
        {
            in_physics = name == "physics";
        } else if let Some(value) = line
            .strip_prefix("common/physics_ticks_per_second=")
            .filter(|_| in_physics)
        {
            return value
                .trim()
                .parse::<u32>()
                .ok()
                .filter(|rate| (1..=1000).contains(rate))
                .unwrap_or(DEFAULT_TICK_RATE);
        }
    }
    DEFAULT_TICK_RATE
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

fn spawn_pipe(run: Arc<Run>, pipe: Option<impl AsyncRead + Unpin + Send + 'static>, stderr: bool) {
    let Some(pipe) = pipe else {
        return;
    };
    run.pipes_open.fetch_add(1, Ordering::AcqRel);
    tokio::spawn(async move {
        let mut reader = BufReader::new(pipe);
        let mut bytes = Vec::new();
        // Lines are read as bytes: output that is not UTF-8 must not end this loop. Closing the
        // read end would kill the game with SIGPIPE at its next print.
        loop {
            bytes.clear();
            match reader.read_until(b'\n', &mut bytes).await {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
            let text = String::from_utf8_lossy(&bytes);
            let line = text.trim_end_matches(['\n', '\r']);
            let level = if line.contains("ERROR") {
                "error"
            } else if line.contains("WARNING") {
                "warning"
            } else {
                "info"
            };
            run.push_line(level, line, Vec::new());
            if stderr && !line.is_empty() {
                let mut tail = lock(&run.stderr_tail);
                tail.push_back(line.chars().take(STDERR_TAIL_CHARS).collect());
                while tail.len() > STDERR_TAIL_LINES {
                    tail.pop_front();
                }
            }
        }
        run.pipes_open.fetch_sub(1, Ordering::AcqRel);
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
                    run.note_exit(Some(code));
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

fn inject_override(project: &Path, source: &RuntimeSource, run_id: &str) -> Result<OverrideState> {
    write_override(project, source, run_id).inspect_err(|_| {
        if let Some(dir) = &source.copied {
            remove_copy(dir);
        }
    })
}

/// Every managed run of a project, from this server or another one on the same machine, shares
/// the project's `override.cfg`. Each run holds a lease in this folder while it may need the
/// file, and only the last run to end gives the person's own file back. Without that, a run
/// ending while another game was still starting removed the runtime autoload under it, and that
/// game came up without the MCP runtime.
const LEASE_DIR: &str = ".godot/godot_mcp/override-leases";
/// Serializes changes to `override.cfg` between servers. A lock older than this was left by a
/// server that died while holding it.
const LOCK_FILE: &str = ".godot/godot_mcp/override.lock";
const STALE_LOCK_AGE: Duration = Duration::from_secs(10);
const LOCK_WAIT: Duration = Duration::from_secs(3);

/// Holds the cross-process override lock until dropped.
struct OverrideLock(Option<PathBuf>);

impl Drop for OverrideLock {
    fn drop(&mut self) {
        if let Some(path) = &self.0 {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// Takes the override lock. When another server holds it past `LOCK_WAIT`, the change goes ahead
/// without it: a rare overlap is better than a run that cannot start or a file never restored.
fn lock_override(project: &Path) -> OverrideLock {
    let path = project.join(LOCK_FILE);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let deadline = std::time::Instant::now() + LOCK_WAIT;
    loop {
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(_) => return OverrideLock(Some(path)),
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                let stale = std::fs::metadata(&path)
                    .and_then(|meta| meta.modified())
                    .ok()
                    .and_then(|modified| modified.elapsed().ok())
                    .is_some_and(|age| age > STALE_LOCK_AGE);
                if stale {
                    let _ = std::fs::remove_file(&path);
                    continue;
                }
            }
            Err(_) => return OverrideLock(None),
        }
        if std::time::Instant::now() >= deadline {
            return OverrideLock(None);
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// The leases of runs that still need `override.cfg`. A lease names the server process that
/// holds it; one whose server is gone (it crashed before restoring) is removed.
fn live_leases(project: &Path) -> usize {
    let Ok(entries) = std::fs::read_dir(project.join(LEASE_DIR)) else {
        return 0;
    };
    let mut live = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        let owner = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| text.trim().parse::<u32>().ok());
        let alive = owner.is_some_and(|pid| pid == std::process::id() || process_runs(pid));
        if alive {
            live += 1;
        } else {
            let _ = std::fs::remove_file(&path);
        }
    }
    live
}

fn write_override(project: &Path, source: &RuntimeSource, run_id: &str) -> Result<OverrideState> {
    let path = project.join("override.cfg");
    let backup_path = project.join(COPY_ROOT).join("override.cfg.user-backup");
    let lease = project.join(LEASE_DIR).join(format!("run-{run_id}"));
    let _guard = lock_override(project);
    // Drops the leases of servers that died, so their marker file is treated as stale below.
    live_leases(project);
    // The person's own file: the current one, or, when the current one is ours (another run's,
    // live or left by a crash), the backup taken when the first run replaced it.
    let current = if path.is_file() {
        Some(std::fs::read(&path)?)
    } else {
        None
    };
    let original = match current {
        Some(bytes) if String::from_utf8_lossy(&bytes).contains(MARKER) => {
            if backup_path.is_file() {
                Some(std::fs::read(&backup_path)?)
            } else {
                None
            }
        }
        Some(bytes) => {
            if let Some(parent) = backup_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&backup_path, &bytes)?;
            Some(bytes)
        }
        None => {
            let _ = std::fs::remove_file(&backup_path);
            None
        }
    };
    if let Some(parent) = lease.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&lease, std::process::id().to_string())?;
    let autoload = format!(
        "{MARKER}\n[autoload]\n\nGodotMcpRuntime=\"{}\"\n",
        source.autoload
    );
    let body = match &original {
        Some(bytes) => format!(
            "{}\n\n{autoload}",
            String::from_utf8_lossy(bytes).trim_end()
        ),
        None => autoload,
    };
    if let Err(err) = std::fs::write(&path, body) {
        let _ = std::fs::remove_file(&lease);
        return Err(err.into());
    }
    Ok(OverrideState {
        project: project.to_path_buf(),
        path,
        backup_path,
        lease,
        copied: source.copied.clone(),
    })
}

/// Ends this run's lease. While another run still holds one, `override.cfg` keeps the runtime
/// autoload; the last run puts the person's file back (or removes ours when there was none).
fn restore_override(state: &OverrideState) {
    {
        let _guard = lock_override(&state.project);
        let _ = std::fs::remove_file(&state.lease);
        if live_leases(&state.project) == 0 {
            if let Ok(bytes) = std::fs::read(&state.backup_path) {
                let _ = std::fs::write(&state.path, bytes);
                let _ = std::fs::remove_file(&state.backup_path);
            } else if std::fs::read(&state.path)
                .is_ok_and(|bytes| String::from_utf8_lossy(&bytes).contains(MARKER))
            {
                let _ = std::fs::remove_file(&state.path);
            }
        }
    }
    prune_empty_parents(&state.lease);
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
    use super::{
        RUNTIME_FILES, ensure_runtime, inject_override, physics_tick_rate, restore_override,
    };
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
    fn the_tick_rate_comes_from_the_physics_section_or_defaults_to_sixty() {
        let rate = |text: &str| {
            let project = tempfile::tempdir().unwrap();
            std::fs::write(project.path().join("project.godot"), text).unwrap();
            physics_tick_rate(project.path())
        };
        assert_eq!(rate("config_version=5\n"), 60);
        assert_eq!(
            rate("[physics]\n\ncommon/physics_ticks_per_second=30\n"),
            30
        );
        assert_eq!(
            rate("[physics]\n\ncommon/physics_ticks_per_second = 30\n"),
            60,
            "only the form the editor writes is read"
        );
        assert_eq!(
            rate("[display]\n\ncommon/physics_ticks_per_second=30\n"),
            60,
            "a key in another section is not the setting"
        );
        for unusable in ["0", "-5", "abc", "30.5", "100000"] {
            assert_eq!(
                rate(&format!(
                    "[physics]\n\ncommon/physics_ticks_per_second={unusable}\n"
                )),
                60,
                "{unusable}"
            );
        }
        let missing = tempfile::tempdir().unwrap();
        assert_eq!(physics_tick_rate(missing.path()), 60);
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
        assert!(copy.join("ops/rt_record.gd").is_file());
        assert!(
            !project.path().join("addons").exists(),
            "the runtime was copied into the project's addons folder"
        );

        let state = inject_override(project.path(), &source, "abc12345").unwrap();
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

        let state = inject_override(project.path(), &source, "abc12345").unwrap();
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
    fn a_project_missing_the_record_script_gets_the_servers_copy() {
        let project = project();
        for (relative, _) in RUNTIME_FILES {
            if relative.ends_with("rt_record.gd") {
                continue;
            }
            write(&project.path().join(relative), "extends Node\n");
        }
        let source = ensure_runtime(project.path(), "abc12345").unwrap();
        assert!(
            source.copied.is_some(),
            "an addon that lacks a runtime file must not be used in place of this server's copy"
        );
        assert!(source.autoload.contains(".godot/godot_mcp/"));
        let state = inject_override(project.path(), &source, "abc12345").unwrap();
        restore_override(&state);
        assert!(
            !project
                .path()
                .join("addons/godot_mcp/runtime/ops/rt_record.gd")
                .exists(),
            "the project's addon was completed with the missing file"
        );
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

        let state = inject_override(project.path(), &source, "abc12345").unwrap();
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
        let state = inject_override(project.path(), &source, "abc12345").unwrap();
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
    fn the_runtime_autoload_stays_until_the_last_overlapping_run_ends() {
        let project = project();
        let user = "[display]\n\nwindow/size/viewport_width=640\n";
        let override_path = project.path().join("override.cfg");
        write(&override_path, user);
        let first_source = ensure_runtime(project.path(), "aaaa1111").unwrap();
        let first = inject_override(project.path(), &first_source, "aaaa1111").unwrap();
        let second_source = ensure_runtime(project.path(), "bbbb2222").unwrap();
        let second = inject_override(project.path(), &second_source, "bbbb2222").unwrap();
        let during = std::fs::read_to_string(&override_path).unwrap();
        assert!(during.contains(&second_source.autoload), "{during}");
        assert_eq!(during.matches("viewport_width=640").count(), 1, "{during}");

        // The first run ends while the second game may still be reading the file.
        restore_override(&first);
        let after_first = std::fs::read_to_string(&override_path).unwrap();
        assert!(after_first.contains("GodotMcpRuntime="), "{after_first}");
        assert!(after_first.contains("viewport_width=640"), "{after_first}");

        restore_override(&second);
        assert_eq!(std::fs::read_to_string(&override_path).unwrap(), user);
        assert!(!project.path().join(".godot").exists());
    }

    #[test]
    fn a_lease_left_by_a_server_that_is_gone_does_not_keep_the_autoload() {
        let project = project();
        let lease = project.path().join(super::LEASE_DIR).join("run-dead0000");
        // No process has this id: Linux, macOS and Windows keep ids far below it.
        write(&lease, "4000000000");
        let source = ensure_runtime(project.path(), "abc12345").unwrap();
        let state = inject_override(project.path(), &source, "abc12345").unwrap();
        restore_override(&state);
        assert!(!lease.exists(), "the dead server's lease was kept");
        assert!(!project.path().join("override.cfg").exists());
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
        let state = inject_override(project.path(), &source, "abc12345").unwrap();
        restore_override(&state);
        assert!(recent.is_file());
    }
}
