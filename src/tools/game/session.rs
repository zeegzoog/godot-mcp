//! Reaching the game: a managed run or the editor's Play session, with the time each kind of
//! call may take.

use crate::bridge_protocol::check_runtime_hello;
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
        let answer = if method == SCRIPT_METHOD {
            let wait = script_wait(&params);
            editor_runtime(self, method, params, wait)
                .await
                .map_err(script_timeout)
        } else if method == "game.record" {
            editor_runtime(self, method, params, RECORD_WAIT)
                .await
                .map_err(record_timeout)
        } else if waits_in_seconds(method, &params) {
            editor_runtime(self, method, params, SECONDS_WAIT).await
        } else if method == SCREENSHOT_METHOD {
            let wait = screenshot_wait(&params);
            editor_runtime(self, method, params, wait).await
        } else {
            editor_runtime(self, method, params, Duration::from_secs(20)).await
        };
        answer.map_err(|error| self.or_ended_run(run_id, error))
    }

    /// No game to reach and a managed run that has ended: that end is the news, and neither the
    /// editor bridge nor its port has anything to do with it. A call that named the editor asked
    /// about the editor.
    fn or_ended_run(&self, run_id: Option<&str>, error: McpError) -> McpError {
        let no_game = match &error {
            McpError::BridgeUnavailable(_) => true,
            McpError::Process(message) => message == NO_EDITOR_GAME,
            _ => false,
        };
        if !no_game || run_id.is_some() {
            return error;
        }
        match self.games.last_ended() {
            Some(run) => McpError::RunEnded(run.ended_report(None)),
            None => error,
        }
    }
}

const NO_EDITOR_GAME: &str = "No game started from the editor is running.";

/// How long a held step may take before the server stops waiting. The game still finishes it.
const HELD_STEP_WAIT: Duration = Duration::from_secs(60);

/// How long a recording may take. Capturing is at most 480 process frames. The PNG files are
/// written after the last frame and before the runtime answers, so this wait covers that disk
/// time. The 512 MB pixel cap keeps the write inside it.
const RECORD_WAIT: Duration = Duration::from_secs(120);

const SCRIPT_METHOD: &str = "game.run_script";

/// How long the server waits for a `godot_game_wait` in game seconds: the game gives up after 120
/// real seconds when its clock does not advance, and answers then.
const SECONDS_WAIT: Duration = Duration::from_secs(135);

/// Whether the call is a wait in game seconds, which may take far longer than a frame wait.
fn waits_in_seconds(method: &str, params: &Value) -> bool {
    method == "input.wait"
        && params
            .get("seconds")
            .is_some_and(|seconds| !seconds.is_null())
}

const SCREENSHOT_METHOD: &str = "game.screenshot";

/// A sheet waits `every` process frames between its shots, up to 8 x 240 frames. The server
/// waits for that at 30 frames a second, on top of the usual wait.
fn screenshot_wait(params: &Value) -> Duration {
    let count = |key: &str| {
        params
            .get(key)
            .and_then(Value::as_u64)
            .unwrap_or(1)
            .min(240)
    };
    let frames = count("frames").min(8);
    let every = count("every");
    Duration::from_secs(20 + frames.saturating_sub(1) * every / 30)
}

/// The game ends a function at its own timeout and then answers, so the server waits a little
/// longer than that. A function that never awaits cannot be ended, and that is what this catches.
fn script_wait(params: &Value) -> Duration {
    let seconds = params
        .get("timeout_seconds")
        .and_then(Value::as_u64)
        .unwrap_or(super::script::DEFAULT_TIMEOUT_SECONDS)
        .min(super::script::MAX_TIMEOUT_SECONDS);
    Duration::from_secs(seconds + 15)
}

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
    let script = method == SCRIPT_METHOD;
    let was_held = physics && run.is_held();
    let wait = if script {
        script_wait(&params)
    } else if recording {
        RECORD_WAIT
    } else if waits_in_seconds(method, &params) {
        SECONDS_WAIT
    } else if method == SCREENSHOT_METHOD {
        screenshot_wait(&params)
    } else if was_held {
        HELD_STEP_WAIT
    } else {
        Duration::from_secs(20)
    };
    let outcome = run.runtime(method, params, wait).await;
    if physics
        && let Some(held) = outcome
            .as_ref()
            .ok()
            .and_then(|value| value.get("held"))
            .and_then(Value::as_bool)
    {
        run.set_held(held);
    }
    outcome.map_err(|error| {
        if script {
            script_timeout(error)
        } else if recording {
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

/// The game ends a function that awaits, so one that outlives the wait is running without an await
/// or the game stopped answering.
fn script_timeout(error: McpError) -> McpError {
    match error {
        McpError::Process(message) if message.starts_with("timed out") => {
            McpError::Process(format!(
                "the game did not answer in time ({message}). A function that loops without an await cannot be ended from outside, and in editor Play a script error stops at the editor's debugger. Other game tools wait until it returns. Stop the run with godot_stop_project if it does not"
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
                && let Some(hello) = session
                    .get("hello")
                    .and_then(Value::as_str)
                    .filter(|hello| !hello.is_empty())
                && let Some(id) = session
                    .get("session")
                    .and_then(Value::as_i64)
                    .filter(|id| *id >= 0)
            {
                let hello = crate::godot::json_text::parse::<Value>(hello).unwrap_or(Value::Null);
                check_runtime_hello(&hello).map_err(McpError::Process)?;
                return Ok(id);
            }
            let state = tools
                .bridge
                .send_command("get_run_status", json!({}))
                .await?;
            if state.get("running").and_then(Value::as_bool) != Some(true) {
                return Err(McpError::Process(NO_EDITOR_GAME.into()));
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
    let session = ready_editor_session(tools).await?;
    let id = NEXT_EDITOR.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let reply = tools.bridge.expect_game_reply(session, id);
    let payload = serde_json::to_string(&json!({"id": id, "method": method, "params": params}))?;
    tools
        .bridge
        .send_command(
            "game_send",
            json!({"command": "mcp:request", "data": [payload], "session": session}),
        )
        .await?;
    let mut parsed = match tokio::time::timeout(wait, reply).await {
        Ok(Ok(parsed)) => parsed,
        Ok(Err(_)) => {
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
    if let Some(err) = parsed.get("error") {
        return Err(McpError::ToolExecution(err.to_string()));
    }
    Ok(parsed
        .get_mut("result")
        .map(Value::take)
        .unwrap_or(Value::Null))
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
    fn a_wait_in_game_seconds_gets_the_long_wait() {
        assert!(waits_in_seconds("input.wait", &json!({"seconds": 30})));
        assert!(!waits_in_seconds("input.wait", &json!({"seconds": null})));
        assert!(!waits_in_seconds("input.wait", &json!({"frames": 600})));
        assert!(!waits_in_seconds("game.tree", &json!({"seconds": 30})));
        assert!(SECONDS_WAIT > Duration::from_secs(120));
    }

    #[test]
    fn a_long_sheet_gets_time_for_its_pauses() {
        assert_eq!(screenshot_wait(&json!({})), Duration::from_secs(20));
        assert_eq!(
            screenshot_wait(&json!({"frames": 8, "every": 240})),
            Duration::from_secs(20 + 7 * 240 / 30)
        );
        assert_eq!(
            screenshot_wait(&json!({"frames": 4, "every": 1})),
            Duration::from_secs(20)
        );
    }

    #[test]
    fn a_script_gets_its_own_timeout_and_a_little_more() {
        assert_eq!(script_wait(&json!({})), Duration::from_secs(75));
        assert_eq!(
            script_wait(&json!({"timeout_seconds": 300})),
            Duration::from_secs(315)
        );
        let text = script_timeout(McpError::Process("timed out waiting".into())).to_string();
        assert!(text.contains("godot_stop_project"), "{text}");
        let other = || McpError::Process("the game disconnected".into());
        assert_eq!(script_timeout(other()).to_string(), other().to_string());
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
