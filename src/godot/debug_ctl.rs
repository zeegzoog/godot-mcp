//! Breakpoint control on top of Godot's remote debugger.
//!
//! Commands and payloads follow Godot 4.x `remote_debugger.cpp` (checked via the
//! shin2344234 server, which verified them on 4.7.2):
//! `core:breakpoint [source, line, enabled]`, `core:break`, and, only while stopped,
//! `continue` / `step` / `next` / `out` / `get_stack_dump` / `get_stack_frame_vars`.

use crate::error::{McpError, Result};
use crate::godot::debug_session::{DebugSession, GameMessage};
use crate::util::lock;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::broadcast;

const QUERY_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Debug)]
pub struct StopInfo {
    pub reason: String,
    pub error: Option<String>,
    pub can_step: bool,
    pub has_stack: bool,
    pub thread: i64,
    pub id: u64,
}

#[derive(Debug, Default)]
struct State {
    breakpoints: BTreeMap<String, BTreeSet<i64>>,
    stop: Option<StopInfo>,
    stepping: bool,
    stops: u64,
}

pub struct DebugCtl {
    session: Arc<DebugSession>,
    state: Mutex<State>,
}

impl DebugCtl {
    pub fn new(session: Arc<DebugSession>) -> Arc<Self> {
        let ctl = Arc::new(Self {
            session: Arc::clone(&session),
            state: Mutex::new(State::default()),
        });
        let watcher = Arc::clone(&ctl);
        let mut rx = session.subscribe();
        tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(message) => watcher.on_message(&message),
                    Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        });
        ctl
    }

    fn on_message(&self, message: &GameMessage) {
        match message.message.as_str() {
            "debug_enter" => {
                let mut state = lock(&self.state);
                let can_continue = message
                    .data
                    .first()
                    .and_then(Value::as_bool)
                    .unwrap_or(true);
                let text = message.data.get(1).and_then(Value::as_str).unwrap_or("");
                let has_stack = message
                    .data
                    .get(2)
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let thread = message
                    .data
                    .get(3)
                    .and_then(Value::as_i64)
                    .filter(|thread| *thread > 0)
                    .unwrap_or(message.thread);
                let reason = if text == "Breakpoint" && state.stepping {
                    "step"
                } else if text == "Breakpoint" {
                    "breakpoint"
                } else if text == "Breakpoint Statement" {
                    "breakpoint_statement"
                } else if text.is_empty() {
                    "pause"
                } else {
                    "error"
                };
                state.stepping = false;
                state.stops += 1;
                state.stop = Some(StopInfo {
                    reason: reason.into(),
                    error: (reason == "error").then(|| text.to_string()),
                    can_step: can_continue && reason != "error",
                    has_stack,
                    thread,
                    id: state.stops,
                });
            }
            "debug_exit" => {
                lock(&self.state).stop = None;
            }
            _ => {}
        }
    }

    pub fn list_breakpoints(&self) -> Vec<String> {
        let state = lock(&self.state);
        let mut out = Vec::new();
        for (file, lines) in &state.breakpoints {
            for line in lines {
                out.push(format!("{file}:{line}"));
            }
        }
        out
    }

    pub fn remember(&self, file: &str, line: i64, enabled: bool) {
        let mut state = lock(&self.state);
        if enabled {
            state
                .breakpoints
                .entry(file.to_string())
                .or_default()
                .insert(line);
        } else if let Some(lines) = state.breakpoints.get_mut(file) {
            lines.remove(&line);
            if lines.is_empty() {
                state.breakpoints.remove(file);
            }
        }
    }

    pub fn seed(&self, specs: &[String]) {
        for spec in specs {
            if let Some((file, line)) = split_breakpoint(spec) {
                self.remember(&file, line, true);
            }
        }
    }

    pub fn set_breakpoint(&self, file: &str, line: i64, enabled: bool) -> Result<()> {
        self.session.send(
            "core:breakpoint",
            vec![json!(file), json!(line), json!(enabled)],
            None,
        )?;
        self.remember(file, line, enabled);
        Ok(())
    }

    pub fn stop(&self) -> Option<StopInfo> {
        lock(&self.state).stop.clone()
    }

    pub async fn pause(&self, wait: Duration) -> Result<StopInfo> {
        if let Some(stop) = self.stop() {
            return Ok(stop);
        }
        self.session.send("core:break", Vec::new(), None)?;
        self.wait_stopped(wait).await
    }

    pub async fn resume(&self) -> Result<()> {
        let stop = self.require_stopped()?;
        self.session
            .send("continue", Vec::new(), Some(stop.thread))?;
        Ok(())
    }

    /// `step` is `into`, `over`, or `out` (Godot commands `step`, `next`, `out`).
    pub async fn step(&self, kind: &str, wait: Duration) -> Result<StopInfo> {
        let stop = self.require_stopped()?;
        if !stop.can_step {
            return Err(McpError::ToolExecution(
                "this stop cannot be stepped (Godot only allows continue after a script error)"
                    .into(),
            ));
        }
        let command = match kind {
            "into" => "step",
            "over" => "next",
            "out" => "out",
            other => {
                return Err(McpError::Validation(format!(
                    "unknown step kind '{other}' (use into, over, or out)"
                )));
            }
        };
        {
            let mut state = lock(&self.state);
            state.stepping = true;
            state.stop = None;
        }
        self.session.send(command, Vec::new(), Some(stop.thread))?;
        self.wait_stopped(wait).await
    }

    pub async fn stack(&self) -> Result<Vec<Value>> {
        let stop = self.require_stopped()?;
        if !stop.has_stack {
            return Ok(Vec::new());
        }
        let mut rx = self.session.subscribe();
        self.session
            .send("get_stack_dump", Vec::new(), Some(stop.thread))?;
        let message = recv_named(&mut rx, "stack_dump", QUERY_TIMEOUT).await?;
        Ok(parse_stack(&message.data))
    }

    pub async fn variables(&self, frame: i64) -> Result<Vec<Value>> {
        let stop = self.require_stopped()?;
        if !stop.has_stack {
            return Ok(Vec::new());
        }
        let mut rx = self.session.subscribe();
        self.session.send(
            "get_stack_frame_vars",
            vec![json!(frame)],
            Some(stop.thread),
        )?;
        let deadline = tokio::time::Instant::now() + QUERY_TIMEOUT;
        let mut expected = -1_i64;
        let mut vars = Vec::new();
        loop {
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            if left.is_zero() {
                return Err(McpError::Process(format!(
                    "timed out waiting for variables of frame {frame}"
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
                        "timed out waiting for variables of frame {frame}"
                    )));
                }
            };
            if message.message == "stack_frame_vars" {
                expected = message.data.first().and_then(Value::as_i64).unwrap_or(0);
                if expected <= 0 {
                    return Ok(vars);
                }
            } else if message.message == "stack_frame_var" && expected >= 0 {
                vars.push(parse_variable(&message.data));
                if vars.len() as i64 >= expected {
                    return Ok(vars);
                }
            }
        }
    }

    fn require_stopped(&self) -> Result<StopInfo> {
        self.stop().ok_or_else(|| {
            McpError::ToolExecution(
                "the game is running, not stopped. Pause it or set a breakpoint first.".into(),
            )
        })
    }

    async fn wait_stopped(&self, wait: Duration) -> Result<StopInfo> {
        let deadline = tokio::time::Instant::now() + wait;
        loop {
            if let Some(stop) = self.stop() {
                return Ok(stop);
            }
            if self.session.is_closed() {
                return Err(McpError::Process("the game disconnected".into()));
            }
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            if left.is_zero() {
                return Err(McpError::Process(
                    "timed out waiting for the game to stop".into(),
                ));
            }
            tokio::time::sleep(Duration::from_millis(20).min(left)).await;
        }
    }
}

pub fn split_breakpoint(spec: &str) -> Option<(String, i64)> {
    let (file, line) = spec.rsplit_once(':')?;
    let line = line.parse::<i64>().ok()?;
    if file.is_empty() || line < 1 {
        return None;
    }
    Some((file.to_string(), line))
}

fn parse_stack(data: &[Value]) -> Vec<Value> {
    let n = data.first().and_then(Value::as_i64).unwrap_or(0) / 3;
    let mut frames = Vec::new();
    for index in 0..n {
        let base = 1 + (index as usize) * 3;
        if base + 2 >= data.len() {
            break;
        }
        frames.push(json!({
            "file": data[base],
            "line": data[base + 1],
            "function": data[base + 2],
        }));
    }
    frames
}

fn parse_variable(data: &[Value]) -> Value {
    json!({
        "name": data.first().cloned().unwrap_or(Value::Null),
        "scope": data.get(1).cloned().unwrap_or(Value::Null),
        "type": data.get(2).cloned().unwrap_or(Value::Null),
        "value": data.get(3).cloned().unwrap_or(Value::Null),
        "type_hint": data.get(4).cloned().unwrap_or(Value::String(String::new())),
    })
}

async fn recv_named(
    rx: &mut broadcast::Receiver<GameMessage>,
    name: &str,
    wait: Duration,
) -> Result<GameMessage> {
    let deadline = tokio::time::Instant::now() + wait;
    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        if left.is_zero() {
            return Err(McpError::Process(format!(
                "timed out waiting for '{name}' from the game"
            )));
        }
        match tokio::time::timeout(left, rx.recv()).await {
            Ok(Ok(message)) if message.message == name => return Ok(message),
            Ok(Ok(_)) => {}
            Ok(Err(broadcast::error::RecvError::Lagged(_))) => {}
            Ok(Err(broadcast::error::RecvError::Closed)) => {
                return Err(McpError::Process("the game disconnected".into()));
            }
            Err(_) => {
                return Err(McpError::Process(format!(
                    "timed out waiting for '{name}' from the game"
                )));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn breakpoint_specs_split_on_the_last_colon() {
        assert_eq!(
            split_breakpoint("res://a.gd:12"),
            Some(("res://a.gd".into(), 12))
        );
        assert!(split_breakpoint("no-line").is_none());
        assert!(split_breakpoint("res://a.gd:0").is_none());
    }
}
