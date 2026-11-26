use crate::bridge_protocol::AddonSurface;
use crate::error::{McpError, Result};
use crate::godot::json_text;
use crate::replies::{ReplyRouter, ReplyWait, route_reply};
use crate::util::lock;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};
use tokio::net::TcpStream;
use tokio::sync::{Mutex, mpsc, oneshot};
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::{WebSocketStream, client_async_with_config};
use tracing::{debug, info, warn};
use uuid::Uuid;

/// The bridge only ever listens on loopback, so a closed port answers (or times out) fast.
const CONNECT_TIMEOUT: Duration = Duration::from_millis(300);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(2);
const DEFAULT_COMMAND_TIMEOUT: Duration = Duration::from_secs(15);
/// After a failed connect, further attempts within this window fail immediately.
const RETRY_AFTER_FAILURE: Duration = Duration::from_millis(500);
/// The command whose answer lists the addon's protocol and commands; every addon has it.
const SURFACE_COMMAND: &str = "get_version";
/// How long the first call of a connection waits for that list. An editor that does not answer
/// in time is sent the call as an addon too old to list it would be.
const SURFACE_TIMEOUT: Duration = Duration::from_secs(2);
/// The largest message (and frame: the editor sends each message as one frame) taken from the
/// editor. It equals the addon's `OUTBOUND_BUFFER_SIZE` in `bridge_server.gd`, the most the
/// editor can send at once, so every reply it can make fits (a screenshot is a base64 PNG well
/// below it). The library defaults would take 64 MiB messages but only 16 MiB frames.
pub const MAX_EDITOR_MESSAGE: usize = 32 << 20;

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
    /// What kind of failure `error` is, from addons that send codes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
}

type Pending = Arc<StdMutex<HashMap<String, oneshot::Sender<Result<Value>>>>>;
type EventSubs = Arc<StdMutex<Vec<mpsc::UnboundedSender<Value>>>>;

#[derive(Debug)]
struct Connection {
    outbound: mpsc::UnboundedSender<String>,
    pending: Pending,
    alive: Arc<AtomicBool>,
    token: Option<String>,
    /// Tells this connection apart from the ones before it (each editor start is a new one).
    generation: u64,
    /// What the addon said it has: `None` until read, `Some(None)` for an addon too old to say.
    surface: StdMutex<Option<Option<Arc<AddonSurface>>>>,
}

/// Numbers the connections this process opens.
static GENERATIONS: AtomicU64 = AtomicU64::new(1);

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
    /// Phrase that says which port is in effect and where the server read it.
    port_origin: String,
    command_timeout: Duration,
    state: Arc<Mutex<State>>,
    events: EventSubs,
    /// Calls waiting for a reply from a game the editor runs, by debugger session and request id.
    game_replies: ReplyRouter<(i64, i64)>,
}

impl BridgeClient {
    pub fn new(port: u16) -> Self {
        Self::with_port_origin(port, format!("port {port}"))
    }

    pub fn with_port_origin(port: u16, port_origin: impl Into<String>) -> Self {
        Self {
            port,
            port_origin: port_origin.into(),
            command_timeout: DEFAULT_COMMAND_TIMEOUT,
            state: Arc::default(),
            events: EventSubs::default(),
            game_replies: ReplyRouter::default(),
        }
    }

    /// Unsolicited editor messages (`{"type":"event",...}`), such as the game debugger relay.
    pub fn subscribe_events(&self) -> mpsc::UnboundedReceiver<Value> {
        let (tx, rx) = mpsc::unbounded_channel();
        lock(&self.events).push(tx);
        rx
    }

    /// Waits for the `mcp:reply` to request `id` from the editor's game debugger session
    /// `session`. Call it before sending the request. The editor relays the reply as a
    /// `game.message` event, which is then not passed to `subscribe_events`.
    pub fn expect_game_reply(&self, session: i64, id: i64) -> ReplyWait<(i64, i64)> {
        self.game_replies.expect((session, id))
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
    ///
    /// A parameter that the connected addon does not list for the command (see
    /// `bridge_protocol`) is refused here, with the text the addon itself would answer, instead
    /// of being sent to an addon that would do less than asked.
    pub async fn send_command(&self, cmd_type: &str, params: Value) -> Result<Value> {
        let connection = self.connection().await?;
        if cmd_type != SURFACE_COMMAND
            && let Some(surface) = self.surface(&connection).await?
        {
            surface
                .check(cmd_type, &params)
                .map_err(McpError::ToolExecution)?;
        }
        self.send_on(&connection, cmd_type, params, self.command_timeout)
            .await
    }

    /// What the addon of `connection` lists, read once per connection; `None` for an addon too
    /// old to list it, or one that did not answer in time. A connection that broke while the
    /// list was read is an error, so the call reports why instead of a bare closed connection.
    async fn surface(&self, connection: &Connection) -> Result<Option<Arc<AddonSurface>>> {
        if let Some(known) = lock(&connection.surface).clone() {
            return Ok(known);
        }
        // No lock is held while the editor answers: two first calls may both ask.
        let wait = SURFACE_TIMEOUT.min(self.command_timeout);
        let read = match self
            .send_on(connection, SURFACE_COMMAND, json!({}), wait)
            .await
        {
            Ok(answer) => AddonSurface::from_version(&answer).map(Arc::new),
            Err(error) if !connection.alive.load(Ordering::Acquire) => return Err(error),
            Err(error) => {
                debug!("The addon's commands could not be read: {error}");
                None
            }
        };
        *lock(&connection.surface) = Some(read.clone());
        Ok(read)
    }

    async fn send_on(
        &self,
        connection: &Connection,
        cmd_type: &str,
        params: Value,
        wait: Duration,
    ) -> Result<Value> {
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

        match timeout(wait, reply_rx).await {
            Ok(Ok(outcome)) => outcome,
            Ok(Err(_)) => Err(McpError::Bridge(
                "The Godot Editor bridge dropped the request".into(),
            )),
            Err(_) => Err(McpError::Bridge(format!(
                "Timed out after {}s waiting for the Godot Editor to answer '{cmd_type}'",
                wait.as_secs_f32()
            ))),
        }
    }

    /// The number of the open connection to the editor, without opening one: `None` when none
    /// is open (or another call is opening one right now).
    pub fn open_connection(&self) -> Option<u64> {
        let state = self.state.try_lock().ok()?;
        state
            .connection
            .as_ref()
            .filter(|connection| connection.alive.load(Ordering::Acquire))
            .map(|connection| connection.generation)
    }

    async fn connection(&self) -> Result<Arc<Connection>> {
        let mut state = self.state.lock().await;

        if let Some(existing) = &state.connection
            && existing.alive.load(Ordering::Acquire)
        {
            return Ok(existing.clone());
        }
        state.connection = None;

        if let Some((failed_at, message)) = &state.last_failure
            && failed_at.elapsed() < RETRY_AFTER_FAILURE
        {
            return Err(McpError::BridgeUnavailable(message.clone()));
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
            Ok(Err(err)) => {
                return Err(self.unavailable(&format!("connection failed: {err}")));
            }
            Err(_) => return Err(self.unavailable("connection timed out")),
        };
        let url = format!("ws://{address}");
        let config = WebSocketConfig::default()
            .max_message_size(Some(MAX_EDITOR_MESSAGE))
            .max_frame_size(Some(MAX_EDITOR_MESSAGE));
        let handshake = client_async_with_config(url.as_str(), tcp, Some(config));
        let websocket = match timeout(HANDSHAKE_TIMEOUT, handshake).await {
            Ok(Ok((websocket, _))) => websocket,
            Ok(Err(err)) => return Err(self.unavailable(&format!("handshake failed: {err}"))),
            Err(_) => return Err(self.unavailable("handshake timed out")),
        };

        let (outbound, queue) = mpsc::unbounded_channel();
        let pending = Pending::default();
        let alive = Arc::new(AtomicBool::new(true));
        tokio::spawn(run_connection(
            websocket,
            queue,
            pending.clone(),
            self.events.clone(),
            self.game_replies.clone(),
            alive.clone(),
        ));

        info!("Connected to the Godot Editor bridge on port {port}");
        Ok(Arc::new(Connection {
            outbound,
            pending,
            alive,
            token: read_token(port),
            generation: GENERATIONS.fetch_add(1, Ordering::Relaxed),
            surface: StdMutex::default(),
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

impl BridgeClient {
    fn unavailable(&self, detail: &str) -> McpError {
        McpError::BridgeUnavailable(format!(
            "ws://127.0.0.1:{}: {detail}. This server is using {}. The Godot MCP addon must listen on that same port. With no port configured, the addon listens on the port derived from its own project path (and on 9333 while that is free), so a server started in the editor's project directory finds it; otherwise set the same port on both sides: the project setting godot_mcp/bridge_port or GODOT_DEBUG_PORT for the editor, --debug-port or GODOT_DEBUG_PORT for this server",
            self.port, self.port_origin
        ))
    }
}

fn connection_closed() -> McpError {
    McpError::Bridge("The connection to the Godot Editor bridge was closed".into())
}

async fn run_connection(
    websocket: WebSocketStream<TcpStream>,
    mut queue: mpsc::UnboundedReceiver<String>,
    pending: Pending,
    events: EventSubs,
    game_replies: ReplyRouter<(i64, i64)>,
    alive: Arc<AtomicBool>,
) {
    let (mut sink, mut stream) = websocket.split();
    let mut closed_by: Option<String> = None;
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
                Some(Ok(Message::Text(text))) => deliver(&pending, &events, &game_replies, text.as_str()),
                Some(Ok(Message::Ping(data))) => {
                    let _ = sink.send(Message::Pong(data)).await;
                }
                Some(Err(err)) => {
                    warn!("Godot Editor bridge connection failed: {err}");
                    closed_by = Some(err.to_string());
                    break;
                }
                Some(Ok(Message::Close(_))) | None => break,
                Some(Ok(_)) => {}
            },
        }
    }

    alive.store(false, Ordering::Release);
    for (_, reply) in lock(&pending).drain() {
        let error = match &closed_by {
            Some(reason) => McpError::Bridge(format!(
                "The connection to the Godot Editor bridge failed: {reason}. The next call reconnects. When the reason names a size, the editor's reply was larger than the {} MiB a reply may have: ask for less (a smaller max_dimension, a narrower query) and retry",
                MAX_EDITOR_MESSAGE >> 20
            )),
            None => connection_closed(),
        };
        let _ = reply.send(Err(error));
    }
    // A game call relayed by the editor cannot be answered once the editor is gone.
    game_replies.close_all();
    debug!("Godot Editor bridge connection closed");
}

/// Parses an editor message once, then hands it to the command, the game call or the event
/// subscribers it is for.
fn deliver(
    pending: &Pending,
    events: &EventSubs,
    game_replies: &ReplyRouter<(i64, i64)>,
    text: &str,
) {
    let text = json_text::repair(text);
    let value = match serde_json::from_str::<Value>(&text) {
        Ok(value) => value,
        Err(err) => {
            warn!(
                "Ignoring malformed bridge message ({err}): {}",
                json_text::head(&text)
            );
            fail_unreadable_reply(pending, &text, &err);
            return;
        }
    };
    if value.get("type").and_then(Value::as_str) == Some("event") {
        if let Some((session, reply)) = game_reply(&value) {
            if let Err(why) = route_reply(game_replies, reply, |id| (session, id)) {
                debug!("Dropped a reply from the editor's game: {why}");
            }
            return;
        }
        lock(events).retain(|subscriber| subscriber.send(value.clone()).is_ok());
        return;
    }
    let response: BridgeResponse = match serde_json::from_value(value) {
        Ok(response) => response,
        Err(err) => {
            warn!(
                "Ignoring malformed bridge message ({err}): {}",
                json_text::head(&text)
            );
            fail_unreadable_reply(pending, &text, &err);
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
        let message = response
            .error
            .unwrap_or_else(|| "Unknown engine error".into());
        match response.code.filter(|code| !code.is_empty()) {
            Some(code) => Err(McpError::CodedToolExecution { code, message }),
            None => Err(McpError::ToolExecution(message)),
        }
    };
    let _ = reply.send(outcome);
}

/// The debugger session and the reply text of a relayed `mcp:reply` from the editor's game.
fn game_reply(event: &Value) -> Option<(i64, &str)> {
    if event.get("event").and_then(Value::as_str) != Some("game.message") {
        return None;
    }
    let data = event.get("data")?;
    if data.get("message").and_then(Value::as_str) != Some("mcp:reply") {
        return None;
    }
    let session = data.get("session").and_then(Value::as_i64)?;
    let text = data.pointer("/data/0").and_then(Value::as_str)?;
    Some((session, text))
}

/// Fails the command an unreadable reply names, so its caller learns at once instead of
/// waiting for the timeout. The id is found by a plain scan for `"commandId":"..."`.
fn fail_unreadable_reply(pending: &Pending, text: &str, err: &serde_json::Error) {
    let Some(command_id) = scan_command_id(text) else {
        return;
    };
    let Some(reply) = lock(pending).remove(command_id) else {
        return;
    };
    let _ = reply.send(Err(McpError::Bridge(format!(
        "The editor's reply to this command could not be read as JSON ({err}). Retry the command; if it fails again, update the editor's addon with godot_update_addon, and if the reply echoes unusual text (control characters in a name or a file), try the command with other input"
    ))));
}

fn scan_command_id(text: &str) -> Option<&str> {
    let key = "\"commandId\"";
    let rest = &text[text.find(key)? + key.len()..];
    let rest = rest.trim_start().strip_prefix(':')?.trim_start();
    let rest = rest.strip_prefix('"')?;
    let end = rest.find(['"', '\\'])?;
    Some(&rest[..end])
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
