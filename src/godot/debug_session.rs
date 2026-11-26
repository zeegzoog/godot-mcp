//! TCP session for one game launched with `--remote-debug tcp://127.0.0.1:<port>`.
//!
//! Framing is a little-endian length plus one Variant (see `variant`). The listener
//! binds to loopback, accepts a single connection, then stops listening.

use crate::error::{McpError, Result};
use crate::godot::variant::{PacketReader, frame_packet};
use crate::replies::{ReplyRouter, ReplyWait, route_reply};
use serde_json::Value;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::{broadcast, mpsc};
use tokio::time::timeout;
use tracing::{debug, warn};

/// One debugger message from the game: `[message, thread_id, data]`.
///
/// Subscribers share one copy (`Arc`). Runtime replies (`mcp:reply`) are not broadcast: they go
/// to the call that waits for them (`expect_reply`).
#[derive(Clone, Debug)]
pub struct GameMessage {
    pub message: String,
    pub thread: i64,
    pub data: Vec<Value>,
}

pub struct DebugSession {
    port: u16,
    outbound: mpsc::UnboundedSender<Vec<u8>>,
    events: broadcast::Sender<Arc<GameMessage>>,
    replies: ReplyRouter<i64>,
    connected: AtomicBool,
    closed: AtomicBool,
    main_thread: AtomicI64,
    /// When true (the default), script errors are reported but do not freeze the game.
    ignore_error_breaks: AtomicBool,
}

impl DebugSession {
    /// Bind `127.0.0.1` on an ephemeral port and accept exactly one game.
    pub async fn listen() -> Result<Arc<Self>> {
        let listener = TcpListener::bind("127.0.0.1:0").await.map_err(|err| {
            McpError::Process(format!("failed to bind the debugger listener: {err}"))
        })?;
        let port = listener
            .local_addr()
            .map_err(|err| McpError::Process(format!("debugger listener has no address: {err}")))?
            .port();
        let (outbound, queue) = mpsc::unbounded_channel();
        let (events, _) = broadcast::channel(512);
        let session = Arc::new(Self {
            port,
            outbound,
            events,
            replies: ReplyRouter::default(),
            connected: AtomicBool::new(false),
            closed: AtomicBool::new(false),
            main_thread: AtomicI64::new(1),
            ignore_error_breaks: AtomicBool::new(true),
        });
        let reader_events = session.events.clone();
        let replies = session.replies.clone();
        let session_flag = Arc::clone(&session);
        tokio::spawn(async move {
            let Ok((socket, _)) = listener.accept().await else {
                session_flag.closed.store(true, Ordering::Release);
                return;
            };
            drop(listener);
            session_flag.connected.store(true, Ordering::Release);
            let _ = socket.set_nodelay(true);
            let (mut read, mut write) = socket.into_split();
            let writer = tokio::spawn(async move {
                let mut queue = queue;
                while let Some(bytes) = queue.recv().await {
                    if write.write_all(&bytes).await.is_err() {
                        break;
                    }
                }
            });
            // The game is already reading. Tell it not to freeze on script errors, and
            // clear SceneDebugger's default node-select mode, which drops injected input
            // after the first pause. Both are no-ops for a peer that is not a game.
            if session_flag.ignore_error_breaks.load(Ordering::Acquire) {
                let _ = session_flag.send(
                    "core:set_ignore_error_breaks",
                    vec![serde_json::json!(true)],
                    None,
                );
            }
            let _ = session_flag.send(
                "scene:runtime_node_select_set_type",
                vec![serde_json::json!(0)],
                None,
            );
            let mut packets = PacketReader::default();
            let mut buf = vec![0_u8; 64 * 1024];
            loop {
                let n = match read.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => n,
                };
                for packet in packets.push(&buf[..n]) {
                    let value = match packet {
                        Ok(value) => value,
                        Err(err) => {
                            warn!("game debugger: {err}");
                            continue;
                        }
                    };
                    let Some(message) = parse_message(value) else {
                        continue;
                    };
                    if message.message == "mcp:reply" {
                        if let Some(text) = message.data.first().and_then(Value::as_str)
                            && let Err(why) = route_reply(&replies, text, |id| id)
                        {
                            debug!("dropped a game runtime reply: {why}");
                        }
                        continue;
                    }
                    if message.message == "set_pid" && message.thread > 0 {
                        session_flag
                            .main_thread
                            .store(message.thread, Ordering::Release);
                    }
                    let _ = reader_events.send(Arc::new(message));
                }
            }
            writer.abort();
            session_flag.connected.store(false, Ordering::Release);
            session_flag.closed.store(true, Ordering::Release);
            replies.close_all();
        });
        Ok(session)
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn uri(&self) -> String {
        format!("tcp://127.0.0.1:{}", self.port)
    }

    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::Acquire)
    }

    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }

    /// `false` keeps Godot's default of stopping the game on a script error.
    pub fn set_ignore_error_breaks(&self, ignore: bool) {
        self.ignore_error_breaks.store(ignore, Ordering::Release);
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Arc<GameMessage>> {
        self.events.subscribe()
    }

    /// Waits for the runtime's `mcp:reply` to request `id`. Call it before sending the request.
    /// The wait fails when the game disconnects.
    pub fn expect_reply(&self, id: i64) -> ReplyWait<i64> {
        self.replies.expect(id)
    }

    pub fn main_thread(&self) -> i64 {
        self.main_thread.load(Ordering::Acquire)
    }

    /// Send a command to the game's main thread unless `thread` is set.
    pub fn send(&self, command: &str, data: Vec<Value>, thread: Option<i64>) -> Result<()> {
        if !self.is_connected() {
            return Err(McpError::Process(
                "the game's debugger is not connected".into(),
            ));
        }
        let thread = thread.unwrap_or_else(|| self.main_thread());
        let packet = frame_packet(&serde_json::json!([command, thread, data])).map_err(|err| {
            McpError::Protocol(format!(
                "failed to encode debugger command '{command}': {err}"
            ))
        })?;
        self.outbound
            .send(packet)
            .map_err(|_| McpError::Process("the debugger connection is closed".into()))
    }

    pub async fn wait_for(&self, name: &str, wait: Duration) -> Result<Arc<GameMessage>> {
        let mut rx = self.subscribe();
        let result = timeout(wait, async {
            loop {
                match rx.recv().await {
                    Ok(message) if message.message == name => return Ok(message),
                    Ok(_) => {}
                    Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => {
                        return Err(McpError::Process("the game disconnected".into()));
                    }
                }
            }
        })
        .await;
        match result {
            Ok(outcome) => outcome,
            Err(_) => Err(McpError::Process(format!(
                "timed out after {}s waiting for '{name}' from the game",
                wait.as_secs()
            ))),
        }
    }
}

/// Takes the message apart without copying its data, which can hold a large screenshot.
fn parse_message(value: Value) -> Option<GameMessage> {
    let Value::Array(items) = value else {
        return None;
    };
    let mut items = items.into_iter();
    let Some(Value::String(message)) = items.next() else {
        return None;
    };
    let thread = items.next()?.as_i64().unwrap_or(1);
    let Some(Value::Array(data)) = items.next() else {
        return None;
    };
    Some(GameMessage {
        message,
        thread,
        data,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn one_connection_roundtrips_a_command() {
        let session = DebugSession::listen().await.expect("listen");
        let mut client = tokio::net::TcpStream::connect(format!("127.0.0.1:{}", session.port()))
            .await
            .expect("connect");
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(session.is_connected());

        let hello = frame_packet(&serde_json::json!(["set_pid", 1, [42]])).unwrap();
        client.write_all(&hello).await.unwrap();
        let incoming = session
            .wait_for("set_pid", Duration::from_secs(1))
            .await
            .unwrap();
        assert_eq!(incoming.data, vec![serde_json::json!(42)]);

        let mut rx = session.subscribe();
        session
            .send("mcp:request", vec![Value::String("{}".into())], None)
            .unwrap();
        let mut reader = PacketReader::default();
        let mut buf = vec![0_u8; 1024];
        let mut commands = Vec::new();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        while commands.len() < 3 && tokio::time::Instant::now() < deadline {
            let n = tokio::time::timeout(Duration::from_secs(1), client.read(&mut buf))
                .await
                .expect("read timeout")
                .unwrap();
            commands.extend(
                reader
                    .push(&buf[..n])
                    .into_iter()
                    .map(|packet| packet.unwrap()),
            );
        }
        let names: Vec<_> = commands
            .iter()
            .filter_map(|packet| packet[0].as_str())
            .collect();
        assert!(names.contains(&"core:set_ignore_error_breaks"), "{names:?}");
        assert!(
            names.contains(&"scene:runtime_node_select_set_type"),
            "{names:?}"
        );
        assert!(names.contains(&"mcp:request"), "{names:?}");
        let _ = rx.try_recv();
    }

    /// A game that sends a packet the reader cannot take, and a reply nobody waits for, keeps its
    /// session; each waiting call gets its own reply, and replies are not broadcast.
    #[tokio::test]
    async fn replies_reach_their_own_caller_and_a_bad_packet_does_not_end_the_session() {
        let session = DebugSession::listen().await.expect("listen");
        let mut client = tokio::net::TcpStream::connect(format!("127.0.0.1:{}", session.port()))
            .await
            .expect("connect");
        tokio::time::sleep(Duration::from_millis(20)).await;
        let mut events = session.subscribe();
        let first = session.expect_reply(101);
        let second = session.expect_reply(102);

        let reply = |id: f64, result: &str| {
            frame_packet(&serde_json::json!([
                "mcp:reply",
                1,
                [serde_json::json!({"id": id, "result": result}).to_string()]
            ]))
            .unwrap()
        };
        let mut stream = reply(999.0, "nobody");
        // An oversized packet: the length prefix and part of the body.
        stream.extend_from_slice(&((9_u32 << 20).to_le_bytes()));
        stream.extend(std::iter::repeat_n(0_u8, 4096));
        client.write_all(&stream).await.unwrap();
        // The rest of it, then more traffic.
        let rest = vec![0_u8; (9 << 20) - 4096];
        client.write_all(&rest).await.unwrap();
        client.write_all(&reply(102.0, "two")).await.unwrap();
        client.write_all(&reply(101.0, "one")).await.unwrap();
        client
            .write_all(&frame_packet(&serde_json::json!(["output", 1, [["hi"]]])).unwrap())
            .await
            .unwrap();

        let wait = Duration::from_secs(5);
        let second = tokio::time::timeout(wait, second).await.unwrap().unwrap();
        let first = tokio::time::timeout(wait, first).await.unwrap().unwrap();
        assert_eq!(first["result"], "one");
        assert_eq!(second["result"], "two");
        let event = tokio::time::timeout(wait, events.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(event.message, "output", "replies are not broadcast");
        assert!(session.is_connected());
        assert!(!session.is_closed());
    }
}
