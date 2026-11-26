//! Shared helpers for the integration tests: an MCP stdio session that drives the real
//! `godot-mcp` binary, and a scriptable fake Godot editor bridge.
#![allow(dead_code)]

pub mod adapter;

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
        if let Ok(text) = std::fs::read_to_string(path)
            && !text.trim().is_empty()
        {
            return text;
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

/// A stand-in for the Godot executable that prints a line, waits, prints another and exits.
pub fn slowly_printing_godot(dir: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        let path = dir.join("printing_godot.bat");
        std::fs::write(
            &path,
            "@echo off\r\necho first line\r\nping -n 3 127.0.0.1 > nul\r\necho second line\r\nping -n 3 127.0.0.1 > nul\r\n",
        )
        .expect("write printing godot");
        path
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join("printing_godot.sh");
        std::fs::write(
            &path,
            "#!/bin/sh\necho first line\nsleep 1\necho second line\nsleep 1\n",
        )
        .expect("write printing godot");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .expect("chmod printing godot");
        path
    }
}

/// A stand-in for the Godot executable that answers the class check of the authoring fallbacks:
/// asked for the class list (its script, `class_list.gd`, is the last argument) it lists a few
/// Node classes as scene roots and a few Resource classes as resources, and adds a line to
/// `class_lists.txt` next to itself. Asked for `--version` it prints a version and adds a line to
/// `versions.txt`. It answers the project setting type check as for a setting the engine does not
/// define, and the resource load check (a `res://` path last) as a file that loaded as the class
/// its header names.
pub fn classifying_godot(dir: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        let path = dir.join("classifying_godot.bat");
        std::fs::write(
            &path,
            "@echo off
setlocal
set PROJ=
set PREV=
set LAST=
for %%a in (%*) do call :arg %%a
if \"%LAST%\"==\"--version\" goto version
if \"%LAST:~-13%\"==\"class_list.gd\" goto classes
echo GODOT_MCP_SETTING={}
if \"%LAST:~0,6%\"==\"res://\" goto load
exit /b 0
:arg
if \"%PREV%\"==\"--path\" set PROJ=%~1
set PREV=%~1
set LAST=%~1
exit /b 0
:version
>>\"%~dp0versions.txt\" echo v
echo 4.8.beta1.stand-in
exit /b 0
:classes
>>\"%~dp0class_lists.txt\" echo c
echo GODOT_MCP_CLASSES={\"scene\":[\"Node\",\"Node2D\",\"Node3D\",\"Control\"],\"resource\":[\"Resource\",\"Curve\",\"Gradient\"]}
exit /b 0
:load
set FILE=%PROJ%\\%LAST:~6%
set FILE=%FILE:/=\\%
set CLASS=
for /f usebackq^ tokens^=2^ delims^=^\" %%t in (`findstr /b /l /c:\"[gd_resource\" \"%FILE%\"`) do set CLASS=%%t
echo GODOT_MCP_RESOURCE={\"loaded\":true,\"class\":\"%CLASS%\"}
exit /b 0
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
here=$(dirname \"$0\")
prev=; project=; last=
for arg; do
  [ \"$prev\" = --path ] && project=$arg
  prev=$arg; last=$arg
done
case \"$last\" in
  --version) echo v >> \"$here/versions.txt\"; echo 4.8.beta1.stand-in; exit 0 ;;
  *class_list.gd) echo c >> \"$here/class_lists.txt\"
    echo 'GODOT_MCP_CLASSES={\"scene\":[\"Node\",\"Node2D\",\"Node3D\",\"Control\"],\"resource\":[\"Resource\",\"Curve\",\"Gradient\"]}'
    exit 0 ;;
esac
echo 'GODOT_MCP_SETTING={}'
case \"$last\" in
  res://*) class=$(sed -n '1s/^\\[gd_resource type=\"\\([A-Za-z0-9_]*\\)\".*/\\1/p' \"$project/${last#res://}\")
    echo \"GODOT_MCP_RESOURCE={\\\"loaded\\\":true,\\\"class\\\":\\\"$class\\\"}\" ;;
esac
",
        )
        .expect("write classifying godot");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .expect("chmod classifying godot");
        path
    }
}

/// How many lines a stand-in added to the file `name` next to itself: how often it was asked.
pub fn stand_in_count(engine_dir: &Path, name: &str) -> usize {
    std::fs::read_to_string(engine_dir.join(name))
        .map(|text| text.lines().filter(|line| !line.trim().is_empty()).count())
        .unwrap_or(0)
}

/// A stand-in for the Godot executable that answers the project setting type check with
/// `answer` (the JSON after `GODOT_MCP_SETTING=`) and copies the request it was given to the
/// returned file.
pub fn setting_godot(dir: &Path, answer: &str) -> (PathBuf, PathBuf) {
    let record = dir.join("setting_request.json");
    #[cfg(windows)]
    {
        let path = dir.join("setting_godot.bat");
        std::fs::write(
            &path,
            format!(
                "@echo off\r\ncopy /y \"%~3\\request.json\" \"{}\" > nul\r\necho GODOT_MCP_SETTING={answer}\r\n",
                record.display()
            ),
        )
        .expect("write setting godot");
        (path, record)
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join("setting_godot.sh");
        std::fs::write(
            &path,
            format!(
                "#!/bin/sh\ncp \"$3/request.json\" '{}'\necho 'GODOT_MCP_SETTING={answer}'\n",
                record.display()
            ),
        )
        .expect("write setting godot");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .expect("chmod setting godot");
        (path, record)
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
    /// An error with a code, as addons that send error codes answer: (code, message).
    ErrCode(&'static str, String),
    After(Duration, Value),
    /// Sends this text as it is, with `$ID` replaced by the command id: a reply that is not
    /// necessarily valid JSON, written the way Godot's `JSON.stringify` writes it.
    Raw(String),
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
            Reply::ErrCode(code, message) => {
                let _ = out_tx.send(
                    json!({"commandId": id, "status": "error", "error": message, "code": code})
                        .to_string(),
                );
            }
            Reply::After(delay, result) => {
                let tx = out_tx.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(delay).await;
                    let _ = tx.send(success(&id, result));
                });
            }
            Reply::Raw(text) => {
                let id = id.as_str().unwrap_or_default();
                let _ = out_tx.send(text.replace("$ID", id));
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
        // The test executable sits in `<profile>/deps/` or, with the build layout of Rust 1.100,
        // in `<profile>/build/<package>/<hash>/out/`. The profile directory is the parent of the
        // nearest `deps` or `build` folder.
        let profile_dir = exe
            .ancestors()
            .skip(1)
            .find(|dir| {
                matches!(
                    dir.file_name().and_then(|name| name.to_str()),
                    Some("deps" | "build")
                )
            })
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
