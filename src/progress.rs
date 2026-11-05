//! MCP progress notifications for the tool call a task is running.
//!
//! A client that wants them sends `_meta.progressToken` with `tools/call`. The server sets the
//! sink for the call's task, so a tool that has something to say finds it without every handler
//! carrying it. Each notification is one more JSON-RPC frame on the stdout writer's queue.

use serde_json::{Value, json};
use std::future::Future;
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::error;

tokio::task_local! {
    static CURRENT: Option<Arc<ProgressSink>>;
}

/// Where the progress of one tool call goes.
#[derive(Debug)]
pub struct ProgressSink {
    token: Value,
    frames: mpsc::UnboundedSender<String>,
}

impl ProgressSink {
    pub fn new(token: Value, frames: mpsc::UnboundedSender<String>) -> Self {
        Self { token, frames }
    }

    /// Sends one notification. `progress` must grow from one call to the next, as MCP requires.
    pub fn send(&self, progress: f64, message: &str) {
        let frame = json!({
            "jsonrpc": "2.0",
            "method": "notifications/progress",
            "params": {
                "progressToken": self.token,
                "progress": progress,
                "message": message,
            }
        });
        match serde_json::to_string(&frame) {
            Ok(text) => {
                let _ = self.frames.send(text);
            }
            Err(err) => error!("Failed to serialize a progress notification: {err}"),
        }
    }
}

/// Runs `call` with `sink` as the current task's progress sink.
pub async fn with_sink<F: Future>(sink: Option<Arc<ProgressSink>>, call: F) -> F::Output {
    CURRENT.scope(sink, call).await
}

/// The sink of the tool call the current task is running, when its client asked for progress.
pub fn current() -> Option<Arc<ProgressSink>> {
    CURRENT.try_with(Clone::clone).ok().flatten()
}
