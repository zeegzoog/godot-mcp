use thiserror::Error;

#[derive(Error, Debug)]
pub enum McpError {
    /// The editor bridge cannot be reached at all (editor closed, plugin disabled, wrong port).
    #[error("Editor bridge unavailable: {0}")]
    BridgeUnavailable(String),

    /// The bridge connection broke or stalled while a command was in flight.
    #[error("Bridge connection error: {0}")]
    Bridge(String),

    #[error("Engine process error: {0}")]
    Process(String),

    /// A managed game process has ended. The text says how; no editor is involved.
    #[error("{0}")]
    RunEnded(String),

    #[error("Protocol error: {0}")]
    Protocol(String),

    /// The editor understood the command and reported a failure.
    #[error("Tool execution error: {0}")]
    ToolExecution(String),

    #[error("Validation error: {0}")]
    Validation(String),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON serialization error: {0}")]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, McpError>;
