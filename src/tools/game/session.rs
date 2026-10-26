//! Reaching the game: a managed run or the editor's Play session, with the time each kind of
//! call may take.

use crate::error::McpError;
use crate::tools::ToolManager;
use serde_json::{Value, json};
use std::time::Duration;

impl ToolManager {
    pub(crate) async fn runtime_call(
        &self,
        run_id: Option<&str>,
        method: &str,
        params: Value,
    ) -> Result<Value, McpError> {
        if run_id != Some("editor") {
            match self.games.select(run_id) {
                Ok(Some(run)) => return held_aware_call(&run, method, params).await,
                Ok(None) => {}
                Err(error) => return Err(error),
            }
        }
        if method == "game.record" {
            return editor_runtime(self, method, params, RECORD_WAIT)
                .await
                .map_err(record_timeout);
        }
        editor_runtime(self, method, params, Duration::from_secs(20)).await
    }
}

/// How long a held step may take before the server stops waiting. The game still finishes it.
const HELD_STEP_WAIT: Duration = Duration::from_secs(60);

/// How long a recording may take. Capturing is at most 480 process frames. The PNG files are
/// written after the last frame and before the runtime answers, so this wait covers that disk
/// time. The 512 MB pixel cap keeps the write inside it.
const RECORD_WAIT: Duration = Duration::from_secs(120);

/// A runtime call that knows whether the game is held: a held step gets the long wait, a timeout
/// of one says the game is still running it, and the answer to a physics call keeps the run's
/// idea of "held" current.
async fn held_aware_call(
    run: &crate::godot::Run,
    method: &str,
    params: Value,
) -> Result<Value, McpError> {
    let physics = method == "game.physics";
    let recording = method == "game.record";
    let was_held = physics && run.is_held();
    let wait = if recording {
        RECORD_WAIT
    } else if was_held {
        HELD_STEP_WAIT
    } else {
        Duration::from_secs(20)
    };
    let outcome = run.runtime(method, params, wait).await;
    if physics {
        if let Some(held) = outcome
            .as_ref()
            .ok()
            .and_then(|value| value.get("held"))
            .and_then(Value::as_bool)
        {
            run.set_held(held);
        }
    }
    outcome.map_err(|error| {
        if recording {
            record_timeout(error)
        } else {
            held_step_timeout(was_held, error)
        }
    })
}

/// A timeout while a held step runs is not a failure of the game: it goes on and holds again.
fn held_step_timeout(was_held: bool, error: McpError) -> McpError {
    match error {
        McpError::Process(message) if was_held && message.starts_with("timed out") => {
            McpError::Process(format!(
                "the held step did not finish within {} s ({message}). The game finishes it and holds again. Read godot_game_physics with action status before stepping again, or lower ticks or max",
                HELD_STEP_WAIT.as_secs()
            ))
        }
        other => other,
    }
}

/// A recording that outlives its wait is still writing the PNG files. That is not a held step.
fn record_timeout(error: McpError) -> McpError {
    match error {
        McpError::Process(message) if message.starts_with("timed out") => {
            McpError::Process(format!(
                "timed out while the game was writing the recorded PNG files ({message}). The frames were already captured. Wait and look in the directory, or record fewer or smaller frames"
            ))
        }
        other => other,
    }
}

/// The debugger session can connect before the runtime registers its capture. Never
/// send input or a mutating request until hello confirms that the destination is ready.
async fn ready_editor_session(tools: &ToolManager) -> Result<i64, McpError> {
    let wait = async {
        loop {
            let status = tools.bridge.send_command("game_status", json!({})).await?;
            // The relay orders active sessions newest first. Do not fall back to an
            // older game while a newer game is still starting.
            if let Some(session) = status
                .get("sessions")
                .and_then(Value::as_array)
                .and_then(|sessions| sessions.first())
            {
                if session
                    .get("hello")
                    .and_then(Value::as_str)
                    .is_some_and(|hello| !hello.is_empty())
                {
                    if let Some(id) = session
                        .get("session")
                        .and_then(Value::as_i64)
                        .filter(|id| *id >= 0)
                    {
                        return Ok(id);
                    }
                }
            }
            let state = tools
                .bridge
                .send_command("get_run_status", json!({}))
                .await?;
            if state.get("running").and_then(Value::as_bool) != Some(true) {
                return Err(McpError::Process(
                    "No game started from the editor is running.".into(),
                ));
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    };
    tokio::time::timeout(Duration::from_secs(20), wait).await.map_err(|_| {
        McpError::Process("timed out waiting for the editor game's runtime hello; check the game for startup errors and ensure the matching Godot MCP addon is enabled".into())
    })?
}

async fn editor_runtime(
    tools: &ToolManager,
    method: &str,
    params: Value,
    wait: Duration,
) -> Result<Value, McpError> {
    let mut events = tools.bridge.subscribe_events();
    let session = ready_editor_session(tools).await?;
    let id = NEXT_EDITOR.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let payload = serde_json::to_string(&json!({"id": id, "method": method, "params": params}))?;
    tools
        .bridge
        .send_command(
            "game_send",
            json!({"command": "mcp:request", "data": [payload], "session": session}),
        )
        .await?;
    let deadline = tokio::time::Instant::now() + wait;
    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        if left.is_zero() {
            return Err(McpError::Process(
                "timed out waiting for the editor's running game".to_string(),
            ));
        }
        let event = match tokio::time::timeout(left, events.recv()).await {
            Ok(Some(event)) => event,
            Ok(None) => {
                return Err(McpError::Process(
                    "the editor bridge closed while waiting for the game".to_string(),
                ));
            }
            Err(_) => {
                return Err(McpError::Process(
                    "timed out waiting for the editor's running game".to_string(),
                ));
            }
        };
        if event.get("event").and_then(Value::as_str) != Some("game.message") {
            continue;
        }
        if event.pointer("/data/session").and_then(Value::as_i64) != Some(session) {
            continue;
        }
        if event.pointer("/data/message").and_then(Value::as_str) != Some("mcp:reply") {
            continue;
        }
        let Some(text) = event.pointer("/data/data/0").and_then(Value::as_str) else {
            continue;
        };
        let parsed: Value = serde_json::from_str(text)?;
        if !crate::godot::json_id_eq(parsed.get("id"), id) {
            continue;
        }
        if let Some(err) = parsed.get("error") {
            return Err(McpError::ToolExecution(err.to_string()));
        }
        return Ok(parsed.get("result").cloned().unwrap_or(Value::Null));
    }
}

static NEXT_EDITOR: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(1);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_held_step_that_times_out_is_still_running_in_the_game() {
        let timeout = || {
            McpError::Process(
                "timed out waiting for the game runtime to answer 'game.physics'".into(),
            )
        };
        let held = held_step_timeout(true, timeout()).to_string();
        assert!(held.contains("finishes it and holds again"), "{held}");
        assert!(held.contains("action status"), "{held}");
        // A timeout of anything else, or of a game that was not held, is reported as it is.
        assert_eq!(
            held_step_timeout(false, timeout()).to_string(),
            timeout().to_string()
        );
        let other = || McpError::Process("the game disconnected".into());
        assert_eq!(
            held_step_timeout(true, other()).to_string(),
            other().to_string()
        );
    }

    #[test]
    fn a_recording_timeout_says_the_png_files_are_still_being_written() {
        let timeout = || {
            McpError::Process(
                "timed out waiting for the game runtime to answer 'game.record'".into(),
            )
        };
        let text = record_timeout(timeout()).to_string();
        assert!(text.contains("writing the recorded PNG files"), "{text}");
        assert!(text.contains("already captured"), "{text}");
        assert!(!text.to_lowercase().contains("held step"), "{text}");
        let other = || McpError::Process("the game disconnected".into());
        assert_eq!(record_timeout(other()).to_string(), other().to_string());
    }
}
