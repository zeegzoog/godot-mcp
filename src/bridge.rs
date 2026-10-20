use crate::error::{McpError, Result};
use crate::util::lock;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};
use tokio::net::TcpStream;
use tokio::sync::{Mutex, mpsc, oneshot};
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{WebSocketStream, client_async};
use tracing::{debug, info, warn};
use uuid::Uuid;

/// The bridge only ever listens on loopback, so a closed port answers (or times out) fast.
const CONNECT_TIMEOUT: Duration = Duration::from_millis(300);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(2);
const DEFAULT_COMMAND_TIMEOUT: Duration = Duration::from_secs(15);
/// After a failed connect, further attempts within this window fail immediately.
const RETRY_AFTER_FAILURE: Duration = Duration::from_millis(500);

#[derive(Debug, Serialize, Deserialize)]
pub struct BridgeCommand {
    #[serde(rename = "commandId")]
    pub command_id: String,
    #[serde(rename = "type")]
    pub command_type: String,
    pub params: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct BridgeResponse {
    #[serde(rename = "commandId")]
    pub command_id: String,
    pub status: String,
    pub result: Option<Value>,
    pub error: Option<String>,
}

type Pending = Arc<StdMutex<HashMap<String, oneshot::Sender<Result<Value>>>>>;
type EventSubs = Arc<StdMutex<Vec<mpsc::UnboundedSender<Value>>>>;

#[derive(Debug)]
struct Connection {
    outbound: mpsc::UnboundedSender<String>,
    pending: Pending,
    alive: Arc<AtomicBool>,
    token: Option<String>,
}

#[derive(Debug, Default)]
struct State {
    connection: Option<Arc<Connection>>,
    last_failure: Option<(Instant, String)>,
}

/// Client for the editor plugin's WebSocket bridge.
///
/// Keeps one connection open, multiplexes concurrent commands over it by command id, and
/// reconnects transparently after the editor restarts.
#[derive(Clone, Debug)]
pub struct BridgeClient {
    port: u16,
    command_timeout: Duration,
    state: Arc<Mutex<State>>,
    events: EventSubs,
}

impl BridgeClient {
    pub fn new(port: u16) -> Self {
        Self {
            port,
            command_timeout: DEFAULT_COMMAND_TIMEOUT,
            state: Arc::default(),
            events: EventSubs::default(),
        }
    }

    /// Unsolicited editor messages (`{"type":"event",...}`), such as the game debugger relay.
    pub fn subscribe_events(&self) -> mpsc::UnboundedReceiver<Value> {
        let (tx, rx) = mpsc::unbounded_channel();
        lock(&self.events).push(tx);
        rx
    }

    /// Overrides how long a command may wait for the editor's answer.
    pub fn with_timeout(mut self, command_timeout: Duration) -> Self {
        self.command_timeout = command_timeout;
        self
    }

    /// The shared secret this client sends, so that text that may reach a public place can be
    /// cleaned of it.
    pub fn secret(&self) -> Option<String> {
        read_token(self.port)
    }

    /// Sends a command to the Godot Editor and waits for its response.
    pub async fn send_command(&self, cmd_type: &str, params: Value) -> Result<Value> {
        let connection = self.connection().await?;

        let command_id = Uuid::new_v4().to_string();
        let (reply_tx, reply_rx) = oneshot::channel();
        lock(&connection.pending).insert(command_id.clone(), reply_tx);
        let _cleanup = PendingGuard {
            pending: connection.pending.clone(),
            command_id: command_id.clone(),
        };
        if !connection.alive.load(Ordering::Acquire) {
            return Err(connection_closed());
        }

        let message = serde_json::to_string(&BridgeCommand {
            command_id,
            command_type: cmd_type.to_string(),
            params,
            token: connection.token.clone(),
        })?;
        connection
            .outbound
            .send(message)
            .map_err(|_| connection_closed())?;

        match timeout(self.command_timeout, reply_rx).await {
            Ok(Ok(outcome)) => outcome,
            Ok(Err(_)) => Err(McpError::Bridge(
                "The Godot Editor bridge dropped the request".into(),
            )),
            Err(_) => Err(McpError::Bridge(format!(
                "Timed out after {}s waiting for the Godot Editor to answer '{cmd_type}'",
                self.command_timeout.as_secs_f32()
            ))),
        }
    }

    async fn connection(&self) -> Result<Arc<Connection>> {
        let mut state = self.state.lock().await;

        if let Some(existing) = &state.connection {
            if existing.alive.load(Ordering::Acquire) {
                return Ok(existing.clone());
            }
        }
        state.connection = None;

        if let Some((failed_at, message)) = &state.last_failure {
            if failed_at.elapsed() < RETRY_AFTER_FAILURE {
                return Err(McpError::BridgeUnavailable(message.clone()));
            }
        }

        match self.connect().await {
            Ok(connection) => {
                state.last_failure = None;
                state.connection = Some(connection.clone());
                Ok(connection)
            }
            Err(error) => {
                if let McpError::BridgeUnavailable(message) = &error {
                    state.last_failure = Some((Instant::now(), message.clone()));
                }
                Err(error)
            }
        }
    }

    async fn connect(&self) -> Result<Arc<Connection>> {
        let port = self.port;
        let address = format!("127.0.0.1:{port}");

        let tcp = match timeout(CONNECT_TIMEOUT, TcpStream::connect(&address)).await {
            Ok(Ok(tcp)) => tcp,
            Ok(Err(err)) => return Err(unavailable(port, &format!("connection failed: {err}"))),
            Err(_) => return Err(unavailable(port, "connection timed out")),
        };
        let url = format!("ws://{address}");
        let websocket = match timeout(HANDSHAKE_TIMEOUT, client_async(url.as_str(), tcp)).await {
            Ok(Ok((websocket, _))) => websocket,
            Ok(Err(err)) => return Err(unavailable(port, &format!("handshake failed: {err}"))),
            Err(_) => return Err(unavailable(port, "handshake timed out")),
        };

        let (outbound, queue) = mpsc::unbounded_channel();
        let pending = Pending::default();
        let alive = Arc::new(AtomicBool::new(true));
        tokio::spawn(run_connection(
            websocket,
            queue,
            pending.clone(),
            self.events.clone(),
            alive.clone(),
        ));

        info!("Connected to the Godot Editor bridge on port {port}");
        Ok(Arc::new(Connection {
            outbound,
            pending,
            alive,
            token: read_token(port),
        }))
    }
}

/// Removes a command's reply slot when its caller stops waiting (timeout or cancellation).
struct PendingGuard {
    pending: Pending,
    command_id: String,
}

impl Drop for PendingGuard {
    fn drop(&mut self) {
        lock(&self.pending).remove(&self.command_id);
    }
}

fn unavailable(port: u16, detail: &str) -> McpError {
    McpError::BridgeUnavailable(format!("ws://127.0.0.1:{port}: {detail}"))
}

fn connection_closed() -> McpError {
    McpError::Bridge("The connection to the Godot Editor bridge was closed".into())
}

async fn run_connection(
    websocket: WebSocketStream<TcpStream>,
    mut queue: mpsc::UnboundedReceiver<String>,
    pending: Pending,
    events: EventSubs,
    alive: Arc<AtomicBool>,
) {
    let (mut sink, mut stream) = websocket.split();
    loop {
        tokio::select! {
            queued = queue.recv() => match queued {
                Some(text) => {
                    if sink.send(Message::Text(text.into())).await.is_err() {
                        break;
                    }
                }
                None => {
                    let _ = sink.close().await;
                    break;
                }
            },
            incoming = stream.next() => match incoming {
                Some(Ok(Message::Text(text))) => deliver(&pending, &events, text.as_str()),
                Some(Ok(Message::Ping(data))) => {
                    let _ = sink.send(Message::Pong(data)).await;
                }
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                Some(Ok(_)) => {}
            },
        }
    }

    alive.store(false, Ordering::Release);
    for (_, reply) in lock(&pending).drain() {
        let _ = reply.send(Err(connection_closed()));
    }
    debug!("Godot Editor bridge connection closed");
}

fn deliver(pending: &Pending, events: &EventSubs, text: &str) {
    if let Ok(value) = serde_json::from_str::<Value>(text) {
        if value.get("type").and_then(Value::as_str) == Some("event") {
            lock(events).retain(|subscriber| subscriber.send(value.clone()).is_ok());
            return;
        }
    }
    let response: BridgeResponse = match serde_json::from_str(text) {
        Ok(response) => response,
        Err(err) => {
            warn!("Ignoring malformed bridge message: {err}");
            return;
        }
    };
    let Some(reply) = lock(pending).remove(&response.command_id) else {
        debug!("Bridge reply for unknown command {}", response.command_id);
        return;
    };
    let outcome = if response.status == "success" {
        Ok(response.result.unwrap_or(Value::Null))
    } else {
        Err(McpError::ToolExecution(
            response
                .error
                .unwrap_or_else(|| "Unknown engine error".into()),
        ))
    };
    let _ = reply.send(outcome);
}

/// Where the editor plugin publishes its per-port bridge token.
///
/// This mirrors Godot's `OS.get_data_dir()`, so the plugin and the server agree without
/// any shared configuration as long as both run as the same OS user.
pub fn token_path(port: u16) -> Option<PathBuf> {
    let data_dir = if cfg!(windows) {
        std::env::var_os("APPDATA").map(PathBuf::from)
    } else if cfg!(target_os = "macos") {
        std::env::var_os("HOME").map(|home| PathBuf::from(home).join("Library/Application Support"))
    } else {
        std::env::var_os("XDG_DATA_HOME")
            .filter(|dir| !dir.is_empty())
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share"))
            })
    }?;
    Some(
        data_dir
            .join("godot-mcp")
            .join(format!("bridge-{port}.token")),
    )
}

/// `GODOT_MCP_TOKEN` wins; otherwise the token file written by the plugin is used.
fn read_token(port: u16) -> Option<String> {
    let from_env = std::env::var("GODOT_MCP_TOKEN")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    if from_env.is_some() {
        return from_env;
    }
    let contents = std::fs::read_to_string(token_path(port)?).ok()?;
    Some(contents.trim().to_string()).filter(|value| !value.is_empty())
}
