//! Minimal JSON-RPC client for the GDScript language server inside a Godot editor.
//!
//! Prefers an editor already listening on `GODOT_MCP_LSP_PORT` (default 6005) when that editor
//! has the same project open. Otherwise starts one private headless editor with `--lsp-port` and
//! stops it when the hub is dropped. A private editor never learns about scripts written after
//! it started, so it is started again when the project's scripts change.

use crate::error::{McpError, Result};
use crate::lsp::framing::{MessageReader, encode_message};
use crate::paths::same_directory;
use crate::util::lock;
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap};
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
    /// The project directory the server says it has open, when that is not the root this client
    /// asked for: Godot answers such an `initialize` with `gdscript_client/changeWorkspace`.
    workspace: Mutex<Option<String>>,
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
    /// The project's scripts when the private editor started; `None` for an editor's server.
    scripts: Option<ScriptStamp>,
}

/// Each script of a project (and `project.godot`) with its modification time and size.
type ScriptStamp = BTreeMap<PathBuf, (Option<std::time::SystemTime>, u64)>;

/// Whether the hub has a usable client for a project.
enum LiveState {
    Ready(Arc<ClientInner>),
    /// A private editor whose project changed since it started: its answers would be stale.
    Stale,
    Missing,
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
    /// Port an editor's language server may already be using. A private editor, when one is
    /// started, gets its own port.
    port: u16,
    live: Mutex<Option<Live>>,
    /// Held while an editor is being started, so that calls arriving meanwhile wait for it
    /// instead of starting another one that would replace it.
    starting: tokio::sync::Mutex<()>,
}

type StartResult = Result<(Arc<ClientInner>, Option<Child>)>;

impl LspHub {
    pub fn new(godot: PathBuf) -> Self {
        Self::with_port(godot, port_from_env())
    }

    /// Same as [`Self::new`], but the port an already-running editor is probed on is `port`
    /// instead of `GODOT_MCP_LSP_PORT`.
    pub fn with_port(godot: PathBuf, port: u16) -> Self {
        Self {
            godot,
            port,
            live: Mutex::new(None),
            starting: tokio::sync::Mutex::new(()),
        }
    }

    pub async fn diagnostics(
        &self,
        project: &Path,
        external_ok: bool,
        file: &Path,
    ) -> Result<Value> {
        let client = self.client_for(project, external_ok, file).await?;
        publish_diagnostics(&client, project, file).await
    }

    /// Diagnostics when a language server is already running for `project`: one this hub
    /// started, or an editor of this project listening on the configured port. Does not start an
    /// editor, except to replace a private one that no longer sees the project's scripts.
    /// `Ok(None)` means nothing was listening.
    pub async fn diagnostics_if_reachable(
        &self,
        project: &Path,
        external_ok: bool,
        file: &Path,
    ) -> Result<Option<Value>> {
        let project = std::fs::canonicalize(project).unwrap_or_else(|_| project.to_path_buf());
        match self.live_state(&project, external_ok, file).await {
            LiveState::Ready(client) => {
                return publish_diagnostics(&client, &project, file).await.map(Some);
            }
            LiveState::Stale => {
                let client = self.client_for(&project, external_ok, file).await?;
                return publish_diagnostics(&client, &project, file).await.map(Some);
            }
            LiveState::Missing => {}
        }
        if !external_ok {
            return Ok(None);
        }
        let client = match connect_client(self.port, &project).await {
            Ok(client) => client,
            Err(_) => return Ok(None),
        };
        let value = publish_diagnostics(&client, &project, file).await?;
        // A private editor is never replaced: dropping its entry kills it, even while another
        // project still uses it.
        let mut live = lock(&self.live);
        if live.as_ref().is_none_or(|current| {
            current.child.is_none() && !(current.root == project && current.client.is_open())
        }) {
            *live = Some(Live {
                client,
                child: None,
                root: project,
                scripts: None,
            });
        }
        Ok(Some(value))
    }

    pub async fn symbols(&self, project: &Path, external_ok: bool, file: &Path) -> Result<Value> {
        let client = self.client_for(project, external_ok, file).await?;
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
        external_ok: bool,
        file: &Path,
        line: u32,
        character: u32,
    ) -> Result<Value> {
        self.position(
            project,
            external_ok,
            file,
            "textDocument/completion",
            (line, character),
            json!({}),
        )
        .await
    }

    pub async fn definition(
        &self,
        project: &Path,
        external_ok: bool,
        file: &Path,
        line: u32,
        character: u32,
    ) -> Result<Value> {
        self.position(
            project,
            external_ok,
            file,
            "textDocument/definition",
            (line, character),
            json!({}),
        )
        .await
    }

    pub async fn references(
        &self,
        project: &Path,
        external_ok: bool,
        file: &Path,
        line: u32,
        character: u32,
    ) -> Result<Value> {
        self.position(
            project,
            external_ok,
            file,
            "textDocument/references",
            (line, character),
            json!({"context": {"includeDeclaration": true}}),
        )
        .await
    }

    async fn position(
        &self,
        project: &Path,
        external_ok: bool,
        file: &Path,
        method: &str,
        (line, character): (u32, u32),
        mut extra: Value,
    ) -> Result<Value> {
        let client = self.client_for(project, external_ok, file).await?;
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

    async fn client_for(
        &self,
        project: &Path,
        external_ok: bool,
        file: &Path,
    ) -> Result<Arc<ClientInner>> {
        let godot = self.godot.clone();
        let port = self.port;
        self.client_with(project, external_ok, file, |root, external_ok| async move {
            connect_or_start(&godot, &root, external_ok, port).await
        })
        .await
    }

    /// The client for `project`: the live one, or the one `start` makes when there is none,
    /// its connection has closed, or it is a private editor that started before a script other
    /// than `file` changed. Only one start runs at a time. `external_ok` says whether a
    /// language server an editor already runs may be used (CI-7): when it is false, only a server
    /// this hub started itself is reused, and `start` is told not to connect to the configured port.
    async fn client_with<F, Fut>(
        &self,
        project: &Path,
        external_ok: bool,
        file: &Path,
        start: F,
    ) -> Result<Arc<ClientInner>>
    where
        F: FnOnce(PathBuf, bool) -> Fut,
        Fut: Future<Output = StartResult>,
    {
        let project = std::fs::canonicalize(project).unwrap_or_else(|_| project.to_path_buf());
        if let LiveState::Ready(client) = self.live_state(&project, external_ok, file).await {
            return Ok(client);
        }
        let _starting = self.starting.lock().await;
        match self.live_state(&project, external_ok, file).await {
            LiveState::Ready(client) => return Ok(client),
            // Stopped before its replacement starts, so two editors never share the project.
            LiveState::Stale => drop(lock(&self.live).take()),
            LiveState::Missing => {}
        }
        // Taken before the start, so that a script written while the editor starts counts as
        // a change the next time.
        let scripts = script_stamp(&project).await;
        let (client, child) = start(project.clone(), external_ok).await?;
        let scripts = child.is_some().then_some(scripts);
        *lock(&self.live) = Some(Live {
            client: Arc::clone(&client),
            child,
            root: project,
            scripts,
        });
        Ok(client)
    }

    async fn live_state(&self, project: &Path, external_ok: bool, file: &Path) -> LiveState {
        let (client, started_with) = {
            let live = lock(&self.live);
            let Some(live) = live.as_ref().filter(|live| {
                live.root == project
                    && live.client.is_open()
                    && (external_ok || live.child.is_some())
            }) else {
                return LiveState::Missing;
            };
            (Arc::clone(&live.client), live.scripts.clone())
        };
        let Some(mut started_with) = started_with else {
            return LiveState::Ready(client);
        };
        // The file asked about is sent to the server with its text, so its own changes do not
        // make the server stale.
        let mut now = script_stamp(project).await;
        let file = std::fs::canonicalize(file).unwrap_or_else(|_| file.to_path_buf());
        started_with.remove(&file);
        now.remove(&file);
        if now == started_with {
            LiveState::Ready(client)
        } else {
            LiveState::Stale
        }
    }
}

/// The scripts of the project at `root` and its `project.godot`, which declares the autoloads.
/// Hidden directories (`.godot`, `.git`) and directories with a `.gdignore` file are skipped,
/// as Godot skips them; symbolic links are not followed.
async fn script_stamp(root: &Path) -> ScriptStamp {
    let root = root.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let mut stamp = ScriptStamp::new();
        let mut pending = vec![root.clone()];
        while let Some(dir) = pending.pop() {
            if dir != root && dir.join(".gdignore").exists() {
                continue;
            }
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let Ok(kind) = entry.file_type() else {
                    continue;
                };
                let path = entry.path();
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if kind.is_dir() {
                    if !name.starts_with('.') {
                        pending.push(path);
                    }
                } else if kind.is_file()
                    && (name.ends_with(".gd") || (dir == root && name == "project.godot"))
                {
                    let meta = entry.metadata().ok();
                    let modified = meta.as_ref().and_then(|meta| meta.modified().ok());
                    let size = meta.map_or(0, |meta| meta.len());
                    stamp.insert(path, (modified, size));
                }
            }
        }
        stamp
    })
    .await
    .unwrap_or_default()
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

fn port_from_env() -> u16 {
    std::env::var("GODOT_MCP_LSP_PORT")
        .ok()
        .and_then(|value| value.trim().parse().ok())
        .unwrap_or(DEFAULT_PORT)
}

async fn publish_diagnostics(client: &ClientInner, project: &Path, file: &Path) -> Result<Value> {
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
            return Err(McpError::Process(format!(
                "the GDScript language server did not publish diagnostics for the project {}",
                project.display()
            )));
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

async fn connect_or_start(
    godot: &Path,
    project: &Path,
    external_ok: bool,
    port: u16,
) -> Result<(Arc<ClientInner>, Option<Child>)> {
    if external_ok {
        if let Ok(client) = connect_client(port, project).await {
            return Ok((client, None));
        }
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
    let child = private_editor_command(godot, project, private_port)
        .spawn()
        .map_err(|err| {
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

/// The headless editor the language server runs in. If the project has this addon enabled, the
/// addon would open its bridge on the project's port in that editor too, and the editor tools
/// would then talk to an editor nobody can see; `GODOT_MCP_NO_BRIDGE` keeps it closed.
fn private_editor_command(godot: &Path, project: &Path, lsp_port: u16) -> Command {
    let mut command = Command::new(godot);
    command
        .arg("--headless")
        .arg("--editor")
        .arg("--path")
        .arg(project)
        .arg("--lsp-port")
        .arg(lsp_port.to_string())
        .env("GODOT_MCP_NO_BRIDGE", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    #[cfg(unix)]
    command.process_group(0);
    command
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
        workspace: Mutex::new(None),
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
    // Godot sends the notification before its answer, so it has been read by now.
    let workspace = lock(&client.workspace).clone();
    if let Some(open) = workspace.filter(|open| !same_directory(project, Path::new(open))) {
        return Err(McpError::Process(format!(
            "the language server on port {port} belongs to an editor with another project open ({open}), not {}",
            project.display()
        )));
    }
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
        } else if method == "gdscript_client/changeWorkspace" {
            let path = message.pointer("/params/path").and_then(Value::as_str);
            *lock(&client.workspace) = path.map(str::to_string);
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
            workspace: Mutex::new(None),
        });
        (client, inbox)
    }

    /// A start-up that counts itself and takes a moment, like starting an editor does.
    fn counted_start(
        started: &Arc<AtomicUsize>,
        client: &Arc<ClientInner>,
    ) -> impl FnOnce(PathBuf, bool) -> std::pin::Pin<Box<dyn Future<Output = StartResult> + Send>>
    {
        let started = Arc::clone(started);
        let client = Arc::clone(client);
        move |_root, _external_ok| {
            Box::pin(async move {
                started.fetch_add(1, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(100)).await;
                Ok((client, None))
            })
        }
    }

    #[tokio::test]
    async fn a_server_the_hub_did_not_start_is_not_reused_for_a_project_it_may_not_serve() {
        let hub = LspHub::new(PathBuf::from("godot"));
        let project = tempfile::tempdir().unwrap();
        let file = project.path().join("a.gd");
        let started = Arc::new(AtomicUsize::new(0));
        let (external, _inbox) = fake_client();
        hub.client_with(
            project.path(),
            true,
            &file,
            counted_start(&started, &external),
        )
        .await
        .unwrap();

        // The bridge now shows that the editor has another project open (CI-7).
        let (own, _inbox) = fake_client();
        let replaced = hub
            .client_with(project.path(), false, &file, counted_start(&started, &own))
            .await
            .unwrap();
        assert_eq!(
            started.load(Ordering::SeqCst),
            2,
            "the external server was reused"
        );
        assert!(Arc::ptr_eq(&replaced, &own));

        // Where an external server is acceptable again, the live one is simply kept.
        hub.client_with(project.path(), true, &file, counted_start(&started, &own))
            .await
            .unwrap();
        assert_eq!(started.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn two_first_calls_start_one_editor() {
        let hub = LspHub::new(PathBuf::from("godot"));
        let project = tempfile::tempdir().unwrap();
        let file = project.path().join("a.gd");
        let started = Arc::new(AtomicUsize::new(0));
        let (client, _inbox) = fake_client();

        let (first, second) = tokio::join!(
            hub.client_with(
                project.path(),
                true,
                &file,
                counted_start(&started, &client)
            ),
            hub.client_with(
                project.path(),
                true,
                &file,
                counted_start(&started, &client)
            ),
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
        let file = project.path().join("a.gd");
        let started = Arc::new(AtomicUsize::new(0));
        let (client, _inbox) = fake_client();

        hub.client_with(
            project.path(),
            true,
            &file,
            counted_start(&started, &client),
        )
        .await
        .unwrap();
        hub.client_with(
            project.path(),
            true,
            &file,
            counted_start(&started, &client),
        )
        .await
        .unwrap();

        assert_eq!(started.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn a_client_the_server_closed_is_replaced_before_anything_is_written() {
        let hub = LspHub::new(PathBuf::from("godot"));
        let project = tempfile::tempdir().unwrap();
        let file = project.path().join("a.gd");
        let started = Arc::new(AtomicUsize::new(0));
        let (first, _inbox) = fake_client();
        hub.client_with(project.path(), true, &file, counted_start(&started, &first))
            .await
            .unwrap();

        first.closed.store(true, Ordering::Release);
        let (second, _inbox) = fake_client();
        let replaced = hub
            .client_with(
                project.path(),
                true,
                &file,
                counted_start(&started, &second),
            )
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
        let file = project.path().join("a.gd");
        let started = Arc::new(AtomicUsize::new(0));
        let (first, inbox) = fake_client();
        hub.client_with(project.path(), true, &file, counted_start(&started, &first))
            .await
            .unwrap();

        // The editor went away: nothing reads what the client writes any more.
        drop(inbox);
        let (second, _inbox) = fake_client();
        let replaced = hub
            .client_with(
                project.path(),
                true,
                &file,
                counted_start(&started, &second),
            )
            .await
            .unwrap();

        assert_eq!(
            started.load(Ordering::SeqCst),
            2,
            "the dead client was kept"
        );
        assert!(Arc::ptr_eq(&replaced, &second));
    }

    /// A start-up that counts itself and returns a running child, as a private editor is.
    #[cfg(unix)]
    fn counted_private_start(
        started: &Arc<AtomicUsize>,
        client: &Arc<ClientInner>,
    ) -> impl FnOnce(PathBuf, bool) -> std::pin::Pin<Box<dyn Future<Output = StartResult> + Send>>
    {
        let started = Arc::clone(started);
        let client = Arc::clone(client);
        move |_root, _external_ok| {
            Box::pin(async move {
                started.fetch_add(1, Ordering::SeqCst);
                let child = Command::new("sleep")
                    .arg("30")
                    .kill_on_drop(true)
                    .spawn()
                    .unwrap();
                Ok((client, Some(child)))
            })
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_private_editor_is_started_again_when_another_script_changes() {
        let hub = LspHub::new(PathBuf::from("godot"));
        let project = tempfile::tempdir().unwrap();
        let file = project.path().join("uses_lift.gd");
        std::fs::write(&file, "extends Node\n").unwrap();
        let started = Arc::new(AtomicUsize::new(0));
        let (client, _inbox) = fake_client();
        hub.client_with(
            project.path(),
            true,
            &file,
            counted_private_start(&started, &client),
        )
        .await
        .unwrap();

        // The script asked about is sent with its text, so its own edits need no new editor.
        std::fs::write(&file, "extends Node\n\nvar lift: CrateLift\n").unwrap();
        hub.client_with(
            project.path(),
            true,
            &file,
            counted_private_start(&started, &client),
        )
        .await
        .unwrap();
        assert_eq!(started.load(Ordering::SeqCst), 1);

        // A new class elsewhere is unknown to an editor that started before it was written.
        std::fs::create_dir(project.path().join("world")).unwrap();
        std::fs::write(
            project.path().join("world/crate_lift.gd"),
            "class_name CrateLift\nextends Node3D\n",
        )
        .unwrap();
        let (second, _inbox) = fake_client();
        let replaced = hub
            .client_with(
                project.path(),
                true,
                &file,
                counted_private_start(&started, &second),
            )
            .await
            .unwrap();
        assert_eq!(
            started.load(Ordering::SeqCst),
            2,
            "the stale editor was kept"
        );
        assert!(Arc::ptr_eq(&replaced, &second));

        // Files Godot does not read as scripts change nothing.
        std::fs::create_dir(project.path().join(".godot")).unwrap();
        std::fs::write(project.path().join(".godot/cache.gd"), "x").unwrap();
        std::fs::write(project.path().join("notes.txt"), "x").unwrap();
        hub.client_with(
            project.path(),
            true,
            &file,
            counted_private_start(&started, &second),
        )
        .await
        .unwrap();
        assert_eq!(started.load(Ordering::SeqCst), 2);
    }

    /// A language server that answers `initialize` the way Godot does for a client whose root is
    /// not the editor's project: a `gdscript_client/changeWorkspace` naming its own, then the
    /// result. `open` is that project, or `None` when the roots agree.
    async fn fake_godot_server(open: Option<PathBuf>) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut reader = MessageReader::default();
            let mut buf = vec![0_u8; 64 * 1024];
            loop {
                let n = stream.read(&mut buf).await.unwrap_or(0);
                if n == 0 {
                    return;
                }
                for message in reader.push(&buf[..n]).unwrap() {
                    let Some(id) = message.get("id").cloned() else {
                        continue;
                    };
                    if let Some(open) = &open {
                        let notice = json!({
                            "jsonrpc": "2.0",
                            "method": "gdscript_client/changeWorkspace",
                            "params": {"path": open.to_string_lossy()},
                        });
                        stream
                            .write_all(&encode_message(&notice).unwrap())
                            .await
                            .unwrap();
                    }
                    let answer =
                        json!({"jsonrpc": "2.0", "id": id, "result": {"capabilities": {}}});
                    stream
                        .write_all(&encode_message(&answer).unwrap())
                        .await
                        .unwrap();
                }
            }
        });
        port
    }

    #[tokio::test]
    async fn a_language_server_with_another_project_open_is_refused() {
        let mine = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let port = fake_godot_server(Some(other.path().to_path_buf())).await;
        let refused = connect_client(port, mine.path()).await;
        let Err(error) = refused else {
            panic!("the other project's language server was used");
        };
        assert!(error.to_string().contains("another project"), "{error}");

        // The same directory spelled another way (macOS temp paths go through a symbolic
        // link) is the same project.
        let port = fake_godot_server(Some(mine.path().to_path_buf())).await;
        let canonical = std::fs::canonicalize(mine.path()).unwrap();
        assert!(connect_client(port, &canonical).await.is_ok());

        let port = fake_godot_server(None).await;
        assert!(connect_client(port, mine.path()).await.is_ok());
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
    fn the_private_editor_is_told_not_to_open_a_bridge() {
        let command = private_editor_command(Path::new("godot"), Path::new("/project"), 6123);
        let command = command.as_std();
        let args: Vec<String> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert!(
            args.windows(2)
                .any(|pair| pair[0] == "--lsp-port" && pair[1] == "6123"),
            "{args:?}"
        );
        assert!(
            command
                .get_envs()
                .any(|(name, value)| name == "GODOT_MCP_NO_BRIDGE"
                    && value == Some(std::ffi::OsStr::new("1"))),
            "the editor may open a bridge"
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
