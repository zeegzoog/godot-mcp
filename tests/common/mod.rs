//! Shared helpers for the integration tests: an MCP stdio session that drives the real
//! `godot-mcp` binary, and a scriptable fake Godot editor bridge.
#![allow(dead_code)]

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::net::TcpListener;
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message;

/// Returns a TCP port on 127.0.0.1 that nothing is listening on right now.
pub fn free_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    listener.local_addr().expect("local addr").port()
}

/// Creates a directory that looks like a Godot project (contains `project.godot`).
pub fn project_dir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("temp project");
    std::fs::write(
        dir.path().join("project.godot"),
        "config_version=5\n\n[application]\n\nconfig/name=\"Test\"\n",
    )
    .expect("write project.godot");
    dir
}

/// A stand-in for the Godot executable that writes noise to stdout and stderr, like a real
/// game process would.
pub fn fake_godot(dir: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        let path = dir.join("fake_godot.bat");
        std::fs::write(
            &path,
            "@echo off\r\necho NOISE_ON_STDOUT\r\necho NOISE_ON_STDERR 1>&2\r\n",
        )
        .expect("write fake godot");
        path
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join("fake_godot.sh");
        std::fs::write(
            &path,
            "#!/bin/sh\necho NOISE_ON_STDOUT\necho NOISE_ON_STDERR >&2\n",
        )
        .expect("write fake godot");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .expect("chmod fake godot");
        path
    }
}

/// A stand-in for the Godot executable that writes the arguments it was started with, on one
/// line, to the returned file. The file appears shortly after the process starts.
pub fn recording_godot(dir: &Path) -> (PathBuf, PathBuf) {
    let record = dir.join("godot_arguments.txt");
    #[cfg(windows)]
    {
        let path = dir.join("recording_godot.bat");
        std::fs::write(
            &path,
            format!("@echo off\r\necho %* > \"{}\"\r\n", record.display()),
        )
        .expect("write recording godot");
        (path, record)
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join("recording_godot.sh");
        std::fs::write(
            &path,
            format!("#!/bin/sh\necho \"$@\" > '{}'\n", record.display()),
        )
        .expect("write recording godot");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .expect("chmod recording godot");
        (path, record)
    }
}

/// Waits for a file a stand-in process writes, then returns what it holds.
pub async fn wait_for_file(path: &Path) -> String {
    for _ in 0..100 {
        if let Ok(text) = std::fs::read_to_string(path) {
            if !text.trim().is_empty() {
                return text;
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("{} was not written", path.display());
}

/// A stand-in for the Godot executable that stays alive for a few seconds, like an editor.
pub fn sleeping_godot(dir: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        let path = dir.join("sleeping_godot.bat");
        std::fs::write(
            &path,
            "@echo off
ping -n 9 127.0.0.1 > nul
",
        )
        .expect("write sleeping godot");
        path
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join("sleeping_godot.sh");
        std::fs::write(
            &path,
            "#!/bin/sh
sleep 8
",
        )
        .expect("write sleeping godot");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .expect("chmod sleeping godot");
        path
    }
}

/// A stand-in for the Godot executable that answers the class check of the authoring fallbacks:
/// it reads the class name after `--` (the last argument) and prints `GODOT_MCP_CLASS=scene` for
/// a few Node classes, `resource` for a few Resource classes and `no` for anything else.
pub fn classifying_godot(dir: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        let path = dir.join("classifying_godot.bat");
        std::fs::write(
            &path,
            "@echo off
for %%a in (%*) do set LAST=%%a
for %%c in (Node Node2D Node3D Control) do if \"%LAST%\"==\"%%c\" (echo GODOT_MCP_CLASS=scene& exit /b 0)
for %%c in (Resource Curve Gradient) do if \"%LAST%\"==\"%%c\" (echo GODOT_MCP_CLASS=resource& exit /b 0)
echo GODOT_MCP_CLASS=no
",
        )
        .expect("write classifying godot");
        path
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join("classifying_godot.sh");
        std::fs::write(
            &path,
            "#!/bin/sh
for last; do :; done
case \"$last\" in
  Node|Node2D|Node3D|Control) echo GODOT_MCP_CLASS=scene ;;
  Resource|Curve|Gradient) echo GODOT_MCP_CLASS=resource ;;
  *) echo GODOT_MCP_CLASS=no ;;
esac
",
        )
        .expect("write classifying godot");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .expect("chmod classifying godot");
        path
    }
}

/// A stand-in for the Godot executable that ends at once with the given exit code.
pub fn exiting_godot(dir: &Path, code: i32) -> PathBuf {
    #[cfg(windows)]
    {
        let path = dir.join("exiting_godot.bat");
        std::fs::write(
            &path,
            format!(
                "@echo off
exit /b {code}
"
            ),
        )
        .expect("write exiting godot");
        path
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join("exiting_godot.sh");
        std::fs::write(
            &path,
            format!(
                "#!/bin/sh
exit {code}
"
            ),
        )
        .expect("write exiting godot");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .expect("chmod exiting godot");
        path
    }
}

/// Concatenated text content of a `tools/call` response.
pub fn tool_text(response: &Value) -> String {
    response["result"]["content"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item["text"].as_str())
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

pub fn is_tool_error(response: &Value) -> bool {
    response["result"]["isError"].as_bool().unwrap_or(false)
}

/// An MCP client talking to a freshly spawned `godot-mcp` process over stdio.
pub struct Session {
    _child: Child,
    stdin: ChildStdin,
    stdout: Lines<BufReader<ChildStdout>>,
    next_id: u64,
    _cwd: tempfile::TempDir,
}

impl Session {
    pub async fn start(bridge_port: u16, godot: &Path) -> Self {
        Self::start_with_env(bridge_port, godot, &[]).await
    }

    pub async fn start_with_env(bridge_port: u16, godot: &Path, env: &[(&str, &str)]) -> Self {
        let cwd = tempfile::tempdir().expect("session cwd");
        Self::spawn(cwd, |command| {
            command
                .env("GODOT_PATH", godot)
                .env("GODOT_DEBUG_PORT", bridge_port.to_string());
            for (key, value) in env {
                command.env(key, value);
            }
        })
        .await
    }

    /// Starts the server with no GODOT_* variables in its environment but a `.env` file with
    /// the given contents in its working directory.
    pub async fn start_with_dotenv(contents: &str) -> Self {
        let cwd = tempfile::tempdir().expect("session cwd");
        std::fs::write(cwd.path().join(".env"), contents).expect("write .env");
        Self::spawn(cwd, |_| {}).await
    }

    async fn spawn(cwd: tempfile::TempDir, configure: impl FnOnce(&mut Command)) -> Self {
        let mut command = Command::new(env!("CARGO_BIN_EXE_godot-mcp"));
        for key in [
            "GODOT_PATH",
            "GODOT_CONSOLE_PATH",
            "GODOT_DEBUG_PORT",
            "GODOT_MCP_TOKEN",
            "GODOT_MCP_FEEDBACK",
            "GODOT_MCP_GH_PATH",
        ] {
            command.env_remove(key);
        }
        // Keep the run hermetic: never pick up a real bridge token file of the developer.
        command
            .env("APPDATA", cwd.path())
            .env("HOME", cwd.path())
            .env("XDG_DATA_HOME", cwd.path());
        configure(&mut command);
        command
            .current_dir(cwd.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);

        let mut child = command.spawn().expect("spawn godot-mcp");
        let stdin = child.stdin.take().expect("child stdin");
        let stdout = BufReader::new(child.stdout.take().expect("child stdout")).lines();
        let mut session = Self {
            _child: child,
            stdin,
            stdout,
            next_id: 1,
            _cwd: cwd,
        };
        // Wait until the server answers, so tests that assert on timing do not include the
        // process start-up (a freshly built executable can take seconds to launch on Windows).
        session
            .request("ping", json!({}), Duration::from_secs(60))
            .await;
        session
    }

    pub async fn send_raw(&mut self, line: &str) {
        self.stdin
            .write_all(format!("{line}\n").as_bytes())
            .await
            .expect("write to godot-mcp stdin");
        self.stdin.flush().await.expect("flush godot-mcp stdin");
    }

    pub async fn send(&mut self, value: &Value) {
        self.send_raw(&value.to_string()).await;
    }

    /// Next stdout frame, or `None` if nothing arrived in time. Panics when stdout carries
    /// anything that is not a JSON document: the channel is reserved for JSON-RPC frames.
    pub async fn recv(&mut self, within: Duration) -> Option<Value> {
        match timeout(within, self.stdout.next_line()).await {
            Err(_) | Ok(Ok(None)) => None,
            Ok(Ok(Some(line))) => Some(
                serde_json::from_str(&line)
                    .unwrap_or_else(|_| panic!("non-JSON line on MCP stdout: {line:?}")),
            ),
            Ok(Err(err)) => panic!("failed reading MCP stdout: {err}"),
        }
    }

    /// Sends a request and waits for the response carrying the same id.
    pub async fn request(&mut self, method: &str, params: Value, within: Duration) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))
            .await;
        loop {
            let frame = self
                .recv(within)
                .await
                .unwrap_or_else(|| panic!("no response to {method} within {within:?}"));
            if frame["id"] == json!(id) {
                return frame;
            }
        }
    }

    pub async fn call_tool(&mut self, name: &str, arguments: Value, within: Duration) -> Value {
        self.request(
            "tools/call",
            json!({"name": name, "arguments": arguments}),
            within,
        )
        .await
    }
}

/// How the fake editor answers a single command.
pub enum Reply {
    Ok(Value),
    OkWithEvent(Value, Value),
    Err(String),
    After(Duration, Value),
    Never,
}

/// A fake Godot editor bridge: a WebSocket server answering commands from a closure.
pub struct MockBridge {
    pub port: u16,
    pub received: Arc<Mutex<Vec<Value>>>,
    accept_task: JoinHandle<()>,
    connections: Arc<Mutex<Vec<JoinHandle<()>>>>,
}

impl MockBridge {
    pub async fn start<F>(handler: F) -> Self
    where
        F: Fn(&Value) -> Reply + Send + Sync + 'static,
    {
        Self::start_on(free_port(), handler).await
    }

    pub async fn start_on<F>(port: u16, handler: F) -> Self
    where
        F: Fn(&Value) -> Reply + Send + Sync + 'static,
    {
        let listener = bind_with_retry(port).await;
        let handler = Arc::new(handler);
        let received = Arc::new(Mutex::new(Vec::new()));
        let connections = Arc::new(Mutex::new(Vec::new()));

        let accept_task = tokio::spawn({
            let received = received.clone();
            let connections = connections.clone();
            async move {
                while let Ok((tcp, _)) = listener.accept().await {
                    let task = tokio::spawn(serve(tcp, handler.clone(), received.clone()));
                    connections.lock().expect("connections lock").push(task);
                }
            }
        });

        Self {
            port,
            received,
            accept_task,
            connections,
        }
    }

    /// Stops accepting and drops every open connection, like an editor being closed.
    pub fn shutdown(&self) {
        self.accept_task.abort();
        for task in self.connections.lock().expect("connections lock").drain(..) {
            task.abort();
        }
    }

    pub fn received(&self) -> Vec<Value> {
        self.received.lock().expect("received lock").clone()
    }
}

impl Drop for MockBridge {
    fn drop(&mut self) {
        self.shutdown();
    }
}

async fn bind_with_retry(port: u16) -> TcpListener {
    for _ in 0..40 {
        if let Ok(listener) = TcpListener::bind(("127.0.0.1", port)).await {
            return listener;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("could not bind mock bridge on port {port}");
}

async fn serve<F>(tcp: tokio::net::TcpStream, handler: Arc<F>, received: Arc<Mutex<Vec<Value>>>)
where
    F: Fn(&Value) -> Reply + Send + Sync + 'static,
{
    let Ok(ws) = tokio_tungstenite::accept_async(tcp).await else {
        return;
    };
    let (mut sink, mut stream) = ws.split();
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<String>();
    let writer = tokio::spawn(async move {
        while let Some(text) = out_rx.recv().await {
            if sink.send(Message::Text(text.into())).await.is_err() {
                break;
            }
        }
    });

    while let Some(Ok(message)) = stream.next().await {
        let Message::Text(text) = message else {
            continue;
        };
        let Ok(command) = serde_json::from_str::<Value>(text.as_str()) else {
            continue;
        };
        received
            .lock()
            .expect("received lock")
            .push(command.clone());
        let id = command["commandId"].clone();
        match handler(&command) {
            Reply::Ok(result) => {
                let _ = out_tx.send(success(&id, result));
            }
            Reply::OkWithEvent(result, event) => {
                let _ = out_tx.send(success(&id, result));
                let _ = out_tx.send(event.to_string());
            }
            Reply::Err(message) => {
                let _ = out_tx.send(
                    json!({"commandId": id, "status": "error", "error": message}).to_string(),
                );
            }
            Reply::After(delay, result) => {
                let tx = out_tx.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(delay).await;
                    let _ = tx.send(success(&id, result));
                });
            }
            Reply::Never => {}
        }
    }
    writer.abort();
}

fn success(id: &Value, result: Value) -> String {
    json!({"commandId": id, "status": "success", "result": result}).to_string()
}

/// The stand-in for the GitHub command-line client: an example target of this package. A test
/// run that did not build it (`cargo test --test ...` builds only the named test) builds it here,
/// once, with the profile of the running test; an up-to-date build costs a moment.
pub fn fake_gh() -> PathBuf {
    static PATH: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    PATH.get_or_init(|| {
        let exe = std::env::current_exe().expect("test executable path");
        let profile_dir = exe
            .parent()
            .and_then(Path::parent)
            .expect("profile directory")
            .to_path_buf();
        let name = if cfg!(windows) {
            "fake_gh.exe"
        } else {
            "fake_gh"
        };
        let built = profile_dir.join("examples").join(name);
        {
            let mut command = std::process::Command::new(env!("CARGO"));
            command
                .args(["build", "--example", "fake_gh", "--manifest-path"])
                .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"));
            match profile_dir.file_name().and_then(|name| name.to_str()) {
                Some("debug") | None => {}
                Some("release") => {
                    command.arg("--release");
                }
                Some(other) => {
                    command.args(["--profile", other]);
                }
            }
            let status = command.status().expect("run cargo for the fake gh example");
            assert!(status.success(), "building the fake gh example failed");
        }
        assert!(built.exists(), "fake gh not found at {}", built.display());
        built
    })
    .clone()
}

/// Environment pairs as the `&str` pairs the session helpers take.
pub fn as_pairs(owned: &[(String, String)]) -> Vec<(&str, &str)> {
    owned
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect()
}

/// A scripted stand-in for `gh`: what it answers is set through its environment, and what it
/// was called with is read back from the files it writes.
pub struct FakeGh {
    dir: tempfile::TempDir,
    script: Vec<(String, String)>,
}

impl FakeGh {
    pub fn new() -> Self {
        Self {
            dir: tempfile::tempdir().expect("fake gh directory"),
            script: Vec::new(),
        }
    }

    /// Sets one scripting variable (`FAKE_GH_CREATE_STDOUT`, `FAKE_GH_LIST_EXIT`, ...).
    pub fn script(mut self, key: &str, value: &str) -> Self {
        self.script.push((key.to_string(), value.to_string()));
        self
    }

    fn log_path(&self) -> PathBuf {
        self.dir.path().join("calls.log")
    }

    fn body_path(&self) -> PathBuf {
        self.dir.path().join("body.txt")
    }

    /// Variables that make a process run the stand-in as scripted and record into this object.
    pub fn env_pairs(&self) -> Vec<(String, String)> {
        let mut pairs = vec![
            (
                "FAKE_GH_LOG".to_string(),
                self.log_path().to_string_lossy().into_owned(),
            ),
            (
                "FAKE_GH_BODY".to_string(),
                self.body_path().to_string_lossy().into_owned(),
            ),
        ];
        pairs.extend(self.script.clone());
        pairs
    }

    /// The same, plus the setting that makes the server use the stand-in as its `gh`.
    pub fn server_env(&self) -> Vec<(String, String)> {
        let mut pairs = self.env_pairs();
        pairs.push((
            "GODOT_MCP_GH_PATH".to_string(),
            fake_gh().to_string_lossy().into_owned(),
        ));
        pairs
    }

    /// Every invocation so far, as its argument list.
    pub fn calls(&self) -> Vec<Vec<String>> {
        let Ok(log) = std::fs::read_to_string(self.log_path()) else {
            return Vec::new();
        };
        log.split("[call]\n")
            .skip(1)
            .map(|block| block.lines().map(str::to_string).collect())
            .collect()
    }

    /// Standard input of the last `issue create`, if there was one.
    pub fn body(&self) -> Option<String> {
        std::fs::read_to_string(self.body_path()).ok()
    }
}
