use crate::bridge::BridgeClient;
use crate::cli_runner::CliRunner;
use crate::config::Config;
use crate::error::Result;
use crate::protocol::JsonRpcResponse;
use crate::tools::ToolManager;
use crate::util::lock;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;
use tokio::task::{AbortHandle, JoinSet};
use tracing::{error, info};

/// Newest first: a client asking for anything else is answered with the first entry.
const SUPPORTED_PROTOCOL_VERSIONS: [&str; 3] = ["2025-06-18", "2025-03-26", "2024-11-05"];

const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;

/// Tool calls that are still running, keyed by the JSON text of their request id.
type InFlight = Arc<Mutex<HashMap<String, AbortHandle>>>;

pub struct McpServer {
    tool_manager: Arc<ToolManager>,
}

enum Incoming {
    Request {
        id: Value,
        method: String,
        params: Value,
    },
    Notification {
        method: String,
        params: Value,
    },
    Invalid(JsonRpcResponse),
}

impl McpServer {
    pub fn new(config: &Config) -> Self {
        // The catalog is built on first use, so the feedback mode must be known before it.
        ToolManager::configure_catalog(config.feedback);
        let godot_bin = config.resolve_godot_path();
        info!("Using Godot binary at: {}", godot_bin.display());
        info!("Editor bridge: {}", config.bridge_port_origin());

        let bridge = BridgeClient::with_port_origin(config.debug_port, config.bridge_port_origin());
        let cli = CliRunner::new(godot_bin).with_bridge_port(config.debug_port);
        let tool_manager = Arc::new(
            ToolManager::new(bridge, cli).with_feedback(config.feedback, config.gh_path.clone()),
        );

        Self { tool_manager }
    }

    /// Run standard I/O loop processing JSON-RPC messages.
    ///
    /// Tool calls run concurrently, so a slow editor command never delays `ping` or other
    /// requests, and `notifications/cancelled` can abort one that is still running.
    pub async fn run_stdio(self) -> Result<()> {
        let (out_tx, mut out_rx) = mpsc::unbounded_channel::<String>();
        let writer = tokio::spawn(async move {
            let mut stdout = tokio::io::stdout();
            while let Some(line) = out_rx.recv().await {
                if write_line(&mut stdout, &line).await.is_err() {
                    break;
                }
            }
        });

        let in_flight: InFlight = Arc::default();
        let mut tasks: JoinSet<()> = JoinSet::new();
        let mut reader = BufReader::new(tokio::io::stdin());
        let mut buffer = Vec::new();

        info!("Godot MCP server listening on stdio");

        loop {
            buffer.clear();
            if reader.read_until(b'\n', &mut buffer).await? == 0 {
                break;
            }
            while tasks.try_join_next().is_some() {}

            let text = String::from_utf8_lossy(&buffer);
            let line = text.trim();
            if line.is_empty() {
                continue;
            }

            match parse_message(line) {
                Incoming::Invalid(response) => send(&out_tx, &response),
                Incoming::Notification { method, params } => {
                    Self::handle_notification(&method, &params, &in_flight);
                }
                Incoming::Request { id, method, params } if method == "tools/call" => {
                    self.spawn_tool_call(&mut tasks, &in_flight, &out_tx, id, &params);
                }
                Incoming::Request { id, method, params } => {
                    send(&out_tx, &self.handle_request(id, &method, &params));
                }
            }
        }

        // The client closed stdin: let running calls finish so their responses are not lost.
        while tasks.join_next().await.is_some() {}
        drop(out_tx);
        let _ = writer.await;

        info!("Godot MCP server stdio session closed");
        Ok(())
    }

    /// Requests that are answered immediately, without touching the editor.
    fn handle_request(&self, id: Value, method: &str, params: &Value) -> JsonRpcResponse {
        match method {
            "initialize" => {
                let requested = params.get("protocolVersion").and_then(Value::as_str);
                let version = SUPPORTED_PROTOCOL_VERSIONS
                    .iter()
                    .find(|supported| Some(**supported) == requested)
                    .unwrap_or(&SUPPORTED_PROTOCOL_VERSIONS[0]);
                let client = params.get("clientInfo");
                let client_field = |key: &str| {
                    client
                        .and_then(|info| info.get(key))
                        .and_then(Value::as_str)
                };
                self.tool_manager
                    .note_client(client_field("name"), client_field("version"));
                let mut result = json!({
                    "protocolVersion": version,
                    "capabilities": { "tools": { "listChanged": false } },
                    "serverInfo": {
                        "name": "godot-mcp",
                        "version": env!("CARGO_PKG_VERSION")
                    }
                });
                if let Some(guidance) = self.tool_manager.guidance() {
                    result["instructions"] = json!(guidance);
                }
                JsonRpcResponse::success(id, result)
            }
            "ping" => JsonRpcResponse::success(id, json!({})),
            "tools/list" => {
                JsonRpcResponse::success(id, json!({ "tools": ToolManager::list_tools() }))
            }
            _ => {
                JsonRpcResponse::error(id, METHOD_NOT_FOUND, format!("Method '{method}' not found"))
            }
        }
    }

    fn handle_notification(method: &str, params: &Value, in_flight: &InFlight) {
        if method != "notifications/cancelled" {
            return;
        }
        let Some(request_id) = params.get("requestId") else {
            return;
        };
        if let Some(handle) = lock(in_flight).remove(&request_id.to_string()) {
            handle.abort();
            info!("Cancelled request {request_id}");
        }
    }

    fn spawn_tool_call(
        &self,
        tasks: &mut JoinSet<()>,
        in_flight: &InFlight,
        out_tx: &mpsc::UnboundedSender<String>,
        id: Value,
        params: &Value,
    ) {
        let Some(name) = params.get("name").and_then(Value::as_str) else {
            send(
                out_tx,
                &JsonRpcResponse::error(id, INVALID_PARAMS, "Invalid params: 'name' is required"),
            );
            return;
        };
        if !ToolManager::has_tool(name) {
            // A request for a tool that does not exist is a request for a missing capability.
            self.tool_manager.record_unknown_tool(name);
            send(
                out_tx,
                &JsonRpcResponse::error(id, INVALID_PARAMS, format!("Unknown tool: {name}")),
            );
            return;
        }
        let name = name.to_string();
        let arguments = match params.get("arguments") {
            None | Some(Value::Null) => json!({}),
            Some(arguments) => arguments.clone(),
        };

        let key = id.to_string();
        let manager = self.tool_manager.clone();
        let responses = out_tx.clone();
        let registry = in_flight.clone();
        let task_key = key.clone();

        // Holding the lock until the handle is registered keeps a fast task from removing
        // its entry before it exists.
        let mut running = lock(in_flight);
        let handle = tasks.spawn(async move {
            let result = manager.call_tool(&name, arguments).await;
            let payload = serde_json::to_value(result).unwrap_or_else(|err| {
                json!({
                    "content": [{"type": "text", "text": format!("Internal error: {err}")}],
                    "isError": true
                })
            });
            send(&responses, &JsonRpcResponse::success(id, payload));
            lock(&registry).remove(&task_key);
        });
        running.insert(key, handle);
    }
}

fn parse_message(line: &str) -> Incoming {
    let value: Value = match serde_json::from_str(line) {
        Ok(value) => value,
        Err(err) => {
            error!("Malformed JSON-RPC message: {err}");
            return Incoming::Invalid(JsonRpcResponse::error(
                Value::Null,
                PARSE_ERROR,
                "Parse error",
            ));
        }
    };
    let Value::Object(mut message) = value else {
        return Incoming::Invalid(JsonRpcResponse::error(
            Value::Null,
            INVALID_REQUEST,
            "Invalid Request: expected a JSON object (batch requests are not supported)",
        ));
    };

    let id = message.remove("id");
    let error_id = match &id {
        Some(id) if id.is_string() || id.is_number() => id.clone(),
        _ => Value::Null,
    };
    let invalid = |reason: &str| {
        Incoming::Invalid(JsonRpcResponse::error(
            error_id.clone(),
            INVALID_REQUEST,
            format!("Invalid Request: {reason}"),
        ))
    };

    if message.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return invalid("'jsonrpc' must be \"2.0\"");
    }
    let Some(method) = message.get("method").and_then(Value::as_str) else {
        return invalid("'method' must be a string");
    };
    let method = method.to_string();
    let params = message.remove("params").unwrap_or(Value::Null);

    match id {
        None => Incoming::Notification { method, params },
        Some(id) if id.is_string() || id.is_number() => Incoming::Request { id, method, params },
        Some(_) => invalid("'id' must be a string or a number"),
    }
}

fn send(out_tx: &mpsc::UnboundedSender<String>, response: &JsonRpcResponse) {
    match serde_json::to_string(response) {
        Ok(text) => {
            let _ = out_tx.send(text);
        }
        Err(err) => error!("Failed to serialize a response: {err}"),
    }
}

async fn write_line(stdout: &mut tokio::io::Stdout, line: &str) -> std::io::Result<()> {
    stdout.write_all(line.as_bytes()).await?;
    stdout.write_all(b"\n").await?;
    stdout.flush().await
}
