//! Minimal JSON-RPC client for the GDScript language server inside a Godot editor.
//!
//! Prefers an editor already listening on `GODOT_MCP_LSP_PORT` (default 6005). Otherwise
//! starts one private headless editor with `--lsp-port` and stops it when the hub is dropped.

use crate::error::{McpError, Result};
use crate::lsp::framing::{MessageReader, encode_message};
use crate::util::lock;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::process::{Child, Command};
use tokio::sync::{mpsc, oneshot};
use tokio::time::timeout;

const DEFAULT_PORT: u16 = 6005;

struct Pending {
    tx: oneshot::Sender<std::result::Result<Value, McpError>>,
}

struct ClientInner {
    outbound: mpsc::UnboundedSender<Vec<u8>>,
    pending: Mutex<HashMap<i64, Pending>>,
    next_id: AtomicI64,
    diagnostics: Mutex<HashMap<String, Vec<Value>>>,
    /// Set when the server closed the connection, which nothing writing to it would notice
    /// until the next write.
    closed: AtomicBool,
}

impl ClientInner {
    fn is_open(&self) -> bool {
        !self.closed.load(Ordering::Acquire) && !self.outbound.is_closed()
    }
}

struct Live {
    client: Arc<ClientInner>,
    child: Option<Child>,
    root: PathBuf,
}

impl Drop for Live {
    fn drop(&mut self) {
        if let Some(child) = self.child.as_mut() {
            let _ = child.start_kill();
        }
    }
}

pub struct LspHub {
    godot: PathBuf,
    live: Mutex<Option<Live>>,
    /// Held while an editor is being started, so that calls arriving meanwhile wait for it
    /// instead of starting another one that would replace it.
    starting: tokio::sync::Mutex<()>,
}

type StartResult = Result<(Arc<ClientInner>, Option<Child>)>;

impl LspHub {
    pub fn new(godot: PathBuf) -> Self {
        Self {
            godot,
            live: Mutex::new(None),
            starting: tokio::sync::Mutex::new(()),
        }
    }

    pub async fn diagnostics(&self, project: &Path, file: &Path) -> Result<Value> {
        let client = self.client_for(project).await?;
        let text = tokio::fs::read_to_string(file).await?;
        let uri = path_to_uri(file);
        let key = uri_key(&uri);
        lock(&client.diagnostics).remove(&key);
        client
            .notify(
                "textDocument/didOpen",
                json!({
                    "textDocument": {"uri": uri, "languageId": "gdscript", "version": 1, "text": text}
                }),
            )
            .await?;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(8);
        let diagnostics = loop {
            if let Some(items) = lock(&client.diagnostics).get(&key).cloned() {
                break items;
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(McpError::Process(
                    "the GDScript language server did not publish diagnostics".into(),
                ));
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        };
        let _ = client
            .notify(
                "textDocument/didClose",
                json!({"textDocument": {"uri": uri}}),
            )
            .await;
        Ok(json!({"uri": uri, "diagnostics": shift_diagnostics(&diagnostics)}))
    }

    pub async fn symbols(&self, project: &Path, file: &Path) -> Result<Value> {
        let client = self.client_for(project).await?;
        self.open_quiet(&client, file).await?;
        let result = client
            .request(
                "textDocument/documentSymbol",
                json!({"textDocument": {"uri": path_to_uri(file)}}),
                Duration::from_secs(15),
            )
            .await;
        close_quiet(&client, file).await;
        Ok(json!({"symbols": result?}))
    }

    pub async fn complete(
        &self,
        project: &Path,
        file: &Path,
        line: u32,
        character: u32,
    ) -> Result<Value> {
        self.position(
            project,
            file,
            "textDocument/completion",
            line,
            character,
            json!({}),
        )
        .await
    }

    pub async fn definition(
        &self,
        project: &Path,
        file: &Path,
        line: u32,
        character: u32,
    ) -> Result<Value> {
        self.position(
            project,
            file,
            "textDocument/definition",
            line,
            character,
            json!({}),
        )
        .await
    }

    pub async fn references(
        &self,
        project: &Path,
        file: &Path,
        line: u32,
        character: u32,
    ) -> Result<Value> {
        self.position(
            project,
            file,
            "textDocument/references",
            line,
            character,
            json!({"context": {"includeDeclaration": true}}),
        )
        .await
    }

    async fn position(
        &self,
        project: &Path,
        file: &Path,
        method: &str,
        line: u32,
        character: u32,
        mut extra: Value,
    ) -> Result<Value> {
        let client = self.client_for(project).await?;
        self.open_quiet(&client, file).await?;
        if let Value::Object(map) = &mut extra {
            map.insert("textDocument".into(), json!({"uri": path_to_uri(file)}));
            map.insert(
                "position".into(),
                json!({
                    "line": line.saturating_sub(1),
                    "character": character.saturating_sub(1),
                }),
            );
        }
        let result = client.request(method, extra, Duration::from_secs(15)).await;
        close_quiet(&client, file).await;
        Ok(json!({"result": result?}))
    }

    async fn open_quiet(&self, client: &ClientInner, file: &Path) -> Result<()> {
        let text = tokio::fs::read_to_string(file).await?;
        client
            .notify(
                "textDocument/didOpen",
                json!({
                    "textDocument": {
                        "uri": path_to_uri(file),
                        "languageId": "gdscript",
                        "version": 1,
                        "text": text
                    }
                }),
            )
            .await
    }

    async fn client_for(&self, project: &Path) -> Result<Arc<ClientInner>> {
        let godot = self.godot.clone();
        self.client_with(project, |root| async move {
            connect_or_start(&godot, &root).await
        })
        .await
    }

    /// The client for `project`: the live one, or the one `start` makes when there is none
    /// or its connection has closed. Only one start runs at a time.
    async fn client_with<F, Fut>(&self, project: &Path, start: F) -> Result<Arc<ClientInner>>
    where
        F: FnOnce(PathBuf) -> Fut,
        Fut: Future<Output = StartResult>,
    {
        let project = std::fs::canonicalize(project).unwrap_or_else(|_| project.to_path_buf());
        if let Some(client) = self.live_client(&project) {
            return Ok(client);
        }
        let _starting = self.starting.lock().await;
        if let Some(client) = self.live_client(&project) {
            return Ok(client);
        }
        let (client, child) = start(project.clone()).await?;
        *lock(&self.live) = Some(Live {
            client: Arc::clone(&client),
            child,
            root: project,
        });
        Ok(client)
    }

    fn live_client(&self, project: &Path) -> Option<Arc<ClientInner>> {
        lock(&self.live)
            .as_ref()
            .filter(|live| live.root == project && live.client.is_open())
            .map(|live| Arc::clone(&live.client))
    }
}

impl ClientInner {
    async fn request(&self, method: &str, params: Value, wait: Duration) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        lock(&self.pending).insert(id, Pending { tx });
        self.write(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))
            .await?;
        match timeout(wait, rx).await {
            Ok(Ok(outcome)) => outcome,
            Ok(Err(_)) => Err(McpError::Process(
                "the language server dropped the request".into(),
            )),
            Err(_) => Err(McpError::Process(format!(
                "the GDScript language server did not answer '{method}' within {}s",
                wait.as_secs()
            ))),
        }
    }

    async fn notify(&self, method: &str, params: Value) -> Result<()> {
        self.write(&json!({"jsonrpc": "2.0", "method": method, "params": params}))
            .await
    }

    async fn write(&self, message: &Value) -> Result<()> {
        let framed = encode_message(message)
            .map_err(|err| McpError::Protocol(format!("LSP framing failed: {err}")))?;
        self.outbound
            .send(framed)
            .map_err(|_| McpError::Process("the language server connection is closed".into()))
    }

    fn write_now(&self, message: &Value) {
        if let Ok(framed) = encode_message(message) {
            let _ = self.outbound.send(framed);
        }
    }
}

async fn connect_or_start(
    godot: &Path,
    project: &Path,
) -> Result<(Arc<ClientInner>, Option<Child>)> {
    let port = std::env::var("GODOT_MCP_LSP_PORT")
        .ok()
        .and_then(|value| value.trim().parse().ok())
        .unwrap_or(DEFAULT_PORT);
    if let Ok(client) = connect_client(port, project).await {
        return Ok((client, None));
    }
    if !godot.exists() {
        return Err(McpError::Process(format!(
            "GDScript language server is not listening on 127.0.0.1:{port} and Godot was not found at '{}'",
            godot.display()
        )));
    }
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let private_port = listener.local_addr()?.port();
    drop(listener);
    let mut command = Command::new(godot);
    command
        .arg("--headless")
        .arg("--editor")
        .arg("--path")
        .arg(project)
        .arg("--lsp-port")
        .arg(private_port.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    #[cfg(unix)]
    command.process_group(0);
    let child = command.spawn().map_err(|err| {
        McpError::Process(format!(
            "failed to start a headless editor for the language server: {err}"
        ))
    })?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    loop {
        if let Ok(client) = connect_client(private_port, project).await {
            return Ok((client, Some(child)));
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(McpError::Process(
                "the headless editor did not open its language server within 60s".into(),
            ));
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

async fn connect_client(port: u16, project: &Path) -> Result<Arc<ClientInner>> {
    let stream = timeout(
        Duration::from_millis(400),
        TcpStream::connect(("127.0.0.1", port)),
    )
    .await
    .map_err(|_| {
        McpError::Process(format!(
            "timed out connecting to the language server on port {port}"
        ))
    })?
    .map_err(|err| {
        McpError::Process(format!(
            "cannot connect to the language server on port {port}: {err}"
        ))
    })?;
    let (mut read, mut write) = stream.into_split();
    let (outbound, mut queue) = mpsc::unbounded_channel();
    let client = Arc::new(ClientInner {
        outbound,
        pending: Mutex::new(HashMap::new()),
        next_id: AtomicI64::new(1),
        diagnostics: Mutex::new(HashMap::new()),
        closed: AtomicBool::new(false),
    });
    tokio::spawn(async move {
        while let Some(bytes) = queue.recv().await {
            if write.write_all(&bytes).await.is_err() {
                break;
            }
        }
    });
    let incoming = Arc::clone(&client);
    tokio::spawn(async move {
        let mut reader = MessageReader::default();
        let mut buf = vec![0_u8; 64 * 1024];
        loop {
            let n = match read.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            let messages = match reader.push(&buf[..n]) {
                Ok(messages) => messages,
                Err(_) => break,
            };
            for message in messages {
                dispatch(&incoming, message);
            }
        }
        incoming.closed.store(true, Ordering::Release);
        for (_, pending) in lock(&incoming.pending).drain() {
            let _ = pending.tx.send(Err(McpError::Process(
                "the language server closed the connection".into(),
            )));
        }
    });
    client
        .request(
            "initialize",
            json!({
                "processId": std::process::id(),
                "rootUri": path_to_uri(project),
                "rootPath": project.display().to_string(),
                "capabilities": {"textDocument": {"completion": {"completionItem": {"snippetSupport": false}}}},
                "clientInfo": {"name": "godot-mcp", "version": env!("CARGO_PKG_VERSION")},
            }),
            Duration::from_secs(20),
        )
        .await?;
    client.notify("initialized", json!({})).await?;
    Ok(client)
}

/// Tells the server the document is no longer open, so a script that is queried again later is
/// opened fresh instead of being opened twice. A failure to say so changes no answer.
async fn close_quiet(client: &ClientInner, file: &Path) {
    let _ = client
        .notify(
            "textDocument/didClose",
            json!({"textDocument": {"uri": path_to_uri(file)}}),
        )
        .await;
}

fn dispatch(client: &ClientInner, message: Value) {
    if let Some(method) = message.get("method").and_then(Value::as_str) {
        if message.get("id").is_some() {
            let id = message.get("id").cloned().unwrap_or(Value::Null);
            let result = if method == "workspace/applyEdit" {
                json!({"applied": false})
            } else {
                Value::Null
            };
            client.write_now(&json!({"jsonrpc": "2.0", "id": id, "result": result}));
            return;
        }
        if method == "textDocument/publishDiagnostics" {
            let uri = message
                .pointer("/params/uri")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let items = message
                .pointer("/params/diagnostics")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            lock(&client.diagnostics).insert(uri_key(&uri), items);
        }
        return;
    }
    let Some(id) = message.get("id").and_then(Value::as_i64) else {
        return;
    };
    let Some(pending) = lock(&client.pending).remove(&id) else {
        return;
    };
    if let Some(err) = message.get("error") {
        let _ = pending
            .tx
            .send(Err(McpError::ToolExecution(err.to_string())));
    } else {
        let _ = pending
            .tx
            .send(Ok(message.get("result").cloned().unwrap_or(Value::Null)));
    }
}

/// The `file://` URI of a path, as an LSP client sends it. On Windows a canonical path starts
/// with `\\?\`, which is not part of the file's name and would make the URI unusable; reserved
/// characters and spaces are percent-encoded.
fn path_to_uri(path: &Path) -> String {
    let absolute = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let mut text = absolute.to_string_lossy().replace('\\', "/");
    if let Some(rest) = text.strip_prefix("//?/") {
        text = rest.to_string();
    }
    if !text.starts_with('/') {
        text.insert(0, '/');
    }
    let mut encoded = String::with_capacity(text.len());
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' | b':' => {
                encoded.push(char::from(byte));
            }
            other => encoded.push_str(&format!("%{other:02X}")),
        }
    }
    format!("file://{encoded}")
}

/// A form of a file URI that is the same for every spelling of one file: Godot publishes
/// `file:///C%3A/My%20Game/a.gd`, a client may send `file:///C:/My%20Game/a.gd`. Percent escapes
/// are decoded, a leading slash before a drive letter is dropped, and on Windows the case is
/// ignored, as the file system does.
fn uri_key(uri: &str) -> String {
    let rest = uri.strip_prefix("file://").unwrap_or(uri);
    let mut bytes = Vec::with_capacity(rest.len());
    let raw = rest.as_bytes();
    let mut index = 0;
    while index < raw.len() {
        let decoded = (raw[index] == b'%' && index + 2 < raw.len())
            .then(|| std::str::from_utf8(&raw[index + 1..index + 3]).ok())
            .flatten()
            .and_then(|hex| u8::from_str_radix(hex, 16).ok());
        match decoded {
            Some(byte) => {
                bytes.push(byte);
                index += 3;
            }
            None => {
                bytes.push(raw[index]);
                index += 1;
            }
        }
    }
    let mut text = String::from_utf8_lossy(&bytes).replace('\\', "/");
    let drive = text.as_bytes();
    if drive.len() >= 3 && drive[0] == b'/' && drive[1].is_ascii_alphabetic() && drive[2] == b':' {
        text.remove(0);
    }
    if cfg!(windows) {
        text = text.to_lowercase();
    }
    text
}

fn shift_diagnostics(items: &[Value]) -> Vec<Value> {
    items
        .iter()
        .map(|item| {
            let mut copy = item.clone();
            if let Some(range) = copy.get_mut("range") {
                bump(range.get_mut("start"));
                bump(range.get_mut("end"));
            }
            copy
        })
        .collect()
}

fn bump(position: Option<&mut Value>) {
    let Some(position) = position else {
        return;
    };
    if let Some(line) = position.get("line").and_then(Value::as_u64) {
        position["line"] = json!(line + 1);
    }
    if let Some(character) = position.get("character").and_then(Value::as_u64) {
        position["character"] = json!(character + 1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    /// A client that is not connected to anything, and the end that keeps it open.
    fn fake_client() -> (Arc<ClientInner>, mpsc::UnboundedReceiver<Vec<u8>>) {
        let (outbound, inbox) = mpsc::unbounded_channel();
        let client = Arc::new(ClientInner {
            outbound,
            pending: Mutex::new(HashMap::new()),
            next_id: AtomicI64::new(1),
            diagnostics: Mutex::new(HashMap::new()),
            closed: AtomicBool::new(false),
        });
        (client, inbox)
    }

    /// A start-up that counts itself and takes a moment, like starting an editor does.
    fn counted_start(
        started: &Arc<AtomicUsize>,
        client: &Arc<ClientInner>,
    ) -> impl FnOnce(PathBuf) -> std::pin::Pin<Box<dyn Future<Output = StartResult> + Send>> {
        let started = Arc::clone(started);
        let client = Arc::clone(client);
        move |_root| {
            Box::pin(async move {
                started.fetch_add(1, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(100)).await;
                Ok((client, None))
            })
        }
    }

    #[tokio::test]
    async fn two_first_calls_start_one_editor() {
        let hub = LspHub::new(PathBuf::from("godot"));
        let project = tempfile::tempdir().unwrap();
        let started = Arc::new(AtomicUsize::new(0));
        let (client, _inbox) = fake_client();

        let (first, second) = tokio::join!(
            hub.client_with(project.path(), counted_start(&started, &client)),
            hub.client_with(project.path(), counted_start(&started, &client)),
        );

        assert_eq!(
            started.load(Ordering::SeqCst),
            1,
            "two editors were started"
        );
        assert!(Arc::ptr_eq(&first.unwrap(), &second.unwrap()));
    }

    #[tokio::test]
    async fn a_live_client_is_reused() {
        let hub = LspHub::new(PathBuf::from("godot"));
        let project = tempfile::tempdir().unwrap();
        let started = Arc::new(AtomicUsize::new(0));
        let (client, _inbox) = fake_client();

        hub.client_with(project.path(), counted_start(&started, &client))
            .await
            .unwrap();
        hub.client_with(project.path(), counted_start(&started, &client))
            .await
            .unwrap();

        assert_eq!(started.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn a_client_the_server_closed_is_replaced_before_anything_is_written() {
        let hub = LspHub::new(PathBuf::from("godot"));
        let project = tempfile::tempdir().unwrap();
        let started = Arc::new(AtomicUsize::new(0));
        let (first, _inbox) = fake_client();
        hub.client_with(project.path(), counted_start(&started, &first))
            .await
            .unwrap();

        first.closed.store(true, Ordering::Release);
        let (second, _inbox) = fake_client();
        let replaced = hub
            .client_with(project.path(), counted_start(&started, &second))
            .await
            .unwrap();

        assert_eq!(
            started.load(Ordering::SeqCst),
            2,
            "the closed client was kept"
        );
        assert!(Arc::ptr_eq(&replaced, &second));
    }

    #[tokio::test]
    async fn a_client_whose_connection_closed_is_replaced() {
        let hub = LspHub::new(PathBuf::from("godot"));
        let project = tempfile::tempdir().unwrap();
        let started = Arc::new(AtomicUsize::new(0));
        let (first, inbox) = fake_client();
        hub.client_with(project.path(), counted_start(&started, &first))
            .await
            .unwrap();

        // The editor went away: nothing reads what the client writes any more.
        drop(inbox);
        let (second, _inbox) = fake_client();
        let replaced = hub
            .client_with(project.path(), counted_start(&started, &second))
            .await
            .unwrap();

        assert_eq!(
            started.load(Ordering::SeqCst),
            2,
            "the dead client was kept"
        );
        assert!(Arc::ptr_eq(&replaced, &second));
    }

    #[test]
    fn the_uri_sent_is_clean_and_the_published_uri_finds_it_again() {
        // What a Windows canonical path looks like, with a space in it.
        let sent = path_to_uri(Path::new(r"\\?\C:\Users\Some One\game\a.gd"));
        assert!(sent.starts_with("file:///C:/"), "{sent}");
        assert!(!sent.contains(' '), "{sent}");
        assert!(!sent.contains("?/"), "{sent}");
        // What Godot publishes for the same file: the colon and the space are escaped.
        let published = "file:///C%3A/Users/Some%20One/game/a.gd";
        assert_eq!(uri_key(&sent), uri_key(published));
        assert_ne!(
            uri_key(&sent),
            uri_key("file:///C%3A/Users/Some%20One/game/b.gd")
        );
    }

    #[test]
    fn a_posix_path_keeps_its_leading_slash() {
        assert_eq!(
            path_to_uri(Path::new("/home/me/game/a.gd")),
            "file:///home/me/game/a.gd"
        );
        assert_eq!(
            uri_key("file:///home/me/game/a%2Bb.gd"),
            uri_key("file:///home/me/game/a+b.gd")
        );
    }
}
