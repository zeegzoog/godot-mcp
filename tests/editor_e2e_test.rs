//! End-to-end tests against a real headless Godot editor with the addon enabled.
//!
//! Ignored by default because they need a Godot 4.5+ executable. Run them with:
//!
//! ```bash
//! GODOT_PATH=/path/to/godot cargo test --test editor_e2e_test -- --ignored --test-threads=1
//! ```
//!
//! `GODOT_PATH` may also come from a `.env` file. `GODOT_CONSOLE_PATH` (Windows) is used
//! for the editor process when set, so its log can be inspected.

mod common;

use godot_mcp::bridge::{BridgeClient, token_path};
use godot_mcp::error::McpError;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};
use tokio::process::{Child, Command};
use tokio::time::{sleep, timeout};

const IGNORE_REASON: &str = "needs GODOT_PATH pointing at a Godot 4.5+ executable";

struct Editor {
    _dir: tempfile::TempDir,
    project: PathBuf,
    port: u16,
    bridge: BridgeClient,
    log: PathBuf,
    _child: Child,
}

impl Drop for Editor {
    fn drop(&mut self) {
        if std::thread::panicking() {
            // A failed assertion rarely explains itself; the editor's own output usually does.
            let log = self.log_text();
            let start = log.len().saturating_sub(6000);
            let start = (start..log.len())
                .find(|&index| log.is_char_boundary(index))
                .unwrap_or(0);
            eprintln!("--- editor log (tail) ---\n{}", &log[start..]);
        }
        if let Some(path) = token_path(self.port) {
            let _ = std::fs::remove_file(path);
        }
    }
}

impl Editor {
    fn log_text(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }
}

fn godot_binaries() -> (PathBuf, PathBuf) {
    // The repository's own .env only, never one in a parent directory.
    let _ = dotenvy::from_path(concat!(env!("CARGO_MANIFEST_DIR"), "/.env"));
    let godot = std::env::var("GODOT_PATH")
        .ok()
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .expect("set GODOT_PATH to run the editor end-to-end tests");
    let console = std::env::var("GODOT_CONSOLE_PATH")
        .ok()
        .map(PathBuf::from)
        .filter(|path| path.exists())
        .unwrap_or_else(|| godot.clone());
    (godot, console)
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).expect("create dir");
    for entry in std::fs::read_dir(from).expect("read dir") {
        let entry = entry.expect("dir entry");
        let target = to.join(entry.file_name());
        if entry.file_type().expect("file type").is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).expect("copy file");
        }
    }
}

const MAIN_SCENE: &str = r#"[gd_scene format=3]

[node name="Main" type="Node2D"]

[node name="Player" type="Node2D" parent="."]
position = Vector2(5, 7)

[node name="Body" type="Sprite2D" parent="Player"]

[node name="Weapon" type="Node2D" parent="Player"]

[node name="UI" type="Control" parent="."]
"#;

/// Test-only command that undoes the last editor action, appended to a *copy* of the addon.
const UNDO_HOOK: &str = r#"        "__test_undo":
            var history_id: int = undo_redo.get_object_history_id(EditorInterface.get_edited_scene_root())
            undo_redo.get_history_undo_redo(history_id).undo()
            send_success(peer, cmd_id, {"undone": true})
        "__test_undo_resource":
            var undone_resource: Resource = load(str(params.get("resource_path", "")))
            undo_redo.get_history_undo_redo(undo_redo.get_object_history_id(undone_resource)).undo()
            send_success(peer, cmd_id, {"undone": true})
        "__test_hand_edit":
            var root: Node = EditorInterface.get_edited_scene_root()
            var node := Node.new()
            node.name = "ByHand"
            undo_redo.create_action("Hand edit", UndoRedo.MERGE_DISABLE, root)
            undo_redo.add_do_method(root, "add_child", node, true)
            undo_redo.add_do_property(node, "owner", root)
            undo_redo.add_do_reference(node)
            undo_redo.add_undo_method(root, "remove_child", node)
            undo_redo.commit_action()
            send_success(peer, cmd_id, {"added": true})
        "__test_open_directly":
            EditorInterface.open_scene_from_path(str(params.get("scene_path", "")))
            send_success(peer, cmd_id, {"opened": true})
        "ping":
"#;

async fn start_editor(with_undo_hook: bool) -> Editor {
    start_editor_configured(with_undo_hook, "").await
}

async fn start_editor_configured(with_undo_hook: bool, project_tail: &str) -> Editor {
    let hook = with_undo_hook.then_some(UNDO_HOOK);
    start_editor_with(hook, "", &[], project_tail).await
}

/// Starts an editor on a fixture project. `hook` is match arms for test-only commands inserted
/// into a *copy* of the bridge, `helpers` is functions appended to it, and `files` are extra
/// project files (name, text).
async fn start_editor_with(
    hook: Option<&str>,
    helpers: &str,
    files: &[(&str, &str)],
    project_tail: &str,
) -> Editor {
    start_editor_with_args(hook, helpers, files, project_tail, &[]).await
}

/// `start_editor_with`, passing `editor_args` to the editor as well.
async fn start_editor_with_args(
    hook: Option<&str>,
    helpers: &str,
    files: &[(&str, &str)],
    project_tail: &str,
    editor_args: &[&str],
) -> Editor {
    let (godot, console) = godot_binaries();
    let dir = tempfile::tempdir().expect("project dir");
    let project = dir.path().to_path_buf();

    std::fs::write(
        project.join("project.godot"),
        format!(
            "config_version=5\n\n[application]\n\nconfig/name=\"MCP E2E Fixture\"\n\
             run/main_scene=\"res://main.tscn\"\nconfig/features=PackedStringArray(\"4.5\")\n\n\
             [editor_plugins]\n\nenabled=PackedStringArray(\"res://addons/godot_mcp/plugin.cfg\")\n{project_tail}"
        ),
    )
    .expect("project.godot");
    std::fs::write(project.join("main.tscn"), MAIN_SCENE).expect("main.tscn");
    std::fs::write(project.join("script.gd"), "extends Node\n").expect("script.gd");
    for (name, text) in files {
        if let Some(parent) = project.join(name).parent() {
            std::fs::create_dir_all(parent).expect("fixture folder");
        }
        std::fs::write(project.join(name), text).expect("fixture file");
    }

    let mut big = String::from("[gd_scene format=3]\n\n[node name=\"Big\" type=\"Node2D\"]\n");
    for i in 0..3000 {
        big.push_str(&format!(
            "\n[node name=\"Node{i:04}\" type=\"Node2D\" parent=\".\"]\n"
        ));
    }
    std::fs::write(project.join("big.tscn"), big).expect("big.tscn");

    let addon = project.join("addons/godot_mcp");
    copy_dir(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("addons/godot_mcp"),
        &addon,
    );
    if hook.is_some() || !helpers.is_empty() {
        let script_path = addon.join("bridge_server.gd");
        let mut source = std::fs::read_to_string(&script_path).expect("bridge script");
        if let Some(hook) = hook {
            let anchor = "        \"ping\":\n";
            assert!(source.contains(anchor), "dispatch table anchor not found");
            source = source.replacen(anchor, hook, 1);
        }
        source.push_str(helpers);
        std::fs::write(&script_path, source).expect("patch bridge script");
    }

    let imported = timeout(
        Duration::from_secs(180),
        Command::new(&godot)
            .args(["--headless", "--path"])
            .arg(&project)
            .arg("--import")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status(),
    )
    .await
    .expect("import timed out")
    .expect("run godot --import");
    assert!(imported.success(), "godot --import failed");

    let port = common::free_port();
    let log = project.join("editor.log");
    let log_file = std::fs::File::create(&log).expect("log file");
    let child = Command::new(&console)
        .args(["--headless", "--editor", "--path"])
        .arg(&project)
        .args(editor_args)
        .env("GODOT_DEBUG_PORT", port.to_string())
        .stdout(Stdio::from(log_file.try_clone().expect("clone log")))
        .stderr(Stdio::from(log_file))
        .kill_on_drop(true)
        .spawn()
        .expect("spawn godot editor");

    let bridge = BridgeClient::new(port);
    wait_until_started(&bridge, &log, &format!("port {port}")).await;

    Editor {
        _dir: dir,
        project,
        port,
        bridge,
        log,
        _child: child,
    }
}

/// Waits until the editor's bridge answers and the editor has finished starting: the bridge
/// listens during the first scan of the project's files, and the editor opens its scenes only
/// when that scan ends, so a scene a test opened before would lose its tab to the main scene.
async fn wait_until_started(bridge: &BridgeClient, log: &Path, port: &str) {
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        if let Ok(pong) = bridge.send_command("ping", json!({})).await
            && pong["starting"] != true
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "editor bridge on {port} did not come up or the editor did not finish starting; log:\n{}",
            std::fs::read_to_string(log).unwrap_or_default()
        );
        sleep(Duration::from_millis(100)).await;
    }
}

/// The macOS spelling `ProjectSettings.localize_path` does not convert.
fn macos_var_spelling(path: &Path) -> PathBuf {
    let text = path.to_string_lossy().replace('\\', "/");
    text.strip_prefix("/private/var/")
        .map(|rest| PathBuf::from(format!("/var/{rest}")))
        .unwrap_or_else(|| path.to_path_buf())
}

/// The editor the server starts for the language server runs the project's addon too. With
/// `GODOT_MCP_NO_BRIDGE=1` the addon must stay loaded but open no bridge, otherwise the editor
/// tools would find a hidden editor on the project's port.
#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn an_editor_told_not_to_open_a_bridge_opens_none() {
    let (godot, console) = godot_binaries();
    let dir = tempfile::tempdir().expect("project dir");
    let project = dir.path();
    std::fs::write(
        project.join("project.godot"),
        "config_version=5\n\n[application]\n\nconfig/name=\"No Bridge\"\nconfig/features=PackedStringArray(\"4.5\")\n\n\
         [editor_plugins]\n\nenabled=PackedStringArray(\"res://addons/godot_mcp/plugin.cfg\")\n",
    )
    .expect("project.godot");
    copy_dir(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("addons/godot_mcp"),
        &project.join("addons/godot_mcp"),
    );
    let imported = timeout(
        Duration::from_secs(180),
        Command::new(&godot)
            .args(["--headless", "--path"])
            .arg(project)
            .arg("--import")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status(),
    )
    .await
    .expect("import timed out")
    .expect("run godot --import");
    assert!(imported.success(), "godot --import failed");

    let port = common::free_port();
    let log = project.join("editor.log");
    let log_file = std::fs::File::create(&log).expect("log file");
    let _child = Command::new(&console)
        .args(["--headless", "--editor", "--path"])
        .arg(project)
        .env("GODOT_DEBUG_PORT", port.to_string())
        .env("GODOT_MCP_NO_BRIDGE", "1")
        .stdout(Stdio::from(log_file.try_clone().expect("clone log")))
        .stderr(Stdio::from(log_file))
        .kill_on_drop(true)
        .spawn()
        .expect("spawn godot editor");

    // The addon says it ran and chose to open nothing, so a closed port is not just an addon
    // that failed to load.
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        let text = std::fs::read_to_string(&log).unwrap_or_default();
        if text.contains("bridge is switched off") {
            break;
        }
        assert!(
            !text.contains("Plugin activated successfully"),
            "the addon opened its bridge; log:\n{text}"
        );
        assert!(
            Instant::now() < deadline,
            "the addon never said what it did; log:\n{text}"
        );
        sleep(Duration::from_millis(500)).await;
    }
    sleep(Duration::from_secs(2)).await;
    assert!(
        tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .is_err(),
        "something is listening on the bridge port"
    );
}

fn expect_error(result: Result<Value, McpError>, needle: &str) {
    match result {
        Err(McpError::ToolExecution(message) | McpError::CodedToolExecution { message, .. }) => {
            assert!(
                message.contains(needle),
                "error {message:?} should mention {needle:?}"
            )
        }
        other => panic!("expected an editor error mentioning {needle:?}, got {other:?}"),
    }
}

fn find<'a>(node: &'a Value, name: &str) -> Option<&'a Value> {
    if node["name"] == name {
        return Some(node);
    }
    node["children"]
        .as_array()?
        .iter()
        .find_map(|child| find(child, name))
}

fn count_nodes(node: &Value) -> usize {
    1 + node["children"]
        .as_array()
        .map(|children| children.iter().map(count_nodes).sum())
        .unwrap_or(0)
}

fn child_names(node: &Value) -> Vec<String> {
    node["children"]
        .as_array()
        .map(|children| {
            children
                .iter()
                .filter_map(|child| child["name"].as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn the_bridge_lists_its_commands_and_refuses_parameters_it_does_not_have() {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::connect_async;
    use tokio_tungstenite::tungstenite::Message;

    let editor = start_editor(false).await;
    wait_until_started(&editor.bridge, &editor.log, "the editor").await;

    let version = editor
        .bridge
        .send_command("get_version", json!({}))
        .await
        .expect("version");
    assert_eq!(
        version["protocol"],
        godot_mcp::bridge_protocol::BRIDGE_PROTOCOL,
        "{version}"
    );
    let open_scene: Vec<&str> = version["commands"]["open_scene"]
        .as_array()
        .expect("open_scene is listed")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    for name in ["scene_path", "reload", "discard"] {
        assert!(open_scene.contains(&name), "{version}");
    }
    assert_eq!(version["commands"]["save_scene"], json!([]), "{version}");

    // The addon itself refuses a parameter it does not have, as an older addon receiving a
    // newer server's call would, instead of ignoring it.
    let token = editor.bridge.secret().expect("bridge token");
    let (mut ws, _) = connect_async(format!("ws://127.0.0.1:{}", editor.port))
        .await
        .expect("loopback handshake");
    let mut ask = async |params: Value| -> Value {
        let command =
            json!({"commandId": "p", "type": "get_open_scenes", "params": params, "token": token});
        ws.send(Message::Text(command.to_string().into()))
            .await
            .expect("send");
        let reply = timeout(Duration::from_secs(5), ws.next())
            .await
            .expect("reply in time")
            .expect("stream open")
            .expect("frame");
        serde_json::from_str(reply.to_text().expect("text frame")).expect("json")
    };
    let refused = ask(json!({"newer_option": true})).await;
    assert_eq!(refused["status"], "error", "{refused}");
    let message = refused["error"].as_str().unwrap_or_default();
    assert!(
        message.contains("Unknown parameter 'newer_option' for command 'get_open_scenes'"),
        "{refused}"
    );
    assert!(message.contains("godot_update_addon"), "{refused}");
    let null_ok = ask(json!({"newer_option": null})).await;
    assert_eq!(
        null_ok["status"], "success",
        "a null value is not given: {null_ok}"
    );

    // The bridge client refuses it from the list, before it is sent.
    expect_error(
        editor
            .bridge
            .send_command(
                "open_scene",
                json!({"scene_path": "res://main.tscn", "newer_option": 1}),
            )
            .await,
        "Unknown parameter 'newer_option' for command 'open_scene'",
    );
}

#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn bridge_is_loopback_only_and_requires_the_token() {
    let _ = IGNORE_REASON;
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::connect_async;
    use tokio_tungstenite::tungstenite::Message;

    let editor = start_editor(false).await;
    let url = format!("ws://127.0.0.1:{}", editor.port);

    for token in [None, Some("wrong-token")] {
        let (mut ws, _) = connect_async(&url).await.expect("loopback handshake");
        let mut command = json!({"commandId": "t", "type": "get_project_info", "params": {}});
        if let Some(token) = token {
            command["token"] = json!(token);
        }
        ws.send(Message::Text(command.to_string().into()))
            .await
            .expect("send");
        let reply = timeout(Duration::from_secs(5), ws.next())
            .await
            .expect("reply in time")
            .expect("stream open")
            .expect("frame");
        let reply: Value =
            serde_json::from_str(reply.to_text().expect("text frame")).expect("json");
        assert_eq!(reply["status"], "error", "{reply}");
        assert!(
            reply["error"]
                .as_str()
                .unwrap_or_default()
                .contains("Unauthorized"),
            "{reply}"
        );
        assert_eq!(reply["code"], "UNAUTHORIZED", "{reply}");
    }

    // The authenticated client works.
    let info = editor
        .bridge
        .send_command("get_project_info", json!({}))
        .await
        .expect("authorized call");
    assert_eq!(info["name"], "MCP E2E Fixture");

    // Other OS users cannot read the token file (it is not written when GODOT_MCP_TOKEN is set).
    #[cfg(unix)]
    if std::env::var_os("GODOT_MCP_TOKEN").is_none() {
        use std::os::unix::fs::PermissionsExt;
        let path = token_path(editor.port).expect("token path");
        let mode = std::fs::metadata(&path)
            .expect("token file")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(
            mode,
            0o600,
            "token file {} has mode {mode:o}",
            path.display()
        );
        // Nor list or open anything in its folder, even while the file is being written.
        let folder = path.parent().expect("token folder");
        let folder_mode = std::fs::metadata(folder)
            .expect("token folder")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(
            folder_mode,
            0o700,
            "token folder {} has mode {folder_mode:o}",
            folder.display()
        );
    }

    // Not reachable through any non-loopback address of this host.
    let lan_ip = std::net::UdpSocket::bind("0.0.0.0:0")
        .and_then(|socket| {
            socket.connect("192.0.2.1:9")?;
            socket.local_addr()
        })
        .map(|addr| addr.ip())
        .ok()
        .filter(|ip| !ip.is_loopback() && !ip.is_unspecified());
    if let Some(ip) = lan_ip {
        let attempt = timeout(
            Duration::from_secs(3),
            tokio::net::TcpStream::connect((ip, editor.port)),
        )
        .await;
        assert!(
            !matches!(attempt, Ok(Ok(_))),
            "bridge accepted a connection on {ip}"
        );
    }
}

type RawPeer =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// Test-only command that sends an event to the bridge's peers.
const BROADCAST_HOOK: &str = r#"        "__test_broadcast":
            broadcast_event("test.event", {"n": 1})
            send_success(peer, cmd_id, {"sent": true})
        "ping":
"#;

/// Opens a WebSocket to the bridge without sending anything.
async fn raw_peer(port: u16) -> RawPeer {
    let url = format!("ws://127.0.0.1:{port}");
    let (ws, _) = timeout(
        Duration::from_secs(5),
        tokio_tungstenite::connect_async(&url),
    )
    .await
    .expect("handshake in time")
    .expect("loopback handshake");
    ws
}

/// Sends one command and returns the editor's reply to it (events are skipped).
async fn raw_command(ws: &mut RawPeer, command: Value) -> Value {
    use futures_util::SinkExt;
    use tokio_tungstenite::tungstenite::Message;
    ws.send(Message::Text(command.to_string().into()))
        .await
        .expect("send");
    raw_reply(ws).await
}

/// The next reply the editor sends (events are skipped).
async fn raw_reply(ws: &mut RawPeer) -> Value {
    use futures_util::StreamExt;
    use tokio_tungstenite::tungstenite::Message;
    loop {
        let frame = timeout(Duration::from_secs(10), ws.next())
            .await
            .expect("reply in time")
            .expect("stream open")
            .expect("frame");
        let Message::Text(text) = frame else {
            panic!("expected a text reply, got {frame:?}");
        };
        let reply: Value = serde_json::from_str(&text).expect("json");
        if reply["type"] != "event" {
            return reply;
        }
    }
}

/// A peer that has sent a command with the token.
async fn authenticated_peer(editor: &Editor) -> RawPeer {
    let mut ws = raw_peer(editor.port).await;
    let token = editor.bridge.secret().expect("bridge token");
    let reply = raw_command(
        &mut ws,
        json!({"commandId": "auth", "type": "ping", "params": {}, "token": token}),
    )
    .await;
    assert_eq!(reply["status"], "success", "{reply}");
    ws
}

/// Waits for the editor to close the connection; panics on any other frame or after `within`.
async fn assert_closed(ws: &mut RawPeer, within: Duration, what: &str) {
    use futures_util::StreamExt;
    use tokio_tungstenite::tungstenite::Message;
    let deadline = Instant::now() + within;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match timeout(left, ws.next()).await {
            Err(_) => panic!("{what}: the editor did not close the connection within {within:?}"),
            Ok(None) | Ok(Some(Err(_))) | Ok(Some(Ok(Message::Close(_)))) => return,
            Ok(Some(Ok(Message::Ping(_) | Message::Pong(_)))) => continue,
            Ok(Some(Ok(frame))) => panic!("{what}: unexpected frame {frame:?}"),
        }
    }
}

/// Game output reaches only peers that sent the token: any web page can open a WebSocket to
/// 127.0.0.1, since browsers do not apply CORS to it.
#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn bridge_sends_events_only_to_peers_that_sent_the_token() {
    use futures_util::StreamExt;
    use tokio_tungstenite::tungstenite::Message;

    let editor = start_editor_with(Some(BROADCAST_HOOK), "", &[], "").await;
    let mut events = editor.bridge.subscribe_events();
    let mut stranger = raw_peer(editor.port).await;
    let mut member = authenticated_peer(&editor).await;

    let sent = editor
        .bridge
        .send_command("__test_broadcast", json!({}))
        .await
        .expect("broadcast");
    assert_eq!(sent["sent"], true, "{sent}");

    let event = timeout(Duration::from_secs(5), events.recv())
        .await
        .expect("event in time")
        .expect("event channel");
    assert_eq!(event["event"], "test.event", "{event}");
    let frame = timeout(Duration::from_secs(5), member.next())
        .await
        .expect("event in time")
        .expect("stream open")
        .expect("frame");
    let Message::Text(text) = frame else {
        panic!("expected an event, got {frame:?}");
    };
    let event: Value = serde_json::from_str(&text).expect("json");
    assert_eq!(event["event"], "test.event", "{event}");

    match timeout(Duration::from_secs(1), stranger.next()).await {
        Err(_) => {}
        Ok(frame) => panic!("a peer without the token received {frame:?}"),
    }
}

/// A peer that sends a wrong token, no token, or nothing at all is closed: before, it stayed
/// connected for as long as it liked.
#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn bridge_closes_peers_that_do_not_authenticate() {
    use futures_util::SinkExt;
    use tokio_tungstenite::tungstenite::Message;

    let editor = start_editor(false).await;

    for token in [None, Some("wrong-token")] {
        let mut ws = raw_peer(editor.port).await;
        let mut command = json!({"commandId": "t", "type": "ping", "params": {}});
        if let Some(token) = token {
            command["token"] = json!(token);
        }
        let reply = raw_command(&mut ws, command).await;
        assert!(
            reply["error"]
                .as_str()
                .unwrap_or_default()
                .contains("Unauthorized"),
            "{reply}"
        );
        assert_closed(&mut ws, Duration::from_secs(5), "wrong token").await;
    }

    // An authenticated peer that later sends a wrong token is closed too.
    let mut member = authenticated_peer(&editor).await;
    let reply = raw_command(
        &mut member,
        json!({"commandId": "t", "type": "ping", "params": {}, "token": "wrong"}),
    )
    .await;
    assert_eq!(reply["status"], "error", "{reply}");
    assert_closed(&mut member, Duration::from_secs(5), "later wrong token").await;

    // Garbage before the token.
    let mut garbage = raw_peer(editor.port).await;
    garbage
        .send(Message::Text("not json".into()))
        .await
        .expect("send");
    let reply = raw_reply(&mut garbage).await;
    assert_eq!(reply["status"], "error", "{reply}");
    assert_closed(&mut garbage, Duration::from_secs(5), "invalid JSON").await;

    // Silence.
    let mut silent = raw_peer(editor.port).await;
    assert_closed(&mut silent, Duration::from_secs(15), "no command").await;

    // The server's own connection is untouched.
    editor
        .bridge
        .send_command("ping", json!({}))
        .await
        .expect("authorized call");
}

/// The bridge holds at most 8 connections. Peers that have not sent the token make room for
/// new ones; when all have, a new connection is refused.
#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn bridge_caps_its_connections() {
    let editor = start_editor(false).await;
    // editor.bridge holds one connection already.
    let mut members = Vec::new();
    for _ in 0..7 {
        members.push(authenticated_peer(&editor).await);
    }

    let url = format!("ws://127.0.0.1:{}", editor.port);
    let refused = timeout(
        Duration::from_secs(5),
        tokio_tungstenite::connect_async(&url),
    )
    .await
    .expect("answer in time");
    if let Ok((mut ws, _)) = refused {
        assert_closed(&mut ws, Duration::from_secs(5), "a ninth connection").await;
    }

    // Free one place and take it without authenticating.
    let mut leaving = members.pop().expect("a member");
    leaving.close(None).await.expect("close");
    drop(leaving);
    let mut stranger = None;
    for _ in 0..50 {
        sleep(Duration::from_millis(100)).await;
        let Ok(Ok((mut ws, _))) = timeout(
            Duration::from_secs(5),
            tokio_tungstenite::connect_async(&url),
        )
        .await
        else {
            continue;
        };
        // A refused connection is closed right away; an accepted one stays open.
        if timeout(
            Duration::from_millis(300),
            futures_util::StreamExt::next(&mut ws),
        )
        .await
        .is_err()
        {
            stranger = Some(ws);
            break;
        }
    }
    let mut stranger = stranger.expect("the freed place was not taken");

    // A client with the token gets the stranger's place once the stranger's grace period is over.
    sleep(Duration::from_millis(1200)).await;
    let mut newcomer = authenticated_peer(&editor).await;
    assert_closed(&mut stranger, Duration::from_secs(5), "the stranger").await;
    let token = editor.bridge.secret().expect("bridge token");
    let reply = raw_command(
        &mut newcomer,
        json!({"commandId": "again", "type": "ping", "params": {}, "token": token}),
    )
    .await;
    assert_eq!(reply["status"], "success", "{reply}");
    editor
        .bridge
        .send_command("ping", json!({}))
        .await
        .expect("the server's own connection still works");
}

/// A flood of connections from another local process does not push out a client that has just
/// connected and not yet sent its first command, and a client connecting during the flood still
/// gets in.
#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn a_flood_of_connections_does_not_push_out_a_new_client() {
    let editor = start_editor(false).await;
    let url = format!("ws://127.0.0.1:{}", editor.port);
    let mut client = raw_peer(editor.port).await;
    let mut flood = Vec::new();
    for _ in 0..12 {
        if let Ok(Ok((ws, _))) = timeout(
            Duration::from_secs(2),
            tokio_tungstenite::connect_async(&url),
        )
        .await
        {
            flood.push(ws);
        }
    }
    let token = editor.bridge.secret().expect("bridge token");
    let reply = raw_command(
        &mut client,
        json!({"commandId": "late", "type": "ping", "params": {}, "token": token}),
    )
    .await;
    assert_eq!(reply["status"], "success", "{reply}");

    // A server connecting while the flood holds every free place gets in once the flood's
    // connections are past their grace period.
    let newcomer = BridgeClient::new(editor.port);
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if newcomer.send_command("ping", json!({})).await.is_ok() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "a new client could not connect during the flood"
        );
        sleep(Duration::from_millis(200)).await;
    }
    drop(flood);
}

/// game_send forwards only the runtime's request channel, so a client cannot send the game
/// other debugger messages, and wrongly typed parameters get an answer instead of a script error.
#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn game_send_forwards_only_runtime_requests() {
    let editor = start_editor(false).await;
    let refused = editor
        .bridge
        .send_command("game_send", json!({"command": "mcp:other", "data": []}))
        .await
        .expect_err("another message name")
        .to_string();
    assert!(refused.contains("mcp:request"), "{refused}");
    for (params, key) in [
        (json!({"command": "mcp:request", "session": "x"}), "session"),
        (json!({"command": "mcp:request", "data": "x"}), "data"),
    ] {
        let error = editor
            .bridge
            .send_command("game_send", params)
            .await
            .expect_err("wrong type")
            .to_string();
        assert!(error.contains(&format!("'{key}'")), "{error}");
    }
}

/// The code an editor error carries, or a panic naming what came back instead.
fn error_code(result: Result<Value, McpError>) -> String {
    match result {
        Err(McpError::CodedToolExecution { code, .. }) => code,
        other => panic!("expected an editor error with a code, got {other:?}"),
    }
}

#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn editor_errors_carry_a_code_for_each_kind_of_failure() {
    let editor = start_editor(false).await;
    let bridge = &editor.bridge;
    let call = |command: &'static str, params: Value| async move {
        bridge.send_command(command, params).await
    };

    assert_eq!(
        error_code(call("no_such_command", json!({})).await),
        "UNKNOWN_COMMAND"
    );
    assert_eq!(
        error_code(call("get_node_properties", json!({"node_path": 7})).await),
        "BAD_PARAMS"
    );
    assert_eq!(
        error_code(call("open_scene", json!({"scene_path": "res://missing.tscn"})).await),
        "NOT_FOUND"
    );
    call("open_scene", json!({"scene_path": "res://main.tscn"}))
        .await
        .expect("open main");
    assert_eq!(
        error_code(call("get_node_properties", json!({"node_path": "NoSuchNode"})).await),
        "NOT_FOUND"
    );
    assert_eq!(
        error_code(call("create_scene", json!({"scene_path": "res://main.tscn"})).await),
        "EXISTS"
    );
    // A failure without a code keeps the reply older servers know.
    match call("remove_node", json!({"node_path": "."})).await {
        Err(McpError::ToolExecution(message)) => {
            assert!(message.contains("scene root"), "{message}")
        }
        other => panic!("expected an uncoded editor error, got {other:?}"),
    }

    // The tool result gives the code in structuredContent and keeps its text.
    use godot_mcp::cli_runner::CliRunner;
    use godot_mcp::tools::ToolManager;
    use std::sync::Arc;
    let (godot, _) = godot_binaries();
    let tools = Arc::new(
        ToolManager::new(BridgeClient::new(editor.port), CliRunner::new(godot))
            .with_working_dir(editor.project.clone()),
    );
    let result = Arc::clone(&tools)
        .call_tool(
            "godot_get_node_properties",
            json!({"node_path": "NoSuchNode"}),
        )
        .await;
    assert!(result.is_error);
    assert_eq!(result.error_code(), Some("NOT_FOUND"), "{result:?}");
    let text = tool_text(&result);
    assert!(
        text.starts_with("Error: Node not found at path: NoSuchNode"),
        "{text}"
    );
    assert!(text.contains("godot_get_scene_tree"), "{text}");
}

#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn scene_and_property_tools_behave() {
    let editor = start_editor(false).await;
    let bridge = &editor.bridge;
    let call = |command: &'static str, params: Value| async move {
        bridge.send_command(command, params).await
    };

    call("open_scene", json!({"scene_path": "res://main.tscn"}))
        .await
        .expect("open main");

    // An already-open scene edited on disk must be reloaded. open_scene used to
    // report success while get_scene_tree still showed the in-memory copy.
    std::fs::write(
        editor.project.join("stale.tscn"),
        "[gd_scene format=3]\n\n[node name=\"Stale\" type=\"Node2D\"]\n\n[node name=\"Before\" type=\"Node\" parent=\".\"]\n",
    )
    .expect("stale scene");
    let first = call("open_scene", json!({"scene_path": "res://stale.tscn"}))
        .await
        .expect("open stale");
    assert_eq!(first["reloaded"], false, "{first}");
    std::fs::write(
        editor.project.join("stale.tscn"),
        "[gd_scene format=3]\n\n[node name=\"Stale\" type=\"Node2D\"]\n\n[node name=\"FromDisk\" type=\"Node\" parent=\".\"]\n",
    )
    .expect("rewrite stale scene");
    let unread = call("get_scene_tree", json!({"scene_path": "res://stale.tscn"}))
        .await
        .expect("tree before reload");
    assert_eq!(unread["stale"], true, "{unread}");
    assert_eq!(unread["reloaded"], false, "{unread}");
    assert!(
        find(&unread["tree"], "Before").is_some(),
        "a read reloaded the scene: {unread}"
    );
    // Reloading a background tab must also make it the edited scene: the reload used
    // to leave the previous tab active, so the next edits went to the wrong scene.
    call("open_scene", json!({"scene_path": "res://main.tscn"}))
        .await
        .expect("switch to main");
    let reloaded = call("open_scene", json!({"scene_path": "res://stale.tscn"}))
        .await
        .expect("reload stale");
    assert_eq!(reloaded["reloaded"], true, "{reloaded}");
    assert_eq!(reloaded["active_scene"], "res://stale.tscn", "{reloaded}");
    let active_tree = call("get_scene_tree", json!({}))
        .await
        .expect("active tree");
    assert_eq!(active_tree["tree"]["name"], "Stale", "{active_tree}");
    let stale_tree = call("get_scene_tree", json!({"scene_path": "res://stale.tscn"}))
        .await
        .expect("stale tree");
    assert!(
        find(&stale_tree["tree"], "FromDisk").is_some(),
        "in-memory scene was not reloaded: {stale_tree}"
    );
    assert!(
        find(&stale_tree["tree"], "Before").is_none(),
        "old node survived the reload: {stale_tree}"
    );
    call("open_scene", json!({"scene_path": "res://main.tscn"}))
        .await
        .expect("return to main");

    // Paths in responses are relative to the edited scene.
    let tree = call("get_scene_tree", json!({})).await.expect("tree");
    assert_eq!(tree["tree"]["path"], ".");
    assert_eq!(
        find(&tree["tree"], "Body").expect("Body")["path"],
        "Player/Body"
    );

    // Node paths cannot leave the edited scene and the scene root cannot be removed.
    for path in ["..", "../..", "/root"] {
        expect_error(
            call("get_node_properties", json!({"node_path": path})).await,
            "",
        );
        expect_error(
            call(
                "set_node_properties",
                json!({"node_path": path, "properties": {"name": "x"}}),
            )
            .await,
            "",
        );
        expect_error(call("remove_node", json!({"node_path": path})).await, "");
    }
    expect_error(call("remove_node", json!({"node_path": "."})).await, "root");
    expect_error(call("remove_node", json!({"node_path": ""})).await, "");

    // Wrongly typed parameters are rejected immediately instead of hanging the call.
    let started = Instant::now();
    expect_error(
        call("get_node_properties", json!({"node_path": 5})).await,
        "node_path",
    );
    expect_error(
        call("resave_resources", json!({"paths": "res://icon.svg"})).await,
        "paths",
    );
    assert!(started.elapsed() < Duration::from_secs(3));

    // Properties come with their types and round-trip through typed conversion.
    let props = call("get_node_properties", json!({"node_path": "Player"}))
        .await
        .expect("props");
    assert_eq!(
        props["properties"]["position"],
        json!({"type": "Vector2", "value": "Vector2(5, 7)"})
    );

    let read = |name: &'static str| async move {
        let props = bridge
            .send_command("get_node_properties", json!({"node_path": "Player"}))
            .await
            .expect("props");
        props["properties"][name]["value"].clone()
    };

    call(
        "set_node_properties",
        json!({"node_path": "Player", "properties": {"position": [10, 20]}}),
    )
    .await
    .expect("set position");
    assert_eq!(read("position").await, "Vector2(10, 20)");

    call(
        "set_node_properties",
        json!({"node_path": "Player", "properties": {"modulate": "#ff0000", "z_index": 3, "visible": false}}),
    )
    .await
    .expect("set mixed");
    assert_eq!(read("modulate").await, "Color(1, 0, 0, 1)");
    assert_eq!(read("z_index").await, "3");
    assert_eq!(read("visible").await, "false");

    expect_error(
        call(
            "set_node_properties",
            json!({"node_path": "Player", "properties": {"position": [1, 2], "definitely_not_a_prop": 1}}),
        )
        .await,
        "definitely_not_a_prop",
    );
    assert_eq!(
        read("position").await,
        "Vector2(10, 20)",
        "a rejected batch must not be partially applied"
    );

    // Only real, instantiable Node classes can be added.
    call(
        "add_node",
        json!({"parent_node_path": "Player", "node_type": "Sprite2D", "node_name": "Hero"}),
    )
    .await
    .expect("add Sprite2D");
    for class in ["Object", "Resource", "CanvasItem", "NotAClass"] {
        expect_error(call("add_node", json!({"node_type": class})).await, class);
    }

    // Godot's localize_path accepts /private/var and misses /var, which is the
    // same directory on macOS. Callers hand us the /var spelling.
    let absolute = macos_var_spelling(&editor.project.join("main.tscn"));
    assert!(
        absolute.exists(),
        "aliased project file is missing: {absolute:?}"
    );
    let opened = call("open_scene", json!({"scene_path": absolute}))
        .await
        .expect("absolute path inside the project");
    assert_eq!(opened["scene_path"], "res://main.tscn");
    let outside = tempfile::tempdir().expect("outside dir");
    std::fs::write(outside.path().join("x.tscn"), MAIN_SCENE).expect("outside scene");
    expect_error(
        call(
            "open_scene",
            json!({"scene_path": outside.path().join("x.tscn")}),
        )
        .await,
        "res://",
    );

    // A big scene is delivered intact (responses far above 64 KiB).
    call("open_scene", json!({"scene_path": "res://big.tscn"}))
        .await
        .expect("open big");
    let big = call("get_scene_tree", json!({})).await.expect("big tree");
    assert!(count_nodes(&big["tree"]) >= 3001, "tree was truncated");

    // A tab that is not active can be inspected without editor errors.
    let main = call("get_scene_tree", json!({"scene_path": "res://main.tscn"}))
        .await
        .expect("inactive tab");
    assert_eq!(
        find(&main["tree"], "Hero").expect("Hero")["path"],
        "Player/Hero"
    );
    assert!(
        !editor.log_text().contains("not in a scene tree"),
        "editor reported path errors:\n{}",
        editor.log_text()
    );
}

#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn undo_and_resource_refresh_behave() {
    let editor = start_editor(true).await;
    let bridge = &editor.bridge;

    bridge
        .send_command("open_scene", json!({"scene_path": "res://main.tscn"}))
        .await
        .expect("open main");
    bridge
        .send_command(
            "add_node",
            json!({"parent_node_path": "Player/Body", "node_type": "Node2D", "node_name": "Child"}),
        )
        .await
        .expect("add Child");

    bridge
        .send_command("remove_node", json!({"node_path": "Player/Body"}))
        .await
        .expect("remove Body");
    bridge
        .send_command("__test_undo", json!({}))
        .await
        .expect("undo");

    let tree = bridge
        .send_command("get_scene_tree", json!({}))
        .await
        .expect("tree");
    let player = find(&tree["tree"], "Player").expect("Player");
    assert_eq!(
        child_names(player),
        ["Body", "Weapon"],
        "sibling order lost"
    );
    assert_eq!(child_names(find(player, "Body").expect("Body")), ["Child"]);

    bridge
        .send_command("save_scene", json!({}))
        .await
        .expect("save");
    let saved = std::fs::read_to_string(editor.project.join("main.tscn")).expect("saved scene");
    assert!(
        saved.contains("[node name=\"Child\"") && saved.contains("parent=\"Player/Body\""),
        "owner of the restored subtree was lost:\n{saved}"
    );

    // Non-importable files are refreshed, not pushed through the importer.
    let refreshed = bridge
        .send_command("resave_resources", json!({"paths": ["res://script.gd"]}))
        .await
        .expect("refresh script");
    assert_eq!(refreshed["mode"], "refresh");
    assert!(
        !editor.log_text().contains("BUG: File queued for import"),
        "{}",
        editor.log_text()
    );

    // A node added through the editor's own undo history, not through a bridge
    // command, is still unsaved. Reloading must not throw it away unless asked.
    std::fs::write(
        editor.project.join("hand.tscn"),
        "[gd_scene format=3]\n\n[node name=\"Hand\" type=\"Node2D\"]\n",
    )
    .expect("hand scene");
    bridge
        .send_command("open_scene", json!({"scene_path": "res://hand.tscn"}))
        .await
        .expect("open hand");
    bridge
        .send_command("__test_hand_edit", json!({}))
        .await
        .expect("hand edit");
    std::fs::write(
        editor.project.join("hand.tscn"),
        "[gd_scene format=3]\n\n[node name=\"Hand\" type=\"Node2D\"]\n\n[node name=\"FromDisk\" type=\"Node\" parent=\".\"]\n",
    )
    .expect("rewrite hand scene");
    let blocked = bridge
        .send_command("open_scene", json!({"scene_path": "res://hand.tscn"}))
        .await;
    expect_error(blocked, "unsaved");
    let kept = bridge
        .send_command("get_scene_tree", json!({"scene_path": "res://hand.tscn"}))
        .await
        .expect("kept tree");
    assert!(
        find(&kept["tree"], "ByHand").is_some(),
        "hand edit was discarded: {kept}"
    );
    let discarded = bridge
        .send_command(
            "open_scene",
            json!({"scene_path": "res://hand.tscn", "discard": true}),
        )
        .await
        .expect("discard hand edits");
    assert_eq!(discarded["reloaded"], true, "{discarded}");
    let after = bridge
        .send_command("get_scene_tree", json!({}))
        .await
        .expect("tree after discard");
    assert!(find(&after["tree"], "ByHand").is_none(), "{after}");
    assert!(find(&after["tree"], "FromDisk").is_some(), "{after}");

    // The same when the person opened the scene themselves: the bridge never saw it open,
    // yet its unsaved edits must survive a reload from disk.
    std::fs::write(
        editor.project.join("opened.tscn"),
        "[gd_scene format=3]

[node name=\"Opened\" type=\"Node2D\"]
",
    )
    .expect("opened scene");
    bridge
        .send_command(
            "__test_open_directly",
            json!({"scene_path": "res://opened.tscn"}),
        )
        .await
        .expect("open directly");
    sleep(Duration::from_millis(500)).await;
    bridge
        .send_command("__test_hand_edit", json!({}))
        .await
        .expect("hand edit in the directly opened scene");
    std::fs::write(
        editor.project.join("opened.tscn"),
        "[gd_scene format=3]

[node name=\"Opened\" type=\"Node2D\"]

[node name=\"FromDisk\" type=\"Node\" parent=\".\"]
",
    )
    .expect("rewrite opened scene");
    expect_error(
        bridge
            .send_command("open_scene", json!({"scene_path": "res://opened.tscn"}))
            .await,
        "unsaved",
    );
    let still = bridge
        .send_command("get_scene_tree", json!({"scene_path": "res://opened.tscn"}))
        .await
        .expect("tree of the directly opened scene");
    assert!(
        find(&still["tree"], "ByHand").is_some(),
        "hand edit in a scene the bridge never opened was discarded: {still}"
    );
    let scan = bridge
        .send_command("resave_resources", json!({}))
        .await
        .expect("full scan");
    assert_eq!(scan["mode"], "full_scan");

    // A file written after the editor started is not in EditorFileSystem. Reporting it as
    // reimported without a .import sidecar is the bug: load() then finds no loader.
    let art = editor.project.join("art/models");
    std::fs::create_dir_all(&art).expect("art dir");
    std::fs::write(
        art.join("x.svg"),
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"2\" height=\"2\"/>",
    )
    .expect("new svg");
    let fresh = editor
        .bridge
        .clone()
        .with_timeout(Duration::from_secs(90))
        .send_command(
            "resave_resources",
            json!({"paths": ["res://art/models/x.svg"]}),
        )
        .await
        .expect("resave unseen svg");
    let reimported = fresh["reimported"].as_array().expect("reimported");
    assert!(
        reimported
            .iter()
            .any(|path| path == "res://art/models/x.svg"),
        "{fresh}"
    );
    assert!(
        art.join("x.svg.import").is_file(),
        "no import sidecar was written: {fresh}"
    );
    let not_scanned = fresh["not_scanned"].as_array().cloned().unwrap_or_default();
    assert!(
        !not_scanned
            .iter()
            .any(|path| path == "res://art/models/x.svg"),
        "{fresh}"
    );

    std::fs::write(
        art.join("y.svg"),
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"2\" height=\"2\"/>",
    )
    .expect("second svg");
    let waited = editor
        .bridge
        .clone()
        .with_timeout(Duration::from_secs(90))
        .send_command("resave_resources", json!({"wait": true}))
        .await
        .expect("full scan with wait");
    assert_eq!(waited["mode"], "full_scan", "{waited}");
    assert_eq!(waited["timed_out"], false, "{waited}");
    assert_eq!(waited["scanning"], false, "{waited}");
    assert!(waited["imported"].as_u64().unwrap_or(0) >= 1, "{waited}");
    assert!(
        art.join("y.svg.import").is_file(),
        "wait did not import the new file: {waited}"
    );
}

#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn play_with_runtime_disabled_does_not_write_override() {
    let editor = start_editor_configured(false, "\n[godot_mcp]\n\nruntime_on_play=false\n").await;
    let bridge = &editor.bridge;
    bridge
        .send_command("run_project", json!({}))
        .await
        .expect("play");
    let override_path = editor.project.join("override.cfg");
    let mut running = false;
    for _ in 0..40 {
        let text = std::fs::read_to_string(&override_path).unwrap_or_default();
        assert!(
            !text.contains("godot_mcp: temporary runtime autoload"),
            "play wrote override.cfg while runtime_on_play is off: {text}"
        );
        let status = bridge
            .send_command("get_run_status", json!({}))
            .await
            .expect("status");
        if status["running"] == true {
            running = true;
            break;
        }
        sleep(Duration::from_millis(50)).await;
    }
    assert!(running, "editor play did not start:\n{}", editor.log_text());
    let text = std::fs::read_to_string(&override_path).unwrap_or_default();
    assert!(
        !text.contains("godot_mcp: temporary runtime autoload"),
        "play wrote override.cfg while runtime_on_play is off: {text}"
    );
    bridge
        .send_command("stop_project", json!({}))
        .await
        .expect("stop");
}

/// Editor Play opens a debugger session before the game's runtime has registered its message
/// capture. A game tool called right after Play used to fail with "No game started from the
/// editor is running"; it now waits for the runtime to say hello.
#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn a_game_tool_called_right_after_editor_play_waits_for_the_runtime() {
    use godot_mcp::cli_runner::CliRunner;
    use godot_mcp::tools::ToolManager;
    use std::sync::Arc;

    // Editor Play opens a game window. A Linux machine without a display (the CI runner) makes
    // the game crash at startup, which says nothing about the code under test.
    if cfg!(target_os = "linux")
        && std::env::var_os("DISPLAY").is_none()
        && std::env::var_os("WAYLAND_DISPLAY").is_none()
    {
        eprintln!("skipped: editor Play needs a display");
        return;
    }

    let editor = start_editor(false).await;
    let (godot, _) = godot_binaries();
    let tools = Arc::new(
        ToolManager::new(BridgeClient::new(editor.port), CliRunner::new(godot))
            .with_working_dir(editor.project.clone()),
    );
    let started = Arc::clone(&tools)
        .call_tool("godot_run_project", json!({"scene": "res://main.tscn"}))
        .await;
    assert!(!started.is_error, "{started:?}");

    let tree = Arc::clone(&tools)
        .call_tool("godot_game_tree", json!({}))
        .await;
    let text = format!("{:?}", tree.content);
    assert!(
        !tree.is_error && text.contains("Main"),
        "the game tree was not read right after Play: {text}"
    );
    let stopped = Arc::clone(&tools)
        .call_tool("godot_stop_project", json!({}))
        .await;
    assert!(!stopped.is_error, "{stopped:?}");
}

/// The authoring commands on a real editor: what they create, what they refuse, and that
/// nothing they refuse touches a file that is already there.
#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn authoring_tools_create_what_they_should_and_refuse_the_rest() {
    let editor = start_editor(false).await;
    let bridge = &editor.bridge;
    let call = |command: &'static str, params: Value| async move {
        bridge.send_command(command, params).await
    };

    // A new scene is created, opened and refused a second time.
    let created = call(
        "create_scene",
        json!({"scene_path": "res://made.tscn", "root_type": "Node2D", "root_name": "Made"}),
    )
    .await
    .expect("create scene");
    assert_eq!(created["created"], true, "{created}");
    let tree = call("get_scene_tree", json!({}))
        .await
        .expect("tree of the new scene");
    assert_eq!(tree["tree"]["name"], "Made", "{tree}");
    let on_disk = std::fs::read_to_string(editor.project.join("made.tscn")).expect("made.tscn");
    expect_error(
        call("create_scene", json!({"scene_path": "res://made.tscn"})).await,
        "already exists",
    );
    assert_eq!(
        std::fs::read_to_string(editor.project.join("made.tscn")).expect("made.tscn again"),
        on_disk,
        "creating a scene that exists changed the file"
    );
    expect_error(
        call("create_scene", json!({"scene_path": "res://notes.txt"})).await,
        ".tscn",
    );
    expect_error(
        call("create_scene", json!({"scene_path": "../outside.tscn"})).await,
        ".tscn",
    );

    // A script is attached and detached; a missing one and one outside the project are refused.
    call(
        "attach_script",
        json!({"node_path": ".", "script_path": "res://script.gd"}),
    )
    .await
    .expect("attach");
    call(
        "attach_script",
        json!({"node_path": ".", "script_path": ""}),
    )
    .await
    .expect("detach");
    expect_error(
        call(
            "attach_script",
            json!({"node_path": ".", "script_path": "res://missing.gd"}),
        )
        .await,
        "does not exist",
    );
    expect_error(
        call(
            "attach_script",
            json!({"node_path": ".", "script_path": "../outside.gd"}),
        )
        .await,
        "does not exist",
    );

    // A signal is connected once, listed, and refused when it is unknown, already connected, or
    // aimed at a method the target does not have (that would fail whenever the signal fires).
    expect_error(
        call(
            "connect_signal",
            json!({"node_path": ".", "signal_name": "ready", "method": "on_ready"}),
        )
        .await,
        "has no method",
    );
    std::fs::write(
        editor.project.join("listener.gd"),
        "extends Node2D

func on_ready() -> void:
	pass
",
    )
    .expect("listener.gd");
    call(
        "attach_script",
        json!({"node_path": ".", "script_path": "res://listener.gd"}),
    )
    .await
    .expect("attach the listener");
    call(
        "connect_signal",
        json!({"node_path": ".", "signal_name": "ready", "method": "on_ready"}),
    )
    .await
    .expect("connect");
    expect_error(
        call(
            "connect_signal",
            json!({"node_path": ".", "signal_name": "ready", "method": "on_ready"}),
        )
        .await,
        "already connected",
    );
    expect_error(
        call(
            "connect_signal",
            json!({"node_path": ".", "signal_name": "nonsense", "method": "on_ready"}),
        )
        .await,
        "no signal",
    );
    expect_error(
        call(
            "connect_signal",
            json!({"node_path": ".", "signal_name": "ready", "method": "not an identifier"}),
        )
        .await,
        "identifier",
    );
    let signals = call("list_signals", json!({"node_path": "."}))
        .await
        .expect("list signals");
    let ready = signals["signals"]
        .as_array()
        .and_then(|all| all.iter().find(|entry| entry["name"] == "ready"))
        .unwrap_or_else(|| panic!("no ready signal: {signals}"));
    assert!(
        ready["connections"].to_string().contains("on_ready"),
        "{signals}"
    );

    // A resource is created once. A second call must not overwrite it, and neither a class
    // that is not a Resource nor a path that is not a resource file is accepted.
    let made = call(
        "create_resource",
        json!({"resource_path": "res://mat.tres", "class_name": "StandardMaterial3D"}),
    )
    .await
    .expect("create resource");
    assert_eq!(made["class_name"], "StandardMaterial3D", "{made}");
    let material = std::fs::read_to_string(editor.project.join("mat.tres")).expect("mat.tres");
    assert!(material.contains("StandardMaterial3D"), "{material}");
    expect_error(
        call(
            "create_resource",
            json!({"resource_path": "res://mat.tres", "class_name": "Curve"}),
        )
        .await,
        "already exists",
    );
    assert_eq!(
        std::fs::read_to_string(editor.project.join("mat.tres")).expect("mat.tres again"),
        material,
        "creating a resource over an existing file replaced it"
    );
    expect_error(
        call(
            "create_resource",
            json!({"resource_path": "res://main.tscn", "class_name": "Curve"}),
        )
        .await,
        "already exists",
    );
    expect_error(
        call(
            "create_resource",
            json!({"resource_path": "res://new.gd", "class_name": "Curve"}),
        )
        .await,
        ".tres",
    );
    expect_error(
        call(
            "create_resource",
            json!({"resource_path": "res://node.tres", "class_name": "Node2D"}),
        )
        .await,
        "Unknown resource class",
    );

    // Project settings: a scalar is saved and the old value comes back; a value of the wrong type
    // for an existing setting (a structure for a string) is refused and changes nothing.
    let set = call(
        "set_project_setting",
        json!({"key": "application/config/name", "value": "Renamed"}),
    )
    .await
    .expect("set name");
    assert_eq!(set["saved"], true, "{set}");
    assert_eq!(set["previous"], "MCP E2E Fixture", "{set}");
    let read = call(
        "get_project_settings",
        json!({"prefix": "application/config/name"}),
    )
    .await
    .expect("get name");
    assert!(read["settings"].to_string().contains("Renamed"), "{read}");
    expect_error(
        call(
            "set_project_setting",
            json!({"key": "application/config/name", "value": {"a": 1}}),
        )
        .await,
        "expects a string",
    );
    expect_error(
        call(
            "set_project_setting",
            json!({"key": "application/config/name", "value": 5}),
        )
        .await,
        "expects",
    );
    expect_error(
        call("set_project_setting", json!({"key": "noslash", "value": 1})).await,
        "section",
    );
    call(
        "set_project_setting",
        json!({"key": "custom/test/flag", "value": true}),
    )
    .await
    .expect("new key");
    let project_file =
        std::fs::read_to_string(editor.project.join("project.godot")).expect("project.godot");
    assert!(
        project_file.contains("name=\"Renamed\"") && project_file.contains("flag=true"),
        "{project_file}"
    );
}

/// `set_project_setting` through a real editor stores what the server's file fallback stores for
/// the same input, and refuses what it refuses (the fallback's real-engine test has the same
/// table): a color from an HTML color or a name, a vector from its `project.godot` text, a whole
/// number as an integer, the type of a base setting for a feature override, and nothing from an
/// object text or a string that does not convert.
#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn project_settings_convert_the_same_text_forms_as_the_file_fallback() {
    let editor = start_editor(false).await;
    let bridge = &editor.bridge;
    let project_file = editor.project.join("project.godot");

    for (key, value) in [
        ("application/boot_splash/bg_color", json!("#102b38")),
        ("display/window/size/viewport_width", json!(1280)),
        ("display/window/size/viewport_height.mobile", json!(720.0)),
        ("physics/2d/default_gravity_vector", json!("Vector2(0, 2)")),
        ("custom/tuning/speed", json!(2.5)),
    ] {
        bridge
            .send_command("set_project_setting", json!({"key": key, "value": value}))
            .await
            .unwrap_or_else(|error| panic!("{key} = {value}: {error}"));
    }
    let written = std::fs::read_to_string(&project_file).expect("project.godot");
    for line in [
        "\nboot_splash/bg_color=Color(0.0627451, 0.16862746, 0.21960784, 1)\n",
        "\nwindow/size/viewport_width=1280\n",
        "\nwindow/size/viewport_height.mobile=720\n",
        "\n2d/default_gravity_vector=Vector2(0, 2)\n",
        "\ntuning/speed=2.5\n",
    ] {
        assert!(written.contains(line), "{line:?} missing from:\n{written}");
    }

    // A color name and a stored color replaced by a new one.
    bridge
        .send_command(
            "set_project_setting",
            json!({"key": "application/boot_splash/bg_color", "value": "white"}),
        )
        .await
        .expect("a color name");
    let written = std::fs::read_to_string(&project_file).expect("project.godot");
    assert!(
        written.contains("\nboot_splash/bg_color=Color(1, 1, 1, 1)\n"),
        "{written}"
    );

    // What does not convert is refused, names the type, and leaves the file as it was.
    let before = std::fs::read_to_string(&project_file).expect("project.godot");
    for (key, value, needle) in [
        (
            "application/boot_splash/bg_color",
            json!("#zzzzzz"),
            "expects a Color",
        ),
        (
            "application/boot_splash/bg_color",
            json!(3),
            "expects a Color",
        ),
        (
            "display/window/size/viewport_width",
            json!("wide"),
            "expects a int",
        ),
        (
            "display/window/size/viewport_width",
            json!(1280.5),
            "expects a int",
        ),
        (
            "physics/2d/default_gravity_vector",
            json!("Object(Node)"),
            "expects a Vector2",
        ),
        (
            "physics/2d/default_gravity_vector",
            json!("Vector3(0, 2, 1)"),
            "expects a Vector2",
        ),
        (
            "physics/2d/default_gravity_vector",
            json!(2),
            "expects a Vector2",
        ),
    ] {
        let result = bridge
            .send_command("set_project_setting", json!({"key": key, "value": value}))
            .await;
        expect_error(result, needle);
    }
    expect_error(
        bridge
            .send_command(
                "set_project_setting",
                json!({"key": "application/boot_splash/bg_color", "value": "#zzzzzz"}),
            )
            .await,
        "#102b38",
    );
    assert_eq!(
        std::fs::read_to_string(&project_file).expect("project.godot"),
        before,
        "a refused value changed project.godot"
    );
}

/// Arrays and objects through a real editor, with the rules of the file fallback (its real-engine
/// test has the same table): an importer's defaults are a Dictionary, the text form in a string
/// works, nested whole numbers are ints, a packed array comes from a JSON array, and a text with
/// an Object or a Resource is refused.
#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn project_settings_store_arrays_and_objects_like_the_file_fallback() {
    let editor = start_editor(false).await;
    let bridge = &editor.bridge;
    let project_file = editor.project.join("project.godot");
    // The editor keeps its own version in the features list, so the test names the version of the
    // Godot build it runs with (`4.7.2.stable.official.abc` is 4.7).
    let (godot, _) = godot_binaries();
    let reported = std::process::Command::new(&godot)
        .arg("--version")
        .output()
        .expect("godot --version");
    let reported = String::from_utf8_lossy(&reported.stdout).trim().to_string();
    let mut parts = reported.split('.');
    let version = format!(
        "{}.{}",
        parts.next().expect("the major version of Godot"),
        parts.next().expect("the minor version of Godot")
    );

    for (key, value) in [
        (
            "importer_defaults/texture",
            json!({"detect_3d/compress_to": 0}),
        ),
        (
            "importer_defaults/scene",
            json!(" {\"meshes/generate_lods\": false}"),
        ),
        ("custom/tuning/table", json!({"speeds": [1, 2.5]})),
        ("application/config/features", json!([version, "Mobile"])),
    ] {
        bridge
            .send_command("set_project_setting", json!({"key": key, "value": value}))
            .await
            .unwrap_or_else(|error| panic!("{key} = {value}: {error}"));
    }
    let written = std::fs::read_to_string(&project_file).expect("project.godot");
    for part in [
        "\n[importer_defaults]\n".to_string(),
        "\ntexture={\n\"detect_3d/compress_to\": 0\n}\n".to_string(),
        "\nscene={\n\"meshes/generate_lods\": false\n}\n".to_string(),
        "\ntuning/table={\n\"speeds\": [1, 2.5]\n}\n".to_string(),
        format!("\nconfig/features=PackedStringArray(\"{version}\", \"Mobile\")\n"),
    ] {
        assert!(written.contains(&part), "{part:?} missing from:\n{written}");
    }

    let before = std::fs::read_to_string(&project_file).expect("project.godot");
    for (key, value, needle) in [
        (
            "importer_defaults/texture",
            json!(5),
            "expects a Dictionary",
        ),
        (
            "importer_defaults/texture",
            json!("{\"a\": Object(Node)}"),
            "expects a Dictionary",
        ),
        (
            "importer_defaults/texture",
            json!("{\"a\": Resource(\"res://x.tres\")}"),
            "expects a Dictionary",
        ),
        (
            "importer_defaults/texture",
            json!([1]),
            "expects a Dictionary",
        ),
        (
            "application/config/features",
            json!(7),
            "expects a PackedStringArray",
        ),
    ] {
        let result = bridge
            .send_command("set_project_setting", json!({"key": key, "value": value}))
            .await;
        expect_error(result, needle);
    }
    assert_eq!(
        std::fs::read_to_string(&project_file).expect("project.godot"),
        before,
        "a refused value changed project.godot"
    );
}

/// The frog-world case through a real editor, with the rules of the file fallback (its
/// real-engine test has the same steps): a global shader uniform given as value_text becomes a
/// Dictionary holding a Vector4; over a String stored by an earlier call it needs replace_type;
/// an engine setting keeps its type; remove erases a project's setting and returns an engine
/// setting to its default, which is then not saved.
#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn project_settings_take_a_value_text_and_remove_like_the_file_fallback() {
    let editor = start_editor(false).await;
    let bridge = &editor.bridge;
    let project_file = editor.project.join("project.godot");
    let key = "shader_globals/frog_push";
    let frog = r#"{"type": "vec4", "value": Vector4(0, -1000, 0, 0)}"#;

    let string = bridge
        .send_command("set_project_setting", json!({"key": key, "value": frog}))
        .await
        .expect("a string");
    assert_eq!(string["type"], "String", "{string}");
    expect_error(
        bridge
            .send_command(
                "set_project_setting",
                json!({"key": key, "value_text": frog}),
            )
            .await,
        "replace_type",
    );
    let replaced = bridge
        .send_command(
            "set_project_setting",
            json!({"key": key, "value_text": frog, "replace_type": true}),
        )
        .await
        .expect("replaced");
    assert_eq!(replaced["type"], "Dictionary", "{replaced}");
    let written = std::fs::read_to_string(&project_file).expect("project.godot");
    assert!(
        written.contains("\n[shader_globals]\n\nfrog_push={\n\"type\": \"vec4\",\n\"value\": Vector4(0, -1000, 0, 0)\n}\n"),
        "{written}"
    );

    let before = std::fs::read_to_string(&project_file).expect("project.godot");
    for (args, needle) in [
        (
            json!({"key": key, "value_text": "{\"a\": Object(Node)}"}),
            "built-in",
        ),
        // Godot reads ; outside a string as a comment: the quote after it opens no string.
        (
            json!({"key": key, "value_text": "{;\"\n\"a\": Object(RefCounted,\"a\":1)\n;\"\n}"}),
            "not a built-in type",
        ),
        (
            json!({"key": key, "value_text": "Vector4(0, "}),
            "not a value",
        ),
        (
            json!({"key": "physics/2d/default_gravity_vector", "value_text": "Vector3(0, 1, 0)", "replace_type": true}),
            "setting of the engine",
        ),
        (
            json!({"key": key, "value": 1, "remove": true}),
            "exactly one",
        ),
    ] {
        expect_error(
            bridge.send_command("set_project_setting", args).await,
            needle,
        );
    }
    assert_eq!(
        std::fs::read_to_string(&project_file).expect("project.godot"),
        before,
        "a refused value changed project.godot"
    );

    bridge
        .send_command(
            "set_project_setting",
            json!({"key": "physics/2d/default_gravity_vector", "value_text": "Vector2(0, 2)"}),
        )
        .await
        .expect("an engine setting from its text");
    let written = std::fs::read_to_string(&project_file).expect("project.godot");
    assert!(
        written.contains("\n2d/default_gravity_vector=Vector2(0, 2)\n"),
        "{written}"
    );

    for removed_key in [key, "physics/2d/default_gravity_vector"] {
        let removed = bridge
            .send_command(
                "set_project_setting",
                json!({"key": removed_key, "remove": true}),
            )
            .await
            .unwrap_or_else(|error| panic!("{removed_key}: {error}"));
        assert_eq!(removed["removed"], true, "{removed}");
    }
    let written = std::fs::read_to_string(&project_file).expect("project.godot");
    assert!(
        !written.contains("frog_push") && !written.contains("default_gravity_vector"),
        "{written}"
    );
    let gravity = bridge
        .send_command(
            "get_project_settings",
            json!({"prefix": "physics/2d/default_gravity_vector"}),
        )
        .await
        .expect("gravity");
    assert_eq!(
        gravity["settings"]["physics/2d/default_gravity_vector"], "(0.0, 1.0)",
        "{gravity}"
    );
    let again = bridge
        .send_command("set_project_setting", json!({"key": key, "remove": true}))
        .await
        .expect("remove again");
    assert_eq!(again["removed"], false, "{again}");
}

fn tool_text(result: &godot_mcp::protocol::ToolResult) -> String {
    result
        .content
        .iter()
        .filter_map(|item| match item {
            godot_mcp::protocol::ToolContent::Text { text } => Some(text.as_str()),
            godot_mcp::protocol::ToolContent::Image { .. } => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Editor Play reaches the same runtime as a managed process: scene generation, a property
/// wait, one real physics step, and a named screenshot baseline.
#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn editor_play_can_wait_step_and_compare_a_baseline() {
    use godot_mcp::cli_runner::CliRunner;
    use godot_mcp::tools::ToolManager;
    use std::sync::Arc;

    if cfg!(target_os = "linux")
        && std::env::var_os("DISPLAY").is_none()
        && std::env::var_os("WAYLAND_DISPLAY").is_none()
    {
        eprintln!("skipped: editor Play needs a display");
        return;
    }

    let editor = start_editor(false).await;
    let (godot, _) = godot_binaries();
    let tools = Arc::new(
        ToolManager::new(BridgeClient::new(editor.port), CliRunner::new(godot))
            .with_working_dir(editor.project.clone()),
    );
    let started = Arc::clone(&tools)
        .call_tool("godot_run_project", json!({"scene": "res://main.tscn"}))
        .await;
    assert!(!started.is_error, "{}", tool_text(&started));

    let snap = Arc::clone(&tools)
        .call_tool("godot_game_snapshot", json!({}))
        .await;
    assert!(!snap.is_error, "{}", tool_text(&snap));
    let snap_json: Value = serde_json::from_str(&tool_text(&snap)).expect("snapshot json");
    assert!(
        snap_json["scene"]
            .as_str()
            .unwrap_or("")
            .ends_with("main.tscn"),
        "{snap_json}"
    );
    assert!(
        snap_json["scene_generation"].as_i64().unwrap_or(0) >= 1,
        "{snap_json}"
    );
    assert!(
        snap_json["scene_instance_id"].as_i64().unwrap_or(0) > 0,
        "{snap_json}"
    );
    assert_eq!(
        snap_json["script_sha256_basis"], "unavailable",
        "{snap_json}"
    );
    assert!(snap_json["script_sha256"].is_null(), "{snap_json}");

    let found = Arc::clone(&tools)
        .call_tool("godot_game_wait", json!({"node": "Player", "timeout": 60}))
        .await;
    assert!(!found.is_error, "{}", tool_text(&found));

    let named = Arc::clone(&tools)
        .call_tool(
            "godot_game_wait",
            json!({"node": ".", "property": "name", "op": "eq", "value": "Main"}),
        )
        .await;
    assert!(!named.is_error, "{}", tool_text(&named));
    let named_json: Value = serde_json::from_str(&tool_text(&named)).expect("wait json");
    assert_eq!(named_json["observed"], "Main", "{named_json}");
    assert!(
        named_json["scene"]["scene_generation"]
            .as_i64()
            .unwrap_or(0)
            >= 1,
        "{named_json}"
    );

    let denied = Arc::clone(&tools)
        .call_tool(
            "godot_game_wait",
            json!({"node": ".", "property": "_secret", "op": "exists", "timeout": 1}),
        )
        .await;
    assert!(denied.is_error, "{}", tool_text(&denied));
    assert!(
        tool_text(&denied).to_lowercase().contains("private"),
        "{}",
        tool_text(&denied)
    );

    let stepped = Arc::clone(&tools)
        .call_tool("godot_game_physics", json!({"action": "step", "ticks": 2}))
        .await;
    assert!(!stepped.is_error, "{}", tool_text(&stepped));
    let stepped_json: Value = serde_json::from_str(&tool_text(&stepped)).expect("physics json");
    assert_eq!(stepped_json["ticks"], 2, "{stepped_json}");
    assert_eq!(stepped_json["paused"], true, "{stepped_json}");
    assert!(
        stepped_json["scene"]["scene_generation"]
            .as_i64()
            .unwrap_or(0)
            >= 1,
        "{stepped_json}"
    );

    let saved = Arc::clone(&tools)
        .call_tool(
            "godot_game_screenshot",
            json!({"max_dimension": 64, "save_baseline": "play"}),
        )
        .await;
    assert!(!saved.is_error, "{}", tool_text(&saved));
    assert!(tool_text(&saved).contains("play"), "{}", tool_text(&saved));
    let compared = Arc::clone(&tools)
        .call_tool(
            "godot_game_screenshot",
            json!({"max_dimension": 64, "baseline": "play", "tolerance": 255}),
        )
        .await;
    assert!(!compared.is_error, "{}", tool_text(&compared));
    let note = tool_text(&compared);
    assert!(note.contains("changed 0"), "{note}");
    assert!(note.contains("exact"), "{note}");
    assert!(note.contains("tolerance 255"), "{note}");

    let resumed = Arc::clone(&tools)
        .call_tool("godot_game_physics", json!({"action": "resume"}))
        .await;
    assert!(!resumed.is_error, "{}", tool_text(&resumed));
    let stopped = Arc::clone(&tools)
        .call_tool("godot_stop_project", json!({}))
        .await;
    assert!(!stopped.is_error, "{}", tool_text(&stopped));
}

/// Editor Play can be held and stepped through the relay, but its frame rate is the editor's, so
/// its steps are never reported as exact.
#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn editor_play_can_be_held_and_stepped_but_is_not_exact() {
    use godot_mcp::cli_runner::CliRunner;
    use godot_mcp::tools::ToolManager;
    use std::sync::Arc;

    if cfg!(target_os = "linux")
        && std::env::var_os("DISPLAY").is_none()
        && std::env::var_os("WAYLAND_DISPLAY").is_none()
    {
        eprintln!("skipped: editor Play needs a display");
        return;
    }

    let editor = start_editor(false).await;
    let (godot, _) = godot_binaries();
    let tools = Arc::new(
        ToolManager::new(BridgeClient::new(editor.port), CliRunner::new(godot))
            .with_working_dir(editor.project.clone()),
    );
    let started = Arc::clone(&tools)
        .call_tool("godot_run_project", json!({"scene": "res://main.tscn"}))
        .await;
    assert!(!started.is_error, "{}", tool_text(&started));

    let held = Arc::clone(&tools)
        .call_tool("godot_game_physics", json!({"action": "hold"}))
        .await;
    assert!(!held.is_error, "{}", tool_text(&held));
    let held_json: Value = serde_json::from_str(&tool_text(&held)).expect("hold json");
    assert_eq!(held_json["held"], true, "{held_json}");
    assert_eq!(held_json["deterministic"], false, "{held_json}");
    assert_eq!(
        held_json["deterministic_reason"], "not_started_held",
        "{held_json}"
    );

    // Reads are answered while the editor's game is held, and it stands still.
    let first = Arc::clone(&tools)
        .call_tool("godot_game_snapshot", json!({}))
        .await;
    assert!(!first.is_error, "{}", tool_text(&first));
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    let second = Arc::clone(&tools)
        .call_tool("godot_game_snapshot", json!({}))
        .await;
    let frame_of = |result: &godot_mcp::protocol::ToolResult| {
        serde_json::from_str::<Value>(&tool_text(result)).expect("snapshot json")["frame"].clone()
    };
    assert_eq!(frame_of(&first), frame_of(&second), "the held game ran");

    let stepped = Arc::clone(&tools)
        .call_tool("godot_game_physics", json!({"action": "step", "ticks": 3}))
        .await;
    assert!(!stepped.is_error, "{}", tool_text(&stepped));
    let stepped_json: Value = serde_json::from_str(&tool_text(&stepped)).expect("step json");
    assert_eq!(stepped_json["ticks_run"], 3, "{stepped_json}");
    assert_eq!(stepped_json["held"], true, "{stepped_json}");
    assert_eq!(stepped_json["deterministic"], false, "{stepped_json}");

    let refused = Arc::clone(&tools)
        .call_tool("godot_game_wait", json!({"frames": 2}))
        .await;
    assert!(refused.is_error, "{}", tool_text(&refused));
    assert!(
        tool_text(&refused).contains("held"),
        "{}",
        tool_text(&refused)
    );

    let resumed = Arc::clone(&tools)
        .call_tool("godot_game_physics", json!({"action": "resume"}))
        .await;
    assert!(!resumed.is_error, "{}", tool_text(&resumed));
    let stopped = Arc::clone(&tools)
        .call_tool("godot_stop_project", json!({}))
        .await;
    assert!(!stopped.is_error, "{}", tool_text(&stopped));
}

/// Test-only commands that drive the script editor the way a person would, to measure what
/// the editor offers before the real script commands are written.
const SCRIPT_PROBE_HOOK: &str = r#"        "__probe_open":
            var path: String = str(params.get("path", ""))
            EditorInterface.edit_script(load(path))
            send_success(peer, cmd_id, {"open": _probe_open_paths()})
        "__probe_shape":
            var editors: Array = EditorInterface.get_script_editor().get_open_script_editors()
            var out: Dictionary = {"count": editors.size(), "scripts": EditorInterface.get_script_editor().get_open_scripts().map(func(s: Script) -> String: return s.resource_path)}
            if editors.size() > 0:
                var names: Array = []
                for m in editors[0].get_method_list():
                    var n: String = str(m["name"])
                    if "edit" in n or "script" in n or "resource" in n or "save" in n or "path" in n:
                        names.append(n)
                out["methods"] = names
                var props: Array = []
                for p in editors[0].get_property_list():
                    props.append(str(p["name"]))
                out["props"] = props
                out["class"] = editors[0].get_class()
            send_success(peer, cmd_id, out)
        "__probe_mixed":
            var se: ScriptEditor = EditorInterface.get_script_editor()
            EditorInterface.edit_resource(load("res://notes.txt"))
            se.goto_help("class_name:Node")
            var editors: Array = se.get_open_script_editors()
            var names: Array = []
            for e in editors:
                names.append([e.get_class(), str(e.get_meta("_tab_name", ""))])
            var current: Variant = se.get_current_editor()
            send_success(peer, cmd_id, {"editors": names, "scripts": se.get_open_scripts().map(func(s: Script) -> String: return s.resource_path), "current": str(current.get_class()) if current != null else "", "current_script": se.get_current_script().resource_path if se.get_current_script() != null else ""})
        "__probe_state":
            var path: String = str(params.get("path", ""))
            var found: Variant = _probe_editor(path)
            if found == null:
                send_success(peer, cmd_id, {"found": false, "open": _probe_open_paths()})
            else:
                var base: CodeEdit = found.get_base_editor() as CodeEdit
                var script: Script = load(path) as Script
                send_success(peer, cmd_id, {"found": true, "text": base.text, "version": base.get_version(), "saved_version": base.get_saved_version(), "source_code": script.source_code})
        "__probe_type":
            var found: Variant = _probe_editor(str(params.get("path", "")))
            var base: CodeEdit = found.get_base_editor() as CodeEdit
            base.insert_text(str(params.get("text", "")), 0, 0)
            send_success(peer, cmd_id, {"version": base.get_version(), "saved_version": base.get_saved_version()})
        "__probe_replace":
            var found: Variant = _probe_editor(str(params.get("path", "")))
            var base: CodeEdit = found.get_base_editor() as CodeEdit
            var old_text: String = str(params.get("old", ""))
            var new_text: String = str(params.get("new", ""))
            var text: String = base.text
            var index: int = text.find(old_text)
            var before: String = text.substr(0, index)
            var line: int = before.count("\n")
            var col: int = index - (before.rfind("\n") + 1)
            var old_lines: int = old_text.count("\n")
            var end_line: int = line + old_lines
            var end_col: int = col + old_text.length()
            if old_lines > 0:
                end_col = old_text.length() - (old_text.rfind("\n") + 1)
            base.begin_complex_operation()
            base.remove_text(line, col, end_line, end_col)
            base.insert_text(new_text, line, col)
            base.end_complex_operation()
            send_success(peer, cmd_id, {"text": base.text, "version": base.get_version()})
        "__probe_undo":
            var found: Variant = _probe_editor(str(params.get("path", "")))
            var base: CodeEdit = found.get_base_editor() as CodeEdit
            base.undo()
            send_success(peer, cmd_id, {"text": base.text, "version": base.get_version(), "saved_version": base.get_saved_version()})
        "__probe_save":
            var path: String = str(params.get("path", ""))
            var found: Variant = _probe_editor(path)
            var base: CodeEdit = found.get_base_editor() as CodeEdit
            var script: Script = load(path) as Script
            script.source_code = base.text
            var save_err: Error = ResourceSaver.save(script, path)
            base.tag_saved_version()
            EditorInterface.get_resource_filesystem().update_file(path)
            var reload_err: Error = script.reload(true)
            send_success(peer, cmd_id, {"save_err": save_err, "reload_err": reload_err, "version": base.get_version(), "saved_version": base.get_saved_version()})
        "__probe_attach":
            var node := Node.new()
            node.set_script(load(str(params.get("path", ""))))
            Engine.set_meta("probe_node", node)
            send_success(peer, cmd_id, {"answer": node.call("answer")})
        "__probe_call":
            var node: Node = Engine.get_meta("probe_node") as Node
            send_success(peer, cmd_id, {"answer": node.call("answer")})
        "ping":
"#;

const SCRIPT_PROBE_HELPERS: &str = r#"

func _probe_open_paths() -> Array:
    var paths: Array = []
    for script in EditorInterface.get_script_editor().get_open_scripts():
        paths.append(script.resource_path)
    return paths

func _probe_editor(path: String) -> Variant:
    var editors: Array = EditorInterface.get_script_editor().get_open_script_editors()
    var scripts: Array = EditorInterface.get_script_editor().get_open_scripts()
    for index in range(scripts.size()):
        if scripts[index].resource_path == path and index < editors.size():
            return editors[index]
    return null
"#;

/// Feasibility measurements for reading and editing a script through the editor. Run with
/// `--nocapture` to see the answers; the assertions hold the facts the design needs.
#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn script_editor_facts_for_reading_and_editing() {
    let probe = "@tool\nextends Node\n\nfunc answer() -> int:\n\treturn 1\n";
    let editor = start_editor_with(
        Some(SCRIPT_PROBE_HOOK),
        SCRIPT_PROBE_HELPERS,
        &[
            ("probe.gd", probe),
            ("other.gd", "extends Node\n\nvar x = 1\n"),
        ],
        "",
    )
    .await;
    let send = |name: &'static str, params: Value| {
        let bridge = &editor.bridge;
        async move {
            bridge
                .send_command(name, params)
                .await
                .unwrap_or_else(|error| panic!("{name} failed: {error:?}"))
        }
    };

    // 1. A script opened for editing is found among the open script editors.
    let opened = send("__probe_open", json!({"path": "res://probe.gd"})).await;
    println!("1 open: {opened}");
    println!("1 shape: {}", send("__probe_shape", json!({})).await);
    sleep(Duration::from_millis(500)).await;
    let state = send("__probe_state", json!({"path": "res://probe.gd"})).await;
    println!("1 state after open: {state}");
    assert_eq!(state["found"], true, "the opened script has an editor");
    assert_eq!(state["text"], probe, "the editor shows the file text");
    assert_eq!(
        state["version"], state["saved_version"],
        "a fresh script is saved"
    );

    // 2. Typing makes the unsaved state detectable and the unsaved text readable.
    let typed = send(
        "__probe_type",
        json!({"path": "res://probe.gd", "text": "# typed\n"}),
    )
    .await;
    println!("2 typed: {typed}");
    let dirty = send("__probe_state", json!({"path": "res://probe.gd"})).await;
    println!("2 state after typing: {dirty}");
    assert_ne!(
        dirty["version"], dirty["saved_version"],
        "typing marks the script unsaved"
    );
    assert!(
        dirty["text"]
            .as_str()
            .unwrap_or("")
            .starts_with("# typed\n")
    );
    println!(
        "2 resource source_code follows the buffer: {}",
        dirty["source_code"] == dirty["text"]
    );
    assert_eq!(
        std::fs::read_to_string(editor.project.join("probe.gd")).expect("file"),
        probe,
        "typing does not touch the file"
    );

    // 3. One complex operation is undone by one undo step. First drop the typed line.
    let undone_typing = send("__probe_undo", json!({"path": "res://probe.gd"})).await;
    println!("3 undo typing: {undone_typing}");
    assert_eq!(undone_typing["text"], probe);
    let replaced = send(
        "__probe_replace",
        json!({"path": "res://probe.gd", "old": "return 1", "new": "return 2"}),
    )
    .await;
    println!("3 replaced: {replaced}");
    assert!(replaced["text"].as_str().unwrap_or("").contains("return 2"));
    let undone = send("__probe_undo", json!({"path": "res://probe.gd"})).await;
    println!("3 undo replace: {undone}");
    assert_eq!(
        undone["text"], probe,
        "one undo step reverses the replacement"
    );

    // 4. Saving writes the new text to disk and a node running the script gets the new code.
    let attached = send("__probe_attach", json!({"path": "res://probe.gd"})).await;
    println!("4 attached: {attached}");
    assert_eq!(attached["answer"], 1);
    send(
        "__probe_replace",
        json!({"path": "res://probe.gd", "old": "return 1", "new": "return 2"}),
    )
    .await;
    let saved = send("__probe_save", json!({"path": "res://probe.gd"})).await;
    println!("4 saved: {saved}");
    assert_eq!(saved["save_err"], 0);
    assert_eq!(
        saved["version"], saved["saved_version"],
        "saving clears the unsaved state"
    );
    let on_disk = std::fs::read_to_string(editor.project.join("probe.gd")).expect("file");
    println!("4 file after save: {on_disk:?}");
    assert!(on_disk.contains("return 2"));
    let called = send("__probe_call", json!({})).await;
    println!("4 running node answers: {called}");
    assert_eq!(called["answer"], 2, "the running script has the new code");

    // 5. A script that was not open is opened, edited and saved the same way.
    let before_open = send("__probe_state", json!({"path": "res://other.gd"})).await;
    println!("5 other before open: {before_open}");
    assert_eq!(before_open["found"], false, "other.gd was never opened");
    send("__probe_open", json!({"path": "res://other.gd"})).await;
    sleep(Duration::from_millis(500)).await;
    send(
        "__probe_replace",
        json!({"path": "res://other.gd", "old": "var x = 1", "new": "var x = 2"}),
    )
    .await;
    let saved_other = send("__probe_save", json!({"path": "res://other.gd"})).await;
    println!("5 other saved: {saved_other}");
    let other_disk = std::fs::read_to_string(editor.project.join("other.gd")).expect("file");
    assert!(
        other_disk.contains("var x = 2"),
        "file after save: {other_disk:?}"
    );

    // Non-script tabs: a text file and a help page share the tab list with the scripts.
    println!("6 mixed: {}", send("__probe_mixed", json!({})).await);
}

/// The version label of the script tools: 64-bit FNV-1a over the UTF-8 bytes of the text with
/// `\r\n` read as `\n`, as 16 lowercase hex digits. The addon computes it in GDScript, the host
/// in Rust; both are held to the same vectors.
fn script_label(text: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.replace("\r\n", "\n").bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

#[test]
fn the_script_label_matches_its_published_vectors() {
    assert_eq!(script_label(""), "cbf29ce484222325");
    assert_eq!(script_label("extends Node\n"), "688cd8553db5c904");
    assert_eq!(script_label("a"), "af63dc4c8601ec8c");
    assert_eq!(
        script_label("extends Node\r\n"),
        script_label("extends Node\n")
    );
}

/// CA-7: a read answers with the text the editor would save, says where it came from and
/// whether the editor holds unsaved changes, and every refusal is a reply with a fix.
#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn read_script_answers_from_the_file_or_from_unsaved_editor_text() {
    let big = "#".repeat(1_048_577);
    let editor = start_editor_with(
        Some(SCRIPT_PROBE_HOOK),
        SCRIPT_PROBE_HELPERS,
        &[
            (
                "probe.gd",
                "@tool\nextends Node\n\nfunc answer() -> int:\n\treturn 1\n",
            ),
            ("other.gd", "extends Node\n"),
            ("crlf.gd", "extends Node\r\n"),
            ("empty.gd", ""),
            ("a.gd", "a"),
            ("notes.txt", "hello\n"),
            ("big.gd", big.as_str()),
        ],
        "",
    )
    .await;
    let read = |path: &'static str| {
        let bridge = &editor.bridge;
        async move {
            bridge
                .send_command("read_script", json!({"script_path": path}))
                .await
        }
    };

    // A script that is not open is read from the file.
    let other = read("res://other.gd").await.expect("read other.gd");
    assert_eq!(other["path"], "res://other.gd");
    assert_eq!(other["text"], "extends Node\n");
    assert_eq!(other["source"], "file");
    assert_eq!(other["dirty"], false);
    assert_eq!(other["version"], "688cd8553db5c904");

    // The label ignores the file's line endings, and the text is given with `\n`.
    let crlf = read("res://crlf.gd").await.expect("read crlf.gd");
    assert_eq!(crlf["text"], "extends Node\n");
    assert_eq!(crlf["version"], "688cd8553db5c904");

    // The shared vectors, computed by the addon.
    assert_eq!(
        read("res://empty.gd").await.expect("empty")["version"],
        "cbf29ce484222325"
    );
    assert_eq!(
        read("res://a.gd").await.expect("a")["version"],
        "af63dc4c8601ec8c"
    );

    // An open script with no unsaved typing is still read from the file.
    editor
        .bridge
        .send_command("__probe_open", json!({"path": "res://probe.gd"}))
        .await
        .expect("open");
    sleep(Duration::from_millis(500)).await;
    let saved_text = "@tool\nextends Node\n\nfunc answer() -> int:\n\treturn 1\n";
    let clean = read("res://probe.gd").await.expect("read clean");
    assert_eq!(clean["source"], "file");
    assert_eq!(clean["dirty"], false);
    assert_eq!(clean["version"], script_label(saved_text));

    // Unsaved typing: the answer carries the editor's text and a label of that text.
    editor
        .bridge
        .send_command(
            "__probe_type",
            json!({"path": "res://probe.gd", "text": "# typed\n"}),
        )
        .await
        .expect("type");
    let typed = read("res://probe.gd").await.expect("read typed");
    let typed_text = format!("# typed\n{saved_text}");
    assert_eq!(typed["source"], "editor");
    assert_eq!(typed["dirty"], true);
    assert_eq!(typed["text"], typed_text);
    assert_eq!(typed["version"], script_label(&typed_text));
    assert_ne!(typed["version"], clean["version"]);
    assert_eq!(
        std::fs::read_to_string(editor.project.join("probe.gd")).expect("file"),
        saved_text,
        "a read never touches the file"
    );

    // Refusals reply, name the problem and leave the editor usable.
    expect_error(read("res://missing.gd").await, "does not exist");
    expect_error(read("res://notes.txt").await, ".gd");
    expect_error(read("res://../outside.gd").await, "inside the project");
    expect_error(read("res://big.gd").await, "1 MiB");
    expect_error(
        editor
            .bridge
            .send_command("read_script", json!({"script_path": 5}))
            .await,
        "must be of type",
    );
    expect_error(
        editor.bridge.send_command("read_script", json!({})).await,
        "script_path",
    );
    assert_eq!(
        read("res://other.gd").await.expect("still answering")["version"],
        "688cd8553db5c904"
    );
}

/// CA-8 to CA-10: an edit replaces one fragment through the script editor as one undo step,
/// saves it, and never touches a script the editor holds unsaved typing for.
#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn edit_script_replaces_one_fragment_and_never_overwrites_typing() {
    let live = "@tool\nextends Node\n\nfunc answer() -> int:\n\treturn 1\n";
    let other = "extends Node\n\nvar x = 1\nvar y = 1\n";
    let editor = start_editor_with(
        Some(SCRIPT_PROBE_HOOK),
        SCRIPT_PROBE_HELPERS,
        &[
            ("probe.gd", live),
            ("live.gd", live),
            ("other.gd", other),
            ("notes.txt", "hello\n"),
        ],
        "",
    )
    .await;
    let bridge = &editor.bridge;
    let file = |name: &str| std::fs::read_to_string(editor.project.join(name)).expect("file");
    let edit = |path: &'static str, old: &str, new: &str| {
        let params = json!({"script_path": path, "old_text": old, "new_text": new});
        async move { bridge.send_command("edit_script", params).await }
    };
    let state = |path: &'static str| async move {
        bridge
            .send_command("__probe_state", json!({"path": path}))
            .await
            .expect("state")
    };

    // One fragment is replaced, the file is saved, the answer carries the new label, and the
    // editor shows a saved script that one undo step takes back.
    let uid_before = std::fs::read_to_string(editor.project.join("other.gd.uid")).ok();
    let edited_text = "extends Node\n\nvar x = 2\nvar y = 1\n";
    let done = edit("res://other.gd", "var x = 1", "var x = 2")
        .await
        .expect("edit other.gd");
    assert_eq!(done["path"], "res://other.gd");
    assert_eq!(done["replaced"], 1);
    assert_eq!(done["source"], "editor");
    assert_eq!(done["version"], script_label(edited_text));
    assert_eq!(file("other.gd"), edited_text);
    let shown = state("res://other.gd").await;
    assert_eq!(
        shown["found"], true,
        "the edited script is open in the editor"
    );
    assert_eq!(shown["text"], edited_text);
    assert_eq!(
        shown["version"], shown["saved_version"],
        "the edit is saved"
    );
    assert_eq!(
        std::fs::read_to_string(editor.project.join("other.gd.uid")).ok(),
        uid_before,
        "the script keeps its uid file"
    );
    let undone = bridge
        .send_command("__probe_undo", json!({"path": "res://other.gd"}))
        .await
        .expect("undo");
    assert_eq!(undone["text"], other, "one undo step reverses the edit");
    // Put the saved text back in the editor so later edits start clean.
    let again = edit("res://other.gd", "var x = 1", "var x = 2").await;
    expect_error(again, "unsaved changes");
    bridge
        .send_command("__probe_save", json!({"path": "res://other.gd"}))
        .await
        .expect("save original text");
    assert_eq!(file("other.gd"), other);

    // Zero and several matches are refused with the count and leave everything as it was.
    expect_error(
        edit("res://other.gd", "var z = 1", "var z = 2").await,
        "0 times",
    );
    expect_error(edit("res://other.gd", "var ", "let ").await, "2 times");
    assert_eq!(file("other.gd"), other);

    // A stale label is refused; the current label is accepted.
    let stale = bridge
        .send_command(
            "edit_script",
            json!({"script_path": "res://other.gd", "old_text": "var y = 1",
                   "new_text": "var y = 3", "version": "0000000000000000"}),
        )
        .await;
    expect_error(stale, "changed since it was read");
    assert_eq!(file("other.gd"), other);
    let fresh = bridge
        .send_command(
            "edit_script",
            json!({"script_path": "res://other.gd", "old_text": "var y = 1",
                   "new_text": "var y = 3", "version": script_label(other)}),
        )
        .await
        .expect("edit with the current label");
    assert_eq!(fresh["replaced"], 1);

    // Text is data: quotes, format markers, braces, backslashes and code-looking lines are
    // inserted as they are.
    let tricky = "var y = \"%s {0} \\\\ \\n $x\"\nfunc injected() -> void:\n\tOS.crash(\"no\")";
    edit("res://other.gd", "var y = 3", tricky)
        .await
        .expect("edit with tricky text");
    assert!(
        file("other.gd").contains(tricky),
        "file: {:?}",
        file("other.gd")
    );

    // Unsaved typing is never overwritten (CA-9).
    bridge
        .send_command("__probe_open", json!({"path": "res://probe.gd"}))
        .await
        .expect("open");
    sleep(Duration::from_millis(500)).await;
    bridge
        .send_command(
            "__probe_type",
            json!({"path": "res://probe.gd", "text": "# typed\n"}),
        )
        .await
        .expect("type");
    expect_error(
        edit("res://probe.gd", "return 1", "return 2").await,
        "unsaved changes",
    );
    assert_eq!(file("probe.gd"), live, "the file is untouched");
    let still = state("res://probe.gd").await;
    assert!(
        still["text"]
            .as_str()
            .unwrap_or("")
            .starts_with("# typed\n"),
        "the typed text is still in the editor"
    );

    // A running script gets the edited code, and the edit opened the script that was closed.
    let before = bridge
        .send_command("__probe_attach", json!({"path": "res://live.gd"}))
        .await
        .expect("attach");
    assert_eq!(before["answer"], 1);
    assert_eq!(state("res://live.gd").await["found"], false);
    edit("res://live.gd", "return 1", "return 2")
        .await
        .expect("edit live.gd");
    let after = bridge
        .send_command("__probe_call", json!({}))
        .await
        .expect("call");
    assert_eq!(after["answer"], 2, "the running node has the new code");

    // Refusals reply with a fix.
    expect_error(edit("res://other.gd", "", "x").await, "must not be empty");
    expect_error(
        edit("res://other.gd", "var x = 1", "var x = 1").await,
        "identical",
    );
    expect_error(
        edit("res://missing.gd", "a", "b").await,
        "godot_create_script",
    );
    expect_error(edit("res://notes.txt", "hello", "bye").await, ".gd");
    expect_error(edit("res://../x.gd", "a", "b").await, "inside the project");
    expect_error(
        bridge
            .send_command(
                "edit_script",
                json!({"script_path": "res://other.gd", "old_text": "a", "new_text": 5}),
            )
            .await,
        "must be of type",
    );
    assert_eq!(file("notes.txt"), "hello\n");
}

/// Two scripts with the same file name in different folders are both open in the script
/// editor, whose tabs then carry the parent folder ("b/x.gd"); each edits through its own tab.
#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn same_named_scripts_in_different_folders_edit_through_their_own_tabs() {
    let a_text = "extends Node\n\nvar a = 1\n";
    let b_text = "extends Node\n\nvar b = 1\n";
    let editor = start_editor_with(
        Some(SCRIPT_PROBE_HOOK),
        SCRIPT_PROBE_HELPERS,
        &[("a/x.gd", a_text), ("b/x.gd", b_text)],
        "",
    )
    .await;
    let bridge = &editor.bridge;
    let file = |name: &str| std::fs::read_to_string(editor.project.join(name)).expect("file");
    let edit = |path: &'static str, old: &str, new: &str| {
        let params = json!({"script_path": path, "old_text": old, "new_text": new});
        async move { bridge.send_command("edit_script", params).await }
    };

    edit("res://a/x.gd", "var a = 1", "var a = 2")
        .await
        .expect("edit a/x.gd while only it is open");
    // b/x.gd is closed: the edit opens it next to a/x.gd, and both tabs get folder prefixes.
    edit("res://b/x.gd", "var b = 1", "var b = 2")
        .await
        .expect("edit b/x.gd, which the edit opens next to a/x.gd");
    assert_eq!(file("b/x.gd"), "extends Node\n\nvar b = 2\n");
    edit("res://a/x.gd", "var a = 2", "var a = 3")
        .await
        .expect("edit a/x.gd while both are open");
    edit("res://b/x.gd", "var b = 2", "var b = 3")
        .await
        .expect("edit b/x.gd while both are open");
    assert_eq!(file("a/x.gd"), "extends Node\n\nvar a = 3\n");
    assert_eq!(file("b/x.gd"), "extends Node\n\nvar b = 3\n");

    let read = bridge
        .send_command("read_script", json!({"script_path": "res://b/x.gd"}))
        .await
        .expect("read b/x.gd");
    assert_eq!(read["text"], "extends Node\n\nvar b = 3\n", "{read}");
    assert!(
        read.get("editor_state").is_none(),
        "the read matches the tab: {read}"
    );
}

/// A script tab that shows older text than the file is reported by a read, reloaded by
/// godot_resave_resources and by godot_create_script's overwrite, and an overwrite of a script
/// with unsaved typing is refused.
#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn an_open_script_tab_follows_the_file_or_says_it_does_not() {
    use godot_mcp::cli_runner::CliRunner;
    use godot_mcp::tools::ToolManager;
    use std::sync::Arc;

    let first = "extends Node\n\nfunc answer() -> int:\n\treturn 1\n";
    let editor = start_editor_with(
        Some(SCRIPT_PROBE_HOOK),
        SCRIPT_PROBE_HELPERS,
        &[("probe.gd", first)],
        "",
    )
    .await;
    let bridge = &editor.bridge;
    let (godot, _) = godot_binaries();
    let tools = Arc::new(
        ToolManager::new(BridgeClient::new(editor.port), CliRunner::new(godot))
            .with_working_dir(editor.project.clone()),
    );
    let path = editor.project.join("probe.gd");
    let file = || std::fs::read_to_string(&path).expect("file");
    let state = || async {
        bridge
            .send_command("__probe_state", json!({"path": "res://probe.gd"}))
            .await
            .expect("state")
    };
    let read = || async {
        bridge
            .send_command("read_script", json!({"script_path": "res://probe.gd"}))
            .await
            .expect("read")
    };
    bridge
        .send_command("__probe_open", json!({"path": "res://probe.gd"}))
        .await
        .expect("open");
    sleep(Duration::from_millis(500)).await;

    // The file changes behind the open tab: the read says so, and an edit names the way out.
    let second = "extends Node\n\nfunc answer() -> int:\n\treturn 2\n";
    std::fs::write(&path, second).expect("change the file");
    let stale = read().await;
    assert_eq!(stale["source"], "file", "{stale}");
    assert_eq!(stale["text"], second, "{stale}");
    assert_eq!(stale["dirty"], false, "{stale}");
    assert_eq!(stale["stale_tab"], true, "{stale}");
    expect_error(
        bridge
            .send_command(
                "edit_script",
                json!({"script_path": "res://probe.gd", "old_text": "return 2", "new_text": "return 3"}),
            )
            .await,
        "godot_resave_resources",
    );

    // godot_resave_resources with the path reloads the tab, which then shows the saved file.
    let refreshed = Arc::clone(&tools)
        .call_tool(
            "godot_resave_resources",
            json!({"paths": ["res://probe.gd"]}),
        )
        .await;
    assert!(!refreshed.is_error, "{}", tool_text(&refreshed));
    let refreshed: Value = serde_json::from_str(&tool_text(&refreshed)).expect("resave json");
    assert_eq!(
        refreshed["reloaded_tabs"],
        json!(["res://probe.gd"]),
        "{refreshed}"
    );
    let shown = state().await;
    assert_eq!(shown["text"], second, "{shown}");
    assert_eq!(shown["version"], shown["saved_version"], "{shown}");
    assert!(read().await.get("stale_tab").is_none());

    // An overwrite through godot_create_script reloads the clean tab, so the next edit works.
    let created = Arc::clone(&tools)
        .call_tool(
            "godot_create_script",
            json!({"script_path": "res://probe.gd", "overwrite": true}),
        )
        .await;
    assert!(!created.is_error, "{}", tool_text(&created));
    assert!(
        tool_text(&created).contains("reloaded"),
        "{}",
        tool_text(&created)
    );
    let fresh = file();
    assert!(fresh.contains("func _ready() -> void:"), "{fresh}");
    let shown = state().await;
    assert_eq!(shown["text"], fresh, "{shown}");
    assert_eq!(shown["version"], shown["saved_version"], "{shown}");
    let edited = Arc::clone(&tools)
        .call_tool(
            "godot_edit_script",
            json!({"script_path": "res://probe.gd", "old_text": "\tpass", "new_text": "\tprint(1)"}),
        )
        .await;
    assert!(!edited.is_error, "{}", tool_text(&edited));
    assert!(file().contains("\tprint(1)"), "{}", file());

    // Unsaved typing in the tab: the overwrite is refused and both texts stay.
    bridge
        .send_command(
            "__probe_type",
            json!({"path": "res://probe.gd", "text": "# typed\n"}),
        )
        .await
        .expect("type");
    let before = file();
    let refused = Arc::clone(&tools)
        .call_tool(
            "godot_create_script",
            json!({"script_path": "res://probe.gd", "overwrite": true}),
        )
        .await;
    assert!(refused.is_error, "{}", tool_text(&refused));
    assert!(
        tool_text(&refused).contains("unsaved changes"),
        "{}",
        tool_text(&refused)
    );
    assert_eq!(file(), before, "the file is untouched");
    assert!(
        state().await["text"]
            .as_str()
            .unwrap_or("")
            .starts_with("# typed\n"),
        "the typed text is still in the editor"
    );

    // A dirty tab is never reloaded by a refresh either.
    let kept = Arc::clone(&tools)
        .call_tool(
            "godot_resave_resources",
            json!({"paths": ["res://probe.gd"]}),
        )
        .await;
    assert!(!kept.is_error, "{}", tool_text(&kept));
    let kept: Value = serde_json::from_str(&tool_text(&kept)).expect("resave json");
    assert_eq!(kept["reloaded_tabs"], json!([]), "{kept}");
    assert_eq!(kept["unsaved_tabs"], json!(["res://probe.gd"]), "{kept}");
}

/// What the amended concepts ask of the editor commands: a script with a byte order mark is read
/// and edited, a new file's folders are created, an unset object property is `null`, a node's
/// signals list only connections inside the scene, and a setting key has no dot segment.
#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn the_editor_commands_follow_the_amended_concepts() {
    let editor = start_editor_with(
        None,
        "",
        &[("bom.gd", "\u{feff}extends Node\n\nvar bom_var: int = 1\n")],
        "",
    )
    .await;
    let bridge = &editor.bridge;
    let call = |command: &'static str, params: Value| async move {
        bridge.send_command(command, params).await
    };

    // A byte order mark does not make the script "not UTF-8" (CA-7).
    let read = call("read_script", json!({"script_path": "res://bom.gd"}))
        .await
        .expect("read a script with a byte order mark");
    assert!(
        read["text"]
            .as_str()
            .unwrap_or("")
            .starts_with("extends Node"),
        "{read}"
    );
    let edited = call(
        "edit_script",
        json!({"script_path": "res://bom.gd", "old_text": "bom_var: int = 1",
               "new_text": "bom_var: int = 2"}),
    )
    .await
    .expect("edit a script with a byte order mark");
    let bytes = std::fs::read(editor.project.join("bom.gd")).expect("bom.gd");
    let kept = bytes.starts_with(&[0xEF, 0xBB, 0xBF]);
    assert_eq!(
        edited["bom_removed"],
        json!(!kept),
        "the answer says what the save did to the mark: {edited}"
    );
    assert!(
        String::from_utf8_lossy(&bytes).contains("bom_var: int = 2"),
        "the edit is on disk"
    );

    // The folders of a new file are created (CA-6) and a folder that cannot be is named.
    call(
        "create_scene",
        json!({"scene_path": "res://newdir/deep/scene.tscn", "root_type": "Node2D"}),
    )
    .await
    .expect("create a scene in a new folder");
    assert!(editor.project.join("newdir/deep/scene.tscn").is_file());
    call(
        "create_resource",
        json!({"resource_path": "res://other/deep/g.tres", "class_name": "Gradient"}),
    )
    .await
    .expect("create a resource in a new folder");
    assert!(editor.project.join("other/deep/g.tres").is_file());
    std::fs::write(editor.project.join("blocker"), "a file, not a folder").expect("blocker");
    expect_error(
        call(
            "create_scene",
            json!({"scene_path": "res://blocker/x.tscn", "root_type": "Node2D"}),
        )
        .await,
        "blocker",
    );

    // An unset object property is null, not a "freed object".
    call("open_scene", json!({"scene_path": "res://main.tscn"}))
        .await
        .expect("open the main scene");
    let properties = call("get_node_properties", json!({"node_path": "Player/Body"}))
        .await
        .expect("properties of a sprite");
    assert_eq!(
        properties["properties"]["texture"]["value"], "null",
        "{properties}"
    );
    assert!(
        !properties.to_string().contains("freed object"),
        "an unset property was reported as freed"
    );

    // A node's signal list holds only connections to nodes of the scene (BR-6 spirit), with the
    // target's scene-relative path.
    std::fs::write(
        editor.project.join("listener.gd"),
        "extends Node2D

func on_ready() -> void:
\tpass
",
    )
    .expect("listener.gd");
    call(
        "attach_script",
        json!({"node_path": ".", "script_path": "res://listener.gd"}),
    )
    .await
    .expect("attach the listener");
    call(
        "connect_signal",
        json!({"node_path": "Player", "signal_name": "ready", "target_path": ".",
               "method": "on_ready"}),
    )
    .await
    .expect("connect ready");
    let signals = call("list_signals", json!({"node_path": "Player"}))
        .await
        .expect("list signals");
    let all = signals["signals"].as_array().expect("signals");
    let ready = all
        .iter()
        .find(|entry| entry["name"] == "ready")
        .expect("ready signal");
    assert_eq!(
        ready["connections"],
        json!([{"target": ".", "method": "on_ready"}]),
        "{signals}"
    );
    for entry in all {
        for connection in entry["connections"].as_array().expect("connections") {
            assert!(
                connection["target"].is_string() && connection["method"].is_string(),
                "{entry}"
            );
            assert!(
                !connection.to_string().contains("::"),
                "an editor-internal connection is listed: {connection}"
            );
        }
    }

    // A setting key has no empty, "." or ".." segment.
    for key in ["simqa/../evil", "simqa/./x", "../x/y", "simqa/a/../b"] {
        expect_error(
            call("set_project_setting", json!({"key": key, "value": 1})).await,
            "key",
        );
    }
}

/// A new script created with a project_path that names the editor's project is picked up by the
/// editor at once (it gets its .uid), so its class_name is known to the editor's language server.
#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn a_new_script_in_the_named_editor_project_is_picked_up_by_the_editor() {
    use godot_mcp::cli_runner::CliRunner;
    use godot_mcp::tools::ToolManager;
    use std::sync::Arc;

    let editor = start_editor(false).await;
    let elsewhere = tempfile::tempdir().expect("working dir");
    let (godot, _) = godot_binaries();
    let tools = Arc::new(
        ToolManager::new(BridgeClient::new(editor.port), CliRunner::new(godot))
            .with_working_dir(elsewhere.path().to_path_buf()),
    );
    let created = Arc::clone(&tools)
        .call_tool(
            "godot_create_script",
            json!({
                "project_path": editor.project,
                "script_path": "res://crate_lift.gd",
                "class_name": "CrateLift",
            }),
        )
        .await;
    assert!(!created.is_error, "{}", tool_text(&created));
    let uid = editor.project.join("crate_lift.gd.uid");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !uid.is_file() {
        assert!(
            std::time::Instant::now() < deadline,
            "the editor did not pick up the new script"
        );
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}

/// Creates `StreamLane` (class_name) through the editor and then a script that uses it as a
/// type, and validates that script with a server whose language server port is `lsp_port`.
async fn validate_a_script_using_a_class_created_just_before(
    editor: &Editor,
    lsp_port: u16,
) -> godot_mcp::protocol::ToolResult {
    use godot_mcp::cli_runner::CliRunner;
    use godot_mcp::tools::ToolManager;
    use std::sync::Arc;

    let (godot, _) = godot_binaries();
    let tools = Arc::new(
        ToolManager::new(BridgeClient::new(editor.port), CliRunner::new(godot))
            .with_working_dir(editor.project.clone())
            .with_lsp_port(lsp_port),
    );
    let call = |name: &'static str, args: Value| {
        let tools = Arc::clone(&tools);
        async move { tools.call_tool(name, args).await }
    };
    let lane = call(
        "godot_create_script",
        json!({
            "script_path": "res://world/stream_lane.gd",
            "extends_class": "Path3D",
            "class_name": "StreamLane",
        }),
    )
    .await;
    assert!(!lane.is_error, "{}", tool_text(&lane));
    let current = call(
        "godot_create_script",
        json!({
            "script_path": "res://world/stream_current.gd",
            "content": "extends Node3D\n\n@export var lane_path: NodePath\n\nfunc lane() -> StreamLane:\n\tvar node: Node = get_node_or_null(lane_path)\n\tif node is StreamLane:\n\t\treturn node as StreamLane\n\treturn null\n",
        }),
    )
    .await;
    assert!(!current.is_error, "{}", tool_text(&current));
    call(
        "godot_validate_script",
        json!({"script_path": "res://world/stream_current.gd"}),
    )
    .await
}

/// The project's own editor serves the language server. A class created through it and a script
/// that uses that class right away validate clean: the editor's class list is not stale.
#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn a_script_using_a_class_created_just_before_validates_in_the_editors_language_server() {
    let lsp_port = common::free_port();
    let lsp_arg = lsp_port.to_string();
    let editor = start_editor_with_args(None, "", &[], "", &["--lsp-port", &lsp_arg]).await;
    let validated = validate_a_script_using_a_class_created_just_before(&editor, lsp_port).await;
    assert!(!validated.is_error, "{}", tool_text(&validated));
}

/// The editor's language server is not reachable (its port was taken by another editor), so
/// the headless check runs. It knows a class the editor registered moments ago.
#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn a_script_using_a_class_created_just_before_validates_in_the_headless_check() {
    let editor = start_editor(false).await;
    let validated =
        validate_a_script_using_a_class_created_just_before(&editor, common::free_port()).await;
    assert!(!validated.is_error, "{}", tool_text(&validated));
}

/// A second editor on a port that is taken says so, and says what it means for the server.
#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn a_second_editor_on_a_taken_port_says_that_the_server_talks_to_the_other_one() {
    let first = start_editor(false).await;
    let second_dir = tempfile::tempdir().expect("second project");
    let addon = second_dir.path().join("addons/godot_mcp");
    copy_dir(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("addons/godot_mcp"),
        &addon,
    );
    std::fs::write(
        second_dir.path().join("project.godot"),
        "config_version=5

[editor_plugins]

enabled=PackedStringArray(\"res://addons/godot_mcp/plugin.cfg\")
",
    )
    .expect("project.godot");
    let (_godot, console) = godot_binaries();
    let output = timeout(
        Duration::from_secs(180),
        Command::new(&console)
            .args(["--headless", "--path"])
            .arg(second_dir.path())
            .arg("--import")
            .env("GODOT_DEBUG_PORT", first.port.to_string())
            .stdin(Stdio::null())
            .output(),
    )
    .await
    .expect("second editor timed out")
    .expect("run the second editor");
    let log = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(log.contains("Already in use"), "{log}");
    assert!(
        log.contains("another editor"),
        "the message does not say who answers on the port: {log}"
    );
    // The first editor still answers, with its own token.
    first
        .bridge
        .send_command("ping", json!({}))
        .await
        .expect("the first editor still answers");
}

/// Test-only command that undoes the last action of the editor's global history, where project
/// setting edits are recorded.
const GLOBAL_UNDO_HOOK: &str = r#"        "__test_undo_global":
            undo_redo.get_history_undo_redo(EditorUndoRedoManager.GLOBAL_HISTORY).undo()
            send_success(peer, cmd_id, {"undone": true})
        "ping":
"#;

/// An InputMap action gets a key, a mouse button and joypad events, is saved through the editor,
/// and one undo restores it. A request the editor cannot apply changes nothing.
#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn input_actions_are_edited_saved_and_undone_through_the_editor() {
    let editor = start_editor_with(
        Some(GLOBAL_UNDO_HOOK),
        "",
        &[],
        "\n[input]\n\ntongue={\n\"deadzone\": 0.2,\n\"events\": [Object(InputEventKey,\"resource_local_to_scene\":false,\"resource_name\":\"\",\"device\":-1,\"window_id\":0,\"alt_pressed\":false,\"shift_pressed\":false,\"ctrl_pressed\":false,\"meta_pressed\":false,\"pressed\":false,\"keycode\":0,\"physical_keycode\":32,\"key_label\":0,\"unicode\":0,\"location\":0,\"echo\":false,\"script\":null)\n]\n}\n",
    )
    .await;
    let bridge = &editor.bridge;
    let call = |command: &'static str, params: Value| async move {
        bridge.send_command(command, params).await
    };
    let project_file =
        || std::fs::read_to_string(editor.project.join("project.godot")).expect("project.godot");

    let listed = call("get_input_map", json!({}))
        .await
        .expect("list actions");
    let actions = listed["actions"].as_array().expect("actions");
    let tongue = actions
        .iter()
        .find(|action| action["name"] == "tongue")
        .expect("tongue listed");
    assert_eq!(
        tongue["events"].as_array().map(Vec::len),
        Some(1),
        "{tongue}"
    );
    assert_eq!(tongue["events"][0]["type"], "key", "{tongue}");
    assert_eq!(tongue["events"][0]["key"], "Space", "{tongue}");
    assert_eq!(tongue["events"][0]["physical"], true, "{tongue}");
    assert!(
        actions
            .iter()
            .all(|action| !action["name"].as_str().unwrap_or("").starts_with("ui_")),
        "built-in actions are listed by default: {listed}"
    );
    assert!(
        listed["builtin_hidden"].as_u64().unwrap_or(0) > 0,
        "{listed}"
    );
    let builtins = call("get_input_map", json!({"include_builtin": true}))
        .await
        .expect("list with built-ins");
    assert!(builtins.to_string().contains("ui_accept"), "{builtins}");

    let edited = call(
        "edit_input_action",
        json!({"action": "tongue", "deadzone": 0.5, "add_events": [
            {"type": "key", "key": "E"},
            {"type": "mouse_button", "button": "right"},
            {"type": "joypad_button", "button": 2},
            {"type": "joypad_motion", "axis": 1, "axis_value": -0.7}
        ]}),
    )
    .await
    .expect("add events");
    assert_eq!(edited["saved"], true, "{edited}");
    assert_eq!(edited["added"], 4, "{edited}");
    assert!(
        (edited["deadzone"].as_f64().unwrap_or(0.0) - 0.5).abs() < 1e-6,
        "{edited}"
    );
    let events = edited["events"].as_array().expect("events");
    assert_eq!(events.len(), 5, "{edited}");
    assert!(
        events.iter().any(|event| event["type"] == "key"
            && event["key"] == "E"
            && event["physical"] == true),
        "{edited}"
    );
    assert!(
        events
            .iter()
            .any(|event| event["type"] == "joypad_motion" && event["axis_value"] == -1.0),
        "{edited}"
    );
    let saved = project_file();
    assert!(saved.contains("\"deadzone\": 0.5"), "{saved}");
    assert!(saved.contains("InputEventMouseButton"), "{saved}");
    assert!(saved.contains("InputEventJoypadMotion"), "{saved}");

    // The same event again is not added twice.
    let again = call(
        "edit_input_action",
        json!({"action": "tongue", "add_events": [{"type": "key", "key": "E"}]}),
    )
    .await
    .expect("add a duplicate");
    assert_eq!(again["added"], 0, "{again}");
    assert_eq!(
        again["already_present"].as_array().map(Vec::len),
        Some(1),
        "{again}"
    );

    // Removing an event the action does not have, a key Godot does not know, or editing a missing
    // action without create, changes nothing.
    for (params, needle) in [
        (
            json!({"action": "tongue", "remove_events": [{"type": "key", "key": "Q"}]}),
            "does not have",
        ),
        (
            json!({"action": "tongue", "add_events": [{"type": "key", "key": "NoSuchKey"}]}),
            "Unknown key",
        ),
        (
            json!({"action": "missing", "add_events": [{"type": "key", "key": "E"}]}),
            "create",
        ),
    ] {
        expect_error(call("edit_input_action", params).await, needle);
    }
    assert_eq!(
        project_file(),
        saved,
        "a refused request changed project.godot"
    );

    let removed = call(
        "edit_input_action",
        json!({"action": "tongue", "remove_events": [{"type": "mouse_button", "button": "right"}]}),
    )
    .await
    .expect("remove an event");
    assert_eq!(removed["removed"], 1, "{removed}");
    assert!(
        !project_file().contains("InputEventMouseButton"),
        "{}",
        project_file()
    );

    // One undo puts the mouse button back, and the file follows.
    call("__test_undo_global", json!({})).await.expect("undo");
    let restored = call("get_input_map", json!({"action": "tongue"}))
        .await
        .expect("read after undo");
    assert_eq!(
        restored["actions"][0]["events"].as_array().map(Vec::len),
        Some(5),
        "{restored}"
    );
    assert!(
        project_file().contains("InputEventMouseButton"),
        "{}",
        project_file()
    );

    // A new action is created and deleted again.
    let created = call(
        "edit_input_action",
        json!({"action": "jump", "create": true, "add_events": [{"type": "key", "key": "Space", "physical": false, "shift": true}]}),
    )
    .await
    .expect("create an action");
    assert_eq!(created["created"], true, "{created}");
    assert_eq!(created["events"][0]["physical"], false, "{created}");
    assert_eq!(created["events"][0]["shift"], true, "{created}");
    assert!(project_file().contains("jump={"), "{}", project_file());
    let deleted = call(
        "edit_input_action",
        json!({"action": "jump", "delete": true}),
    )
    .await
    .expect("delete an action");
    assert_eq!(deleted["deleted"], true, "{deleted}");
    assert!(!project_file().contains("jump={"), "{}", project_file());
    expect_error(
        call(
            "edit_input_action",
            json!({"action": "ui_accept", "delete": true}),
        )
        .await,
        "built-in",
    );
}

/// Test-only command that makes the bridge act as if the editor were still starting, for
/// `hold_ms` milliseconds.
const STARTUP_HOLD_HOOK: &str = r#"        "__test_hold_startup":
            var until: int = Time.get_ticks_msec() + int(params.get("hold_ms", 0))
            _editor_started = false
            _startup_probe = func() -> bool: return Time.get_ticks_msec() >= until
            send_success(peer, cmd_id, {"holding": true})
        "ping":
"#;

/// The editor opens the project's main scene only when the first scan of its files ends, while
/// the bridge already listens during that scan. A scene opened before then lost its tab to the
/// main scene, so the next edit went to the wrong scene. Until the editor has started, the bridge
/// answers ping (with `starting`) and the facts the scan does not change, and refuses the rest.
#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn commands_wait_until_the_editor_has_opened_its_startup_scenes() {
    let editor = start_editor_with(
        Some(STARTUP_HOLD_HOOK),
        "",
        &[(
            "pond.tscn",
            "[gd_scene format=3]\n\n[node name=\"Pond\" type=\"Node3D\"]\n",
        )],
        "",
    )
    .await;
    let bridge = &editor.bridge;
    // The editor opened the main scene at the end of its start.
    let open = bridge
        .send_command("get_open_scenes", json!({}))
        .await
        .expect("open scenes");
    assert_eq!(open["active_scene"], "res://main.tscn", "{open}");
    let pong = bridge.send_command("ping", json!({})).await.expect("ping");
    assert_eq!(pong["starting"], false, "{pong}");

    bridge
        .send_command("__test_hold_startup", json!({"hold_ms": 3000}))
        .await
        .expect("hold");
    let pong = bridge.send_command("ping", json!({})).await.expect("ping");
    assert_eq!(pong["starting"], true, "{pong}");
    expect_error(
        bridge
            .send_command("open_scene", json!({"scene_path": "res://pond.tscn"}))
            .await,
        "still starting",
    );
    let info = bridge
        .send_command("get_project_info", json!({}))
        .await
        .expect("project info while starting");
    assert_eq!(info["main_scene"], "res://main.tscn", "{info}");
    bridge
        .send_command("get_version", json!({}))
        .await
        .expect("version while starting");
    let open = bridge.send_command("get_open_scenes", json!({})).await;
    expect_error(open, "still starting");

    wait_until_started(bridge, &editor.log, "the held editor").await;
    let opened = bridge
        .send_command("open_scene", json!({"scene_path": "res://pond.tscn"}))
        .await
        .expect("open pond once started");
    assert_eq!(opened["active_scene"], "res://pond.tscn", "{opened}");
    sleep(Duration::from_millis(500)).await;
    let open = bridge
        .send_command("get_open_scenes", json!({}))
        .await
        .expect("open scenes");
    assert_eq!(open["active_scene"], "res://pond.tscn", "{open}");
}

/// A scene whose own node `A` is added under an instance that already has a child `A`. Godot
/// renames the incoming node while loading and reports it only in its "Load Errors" dialog.
const CLASH_SUB: &str = "[gd_scene format=3]\n\n[node name=\"Sub\" type=\"Node2D\"]\n\n[node name=\"A\" type=\"Node\" parent=\".\"]\n";
const CLASH_SCENE: &str = "[gd_scene load_steps=2 format=3]\n\n[ext_resource type=\"PackedScene\" path=\"res://sub.tscn\" id=\"1\"]\n\n[node name=\"Clash\" type=\"Node2D\"]\n\n[node name=\"Sub\" parent=\".\" instance=ExtResource(\"1\")]\n\n[node name=\"A\" type=\"Node\" parent=\"Sub\"]\n";

#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn open_scene_returns_what_the_load_errors_dialog_shows() {
    // `--language` sets the editor's language from Godot 4.6; a 4.5 editor keeps the language
    // of this machine's settings, so only the parts no translation changes are checked there.
    let editor = start_editor_with_args(
        None,
        "",
        &[("sub.tscn", CLASH_SUB), ("clash.tscn", CLASH_SCENE)],
        "",
        &["--language", "en"],
    )
    .await;
    let bridge = &editor.bridge;
    let version = bridge
        .send_command("get_version", json!({}))
        .await
        .expect("version");
    let english = version["minor"].as_u64().expect("minor") >= 6;

    let clean = bridge
        .send_command("open_scene", json!({"scene_path": "res://main.tscn"}))
        .await
        .expect("open main");
    assert_eq!(clean["load_errors"], json!([]), "{clean}");
    assert!(
        clean
            .get("unsaved")
            .is_none_or(|unsaved| unsaved.as_bool() == Some(false)),
        "{clean}"
    );
    assert!(clean.get("dialog").is_none(), "{clean}");
    assert!(clean.get("warning").is_none(), "{clean}");

    let clash = bridge
        .send_command("open_scene", json!({"scene_path": "res://clash.tscn"}))
        .await
        .expect("open clash");
    assert_eq!(clash["opened"], true, "{clash}");
    let errors = clash["load_errors"].as_array().expect("load_errors");
    assert_eq!(errors.len(), 1, "{clash}");
    let message = errors[0].as_str().expect("message");
    assert!(message.contains("Sub/A"), "{clash}");
    let title = clash["dialog"].as_str().expect("dialog title");
    assert!(!title.is_empty(), "{clash}");
    if english {
        assert!(
            message.starts_with("An incoming node's name clashes with Sub/A"),
            "{clash}"
        );
        assert!(
            message.ends_with("Please fix and re-save the scene."),
            "{clash}"
        );
        assert_eq!(title, "Load Errors", "{clash}");
    }
    assert!(
        clash
            .get("unsaved")
            .is_none_or(|unsaved| unsaved.as_bool() == Some(true)),
        "{clash}"
    );
    assert!(
        clash["warning"]
            .as_str()
            .is_some_and(|w| w.contains("reload")),
        "{clash}"
    );
    let tree = bridge
        .send_command("get_scene_tree", json!({}))
        .await
        .expect("clash tree");
    let sub = find(&tree["tree"], "Sub").expect("Sub");
    assert_eq!(child_names(sub), ["A", "A2"], "{tree}");

    // The dialog stays open until the person closes it. What it already showed is not
    // reported again for a scene that loads cleanly.
    let again = bridge
        .send_command("open_scene", json!({"scene_path": "res://main.tscn"}))
        .await
        .expect("back to main");
    assert_eq!(again["load_errors"], json!([]), "{again}");
    assert!(again.get("dialog").is_none(), "{again}");

    // A reload adds its messages to the open dialog; only the new ones are returned.
    let reloaded = bridge
        .send_command(
            "open_scene",
            json!({"scene_path": "res://clash.tscn", "reload": true}),
        )
        .await
        .expect("reload clash");
    assert_eq!(reloaded["reloaded"], true, "{reloaded}");
    assert_eq!(
        reloaded["load_errors"].as_array().map(Vec::len),
        Some(1),
        "{reloaded}"
    );
    assert_eq!(reloaded["dialog"], clash["dialog"], "{reloaded}");
}

/// Turns on the script editor's rewrites on save, the way a person's editor settings may have
/// them, so a test does not depend on the settings of the machine it runs on.
const SAVE_REWRITE_HOOK: &str = r#"        "__test_rewrites_on_save":
            var settings: EditorSettings = EditorInterface.get_editor_settings()
            for name: String in ["text_editor/behavior/files/convert_indent_on_save", "text_editor/behavior/files/trim_trailing_whitespace_on_save", "text_editor/behavior/files/trim_final_newlines_on_save"]:
                if settings.has_setting(name):
                    settings.set_setting(name, true)
            settings.set_setting("text_editor/behavior/indent/type", 0)
            send_success(peer, cmd_id, {"on": true})
        "__test_convert_indent_on_save":
            send_success(peer, cmd_id, {"on": EditorInterface.get_editor_settings().get_setting("text_editor/behavior/files/convert_indent_on_save")})
"#;

/// Saving a scene saves the script editor's tabs as well, and the editor's convert-indent and
/// trim settings rewrote every open script on the way, even one with no unsaved changes. The
/// save leaves such a script alone and names any script the editor did write.
#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn save_scene_leaves_open_scripts_alone_and_names_the_ones_it_saved() {
    let spaces = "extends Node\n\nfunc f() -> void:\n    if true:\n        pass \n\n\n";
    let typed = "extends Node\n";
    let hook = format!("{SAVE_REWRITE_HOOK}{SCRIPT_PROBE_HOOK}");
    let editor = start_editor_with(
        Some(&hook),
        SCRIPT_PROBE_HELPERS,
        &[("spaces.gd", spaces), ("typed.gd", typed)],
        "",
    )
    .await;
    editor
        .bridge
        .send_command("__test_rewrites_on_save", json!({}))
        .await
        .expect("turn on rewrites");
    for path in ["res://spaces.gd", "res://typed.gd"] {
        editor
            .bridge
            .send_command("__probe_open", json!({"path": path}))
            .await
            .expect("open script");
    }
    sleep(Duration::from_millis(500)).await;
    editor
        .bridge
        .send_command("open_scene", json!({"scene_path": "res://main.tscn"}))
        .await
        .expect("open scene");
    editor
        .bridge
        .send_command(
            "set_node_properties",
            json!({"node_path": "Player", "properties": {"position": "Vector2(9, 9)"}}),
        )
        .await
        .expect("change scene");

    let saved = editor
        .bridge
        .send_command("save_scene", json!({}))
        .await
        .expect("save");
    assert_eq!(saved["saved"], true);
    assert_eq!(saved["scene_path"], "res://main.tscn");
    assert_eq!(saved["scripts_saved"], json!([]), "{saved}");
    assert!(
        std::fs::read_to_string(editor.project.join("main.tscn"))
            .expect("scene")
            .contains("Vector2(9, 9)"),
        "the scene itself was not saved"
    );
    assert_eq!(
        std::fs::read_to_string(editor.project.join("spaces.gd")).expect("spaces.gd"),
        spaces,
        "a clean open script was rewritten by the scene save"
    );

    // Unsaved typing in a script tab is saved with the scene, as the editor's own save does,
    // and the answer says so.
    editor
        .bridge
        .send_command(
            "__probe_type",
            json!({"path": "res://typed.gd", "text": "# typed\n"}),
        )
        .await
        .expect("type");
    let saved = editor
        .bridge
        .send_command("save_scene", json!({}))
        .await
        .expect("save again");
    assert_eq!(saved["scripts_saved"], json!(["res://typed.gd"]), "{saved}");
    assert_eq!(
        std::fs::read_to_string(editor.project.join("typed.gd")).expect("typed.gd"),
        format!("# typed\n{typed}")
    );
    assert_eq!(
        std::fs::read_to_string(editor.project.join("spaces.gd")).expect("spaces.gd"),
        spaces,
        "the second save rewrote the clean script"
    );
    assert_eq!(
        editor
            .bridge
            .send_command("__probe_state", json!({"path": "res://spaces.gd"}))
            .await
            .expect("state")["text"],
        spaces,
        "the editor's text no longer matches the file"
    );
    assert_eq!(
        editor
            .bridge
            .send_command("__test_convert_indent_on_save", json!({}))
            .await
            .expect("setting")["on"],
        true,
        "the person's editor setting was not restored"
    );
}

/// Test-only command that opens a script tab and reports what the editor's file system and tabs
/// hold for a path.
const DELETE_PROBE_HOOK: &str = r#"        "__test_delete_probe":
            var path: String = str(params.get("path", ""))
            if bool(params.get("open_script", false)):
                EditorInterface.edit_script(load(path))
            var scripts: Array = EditorInterface.get_script_editor().get_open_scripts().map(func(s: Script) -> String: return s.resource_path)
            send_success(peer, cmd_id, {"in_filesystem": _filesystem_has(EditorInterface.get_resource_filesystem(), path), "script_tabs": scripts, "open_scenes": EditorInterface.get_open_scenes()})
"#;

/// godot_delete_file through the editor: a referenced script and a scene with unsaved changes are
/// refused; a scene that a script only saves is deleted without force; a script with its .uid, an imported image with its .import and imported data, and an
/// open scene are deleted, their tabs closed and the FileSystem dock updated. A reference from a
/// nested project, which Godot does not scan, does not count.
#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn files_are_deleted_through_the_editor_with_companions_and_tabs() {
    use godot_mcp::cli_runner::CliRunner;
    use godot_mcp::tools::ToolManager;
    use std::sync::Arc;

    let simple_scene = "[gd_scene format=3]\n\n[node name=\"Root\" type=\"Node2D\"]\n";
    let hook = format!("{DELETE_PROBE_HOOK}{UNDO_HOOK}");
    let editor = start_editor_with(
        Some(&hook),
        "",
        &[
            ("used.gd", "extends Node\n"),
            (
                "uses.tscn",
                "[gd_scene format=3]\n\n[ext_resource type=\"Script\" path=\"res://used.gd\" id=\"1\"]\n\n[node name=\"Uses\" type=\"Node\"]\nscript = ExtResource(\"1\")\n",
            ),
            ("old.gd", "extends Node\n"),
            ("nested/project.godot", "config_version=5\n"),
            (
                "nested/uses_old.tscn",
                "[gd_scene format=3]\n\n[ext_resource type=\"Script\" path=\"res://old.gd\" id=\"1\"]\n\n[node name=\"Uses\" type=\"Node\"]\nscript = ExtResource(\"1\")\n",
            ),
            ("old_scene.tscn", simple_scene),
            ("dirty.tscn", simple_scene),
            ("generated.tscn", simple_scene),
            (
                "tools/generate.gd",
                "extends RefCounted\n\nconst OUT := \"res://generated.tscn\"\n\nfunc run() -> void:\n\tResourceSaver.save(PackedScene.new(), OUT)\n",
            ),
            (
                "icon.svg",
                "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"16\" height=\"16\"><rect width=\"16\" height=\"16\" fill=\"#3a3\"/></svg>\n",
            ),
        ],
        "",
    )
    .await;
    let project = editor.project.clone();
    let (godot, _) = godot_binaries();
    let tools = Arc::new(
        ToolManager::new(BridgeClient::new(editor.port), CliRunner::new(godot))
            .with_working_dir(project.clone()),
    );
    let delete = |args: Value| {
        let tools = Arc::clone(&tools);
        async move { tools.call_tool("godot_delete_file", args).await }
    };
    let probe = |path: &str, open_script: bool| {
        let bridge = editor.bridge.clone();
        let params = json!({"path": path, "open_script": open_script});
        async move {
            bridge
                .send_command("__test_delete_probe", params)
                .await
                .expect("probe")
        }
    };

    assert!(
        project.join("old.gd.uid").is_file(),
        "the import gives scripts a .uid"
    );
    let import = std::fs::read_to_string(project.join("icon.svg.import")).expect("icon.svg.import");
    let imported: Vec<PathBuf> = import
        .split('"')
        .filter_map(|part| part.strip_prefix("res://"))
        .filter(|part| part.starts_with(".godot/imported/"))
        .map(|part| project.join(part))
        .collect();
    assert!(
        !imported.is_empty() && imported.iter().all(|path| path.is_file()),
        "{import}"
    );

    // A script a scene still uses is refused, and nothing changes.
    let refused = delete(json!({"paths": ["res://used.gd"], "trash": false})).await;
    assert!(refused.is_error, "{}", tool_text(&refused));
    assert!(
        tool_text(&refused).contains("res://uses.tscn"),
        "{}",
        tool_text(&refused)
    );
    assert!(project.join("used.gd").is_file());

    // A scene a script only saves is deleted without force, and the answer names the writer.
    let generated = delete(json!({"paths": ["res://generated.tscn"], "trash": false})).await;
    assert!(!generated.is_error, "{}", tool_text(&generated));
    let report: Value = serde_json::from_str(&tool_text(&generated)).expect("JSON report");
    assert_eq!(
        report["references"][0]["referenced_by"][0]["kind"], "written",
        "{report}"
    );
    assert!(
        report["warnings"]
            .to_string()
            .contains("res://tools/generate.gd"),
        "{report}"
    );
    assert!(!project.join("generated.tscn").exists());

    // A scene with unsaved changes is refused.
    editor
        .bridge
        .send_command("open_scene", json!({"scene_path": "res://dirty.tscn"}))
        .await
        .expect("open dirty.tscn");
    editor
        .bridge
        .send_command("__test_hand_edit", json!({}))
        .await
        .expect("hand edit");
    let unsaved = delete(json!({"paths": ["res://dirty.tscn"], "trash": false})).await;
    assert!(unsaved.is_error, "{}", tool_text(&unsaved));
    assert!(
        tool_text(&unsaved).contains("unsaved changes"),
        "{}",
        tool_text(&unsaved)
    );
    assert!(project.join("dirty.tscn").is_file());

    editor
        .bridge
        .send_command("open_scene", json!({"scene_path": "res://old_scene.tscn"}))
        .await
        .expect("open old_scene.tscn");
    let before = probe("res://old.gd", true).await;
    assert!(
        before["script_tabs"].to_string().contains("res://old.gd"),
        "{before}"
    );
    assert!(before["in_filesystem"] == true, "{before}");

    let deleted = delete(json!({
        "paths": ["res://old.gd", "res://icon.svg", "res://old_scene.tscn"],
        "trash": false
    }))
    .await;
    assert!(!deleted.is_error, "{}", tool_text(&deleted));
    let report: Value = serde_json::from_str(&tool_text(&deleted)).expect("JSON report");
    assert_eq!(report["source"], "editor", "{report}");
    assert_eq!(
        report["deleted"].as_array().map(Vec::len),
        Some(3),
        "{report}"
    );
    assert_eq!(
        report["deleted"][0]["companions"],
        json!(["res://old.gd.uid"]),
        "{report}"
    );
    let icon_companions = report["deleted"][1]["companions"].to_string();
    assert!(
        icon_companions.contains("res://icon.svg.import"),
        "{report}"
    );
    assert!(
        icon_companions.contains("res://.godot/imported/"),
        "{report}"
    );
    let closed = report["closed_tabs"].to_string();
    assert!(closed.contains("res://old_scene.tscn"), "{report}");
    assert!(closed.contains("res://old.gd"), "{report}");
    for gone in [
        "old.gd",
        "old.gd.uid",
        "icon.svg",
        "icon.svg.import",
        "old_scene.tscn",
    ] {
        assert!(
            !project.join(gone).exists(),
            "{gone} still exists: {report}"
        );
    }
    for path in &imported {
        assert!(!path.exists(), "{} still exists", path.display());
    }
    for path in ["res://old.gd", "res://icon.svg", "res://old_scene.tscn"] {
        let after = probe(path, false).await;
        assert!(after["in_filesystem"] == false, "{path}: {after}");
        assert!(
            !after["open_scenes"].to_string().contains(path),
            "{path}: {after}"
        );
        assert!(
            !after["script_tabs"].to_string().contains(path),
            "{path}: {after}"
        );
    }
    assert!(project.join("used.gd").is_file() && project.join("dirty.tscn").is_file());
}

/// Test commands for the move: save a binary scene whose node uses a script, and report what the
/// editor knows about a path (file system, open scene and script tabs, a setting, a uid's path).
const MOVE_PROBE_HOOK: &str = r#"        "__test_save_binary":
            var holder := Node.new()
            holder.name = "Holder"
            holder.set_script(load(str(params.get("script", ""))))
            var packed := PackedScene.new()
            packed.pack(holder)
            holder.free()
            var target: String = str(params.get("path", ""))
            var saved: Error = ResourceSaver.save(packed, target)
            EditorInterface.get_resource_filesystem().update_file(target)
            send_success(peer, cmd_id, {"saved": saved == OK})
        "__test_move_probe":
            var path: String = str(params.get("path", ""))
            if bool(params.get("open_script", false)):
                EditorInterface.edit_script(load(path))
            var scripts: Array = EditorInterface.get_script_editor().get_open_scripts().map(func(s: Script) -> String: return s.resource_path)
            var uid_text: String = str(params.get("uid", ""))
            var uid_path: String = ""
            if not uid_text.is_empty() and ResourceUID.has_id(ResourceUID.text_to_id(uid_text)):
                uid_path = ResourceUID.get_id_path(ResourceUID.text_to_id(uid_text))
            send_success(peer, cmd_id, {"in_filesystem": _filesystem_has(EditorInterface.get_resource_filesystem(), path), "script_tabs": scripts, "open_scenes": EditorInterface.get_open_scenes(), "setting": str(ProjectSettings.get_setting(str(params.get("setting", "application/config/name")), "")), "uid_path": uid_path})
"#;

/// The editor's part of a move refuses a destination name Godot cannot store in its text files
/// with the same rule the server applies (`move_name_problem` in src/paths.rs), so a server that
/// skipped its check still could not have the editor take part in such a move.
#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn the_editor_refuses_the_destination_names_the_server_refuses() {
    let editor = start_editor(false).await;
    let cases = [
        ("res://a.gd", "res://b.gd"),
        ("res://a.gd", "res://Σκηνές και κώδικας/my script.gd"),
        ("res://a.gd", "res://b\".gd"),
        (
            "res://a.gd",
            "res://b\n[autoload]\nEvil=\"*res://evil.gd\".gd",
        ),
        ("res://a.gd", "res://b\r.gd"),
        ("res://a.gd", "res://b\t.gd"),
        ("res://a.gd", "res://dir\u{1}/b.gd"),
        ("res://a.gd", "res://b\u{7f}.gd"),
        ("res://a.gd", "res://b\u{85}.gd"),
        ("res://a.gd", "res://b\\c.gd"),
        ("res://a.gd", "res://b:c.gd"),
        ("res://a.gd", "res://b?.gd"),
        ("res://a.gd", "res://b*.gd"),
        ("res://a.gd", "res://b|.gd"),
        ("res://a.gd", "res://b<.gd"),
        ("res://a.gd", "res://b>.gd"),
        ("res://a.gd", "res://b%20.gd"),
        ("res://a.gd", "res:// b.gd"),
        ("res://a.gd", "res://b.gd "),
        ("res://a.gd", "res://b#;[]'=.gd"),
        ("res://odd%dir/a.gd", "res://odd%dir/b.gd"),
        ("res://in/odd%name.gd", "res://out/odd%name.gd"),
        ("res://lv1/odd%name.png", "res://lv\"2/odd%name.png"),
    ];
    for (from, to) in cases {
        let check = editor
            .bridge
            .send_command(
                "move_files_check",
                json!({"files": [{"from": from, "to": to}], "rewrite": [], "binary": []}),
            )
            .await
            .expect("move_files_check");
        let refused = check["refused"].as_array().expect("refused list");
        let server = godot_mcp::paths::move_name_problem(from, to);
        assert_eq!(
            !refused.is_empty(),
            server.is_some(),
            "{from:?} -> {to:?}: editor {check}, server {server:?}"
        );
        if server.is_some() {
            assert_eq!(refused[0]["path"], from, "{check}");
            assert!(
                refused[0]["reason"]
                    .as_str()
                    .is_some_and(|reason| reason.contains("cannot be used in a file name")),
                "{from:?} -> {to:?}: {check}"
            );
        }
    }
}

/// godot_move_file through the editor: a scene with unsaved changes is refused; a script and a
/// folder (a scene, its script and an imported image) move with their companions; the text
/// references are rewritten, a binary scene that uses the moved script is saved again by the
/// editor with the new path, the open scene and script tabs follow the new paths, the autoload in
/// the editor's project settings is rewritten, the uid points at the new path, and the image is
/// not imported again.
#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn files_and_folders_are_moved_through_the_editor_with_references_and_tabs() {
    use godot_mcp::cli_runner::CliRunner;
    use godot_mcp::tools::ToolManager;
    use std::sync::Arc;

    let simple_scene = "[gd_scene format=3]\n\n[node name=\"Root\" type=\"Node2D\"]\n";
    let hook = format!("{MOVE_PROBE_HOOK}{UNDO_HOOK}");
    let editor = start_editor_with(
        Some(&hook),
        "",
        &[
            ("scripts/player.gd", "extends Node\n"),
            ("levels/level_01/enemy.gd", "extends Node\n\nconst ICON := preload(\"res://levels/level_01/icon.svg\")\n"),
            (
                "levels/level_01/level.tscn",
                "[gd_scene format=3 uid=\"uid://e2elevelone\"]\n\n[ext_resource type=\"Script\" path=\"res://levels/level_01/enemy.gd\" id=\"1\"]\n[ext_resource type=\"Script\" path=\"res://scripts/player.gd\" id=\"2\"]\n\n[node name=\"Level\" type=\"Node\"]\nscript = ExtResource(\"1\")\n\n[node name=\"Player\" type=\"Node\" parent=\".\"]\nscript = ExtResource(\"2\")\n",
            ),
            (
                "levels/level_01/icon.svg",
                "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"16\" height=\"16\"><rect width=\"16\" height=\"16\" fill=\"#3a3\"/></svg>\n",
            ),
            (
                "uses_level.tscn",
                "[gd_scene format=3]\n\n[ext_resource type=\"PackedScene\" path=\"res://levels/level_01/level.tscn\" id=\"1\"]\n\n[node name=\"Uses\" type=\"Node\"]\n\n[node name=\"Level\" parent=\".\" instance=ExtResource(\"1\")]\n",
            ),
            ("dirty.tscn", simple_scene),
        ],
        "\n[autoload]\n\nPlayer=\"*res://scripts/player.gd\"\n",
    )
    .await;
    let project = editor.project.clone();
    let (godot, _) = godot_binaries();
    let tools = Arc::new(
        ToolManager::new(BridgeClient::new(editor.port), CliRunner::new(godot))
            .with_working_dir(project.clone()),
    );
    let mv = |args: Value| {
        let tools = Arc::clone(&tools);
        async move { tools.call_tool("godot_move_file", args).await }
    };
    let send = |command: &'static str, params: Value| {
        let bridge = editor.bridge.clone();
        async move { bridge.send_command(command, params).await.expect(command) }
    };
    let read = |name: &str| std::fs::read_to_string(project.join(name)).expect("readable file");

    send(
        "__test_save_binary",
        json!({"path": "res://levels/holder.scn", "script": "res://scripts/player.gd"}),
    )
    .await;
    let holder = std::fs::read(project.join("levels/holder.scn")).expect("holder.scn");
    assert!(
        String::from_utf8_lossy(&holder).contains("res://scripts/player.gd"),
        "the binary scene names the script"
    );

    // A scene with unsaved changes is refused, and nothing moves.
    send("open_scene", json!({"scene_path": "res://dirty.tscn"})).await;
    send("__test_hand_edit", json!({})).await;
    let unsaved =
        mv(json!({"moves": [{"from": "res://dirty.tscn", "to": "res://old/dirty.tscn"}]})).await;
    assert!(unsaved.is_error, "{}", tool_text(&unsaved));
    assert!(
        tool_text(&unsaved).contains("unsaved changes"),
        "{}",
        tool_text(&unsaved)
    );
    assert!(project.join("dirty.tscn").is_file() && !project.join("old").exists());

    send(
        "open_scene",
        json!({"scene_path": "res://levels/level_01/level.tscn"}),
    )
    .await;
    send(
        "__test_move_probe",
        json!({"path": "res://scripts/player.gd", "open_script": true}),
    )
    .await;
    let moves = json!([
        {"from": "res://scripts/player.gd", "to": "res://scripts/actors/player.gd"},
        {"from": "res://levels/level_01", "to": "res://legacy/"},
    ]);
    let dry = mv(json!({"moves": moves, "dry_run": true})).await;
    assert!(!dry.is_error, "{}", tool_text(&dry));
    assert!(project.join("scripts/player.gd").is_file());

    let moved = mv(json!({"moves": moves})).await;
    assert!(!moved.is_error, "{}", tool_text(&moved));
    let report: Value = serde_json::from_str(&tool_text(&moved)).expect("JSON report");
    assert_eq!(report["source"], "editor", "{report}");
    assert_eq!(
        report["resaved"],
        json!(["res://levels/holder.scn"]),
        "{report}"
    );
    assert!(
        report["reloaded_scenes"]
            .to_string()
            .contains("res://legacy/level_01/level.tscn"),
        "{report}"
    );
    assert!(
        report["project_settings"]
            .to_string()
            .contains("autoload/Player"),
        "{report}"
    );
    assert_eq!(report["rescan_needed"], false, "{report}");
    assert!(read("scripts/actors/player.gd.uid").starts_with("uid://"));
    assert!(
        !project.join("scripts/player.gd").exists() && !project.join("levels/level_01").exists()
    );
    let level = read("legacy/level_01/level.tscn");
    assert!(
        level.contains("path=\"res://legacy/level_01/enemy.gd\""),
        "{level}"
    );
    assert!(
        level.contains("path=\"res://scripts/actors/player.gd\""),
        "{level}"
    );
    assert!(read("uses_level.tscn").contains("res://legacy/level_01/level.tscn"));
    assert!(
        read("legacy/level_01/enemy.gd").contains("preload(\"res://legacy/level_01/icon.svg\")")
    );
    assert!(read("project.godot").contains("Player=\"*res://scripts/actors/player.gd\""));
    let holder = String::from_utf8_lossy(
        &std::fs::read(project.join("levels/holder.scn")).expect("holder.scn"),
    )
    .into_owned();
    assert!(
        holder.contains("res://scripts/actors/player.gd"),
        "the editor saved the binary scene again"
    );
    assert!(!holder.contains("res://scripts/player.gd"), "{holder}");

    // After the editor's scan: the moved files are where the editor looks, and nothing was
    // imported again (the imported data was renamed for the new path).
    let import_before = read("legacy/level_01/icon.svg.import");
    send("resave_resources", json!({"wait": true})).await;
    let probe = send(
        "__test_move_probe",
        json!({"path": "res://legacy/level_01/level.tscn", "setting": "autoload/Player", "uid": "uid://e2elevelone"}),
    )
    .await;
    assert_eq!(probe["in_filesystem"], true, "{probe}");
    assert!(
        probe["open_scenes"]
            .to_string()
            .contains("res://legacy/level_01/level.tscn"),
        "{probe}"
    );
    assert!(
        !probe["open_scenes"]
            .to_string()
            .contains("res://levels/level_01/"),
        "{probe}"
    );
    assert!(
        probe["script_tabs"]
            .to_string()
            .contains("res://scripts/actors/player.gd"),
        "{probe}"
    );
    assert_eq!(
        probe["setting"], "*res://scripts/actors/player.gd",
        "{probe}"
    );
    assert_eq!(
        probe["uid_path"], "res://legacy/level_01/level.tscn",
        "{probe}"
    );
    let import = read("legacy/level_01/icon.svg.import");
    assert_eq!(import, import_before, "the image was imported again");
    assert!(
        import.contains("source_file=\"res://legacy/level_01/icon.svg\""),
        "{import}"
    );
    for part in import
        .split('"')
        .filter(|part| part.starts_with("res://.godot/imported/"))
    {
        assert!(
            project.join(&part["res://".len()..]).is_file(),
            "{part} is missing"
        );
    }
    assert!(project.join("dirty.tscn").is_file());
}

/// Moving or deleting files through the editor never loses what a person typed in the shader
/// editor and did not save: the call is refused and nothing changes. A saved shader tab whose file
/// a move rewrites shows the new text. The move commands refuse a path that leaves the project.
#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn moves_and_deletes_respect_the_shader_editor_and_stay_in_the_project() {
    use godot_mcp::cli_runner::CliRunner;
    use godot_mcp::tools::ToolManager;
    use std::sync::Arc;

    let include = "float wave(float x) {
	return sin(x);
}
";
    let water = "shader_type canvas_item;

#include \"res://fx/common.gdshaderinc\"

void fragment() {
	COLOR = vec4(wave(1.0));
}
";
    let typed = "shader_type canvas_item;

void fragment() {
	COLOR = vec4(1.0);
}
";
    let outer = "#include \"res://fx/common.gdshaderinc\"

float twice(float x) {
	return wave(x) * 2.0;
}
";
    let editor = start_editor_with(
        Some(SHADER_PROBE_HOOK),
        "",
        &[
            ("fx/common.gdshaderinc", include),
            ("fx/outer.gdshaderinc", outer),
            ("water.gdshader", water),
            ("typed.gdshader", typed),
        ],
        "",
    )
    .await;
    let project = editor.project.clone();
    let (godot, _) = godot_binaries();
    let tools = Arc::new(
        ToolManager::new(BridgeClient::new(editor.port), CliRunner::new(godot))
            .with_working_dir(project.clone()),
    );
    let call = |tool: &'static str, args: Value| {
        let tools = Arc::clone(&tools);
        async move { tools.call_tool(tool, args).await }
    };
    let send = |command: &'static str, params: Value| {
        let bridge = editor.bridge.clone();
        async move { bridge.send_command(command, params).await }
    };
    let read = |name: &str| std::fs::read_to_string(project.join(name)).expect("readable file");

    // An edit opens each shader in the shader editor; one of them then gets unsaved typing.
    for (path, old, new) in [
        ("res://fx/outer.gdshaderinc", "* 2.0", "* 3.0"),
        ("res://water.gdshader", "wave(1.0)", "wave(2.0)"),
        ("res://typed.gdshader", "vec4(1.0)", "vec4(0.5)"),
    ] {
        send(
            "edit_shader",
            json!({"shader_path": path, "old_text": old, "new_text": new}),
        )
        .await
        .expect("open the shader by editing it");
    }
    send(
        "__shader_type",
        json!({"path": "res://typed.gdshader", "text": "// typed
"}),
    )
    .await
    .expect("type");
    let saved = read("typed.gdshader");

    let moved = call(
        "godot_move_file",
        json!({"moves": [{"from": "res://typed.gdshader", "to": "res://fx/typed.gdshader"}]}),
    )
    .await;
    assert!(moved.is_error, "{}", tool_text(&moved));
    assert!(
        tool_text(&moved).contains("unsaved changes in the shader editor"),
        "{}",
        tool_text(&moved)
    );
    let deleted = call(
        "godot_delete_file",
        json!({"paths": ["res://typed.gdshader"], "trash": false}),
    )
    .await;
    assert!(deleted.is_error, "{}", tool_text(&deleted));
    assert!(
        tool_text(&deleted).contains("unsaved changes in the shader editor"),
        "{}",
        tool_text(&deleted)
    );
    assert_eq!(read("typed.gdshader"), saved, "nothing moved or deleted");
    assert!(!project.join("fx/typed.gdshader").exists());

    // Unsaved typing in a file the move would only rewrite, a shader or an include, refuses too.
    let include_move = json!({"moves": [{"from": "res://fx/common.gdshaderinc", "to": "res://lib/common.gdshaderinc"}]});
    for path in ["res://water.gdshader", "res://fx/outer.gdshaderinc"] {
        send(
            "__shader_type",
            json!({"path": path, "text": "// typed
"}),
        )
        .await
        .expect("type into a file the move rewrites");
        let refused = call("godot_move_file", include_move.clone()).await;
        assert!(refused.is_error, "{path}: {}", tool_text(&refused));
        assert!(
            tool_text(&refused).contains("unsaved changes in the shader editor"),
            "{path}: {}",
            tool_text(&refused)
        );
        assert!(
            project.join("fx/common.gdshaderinc").is_file(),
            "nothing moved"
        );
        send("__shader_undo", json!({"path": path}))
            .await
            .expect("undo the typing");
    }

    // Moving the include rewrites the saved files that name it, and their tabs follow.
    let moved = call("godot_move_file", include_move.clone()).await;
    assert!(!moved.is_error, "{}", tool_text(&moved));
    let rewritten = read("water.gdshader");
    assert!(
        rewritten.contains("res://lib/common.gdshaderinc"),
        "{rewritten}"
    );
    let shown = send("__shader_state", json!({"path": "res://water.gdshader"}))
        .await
        .expect("water state");
    assert_eq!(shown["text"], rewritten, "{shown}");
    assert_eq!(shown["memory_code"], rewritten, "{shown}");
    assert_eq!(shown["dirty"], false, "{shown}");
    let included = read("fx/outer.gdshaderinc");
    assert!(
        included.contains("res://lib/common.gdshaderinc"),
        "{included}"
    );
    let shown = send(
        "__shader_state",
        json!({"path": "res://fx/outer.gdshaderinc"}),
    )
    .await
    .expect("outer state");
    assert_eq!(shown["text"], included, "{shown}");
    assert_eq!(shown["dirty"], false, "{shown}");

    let outside = json!([{"from": "res://../outside.txt", "to": "res://inside.txt"}]);
    expect_error(
        send(
            "move_files_check",
            json!({"files": outside, "rewrite": [], "binary": []}),
        )
        .await,
        "inside the project",
    );
    expect_error(
        send(
            "move_files_apply",
            json!({"files": [], "folders": [], "rewritten": [], "binary": ["res://a/../../outside.res"], "pairs": []}),
        )
        .await,
        "inside the project",
    );
}

/// Test-only command that lists the text of every shader tab the editor holds.
const SHADER_TABS_HOOK: &str = r#"        "__shader_tabs":
            var texts: Array = []
            for tab: Node in EditorInterface.get_base_control().find_children("*", "ShaderTextEditor", true, false):
                if not tab.is_queued_for_deletion():
                    texts.append(_shader_ops()._buffer(tab).text)
            send_success(peer, cmd_id, {"texts": texts})
        "ping":
"#;

/// A shader deleted through the editor leaves no tab behind that a later save could write the
/// file back from, and the tab of another shader stays.
#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn deleting_an_open_shader_closes_its_tab() {
    use godot_mcp::cli_runner::CliRunner;
    use godot_mcp::tools::ToolManager;

    let gone = "shader_type canvas_item;

void fragment() {
	COLOR = vec4(0.1);
}
";
    let kept = "shader_type canvas_item;

void fragment() {
	COLOR = vec4(0.2);
}
";
    let editor = start_editor_with(
        Some(SHADER_TABS_HOOK),
        "",
        &[("gone.gdshader", gone), ("kept.gdshader", kept)],
        "",
    )
    .await;
    let project = editor.project.clone();
    let (godot, _) = godot_binaries();
    let tools = std::sync::Arc::new(
        ToolManager::new(BridgeClient::new(editor.port), CliRunner::new(godot))
            .with_working_dir(project.clone()),
    );
    let send = |command: &'static str, params: Value| {
        let bridge = editor.bridge.clone();
        async move { bridge.send_command(command, params).await.expect(command) }
    };

    send("open_scene", json!({"scene_path": "res://main.tscn"})).await;
    for (path, old, new) in [
        ("res://kept.gdshader", "vec4(0.2)", "vec4(0.4)"),
        ("res://gone.gdshader", "vec4(0.1)", "vec4(0.3)"),
    ] {
        send(
            "edit_shader",
            json!({"shader_path": path, "old_text": old, "new_text": new}),
        )
        .await;
    }
    let before = send("__shader_tabs", json!({})).await;
    assert_eq!(
        before["texts"].as_array().map(Vec::len),
        Some(2),
        "{before}"
    );

    let deleted = tools
        .call_tool(
            "godot_delete_file",
            json!({"paths": ["res://gone.gdshader"], "trash": false}),
        )
        .await;
    assert!(!deleted.is_error, "{}", tool_text(&deleted));
    // A save writes only tabs with unsaved text; the tab check below is what proves nothing
    // can write the file back.
    send("save_scene", json!({})).await;
    sleep(Duration::from_secs(1)).await;
    let after = send("__shader_tabs", json!({})).await;
    assert!(
        !project.join("gone.gdshader").exists(),
        "a save wrote the deleted shader back: {after}"
    );
    let texts = after["texts"].to_string();
    assert!(
        !texts.contains("vec4(0.3)"),
        "its tab is still open: {after}"
    );
    assert!(texts.contains("vec4(0.4)"), "the other tab stays: {after}");
}

#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn set_node_properties_adds_metadata_the_way_the_inspector_does() {
    let editor = start_editor(true).await;
    let bridge = &editor.bridge;
    let call = |command: &'static str, params: Value| async move {
        bridge.send_command(command, params).await
    };
    let props = || async {
        call("get_node_properties", json!({"node_path": "Player"}))
            .await
            .expect("props")["properties"]
            .clone()
    };
    call("open_scene", json!({"scene_path": "res://main.tscn"}))
        .await
        .expect("open main");

    // A metadata entry the node does not have yet is created, with the JSON value's own type.
    let set = call(
        "set_node_properties",
        json!({"node_path": "Player", "properties": {
            "metadata/catch_hint": "text",
            "metadata/count": 3,
            "metadata/ratio": 1.5,
            "metadata/tags": ["a", "b"],
            "position": [1, 2]
        }}),
    )
    .await
    .expect("add metadata");
    assert_eq!(set["applied"]["metadata/catch_hint"], "\"text\"", "{set}");
    let read = props().await;
    assert_eq!(read["metadata/catch_hint"]["type"], "String", "{read}");
    assert_eq!(read["metadata/count"]["type"], "int", "{read}");
    assert_eq!(read["metadata/ratio"]["type"], "float", "{read}");
    assert_eq!(read["metadata/tags"]["type"], "Array", "{read}");

    // An existing entry keeps its type.
    call(
        "set_node_properties",
        json!({"node_path": "Player", "properties": {"metadata/count": "7"}}),
    )
    .await
    .expect("update metadata");
    assert_eq!(props().await["metadata/count"]["value"], "7");

    // A name Godot refuses, an empty name, and null are rejected without a partial change.
    for (key, value, needle) in [
        ("metadata/1bad", json!(1), "identifier"),
        ("metadata/", json!(1), "identifier"),
        ("metadata/fresh", Value::Null, "null"),
    ] {
        expect_error(
            call(
                "set_node_properties",
                json!({"node_path": "Player", "properties": {key: value, "position": [9, 9]}}),
            )
            .await,
            needle,
        );
    }
    let read = props().await;
    assert_eq!(read["position"]["value"], "Vector2(1, 2)", "{read}");
    assert!(read.get("metadata/fresh").is_none(), "{read}");

    // The new entries are saved with the scene and undo removes them.
    call("save_scene", json!({})).await.expect("save");
    let saved = std::fs::read_to_string(editor.project.join("main.tscn")).expect("saved scene");
    assert!(saved.contains("metadata/catch_hint = \"text\""), "{saved}");
    call("__test_undo", json!({})).await.expect("undo update");
    call("__test_undo", json!({})).await.expect("undo add");
    let read = props().await;
    assert!(read.get("metadata/catch_hint").is_none(), "{read}");
    assert!(read.get("metadata/count").is_none(), "{read}");
    assert_eq!(read["position"]["value"], "Vector2(5, 7)", "{read}");
}

/// A bus layout with an SFX bus, the file the editor loads its AudioServer buses from.
const SFX_BUS_LAYOUT: &str = r#"[gd_resource type="AudioBusLayout" format=3]

[resource]
bus/1/name = &"SFX"
bus/1/solo = false
bus/1/mute = false
bus/1/bypass_fx = false
bus/1/volume_db = 0.0
bus/1/send = &"Master"
"#;

#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn property_edits_refuse_a_value_godot_does_not_keep() {
    let editor = start_editor(false).await;
    let bridge = &editor.bridge;
    let call = |command: &'static str, params: Value| async move {
        bridge.send_command(command, params).await
    };
    call("open_scene", json!({"scene_path": "res://main.tscn"}))
        .await
        .expect("open main");

    // The getter answers Master for a bus the editor's AudioServer lacks, and the scene would save
    // Master, so the node is not added and the message names the bus and the layout file.
    for needle in [
        "no bus 'SFX'",
        "Master",
        "default_bus_layout",
        "restart the editor",
    ] {
        expect_error(
            call(
                "add_node",
                json!({"parent_node_path": ".", "node_type": "AudioStreamPlayer",
                       "node_name": "Sound", "properties": {"bus": "SFX"}}),
            )
            .await,
            needle,
        );
    }
    let tree = call("get_scene_tree", json!({})).await.expect("tree");
    assert!(!tree.to_string().contains("\"Sound\""), "{tree}");

    // Values Godot keeps are added and reported back.
    let added = call(
        "add_node",
        json!({"parent_node_path": ".", "node_type": "AudioStreamPlayer",
               "node_name": "Sound", "properties": {"bus": "Master", "volume_db": -6.3}}),
    )
    .await
    .expect("add with Master");
    assert_eq!(added["applied"]["bus"], "&\"Master\"", "{added}");
    assert!(
        added["applied"]["volume_db"]
            .as_str()
            .is_some_and(|v| v.starts_with("-6.3")),
        "{added}"
    );

    // set_node_properties refuses the whole call, including the values that would have stuck.
    let volume = || async {
        call(
            "get_node_properties",
            json!({"node_path": "Sound", "properties": ["volume_db", "pitch_scale"]}),
        )
        .await
        .expect("props")["properties"]
            .clone()
    };
    let before = volume().await;
    expect_error(
        call(
            "set_node_properties",
            json!({"node_path": "Sound", "properties": {"volume_db": -1, "bus": "SFX"}}),
        )
        .await,
        "No properties were changed. bus reads back &\"Master\"",
    );
    // A setter that refuses a value (pitch_scale must be above 0) is refused the same way.
    expect_error(
        call(
            "set_node_properties",
            json!({"node_path": "Sound", "properties": {"pitch_scale": 0}}),
        )
        .await,
        "pitch_scale reads back 1.0 after being set to 0",
    );
    assert_eq!(volume().await, before);

    // Properties that limit each other are checked together, in the order given.
    call(
        "add_node",
        json!({"parent_node_path": "UI", "node_type": "HSlider", "node_name": "Slider"}),
    )
    .await
    .expect("add slider");
    let set = call(
        "set_node_properties",
        json!({"node_path": "UI/Slider", "properties": {"max_value": 500, "value": 300}}),
    )
    .await
    .expect("raise max, then value");
    assert_eq!(set["applied"]["value"], "300.0", "{set}");
    expect_error(
        call(
            "set_node_properties",
            json!({"node_path": "UI/Slider", "properties": {"value": 900}}),
        )
        .await,
        "value reads back 500.0 after being set to 900",
    );
}

#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn audio_bus_from_the_bus_layout_is_kept() {
    let editor =
        start_editor_with(None, "", &[("default_bus_layout.tres", SFX_BUS_LAYOUT)], "").await;
    let bridge = &editor.bridge;
    let call = |command: &'static str, params: Value| async move {
        bridge.send_command(command, params).await
    };
    call("open_scene", json!({"scene_path": "res://main.tscn"}))
        .await
        .expect("open main");
    let added = call(
        "add_node",
        json!({"parent_node_path": ".", "node_type": "AudioStreamPlayer2D",
               "node_name": "Sound", "properties": {"bus": "SFX"}}),
    )
    .await
    .expect("add with SFX");
    assert_eq!(added["applied"]["bus"], "&\"SFX\"", "{added}");
    call("save_scene", json!({})).await.expect("save");
    let saved = std::fs::read_to_string(editor.project.join("main.tscn")).expect("saved scene");
    assert!(saved.contains("bus = &\"SFX\""), "{saved}");
}

/// A node whose exports are typed containers, as a global class so godot_add_node can create it.
const LISTS_SCRIPT: &str = r#"@tool
class_name Lists
extends Node

enum Kind { A, B }

@export var paths: Array[NodePath] = []
@export var names: Array[String] = []
@export var counts: Array[int] = []
@export var kinds: Array[Kind] = []
@export var points: Array[Vector2] = []
@export var mats: Array[Material] = []
@export var tags: Array[Tag] = []
@export var nodes: Array[Node] = []
@export var loose: Array = []
@export var weights: Dictionary[String, int] = {}
var hidden_paths: Array[NodePath] = []
"#;

#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn typed_container_properties_convert_each_element_or_refuse() {
    let editor = start_editor_with(
        None,
        "",
        &[
            ("lists.gd", LISTS_SCRIPT),
            ("tag.gd", "class_name Tag\nextends Resource\n"),
            (
                "mat.tres",
                "[gd_resource type=\"StandardMaterial3D\" format=3]\n\n[resource]\n",
            ),
            (
                "notes.tres",
                "[gd_resource type=\"Resource\" format=3]\n\n[resource]\n",
            ),
            (
                "tag.tres",
                "[gd_resource type=\"Resource\" script_class=\"Tag\" format=3]\n\n\
                 [ext_resource type=\"Script\" path=\"res://tag.gd\" id=\"1_tag\"]\n\n\
                 [resource]\nscript = ExtResource(\"1_tag\")\n",
            ),
        ],
        "",
    )
    .await;
    let bridge = &editor.bridge;
    let call = |command: &'static str, params: Value| async move {
        bridge.send_command(command, params).await
    };
    let props = || async {
        call("get_node_properties", json!({"node_path": "Lists"}))
            .await
            .expect("props")["properties"]
            .clone()
    };
    call("open_scene", json!({"scene_path": "res://main.tscn"}))
        .await
        .expect("open main");

    // godot_add_node refuses an element that does not convert instead of dropping the array.
    expect_error(
        call(
            "add_node",
            json!({"parent_node_path": ".", "node_type": "Lists", "node_name": "Lists",
                   "properties": {"paths": ["../Player"], "counts": [1, "many"]}}),
        )
        .await,
        "counts: element 1",
    );
    let tree = call("get_scene_tree", json!({})).await.expect("tree");
    assert!(find(&tree, "Lists").is_none(), "{tree}");

    // A JSON array becomes the typed array, element by element.
    call(
        "add_node",
        json!({"parent_node_path": ".", "node_type": "Lists", "node_name": "Lists",
               "properties": {"paths": ["../Player", "../UI"]}}),
    )
    .await
    .expect("add lists");
    let read = props().await;
    assert_eq!(read["paths"]["type"], "Array[NodePath]", "{read}");
    assert_eq!(
        read["paths"]["value"], "Array[NodePath]([NodePath(\"../Player\"), NodePath(\"../UI\")])",
        "{read}"
    );

    let set = call(
        "set_node_properties",
        json!({"node_path": "Lists", "properties": {
            "names": ["a", "b"],
            "counts": [1, 2],
            "kinds": [1],
            "points": [[1, 2], {"x": 3, "y": 4}],
            "mats": ["res://mat.tres", null],
            "tags": ["res://tag.tres"],
            "loose": ["x", 1],
            "weights": {"a": 1, "b": 2},
            "hidden_paths": ["../UI"]
        }}),
    )
    .await
    .expect("set typed containers");
    let applied = &set["applied"];
    assert_eq!(applied["names"], "Array[String]([\"a\", \"b\"])", "{set}");
    assert_eq!(applied["counts"], "Array[int]([1, 2])", "{set}");
    assert_eq!(applied["kinds"], "Array[int]([1])", "{set}");
    assert_eq!(
        applied["points"], "Array[Vector2]([Vector2(1, 2), Vector2(3, 4)])",
        "{set}"
    );
    assert_eq!(
        applied["weights"], "Dictionary[String, int]({\n\"a\": 1,\n\"b\": 2\n})",
        "{set}"
    );
    assert_eq!(
        applied["hidden_paths"], "Array[NodePath]([NodePath(\"../UI\")])",
        "{set}"
    );

    // A Godot literal still works, and is checked against the element type too.
    call(
        "set_node_properties",
        json!({"node_path": "Lists", "properties": {"paths": "Array[NodePath]([NodePath(\"../UI\")])"}}),
    )
    .await
    .expect("set literal");
    assert_eq!(
        props().await["paths"]["value"],
        "Array[NodePath]([NodePath(\"../UI\")])"
    );

    // Every refusal names the element and changes nothing, not even the valid entry beside it.
    for (key, value, needle) in [
        ("paths", json!([1]), "paths: element 0: expected NodePath"),
        ("paths", json!("../Player"), "a JSON array"),
        ("counts", json!([1.5]), "counts: element 0: expected int"),
        ("points", json!([[1, 2], [1]]), "points: element 1"),
        ("mats", json!(["res://notes.tres"]), "not a Material"),
        ("mats", json!(["res://missing.tres"]), "Resource not found"),
        ("tags", json!(["res://mat.tres"]), "not a Tag"),
        (
            "nodes",
            json!(["../Player"]),
            "nodes: element 0: expected Node",
        ),
        ("weights", json!({"a": "heavy"}), "weights: value of \"a\""),
        ("weights", json!([1]), "a JSON object"),
        (
            "names",
            json!("Array[NodePath]([NodePath(\"a\")])"),
            "names: element 0: expected String",
        ),
        // Text is never parsed into an Object or a loaded Resource, even inside an array whose
        // elements would then pass the class check.
        (
            "nodes",
            json!("[Object(Node,\"name\":\"Planted\")]"),
            "not a built-in type",
        ),
        (
            "mats",
            json!("[Resource(\"res://mat.tres\")]"),
            "not a built-in type",
        ),
        (
            "weights",
            json!("{\"a\": Object(Node,\"name\":\"Planted\")}"),
            "not a built-in type",
        ),
        (
            "mats",
            json!(["res://../mat.tres"]),
            "not a path inside the project",
        ),
        (
            "nodes",
            json!("[;\"\nObject(Node,\"name\":\"Planted\")\n;\"\n]"),
            "not a built-in type",
        ),
    ] {
        expect_error(
            call(
                "set_node_properties",
                json!({"node_path": "Lists", "properties": {key: value, "loose": []}}),
            )
            .await,
            needle,
        );
    }
    expect_error(
        call(
            "set_node_properties",
            json!({"node_path": "Lists", "properties": {"loose": "[Object(Node,\"name\":\"Planted\")]"}}),
        )
        .await,
        "not a built-in type",
    );
    let read = props().await;
    assert_eq!(read["loose"]["value"], "[\"x\", 1.0]", "{read}");
    assert_eq!(read["nodes"]["value"], "Array[Node]([])", "{read}");
    assert_eq!(
        read["paths"]["value"],
        "Array[NodePath]([NodePath(\"../UI\")])"
    );

    // The typed values are what the scene saves.
    call("save_scene", json!({})).await.expect("save");
    let saved = std::fs::read_to_string(editor.project.join("main.tscn")).expect("saved scene");
    for needle in [
        "paths = Array[NodePath]([NodePath(\"../UI\")])",
        "names = Array[String]([\"a\", \"b\"])",
        "mats = Array[Material]([ExtResource(",
        "tags = Array[",
    ] {
        assert!(saved.contains(needle), "{needle} in {saved}");
    }
}

/// Node-reference exports: native node types, a class_name type and a plain Node. Not a tool
/// script, so the editor holds its values in a placeholder instance as it does in a game project.
const HOLDER_SCRIPT: &str = r#"class_name Holder
extends Node

@export var target: Node2D
@export var sprite: Sprite2D
@export var marker: Marker
@export var any: Node
"#;

#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn node_reference_exports_take_a_node_path_like_the_node_picker() {
    // The editor cannot create a node of a script class that is not a tool script, so the Marker
    // stands in the scene and the Holder comes as an instanced scene.
    let main_with_marker = MAIN_SCENE.replacen(
        "[node name=\"Main\"",
        "[ext_resource type=\"Script\" path=\"res://marker.gd\" id=\"1_marker\"]\n\n[node name=\"Main\"",
        1,
    ) + "\n[node name=\"Marker\" type=\"Node2D\" parent=\".\"]\nscript = ExtResource(\"1_marker\")\n";
    let editor = start_editor_with(
        Some(UNDO_HOOK),
        "",
        &[
            ("holder.gd", HOLDER_SCRIPT),
            ("marker.gd", "class_name Marker\nextends Node2D\n"),
            (
                "holder.tscn",
                "[gd_scene format=3]\n\n\
                 [ext_resource type=\"Script\" path=\"res://holder.gd\" id=\"1_holder\"]\n\n\
                 [node name=\"Holder\" type=\"Node\"]\nscript = ExtResource(\"1_holder\")\n",
            ),
            ("main.tscn", &main_with_marker),
        ],
        "",
    )
    .await;
    let bridge = &editor.bridge;
    let call = |command: &'static str, params: Value| async move {
        bridge.send_command(command, params).await
    };
    let props = || async {
        call("get_node_properties", json!({"node_path": "Holder"}))
            .await
            .expect("props")["properties"]
            .clone()
    };
    call("open_scene", json!({"scene_path": "res://main.tscn"}))
        .await
        .expect("open main");
    // add_node resolves the path from the place the new node gets, here under the root.
    let added = call(
        "add_node",
        json!({"parent_node_path": ".", "scene": "res://holder.tscn",
               "properties": {"target": "../Player"}}),
    )
    .await
    .expect("add holder");
    assert_eq!(
        added["applied"]["target"], "<Node2D at \"Player\">",
        "{added}"
    );

    let set = call(
        "set_node_properties",
        json!({"node_path": "Holder", "properties": {
            "sprite": "../Player/Body",
            "marker": "../Marker",
            "any": ".."
        }}),
    )
    .await
    .expect("set node references");
    let applied = &set["applied"];
    assert_eq!(applied["sprite"], "<Sprite2D at \"Player/Body\">", "{set}");
    assert_eq!(applied["marker"], "<Node2D at \"Marker\">", "{set}");
    assert_eq!(applied["any"], "<Node2D at \".\">", "{set}");
    assert_eq!(props().await["target"]["value"], "<Node2D at \"Player\">");

    // A path that does not resolve, a node of the wrong type and other shapes are refused, and the
    // valid value beside them is not set.
    for (key, value, needle) in [
        ("target", json!("../Missing"), "no node at '../Missing'"),
        (
            "target",
            json!("../UI"),
            "the node 'UI' is a Control, not a Node2D",
        ),
        ("sprite", json!("../Player"), "is a Node2D, not a Sprite2D"),
        ("marker", json!("../Player"), "not a Marker"),
        ("target", json!("../.."), "in the edited scene"),
        ("target", json!("/root/Main/Player"), "absolute path"),
        ("target", json!("../Player:position"), "names a property"),
        ("target", json!(5), "expected a node path"),
    ] {
        expect_error(
            call(
                "set_node_properties",
                json!({"node_path": "Holder", "properties": {key: value, "any": "../UI"}}),
            )
            .await,
            needle,
        );
    }
    assert_eq!(props().await["any"]["value"], "<Node2D at \".\">");

    // Godot saves the nodes as node_paths references relative to the holder.
    call("save_scene", json!({})).await.expect("save");
    let saved = std::fs::read_to_string(editor.project.join("main.tscn")).expect("saved scene");
    for needle in [
        "node_paths=PackedStringArray(",
        "target = NodePath(\"../Player\")",
        "sprite = NodePath(\"../Player/Body\")",
        "marker = NodePath(\"../Marker\")",
        "any = NodePath(\"..\")",
    ] {
        assert!(saved.contains(needle), "{needle} in {saved}");
    }

    // null clears a reference, and undo brings it back.
    call(
        "set_node_properties",
        json!({"node_path": "Holder", "properties": {"target": null}}),
    )
    .await
    .expect("clear target");
    assert_eq!(props().await["target"]["value"], "null");
    call("__test_undo", json!({})).await.expect("undo clear");
    assert_eq!(props().await["target"]["value"], "<Node2D at \"Player\">");
}

/// A slice of a 3D level: its script is not a tool script and is attached to a node of the scene
/// itself (not an instanced one), with node exports of a class_name type and of a native type.
const SLICE_SCRIPT: &str = r#"class_name DemoSlice
extends Node3D

@export var outpost: ToadOutpost
@export var anchor: Node3D
"#;

const SLICE_WORLD: &str = r#"[gd_scene format=3]

[ext_resource type="Script" path="res://outpost.gd" id="1_outpost"]
[ext_resource type="Script" path="res://slice.gd" id="2_slice"]

[node name="World" type="Node3D"]

[node name="Toads" type="Node3D" parent="."]

[node name="FallenTreeOutpost" type="Node3D" parent="Toads"]
script = ExtResource("1_outpost")

[node name="Rock" type="Node3D" parent="Toads"]

[node name="Slice" type="Node3D" parent="."]
script = ExtResource("2_slice")
"#;

#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn node_exports_of_a_scripted_node_in_the_scene_and_of_a_new_script_class_node_take_paths() {
    let editor = start_editor_with(
        Some(UNDO_HOOK),
        "",
        &[
            ("outpost.gd", "class_name ToadOutpost\nextends Node3D\n"),
            ("slice.gd", SLICE_SCRIPT),
            ("world.tscn", SLICE_WORLD),
        ],
        "",
    )
    .await;
    let bridge = &editor.bridge;
    let call = |command: &'static str, params: Value| async move {
        bridge.send_command(command, params).await
    };
    call("open_scene", json!({"scene_path": "res://world.tscn"}))
        .await
        .expect("open world");
    let props = call("get_node_properties", json!({"node_path": "Slice"}))
        .await
        .expect("props")["properties"]
        .clone();
    assert_eq!(props["outpost"]["value"], "null", "{props}");

    let set = call(
        "set_node_properties",
        json!({"node_path": "Slice", "properties": {
            "outpost": "../Toads/FallenTreeOutpost",
            "anchor": "../Toads"
        }}),
    )
    .await
    .expect("set node references");
    assert_eq!(
        set["applied"]["outpost"], "<Node3D at \"Toads/FallenTreeOutpost\">",
        "{set}"
    );
    assert_eq!(set["applied"]["anchor"], "<Node3D at \"Toads\">", "{set}");
    expect_error(
        call(
            "set_node_properties",
            json!({"node_path": "Slice", "properties": {"outpost": "../Toads/Rock"}}),
        )
        .await,
        "not a ToadOutpost",
    );

    // A node of a script class that is not a tool script is made as the Create Node dialog makes
    // it: its native base with the script attached, and its node exports take paths at once.
    let added = call(
        "add_node",
        json!({"parent_node_path": ".", "node_type": "DemoSlice", "node_name": "Second",
               "properties": {"outpost": "../Toads/FallenTreeOutpost"}}),
    )
    .await
    .expect("add a DemoSlice");
    assert_eq!(
        added["applied"]["outpost"], "<Node3D at \"Toads/FallenTreeOutpost\">",
        "{added}"
    );
    expect_error(
        call(
            "add_node",
            json!({"parent_node_path": ".", "node_type": "DemoSlice", "node_name": "Third",
                   "properties": {"outpost": "../Toads/Rock"}}),
        )
        .await,
        "not a ToadOutpost",
    );

    call("save_scene", json!({})).await.expect("save");
    let saved = std::fs::read_to_string(editor.project.join("world.tscn")).expect("saved scene");
    for needle in [
        "outpost = NodePath(\"../Toads/FallenTreeOutpost\")",
        "anchor = NodePath(\"../Toads\")",
        "[node name=\"Second\" type=\"Node3D\" parent=\".\"",
    ] {
        assert!(saved.contains(needle), "{needle} in {saved}");
    }
    let second = &saved[saved.find("[node name=\"Second\"").expect("Second saved")..];
    assert!(
        second.contains("script = ExtResource(\"2_slice\")"),
        "{second}"
    );
    assert!(!saved.contains("\"Third\""), "{saved}");
}

/// The editor reports the fingerprint of the addon scripts it loaded, computed as the server
/// computes it over the addon it embeds, so a stale build of the same -dev version shows.
#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn the_editor_reports_the_build_of_the_addon_it_loaded() {
    let editor = start_editor_with(None, "", &[], "").await;
    let info = editor
        .bridge
        .send_command("get_project_info", json!({}))
        .await
        .expect("project info");
    assert_eq!(
        info["addon_build"],
        godot_mcp::addon::server_build(),
        "{info}"
    );
}

const SNAG_SCENE: &str = r#"[gd_scene format=3 uid="uid://snagfixture"]

[node name="Snag" type="Node2D"]

[node name="Leaf" type="Sprite2D" parent="."]
"#;

/// A scene that instances main.tscn: adding it to main would make main contain itself.
const OUTER_SCENE: &str = r#"[gd_scene format=3]

[ext_resource type="PackedScene" path="res://main.tscn" id="1_main"]

[node name="Outer" type="Node2D"]

[node name="Inner" parent="." instance=ExtResource("1_main")]
"#;

#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn add_node_instances_a_saved_scene_the_way_the_editor_does() {
    let editor = start_editor_with(
        Some(UNDO_HOOK),
        "",
        &[
            ("snag.tscn", SNAG_SCENE),
            ("outer.tscn", OUTER_SCENE),
            (
                "notes.tres",
                "[gd_resource type=\"Resource\" format=3]\n\n[resource]\n",
            ),
        ],
        "",
    )
    .await;
    let bridge = &editor.bridge;
    let call = |command: &'static str, params: Value| async move {
        bridge.send_command(command, params).await
    };
    call("open_scene", json!({"scene_path": "res://main.tscn"}))
        .await
        .expect("open main");

    let added = call(
        "add_node",
        json!({"parent_node_path": "Player", "scene": "res://snag.tscn", "node_name": "Snag1",
               "properties": {"position": [3, 4], "rotation_degrees": 90}}),
    )
    .await
    .expect("instance snag");
    assert_eq!(added["path"], "Player/Snag1", "{added}");
    assert_eq!(added["scene"], "res://snag.tscn", "{added}");
    assert_eq!(added["class"], "Node2D", "{added}");
    let props = call("get_node_properties", json!({"node_path": "Player/Snag1"}))
        .await
        .expect("props")["properties"]
        .clone();
    assert_eq!(props["position"]["value"], "Vector2(3, 4)", "{props}");

    // Refused without changing the scene: no file, not a scene, the scene itself, a scene that
    // instances it, both sources at once, and a bad initial property.
    for (params, needle) in [
        (json!({"scene": "res://missing.tscn"}), "does not exist"),
        (json!({"scene": "res://notes.tres"}), "not a PackedScene"),
        (json!({"scene": "res://main.tscn"}), "itself"),
        (json!({"scene": "res://outer.tscn"}), "itself"),
        (json!({"scene": "res://../snag.tscn"}), "inside the project"),
        (
            json!({"scene": "res://snag.tscn", "node_type": "Node2D"}),
            "not both",
        ),
        (
            json!({"scene": "res://snag.tscn", "properties": {"not_a_prop": 1}}),
            "not_a_prop",
        ),
    ] {
        expect_error(call("add_node", params).await, needle);
    }
    let tree = call("get_scene_tree", json!({})).await.expect("tree");
    let player = find(&tree["tree"], "Player").expect("Player");
    assert_eq!(child_names(player), ["Body", "Weapon", "Snag1"], "{tree}");

    // Saved as an instance of the scene, by its uid, with the instance's own nodes left inside it.
    call("save_scene", json!({})).await.expect("save");
    let saved = std::fs::read_to_string(editor.project.join("main.tscn")).expect("saved scene");
    assert!(
        saved.contains(r#"type="PackedScene" uid="uid://snagfixture" path="res://snag.tscn""#),
        "{saved}"
    );
    // Godot 4.6+ writes a unique_id between the parent and the instance.
    assert!(
        saved.lines().any(
            |line| line.starts_with(r#"[node name="Snag1" parent="Player""#)
                && line.contains("instance=ExtResource(")
        ),
        "{saved}"
    );
    assert!(saved.contains("position = Vector2(3, 4)"), "{saved}");
    assert!(!saved.contains(r#"name="Leaf""#), "{saved}");

    // One undo removes the whole instance.
    call("__test_undo", json!({})).await.expect("undo add");
    let tree = call("get_scene_tree", json!({})).await.expect("tree");
    let player = find(&tree["tree"], "Player").expect("Player");
    assert_eq!(child_names(player), ["Body", "Weapon"], "{tree}");
}

/// The editor's played game loads the project's adapter like a managed run does, and the same two
/// tools reach it through the plugin's relay.
#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn editor_play_answers_the_project_adapter() {
    use godot_mcp::cli_runner::CliRunner;
    use godot_mcp::tools::ToolManager;
    use std::sync::Arc;

    if cfg!(target_os = "linux")
        && std::env::var_os("DISPLAY").is_none()
        && std::env::var_os("WAYLAND_DISPLAY").is_none()
    {
        eprintln!("skipped: editor Play needs a display");
        return;
    }

    let editor = start_editor_with(
        None,
        "",
        &[("adapter.gd", common::adapter::FIXTURE_ADAPTER)],
        "\n[godot_mcp]\n\nadapter=\"res://adapter.gd\"\n",
    )
    .await;
    let (godot, _) = godot_binaries();
    let tools = Arc::new(
        ToolManager::new(BridgeClient::new(editor.port), CliRunner::new(godot))
            .with_working_dir(editor.project.clone()),
    );
    let started = Arc::clone(&tools)
        .call_tool("godot_run_project", json!({"scene": "res://main.tscn"}))
        .await;
    assert!(!started.is_error, "{}", tool_text(&started));

    let read = Arc::clone(&tools)
        .call_tool("godot_game_state", json!({"names": ["level", "balls"]}))
        .await;
    assert!(!read.is_error, "{}", tool_text(&read));
    let read_json: Value = serde_json::from_str(&tool_text(&read)).expect("state json");
    assert_eq!(read_json["values"]["level"]["index"], 3, "{read_json}");
    assert_eq!(read_json["values"]["balls"], json!([1, 2]), "{read_json}");
    assert_eq!(read_json["adapter"]["loaded"], true, "{read_json}");
    assert_eq!(read_json["run_id"], "editor", "{read_json}");

    let ran = Arc::clone(&tools)
        .call_tool(
            "godot_game_command",
            json!({"name": "grant", "args": {"name": "machine_gun"}}),
        )
        .await;
    assert!(!ran.is_error, "{}", tool_text(&ran));
    let ran_json: Value = serde_json::from_str(&tool_text(&ran)).expect("command json");
    assert_eq!(
        ran_json["value"],
        json!({"granted": "machine_gun"}),
        "{ran_json}"
    );

    let unknown = Arc::clone(&tools)
        .call_tool("godot_game_command", json!({"name": "no_such_command"}))
        .await;
    assert!(unknown.is_error, "{}", tool_text(&unknown));
    assert!(
        tool_text(&unknown).contains("NOT_FOUND") && tool_text(&unknown).contains("grant"),
        "{}",
        tool_text(&unknown)
    );

    let stopped = Arc::clone(&tools)
        .call_tool("godot_stop_project", json!({}))
        .await;
    assert!(!stopped.is_error, "{}", tool_text(&stopped));
}

const GLOW_SHADER: &str = "shader_type spatial;\n\nuniform float glow_amount = 0.5;\n";

const SHARED_GLOW: &str = r#"[gd_resource type="ShaderMaterial" format=3]

[ext_resource type="Shader" path="res://glow.gdshader" id="1"]

[resource]
shader = ExtResource("1")
shader_parameter/glow_amount = 0.5
"#;

const LAMP_SCENE: &str = r#"[gd_scene format=3]

[ext_resource type="Shader" path="res://glow.gdshader" id="1"]

[sub_resource type="ShaderMaterial" id="ShaderMaterial_lamp"]
shader = ExtResource("1")
shader_parameter/glow_amount = 0.75

[node name="Lamp" type="MeshInstance3D"]
material_override = SubResource("ShaderMaterial_lamp")
"#;

const POND_SCENE: &str = r#"[gd_scene format=3]

[ext_resource type="Shader" path="res://glow.gdshader" id="1"]
[ext_resource type="ShaderMaterial" path="res://shared_glow.tres" id="2"]
[ext_resource type="PackedScene" path="res://lamp.tscn" id="3"]

[sub_resource type="ShaderMaterial" id="ShaderMaterial_water"]
shader = ExtResource("1")
shader_parameter/glow_amount = 0.25

[node name="Pond" type="Node3D"]

[node name="Water" type="MeshInstance3D" parent="."]
material_override = SubResource("ShaderMaterial_water")

[node name="Shared" type="MeshInstance3D" parent="."]
material_override = ExtResource("2")

[node name="Bare" type="MeshInstance3D" parent="."]

[node name="Lamp" parent="." instance=ExtResource("3")]
"#;

/// `:` steps through a resource in the editor the way godot_game_set does at run time, so a shader
/// uniform of a node's own material is changed in one undo action, while a material saved in a
/// .tres file or in another scene is refused instead of being changed for every scene that uses it.
#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn node_properties_step_through_the_scenes_own_resources() {
    let editor = start_editor_with(
        Some(UNDO_HOOK),
        "",
        &[
            ("glow.gdshader", GLOW_SHADER),
            ("shared_glow.tres", SHARED_GLOW),
            ("lamp.tscn", LAMP_SCENE),
            ("pond.tscn", POND_SCENE),
        ],
        "",
    )
    .await;
    let bridge = &editor.bridge;
    let call = |command: &'static str, params: Value| async move {
        bridge.send_command(command, params).await
    };
    let glow = "material_override:shader_parameter/glow_amount";
    let read = |node: &'static str| async move {
        call(
            "get_node_properties",
            json!({"node_path": node, "properties": [glow, "visible"]}),
        )
        .await
        .expect("read")
    };
    call("open_scene", json!({"scene_path": "res://pond.tscn"}))
        .await
        .expect("open pond");

    // A read names only the properties asked for; one that cannot be read is reported beside them.
    let water = call(
        "get_node_properties",
        json!({"node_path": "Water", "properties": [
            glow,
            "material_override:shader_parameter/missing",
            "material_override:script",
            "visible"
        ]}),
    )
    .await
    .expect("read water");
    assert_eq!(
        water["properties"][glow],
        json!({"type": "float", "value": "0.25"}),
        "{water}"
    );
    assert_eq!(water["properties"]["visible"]["value"], "true", "{water}");
    assert!(
        water["property_errors"]["material_override:shader_parameter/missing"]
            .as_str()
            .is_some_and(|message| message.contains("ShaderMaterial")),
        "{water}"
    );
    assert!(
        water["properties"]
            .get("material_override:script")
            .is_none(),
        "{water}"
    );
    assert_eq!(
        read("Shared").await["properties"][glow]["value"],
        "0.5",
        "reading a shared resource is allowed"
    );
    expect_error(
        call(
            "get_node_properties",
            json!({"node_path": "Water", "properties": [5]}),
        )
        .await,
        "properties",
    );

    // The scene's own material is changed with the node in one undo action, and the scene is dirty.
    let set = call(
        "set_node_properties",
        json!({"node_path": "Water", "properties": {glow: 0.9, "visible": false}}),
    )
    .await
    .expect("set glow");
    assert_eq!(set["applied"][glow], "0.9", "{set}");
    let after = read("Water").await;
    assert_eq!(after["properties"][glow]["value"], "0.9", "{after}");
    assert_eq!(after["properties"]["visible"]["value"], "false", "{after}");
    expect_error(
        call(
            "open_scene",
            json!({"scene_path": "res://pond.tscn", "reload": true}),
        )
        .await,
        "unsaved",
    );
    call("__test_undo", json!({})).await.expect("undo");
    let undone = read("Water").await;
    assert_eq!(undone["properties"][glow]["value"], "0.25", "{undone}");
    assert_eq!(undone["properties"]["visible"]["value"], "true", "{undone}");

    // A shared material, another scene's material, an empty slot and refused segments change nothing.
    let shared_before =
        std::fs::read_to_string(editor.project.join("shared_glow.tres")).expect("tres");
    for (node, key, needle) in [
        ("Shared", glow, "res://shared_glow.tres"),
        ("Lamp", glow, "res://lamp.tscn"),
        ("Bare", glow, "Assign a resource"),
        ("Water", "material_override:script", "Refusing"),
        ("Water", "material_override:_private", "Refusing"),
        ("Water", "visible:x", "not a resource"),
        (
            "Water",
            "material_override:shader_parameter/missing",
            "Unknown property",
        ),
    ] {
        expect_error(
            call(
                "set_node_properties",
                json!({"node_path": node, "properties": {key: 0.1, "visible": false}}),
            )
            .await,
            needle,
        );
        assert_eq!(
            read(node).await["properties"]["visible"]["value"],
            "true",
            "{node} {key}: a rejected batch must not be partially applied"
        );
    }
    assert_eq!(read("Shared").await["properties"][glow]["value"], "0.5");
    assert_eq!(read("Lamp").await["properties"][glow]["value"], "0.75");
    expect_error(
        call(
            "set_node_properties",
            json!({"node_path": "Water", "properties": {glow: "bright"}}),
        )
        .await,
        "float",
    );

    // The change is saved inside the scene, and the shared file is untouched.
    call(
        "set_node_properties",
        json!({"node_path": "Water", "properties": {glow: 0.6}}),
    )
    .await
    .expect("set glow again");
    call("save_scene", json!({})).await.expect("save");
    let saved = std::fs::read_to_string(editor.project.join("pond.tscn")).expect("saved scene");
    assert!(
        saved.contains("shader_parameter/glow_amount = 0.6"),
        "{saved}"
    );
    assert_eq!(
        std::fs::read_to_string(editor.project.join("shared_glow.tres")).expect("tres"),
        shared_before
    );
}

/// A material saved in its own .tres is changed through the editor's loaded copy, so a scene that
/// uses it follows, and saved; undo puts the value back in the file. A refused value changes
/// nothing.
#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn resource_file_properties_are_set_saved_and_undone_through_the_editor() {
    let editor = start_editor_with(
        Some(UNDO_HOOK),
        "",
        &[
            ("glow.gdshader", GLOW_SHADER),
            ("shared_glow.tres", SHARED_GLOW),
            ("lamp.tscn", LAMP_SCENE),
            ("pond.tscn", POND_SCENE),
        ],
        "",
    )
    .await;
    let bridge = &editor.bridge;
    let call = |command: &'static str, params: Value| async move {
        bridge.send_command(command, params).await
    };
    let glow = "material_override:shader_parameter/glow_amount";
    let shared_glow = || async {
        call(
            "get_node_properties",
            json!({"node_path": "Shared", "properties": [glow]}),
        )
        .await
        .expect("read shared")["properties"][glow]["value"]
            .clone()
    };
    let file = || std::fs::read_to_string(editor.project.join("shared_glow.tres")).expect("tres");
    call("open_scene", json!({"scene_path": "res://pond.tscn"}))
        .await
        .expect("open pond");
    let original = file();

    for (properties, needle) in [
        (
            json!({"shader_parameter/glow_amount": 0.9, "shader_parameter/missing": 1}),
            "no uniform 'missing'",
        ),
        (json!({"shader_parameter/glow_amount": "bright"}), "float"),
        (json!({"script": null}), "Refusing"),
        (json!({"shader:code": "x"}), "':' steps"),
    ] {
        expect_error(
            call(
                "set_resource_properties",
                json!({"resource_path": "res://shared_glow.tres", "properties": properties}),
            )
            .await,
            needle,
        );
        assert_eq!(
            file(),
            original,
            "{properties}: a refused call changed the file"
        );
    }
    expect_error(
        call(
            "set_resource_properties",
            json!({"resource_path": "res://pond.tscn", "properties": {"a": 1}}),
        )
        .await,
        ".tres or .res",
    );

    let set = call(
        "set_resource_properties",
        json!({"resource_path": "res://shared_glow.tres", "properties": {"shader_parameter/glow_amount": 0.8}}),
    )
    .await
    .expect("set the shared material");
    assert_eq!(
        set["changed"]["shader_parameter/glow_amount"],
        json!({"before": "0.5", "after": "0.8"}),
        "{set}"
    );
    assert_eq!(set["undoable"], true, "{set}");
    assert_eq!(shared_glow().await, "0.8", "the loaded copy did not follow");
    let saved = file();
    assert!(
        saved.contains("shader_parameter/glow_amount = 0.8") && !saved.contains("= null"),
        "{saved}"
    );

    call(
        "__test_undo_resource",
        json!({"resource_path": "res://shared_glow.tres"}),
    )
    .await
    .expect("undo");
    assert_eq!(
        shared_glow().await,
        "0.5",
        "undo did not restore the loaded copy"
    );
    assert!(
        file().contains("shader_parameter/glow_amount = 0.5"),
        "undo did not save the file: {}",
        file()
    );
}

/// One call sets a count and the entries it adds (streams_count, then stream_<n>/stream), and a
/// truly unknown name still changes nothing. Undoing a smaller count brings back the entries it
/// removed.
#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn resource_count_and_the_entries_it_adds_are_set_in_one_call() {
    let editor = start_editor_with(
        Some(UNDO_HOOK),
        "",
        &[
            ("croak.tres", "[gd_resource type=\"AudioStreamRandomizer\" format=3]\n\n[resource]\n"),
            ("beep.tres", "[gd_resource type=\"AudioStreamGenerator\" format=3]\n\n[resource]\n"),
            ("chirp.tres", "[gd_resource type=\"AudioStreamGenerator\" format=3]\n\n[resource]\nmix_rate = 22050.0\n"),
        ],
        "",
    )
    .await;
    let bridge = &editor.bridge;
    let set = |properties: Value| async move {
        bridge
            .send_command(
                "set_resource_properties",
                json!({"resource_path": "res://croak.tres", "properties": properties}),
            )
            .await
    };
    let file = || std::fs::read_to_string(editor.project.join("croak.tres")).expect("tres");
    let original = file();

    expect_error(
        set(json!({"streams_count": 1, "stream_0/stream": "res://beep.tres", "stream_9/stream": "res://beep.tres"})).await,
        "Unknown property 'stream_9/stream' on AudioStreamRandomizer",
    );
    assert_eq!(file(), original, "a refused call changed the file");

    let set_both = set(json!({
        "streams_count": 2,
        "stream_0/stream": "res://beep.tres",
        "stream_1/stream": "res://chirp.tres",
        "random_pitch": 1.08
    }))
    .await
    .expect("set the count and its streams in one call");
    assert_eq!(
        set_both["changed"]["streams_count"],
        json!({"before": "0", "after": "2"}),
        "{set_both}"
    );
    assert_eq!(
        set_both["changed"]["stream_1/stream"]["before"], "null",
        "{set_both}"
    );
    let saved = file();
    assert!(
        saved.contains("streams_count = 2")
            && saved.contains("res://beep.tres")
            && saved.contains("res://chirp.tres")
            && saved.contains("random_pitch = 1.08"),
        "{saved}"
    );

    set(json!({"streams_count": 1}))
        .await
        .expect("shrink the count");
    assert!(!file().contains("res://chirp.tres"), "{}", file());
    bridge
        .send_command(
            "__test_undo_resource",
            json!({"resource_path": "res://croak.tres"}),
        )
        .await
        .expect("undo");
    let restored = file();
    assert!(
        restored.contains("streams_count = 2") && restored.contains("res://chirp.tres"),
        "undo did not bring back the stream a smaller count removed:\n{restored}"
    );
}

/// A class_name script written to disk while the editor is open is not in the editor's class
/// list until the editor rescans, so its language server calls the class unknown. Validation
/// registers the declaring script through the editor and asks again.
#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn a_class_written_behind_the_editors_back_is_registered_before_validation() {
    use godot_mcp::cli_runner::CliRunner;
    use godot_mcp::tools::ToolManager;
    use std::sync::Arc;

    let lsp_port = common::free_port();
    let lsp_arg = lsp_port.to_string();
    let editor = start_editor_with_args(None, "", &[], "", &["--lsp-port", &lsp_arg]).await;
    std::fs::create_dir_all(editor.project.join("world")).expect("world dir");
    std::fs::write(
        editor.project.join("world/stream_lane.gd"),
        "class_name StreamLane\nextends Path3D\n",
    )
    .expect("stream_lane.gd");
    std::fs::write(
        editor.project.join("world/stream_current.gd"),
        "extends Node3D\n\nfunc lane(node: Node) -> StreamLane:\n\tif node is StreamLane:\n\t\treturn node as StreamLane\n\treturn null\n",
    )
    .expect("stream_current.gd");
    let (godot, _) = godot_binaries();
    let tools = Arc::new(
        ToolManager::new(BridgeClient::new(editor.port), CliRunner::new(godot))
            .with_working_dir(editor.project.clone())
            .with_lsp_port(lsp_port),
    );
    let validated = tools
        .call_tool(
            "godot_validate_script",
            json!({"script_path": "res://world/stream_current.gd"}),
        )
        .await;
    assert!(!validated.is_error, "{}", tool_text(&validated));
}

const UID_PROBE_HOOK: &str = r#"        "__test_uid_probe":
            var path: String = str(params.get("path", ""))
            var id: int = ResourceLoader.get_resource_uid(path)
            send_success(peer, cmd_id, {"uid": ResourceUID.id_to_text(id) if id != ResourceUID.INVALID_ID else "", "cached_path": ResourceUID.get_id_path(id) if ResourceUID.has_id(id) else "", "in_filesystem": _filesystem_has(EditorInterface.get_resource_filesystem(), path)})
"#;

/// A level placed by a headless script, as Frog World places its scenes: a new scene that
/// instances another scene and uses a shader and a script, plus an existing scene saved again.
const PLACE_SCRIPT: &str = "extends SceneTree\n\nfunc _initialize() -> void:\n\tvar root := Node3D.new()\n\troot.name = \"Placed\"\n\tvar frog: Node = (load(\"res://frog.tscn\") as PackedScene).instantiate()\n\troot.add_child(frog)\n\tfrog.owner = root\n\tvar water := MeshInstance3D.new()\n\twater.name = \"Water\"\n\troot.add_child(water)\n\twater.owner = root\n\tvar material := ShaderMaterial.new()\n\tmaterial.shader = load(\"res://water.gdshader\")\n\tmaterial.set_shader_parameter(\"speed\", 2.0)\n\twater.material_override = material\n\twater.set_script(load(\"res://helper.gd\"))\n\tvar placed := PackedScene.new()\n\tplaced.pack(root)\n\tprint(\"PLACED \", ResourceSaver.save(placed, \"res://placed.tscn\"))\n\troot.free()\n\tvar frog_scene: PackedScene = load(\"res://frog.tscn\")\n\tprint(\"FROG \", ResourceSaver.save(frog_scene))\n\tquit(0)\n";

/// godot_run_script with the editor open on the same project: the uids the headless saver drops
/// are written back, and the editor's file system and uid cache learn the new scene's uid.
#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn run_script_keeps_uids_and_the_editor_follows_the_saved_files() {
    use godot_mcp::cli_runner::CliRunner;
    use godot_mcp::tools::ToolManager;
    use std::sync::Arc;

    let hook = format!("{UID_PROBE_HOOK}{UNDO_HOOK}");
    let editor = start_editor_with(
        Some(&hook),
        "",
        &[
            (
                "water.gdshader",
                "shader_type spatial;\nuniform float speed = 1.0;\nuniform vec4 tint : source_color = vec4(1.0);\nvoid fragment() { ALBEDO = tint.rgb * speed; }\n",
            ),
            ("helper.gd", "extends Node\n"),
            (
                "frog.tscn",
                "[gd_scene format=3 uid=\"uid://cfrogscene1\"]\n\n[node name=\"Frog\" type=\"Node3D\"]\n",
            ),
            ("place.gd", PLACE_SCRIPT),
        ],
        "",
    )
    .await;
    let project = editor.project.clone();
    let uid_file = |name: &str| {
        std::fs::read_to_string(project.join(name))
            .unwrap_or_else(|_| panic!("{name}: the import gives it a .uid"))
            .trim()
            .to_string()
    };
    let shader_uid = uid_file("water.gdshader.uid");
    let helper_uid = uid_file("helper.gd.uid");
    let (godot, _) = godot_binaries();
    let tools = Arc::new(
        ToolManager::new(BridgeClient::new(editor.port), CliRunner::new(godot))
            .with_working_dir(project.clone()),
    );

    let result = Arc::clone(&tools)
        .call_tool(
            "godot_run_script",
            json!({"script": "res://place.gd", "timeout_seconds": 120}),
        )
        .await;
    let answer = tool_text(&result);
    assert!(!result.is_error, "{answer}");
    assert!(
        answer.contains("PLACED 0") && answer.contains("FROG 0") && !answer.starts_with("WARNING"),
        "{answer}"
    );
    let body: Value = serde_json::from_str(&answer).expect("JSON answer");
    assert_eq!(body["resource_damage"], json!([]), "{answer}");

    let frog = std::fs::read_to_string(project.join("frog.tscn")).expect("frog");
    assert!(
        frog.starts_with("[gd_scene format=3 uid=\"uid://cfrogscene1\"]"),
        "{frog}"
    );
    let placed = std::fs::read_to_string(project.join("placed.tscn")).expect("placed");
    let header = placed.lines().next().unwrap_or_default();
    let placed_uid = header
        .split("uid=\"")
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .unwrap_or_else(|| panic!("the new scene has no uid: {placed}"))
        .to_string();
    for (target, uid) in [
        ("res://frog.tscn", "uid://cfrogscene1"),
        ("res://water.gdshader", shader_uid.as_str()),
        ("res://helper.gd", helper_uid.as_str()),
    ] {
        assert!(
            placed.contains(&format!("uid=\"{uid}\" path=\"{target}\"")),
            "{target} lost its uid {uid}:\n{placed}"
        );
    }
    assert!(!placed.contains("= null"), "{placed}");

    let refreshed = body["editor_refresh"]["refreshed"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(
        refreshed.contains(&json!("res://placed.tscn"))
            && refreshed.contains(&json!("res://frog.tscn")),
        "{answer}"
    );
    let probe = editor
        .bridge
        .send_command("__test_uid_probe", json!({"path": "res://placed.tscn"}))
        .await
        .expect("uid probe");
    assert_eq!(
        probe,
        json!({"uid": placed_uid, "cached_path": "res://placed.tscn", "in_filesystem": true})
    );
}

/// Holds a material that uses res://water.gdshader and a script without a tab, as an editor does
/// for an open scene, and reports what the loaded shader and script now hold.
const LOADED_PROBE_HOOK: &str = r#"        "__test_loaded_probe":
            if not has_meta("__held_material"):
                set_meta("__held_material", load("res://foam.tres"))
                set_meta("__held_script", load("res://mover.gd"))
            var material: ShaderMaterial = get_meta("__held_material")
            var names: Array = []
            for uniform: Dictionary in material.shader.get_shader_uniform_list():
                names.append(uniform["name"])
            var held_script: Script = get_meta("__held_script")
            send_success(peer, cmd_id, {"uniforms": names, "pulse_default": "foam_pulse = 0.5" in material.shader.code, "same_shader": material.shader == load("res://water.gdshader"), "script_source": held_script.source_code})
        "ping":
"#;

/// A one-shot script that rewrites a shader and a script with FileAccess, as Frog World tunes its
/// water shader.
const REWRITE_SHADER_SCRIPT: &str = "extends SceneTree\n\nfunc _initialize() -> void:\n\tvar shader := FileAccess.open(\"res://water.gdshader\", FileAccess.WRITE)\n\tshader.store_string(\"shader_type spatial;\\nuniform float foam_speed = 1.0;\\nuniform float foam_pulse = 0.5;\\nvoid fragment() { ALBEDO = vec3(foam_pulse * foam_speed); }\\n\")\n\tshader.close()\n\tvar mover := FileAccess.open(\"res://mover.gd\", FileAccess.WRITE)\n\tmover.store_string(\"extends Node\\n\\nconst SPEED: float = 2.0\\n\")\n\tmover.close()\n\tprint(\"REWROTE\")\n\tquit(0)\n";

/// godot_run_script rewrites a shader and a script the editor has loaded: the editor reloads both,
/// so a material sees the new uniforms and defaults instead of pinning the old ones on next save.
#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn run_script_has_the_editor_reload_a_shader_and_a_script_it_rewrote() {
    use godot_mcp::cli_runner::CliRunner;
    use godot_mcp::tools::ToolManager;
    use std::sync::Arc;

    let editor = start_editor_with(
        Some(LOADED_PROBE_HOOK),
        "",
        &[
            (
                "water.gdshader",
                "shader_type spatial;\nuniform float foam_pulse = 0.35;\nvoid fragment() { ALBEDO = vec3(foam_pulse); }\n",
            ),
            (
                "foam.tres",
                "[gd_resource type=\"ShaderMaterial\" load_steps=2 format=3]\n\n[ext_resource type=\"Shader\" path=\"res://water.gdshader\" id=\"1_a\"]\n\n[resource]\nshader = ExtResource(\"1_a\")\n",
            ),
            ("mover.gd", "extends Node\n\nconst SPEED: float = 1.0\n"),
            ("rewrite.gd", REWRITE_SHADER_SCRIPT),
        ],
        "",
    )
    .await;
    let before = editor
        .bridge
        .send_command("__test_loaded_probe", json!({}))
        .await
        .expect("probe before");
    assert_eq!(before["uniforms"], json!(["foam_pulse"]), "{before}");
    // The e2e editor is headless and has no renderer, so it reads no uniform default; the
    // shader's own code says which default it holds.
    assert_eq!(before["pulse_default"], json!(false), "{before}");

    let (godot, _) = godot_binaries();
    let tools = Arc::new(
        ToolManager::new(BridgeClient::new(editor.port), CliRunner::new(godot))
            .with_working_dir(editor.project.clone()),
    );
    let result = Arc::clone(&tools)
        .call_tool(
            "godot_run_script",
            json!({"script": "res://rewrite.gd", "timeout_seconds": 120}),
        )
        .await;
    let answer = tool_text(&result);
    assert!(!result.is_error, "{answer}");
    assert!(answer.contains("REWROTE"), "{answer}");
    let body: Value = serde_json::from_str(&answer).expect("JSON answer");
    let refresh = &body["editor_refresh"];
    for path in ["res://mover.gd", "res://water.gdshader"] {
        assert!(
            refresh["refreshed"]
                .as_array()
                .is_some_and(|paths| paths.contains(&json!(path))),
            "{path} was not refreshed: {answer}"
        );
        assert!(
            refresh["reloaded_resources"]
                .as_array()
                .is_some_and(|paths| paths.contains(&json!(path))),
            "{path} was not reloaded: {answer}"
        );
    }

    let after = editor
        .bridge
        .send_command("__test_loaded_probe", json!({}))
        .await
        .expect("probe after");
    assert_eq!(
        after["uniforms"],
        json!(["foam_speed", "foam_pulse"]),
        "{after}"
    );
    assert_eq!(after["pulse_default"], json!(true), "{after}");
    assert_eq!(after["same_shader"], json!(true), "{after}");
    assert_eq!(
        after["script_source"],
        json!("extends Node\n\nconst SPEED: float = 2.0\n"),
        "{after}"
    );
}

/// Runs `tests/bridge-port-vectors.json` through the addon's formula and prints the canonical
/// path and port of its own project.
const BRIDGE_PORT_CHECK: &str = r#"extends SceneTree

const BridgePort: GDScript = preload("res://bridge_port.gd")

func _init() -> void:
    var vectors: Dictionary = JSON.parse_string(FileAccess.get_file_as_string(OS.get_cmdline_user_args()[0]))
    var failures: int = 0
    for case: Dictionary in vectors["ports"]:
        var port: int = BridgePort.port_for(str(case["path"]))
        if port != int(case["port"]):
            failures += 1
            print("MISMATCH port ", case["path"], " ", port, " != ", int(case["port"]))
    for case: Dictionary in vectors["normalize"]:
        var canonical: String = BridgePort.normalize(str(case["input"]), bool(case["windows"]))
        if canonical != str(case["canonical"]):
            failures += 1
            print("MISMATCH normalize ", case["input"], " -> ", canonical, " != ", case["canonical"])
    var project: String = BridgePort.project_path()
    print("PROJECT=", project)
    print("PORT=", BridgePort.port_for(project))
    print("FAILURES=", failures)
    quit()
"#;

/// The addon and the server compute the same bridge port: the shared vectors hold for the
/// GDScript formula, and both canonicalize a project reached through a symlink the same way.
#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn the_addon_derives_the_same_bridge_port_as_the_server() {
    let (godot, _) = godot_binaries();
    let dir = tempfile::tempdir().expect("project dir");
    let real = dir.path().join("real");
    std::fs::create_dir(&real).expect("mkdir");
    std::fs::write(real.join("project.godot"), "config_version=5\n").expect("project.godot");
    std::fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("addons/godot_mcp/core/bridge_port.gd"),
        real.join("bridge_port.gd"),
    )
    .expect("copy bridge_port.gd");
    std::fs::write(real.join("check.gd"), BRIDGE_PORT_CHECK).expect("check.gd");
    // The temporary directory is itself reached through a symlink on macOS (/var -> /private/var).
    #[cfg(unix)]
    let project = {
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&real, &link).expect("symlink");
        link
    };
    #[cfg(not(unix))]
    let project = real.clone();

    let output = timeout(
        Duration::from_secs(120),
        Command::new(&godot)
            .args(["--headless", "--path"])
            .arg(&project)
            .args(["--script", "res://check.gd", "--"])
            .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/bridge-port-vectors.json"))
            .stdin(Stdio::null())
            .output(),
    )
    .await
    .expect("check timed out")
    .expect("run godot");
    let text = String::from_utf8_lossy(&output.stdout).to_string()
        + &String::from_utf8_lossy(&output.stderr);
    let line = |key: &str| {
        text.lines()
            .find_map(|line| line.strip_prefix(key))
            .unwrap_or_else(|| panic!("no {key} line in:\n{text}"))
            .trim()
            .to_string()
    };
    assert_eq!(line("FAILURES="), "0", "{text}");
    let canonical = godot_mcp::bridge_port::canonical_project_path(&project);
    assert_eq!(line("PROJECT="), canonical, "{text}");
    assert_eq!(
        line("PORT="),
        godot_mcp::bridge_port::port_for(&canonical).to_string(),
        "{text}"
    );
}

/// Runs `tests/script-reference-vectors.json` through the addon's reference classifier.
const SCRIPT_REFERENCE_CHECK: &str = r#"extends SceneTree

func _init() -> void:
    var ops: RefCounted = (load("res://file_ops.gd") as GDScript).new() as RefCounted
    var vectors: Dictionary = JSON.parse_string(FileAccess.get_file_as_string(OS.get_cmdline_user_args()[0]))
    var failures: int = 0
    var cases: int = 0
    for case: Dictionary in vectors["cases"]:
        cases += 1
        var kind: String = str(ops.call("classify_script", str(case["script"]), str(case["path"])))
        if kind != str(case["kind"]):
            failures += 1
            print("MISMATCH ", case["name"], ": ", kind, " != ", case["kind"])
    print("CASES=", cases)
    print("FAILURES=", failures)
    quit()
"#;

/// The addon tells a script that only writes a file from one that uses it the same way as the
/// server: the shared vectors hold for `classify_script` in `file_ops.gd`.
#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn the_addon_classifies_script_references_like_the_server() {
    let (godot, _) = godot_binaries();
    let dir = tempfile::tempdir().expect("project dir");
    let project = dir.path();
    std::fs::write(project.join("project.godot"), "config_version=5\n").expect("project.godot");
    std::fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("addons/godot_mcp/editor/file_ops.gd"),
        project.join("file_ops.gd"),
    )
    .expect("copy file_ops.gd");
    std::fs::write(project.join("check.gd"), SCRIPT_REFERENCE_CHECK).expect("check.gd");
    let output = timeout(
        Duration::from_secs(120),
        Command::new(&godot)
            .args(["--headless", "--path"])
            .arg(project)
            .args(["--script", "res://check.gd", "--"])
            .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/script-reference-vectors.json"))
            .stdin(Stdio::null())
            .output(),
    )
    .await
    .expect("check timed out")
    .expect("run godot");
    let text = String::from_utf8_lossy(&output.stdout).to_string()
        + &String::from_utf8_lossy(&output.stderr);
    let line = |key: &str| {
        text.lines()
            .find_map(|line| line.strip_prefix(key))
            .unwrap_or_else(|| panic!("no {key} line in:\n{text}"))
            .trim()
            .to_string()
    };
    assert_eq!(line("FAILURES="), "0", "{text}");
    assert_ne!(line("CASES="), "0", "{text}");
}

/// With no port configured on either side, a server started in the project reaches the
/// editor on the port derived from the project path.
#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn an_editor_without_a_port_setting_is_found_on_the_derived_port() {
    use clap::Parser;
    use godot_mcp::config::Config;

    let (godot, console) = godot_binaries();
    let dir = tempfile::tempdir().expect("project dir");
    let project = dir.path();
    std::fs::write(
        project.join("project.godot"),
        "config_version=5\n\n[application]\n\nconfig/name=\"Derived Port\"\nconfig/features=PackedStringArray(\"4.5\")\n\n\
         [editor_plugins]\n\nenabled=PackedStringArray(\"res://addons/godot_mcp/plugin.cfg\")\n",
    )
    .expect("project.godot");
    copy_dir(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("addons/godot_mcp"),
        &project.join("addons/godot_mcp"),
    );
    let imported = timeout(
        Duration::from_secs(180),
        Command::new(&godot)
            .args(["--headless", "--path"])
            .arg(project)
            .arg("--import")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status(),
    )
    .await
    .expect("import timed out")
    .expect("run godot --import");
    assert!(imported.success(), "godot --import failed");

    // The server side: no --debug-port, no GODOT_DEBUG_PORT, started inside the project.
    let mut config = Config::try_parse_from(["godot-mcp", "--debug-port", ""]).expect("config");
    config.resolve_bridge_port(Some(&project.join("addons")), None);
    let port = config.debug_port;
    assert_eq!(port, godot_mcp::bridge_port::derived_port(project));
    assert!(
        config
            .bridge_port_origin()
            .contains("derived from the project path"),
        "{}",
        config.bridge_port_origin()
    );

    let log = project.join("editor.log");
    let log_file = std::fs::File::create(&log).expect("log file");
    let _child = Command::new(&console)
        .args(["--headless", "--editor", "--path"])
        .arg(project)
        .env_remove("GODOT_DEBUG_PORT")
        .stdout(Stdio::from(log_file.try_clone().expect("clone log")))
        .stderr(Stdio::from(log_file))
        .kill_on_drop(true)
        .spawn()
        .expect("spawn godot editor");

    let bridge = BridgeClient::with_port_origin(port, config.bridge_port_origin());
    wait_until_started(&bridge, &log, &format!("port {port}")).await;
    let text = std::fs::read_to_string(&log).unwrap_or_default();
    assert!(
        text.contains(&format!(
            "listening on 127.0.0.1:{port} (derived from the project path"
        )),
        "{text}"
    );
    // The editor is killed, so it cannot remove its token files itself. The legacy one is
    // only ours when this editor said it took the legacy port.
    let mut ports = vec![port];
    if text.contains("Also listening on the legacy port") {
        ports.push(godot_mcp::config::LEGACY_DEBUG_PORT);
    }
    for port in ports {
        if let Some(path) = token_path(port) {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// A tool manager that talks to `editor` and runs in its project.
fn tools_for(editor: &Editor) -> std::sync::Arc<godot_mcp::tools::ToolManager> {
    let (godot, _) = godot_binaries();
    std::sync::Arc::new(
        godot_mcp::tools::ToolManager::new(
            BridgeClient::new(editor.port),
            godot_mcp::cli_runner::CliRunner::new(godot),
        )
        .with_working_dir(editor.project.clone()),
    )
}

fn result_text(result: &godot_mcp::protocol::ToolResult) -> String {
    format!("{:?}", result.content)
}

/// Test-only command that answers the editor's process id.
const PID_HOOK: &str = r#"        "__editor_pid":
            send_success(peer, cmd_id, {"pid": OS.get_process_id()})
        "ping":
"#;

/// godot_close_editor quits the editor through its bridge and answers once the process is gone.
#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn close_editor_quits_the_editor_and_its_process_exits() {
    let mut editor = start_editor_with(Some(PID_HOOK), "", &[], "").await;
    // The editor says which process it is: a launcher such as the Windows console build starts
    // it as a child, so the process this test spawned may have another id.
    let pid = editor
        .bridge
        .send_command("__editor_pid", json!({}))
        .await
        .expect("editor pid")["pid"]
        .as_u64()
        .expect("a process id");
    let tools = tools_for(&editor);

    // This test is the editor's parent, so it reaps the process; until then the editor would
    // stay a zombie that still looks alive to the server.
    let (closed, status) = tokio::join!(
        tools.call_tool("godot_close_editor", json!({})),
        timeout(Duration::from_secs(60), editor._child.wait())
    );
    assert!(!closed.is_error, "{}", result_text(&closed));
    let shown = result_text(&closed);
    assert!(shown.contains("exited"), "{shown}");
    assert!(shown.contains(&pid.to_string()), "{shown}");
    let status = status
        .expect("the editor process did not exit")
        .expect("wait for the editor");
    eprintln!("editor exited with {status}");
}

/// Unsaved scene changes keep the editor open and are named; discard closes it anyway.
#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn close_editor_refuses_unsaved_changes_until_discard() {
    let mut editor = start_editor(false).await;
    editor
        .bridge
        .send_command("open_scene", json!({"scene_path": "res://main.tscn"}))
        .await
        .expect("open main.tscn");
    editor
        .bridge
        .send_command(
            "add_node",
            json!({"parent_node_path": ".", "node_type": "Node2D", "node_name": "Unsaved"}),
        )
        .await
        .expect("add a node");
    let tools = tools_for(&editor);

    let refused = std::sync::Arc::clone(&tools)
        .call_tool("godot_close_editor", json!({}))
        .await;
    assert!(refused.is_error, "{}", result_text(&refused));
    let shown = result_text(&refused);
    assert!(shown.contains("res://main.tscn"), "{shown}");
    assert!(shown.contains("discard"), "{shown}");
    sleep(Duration::from_secs(1)).await;
    assert!(
        editor._child.try_wait().expect("poll editor").is_none(),
        "the editor quit although its changes were not saved"
    );
    editor
        .bridge
        .send_command("ping", json!({}))
        .await
        .expect("the bridge still answers");

    let (closed, status) = tokio::join!(
        tools.call_tool("godot_close_editor", json!({"discard": true})),
        timeout(Duration::from_secs(60), editor._child.wait())
    );
    assert!(!closed.is_error, "{}", result_text(&closed));
    assert!(
        result_text(&closed).contains("res://main.tscn"),
        "the answer names what was discarded: {}",
        result_text(&closed)
    );
    status
        .expect("the editor process did not exit")
        .expect("wait for the editor");
    assert!(
        !std::fs::read_to_string(editor.project.join("main.tscn"))
            .expect("main.tscn")
            .contains("Unsaved"),
        "the discarded change was saved"
    );
}

/// A scene with inner nodes, instanced into main by the editable-children test.
const SHORE_SCENE: &str = r#"[gd_scene format=3]

[node name="Shore" type="Node2D"]

[node name="Bed" type="Node2D" parent="."]

[node name="Timer" type="Timer" parent="Bed"]
"#;

/// A scene that instances shore.tscn, so its Shore/Bed lies two instances deep.
const NEST_SCENE: &str = r#"[gd_scene format=3]

[ext_resource type="PackedScene" path="res://shore.tscn" id="1_shore"]

[node name="Nest" type="Node2D"]

[node name="Shore" parent="." instance=ExtResource("1_shore")]
"#;

/// Godot saves no change to a node inside an instance whose children are not editable, so the
/// editing commands refuse it instead of reporting a change the next save drops, and
/// make_editable turns on Editable Children for every instance on the way, in the same undo
/// action.
#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn edits_inside_an_instance_need_editable_children() {
    let editor = start_editor_with(
        Some(UNDO_HOOK),
        "",
        &[
            ("shore.tscn", SHORE_SCENE),
            ("nest.tscn", NEST_SCENE),
            (
                "hand.gd",
                "extends Node2D\n\nfunc on_timeout() -> void:\n\tpass\n",
            ),
        ],
        "",
    )
    .await;
    let bridge = &editor.bridge;
    let call = |command: &'static str, params: Value| async move {
        bridge.send_command(command, params).await
    };
    call("open_scene", json!({"scene_path": "res://main.tscn"}))
        .await
        .expect("open main");
    call(
        "add_node",
        json!({"parent_node_path": ".", "scene": "res://shore.tscn", "node_name": "ShoreReview"}),
    )
    .await
    .expect("instance shore");
    call(
        "add_node",
        json!({"parent_node_path": ".", "scene": "res://nest.tscn"}),
    )
    .await
    .expect("instance nest");
    call(
        "attach_script",
        json!({"node_path": "Player", "script_path": "res://hand.gd"}),
    )
    .await
    .expect("script on Player");

    // The tree says which nodes are instances and whether their children are editable.
    let tree = call("get_scene_tree", json!({})).await.expect("tree");
    let shore = find(&tree["tree"], "ShoreReview").expect("ShoreReview");
    assert_eq!(shore["instance"], "res://shore.tscn", "{tree}");
    assert_eq!(shore["editable_children"], false, "{tree}");

    // Refused, naming the instance and the way on, with nothing changed.
    for (command, params) in [
        (
            "set_node_properties",
            json!({"node_path": "ShoreReview/Bed", "properties": {"visible": false}}),
        ),
        (
            "add_node",
            json!({"parent_node_path": "ShoreReview/Bed", "node_type": "Node"}),
        ),
        (
            "attach_script",
            json!({"node_path": "ShoreReview/Bed", "script_path": "res://hand.gd"}),
        ),
        (
            "connect_signal",
            json!({"node_path": "ShoreReview/Bed/Timer", "signal_name": "timeout",
                   "target_path": "Player", "method": "on_timeout"}),
        ),
    ] {
        let err = call(command, params).await.expect_err(command).to_string();
        for needle in ["'ShoreReview'", "res://shore.tscn", "make_editable"] {
            assert!(err.contains(needle), "{command}: {needle} in {err}");
        }
    }
    let err = call("remove_node", json!({"node_path": "ShoreReview/Bed"}))
        .await
        .expect_err("remove inner node")
        .to_string();
    assert!(err.contains("res://shore.tscn"), "{err}");
    assert!(err.contains("visible"), "{err}");
    let props = call(
        "get_node_properties",
        json!({"node_path": "ShoreReview/Bed", "properties": ["visible"]}),
    )
    .await
    .expect("props")["properties"]
        .clone();
    assert_eq!(props["visible"]["value"], "true", "{props}");
    let tree = call("get_scene_tree", json!({})).await.expect("tree");
    assert!(find(&tree["tree"], "Bed").is_some(), "{tree}");

    // The instance root itself belongs to main, and a signal into an instance is saved too.
    call(
        "set_node_properties",
        json!({"node_path": "ShoreReview", "properties": {"position": [1, 2]}}),
    )
    .await
    .expect("instance root");
    call(
        "connect_signal",
        json!({"node_path": "Player", "signal_name": "visibility_changed",
               "target_path": "Player", "method": "on_timeout"}),
    )
    .await
    .expect("signal on own node");

    // make_editable enables Editable Children and says so; later edits need no flag.
    let set = call(
        "set_node_properties",
        json!({"node_path": "ShoreReview/Bed", "properties": {"visible": false}, "make_editable": true}),
    )
    .await
    .expect("set with make_editable");
    assert_eq!(set["made_editable"], json!(["ShoreReview"]), "{set}");
    assert_eq!(set["applied"]["visible"], "false", "{set}");
    let added = call(
        "add_node",
        json!({"parent_node_path": "ShoreReview/Bed", "node_type": "Node", "node_name": "Extra"}),
    )
    .await
    .expect("add under an editable instance");
    assert!(added.get("made_editable").is_none(), "{added}");
    call(
        "connect_signal",
        json!({"node_path": "ShoreReview/Bed/Timer", "signal_name": "timeout",
               "target_path": "Player", "method": "on_timeout"}),
    )
    .await
    .expect("connect from an editable instance");
    // Godot cannot delete a node an instance brings, even with editable children.
    expect_error(
        call("remove_node", json!({"node_path": "ShoreReview/Bed"})).await,
        "res://shore.tscn",
    );

    // Two instances deep, both are made editable; one undo reverts the change and both flags.
    let nested = call(
        "set_node_properties",
        json!({"node_path": "Nest/Shore/Bed", "properties": {"visible": false}, "make_editable": true}),
    )
    .await
    .expect("nested set");
    assert_eq!(
        nested["made_editable"],
        json!(["Nest", "Nest/Shore"]),
        "{nested}"
    );
    call("__test_undo", json!({})).await.expect("undo nested");
    let tree = call("get_scene_tree", json!({})).await.expect("tree");
    let nest = find(&tree["tree"], "Nest").expect("Nest");
    assert_eq!(nest["editable_children"], false, "{tree}");
    expect_error(
        call(
            "set_node_properties",
            json!({"node_path": "Nest/Shore/Bed", "properties": {"visible": false}}),
        )
        .await,
        "'Nest'",
    );
    call(
        "set_node_properties",
        json!({"node_path": "Nest/Shore/Bed", "properties": {"visible": false}, "make_editable": true}),
    )
    .await
    .expect("nested set again");

    call("save_scene", json!({})).await.expect("save");
    let saved = std::fs::read_to_string(editor.project.join("main.tscn")).expect("saved scene");
    for needle in [
        r#"[editable path="ShoreReview"]"#,
        r#"[editable path="Nest"]"#,
        r#"[editable path="Nest/Shore"]"#,
        r#"[node name="Bed" parent="ShoreReview""#,
        r#"[node name="Bed" parent="Nest/Shore""#,
        r#"[node name="Extra" type="Node" parent="ShoreReview/Bed""#,
        r#"[connection signal="timeout" from="ShoreReview/Bed/Timer" to="Player" method="on_timeout"]"#,
        "visible = false",
    ] {
        assert!(saved.contains(needle), "{needle} in {saved}");
    }
}

/// Test-only commands that look at a shader's tab and type into it, through the same tab
/// pairing the shader commands use.
const SHADER_PROBE_HOOK: &str = r#"        "__shader_state":
            var path: String = str(params.get("path", ""))
            var pair: Dictionary = _shader_ops()._pair_editor(path)
            var out: Dictionary = {"state": str(pair["state"]), "found": str(pair["state"]) == "found"}
            if ResourceLoader.has_cached(path):
                out["memory_code"] = str(load(path).get("code"))
            if str(pair["state"]) == "found":
                var base: CodeEdit = _shader_ops()._buffer(pair["editor"])
                out["text"] = base.text
                out["dirty"] = base.get_version() != base.get_saved_version()
            send_success(peer, cmd_id, out)
        "__shader_type":
            var pair: Dictionary = _shader_ops()._pair_editor(str(params.get("path", "")))
            var base: CodeEdit = _shader_ops()._buffer(pair["editor"])
            base.insert_text(str(params.get("text", "")), 0, 0)
            send_success(peer, cmd_id, {"text": base.text})
        "__shader_undo":
            var pair: Dictionary = _shader_ops()._pair_editor(str(params.get("path", "")))
            var base: CodeEdit = _shader_ops()._buffer(pair["editor"])
            base.undo()
            send_success(peer, cmd_id, {"text": base.text})
        "ping":
"#;

/// The create, read and edit commands for shaders go through the editor's shader tab and the
/// shader it holds in memory, keep the file's uid, and never overwrite unsaved typing.
#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn shader_commands_keep_the_shader_editor_in_step() {
    let water = "shader_type spatial;\n\nuniform float depth = 1.0;\n\nvoid fragment() {\n\tALBEDO = vec3(0.1, 0.3, depth);\n}\n";
    let clean = "shader_type canvas_item;\n\nvoid fragment() {\n\tCOLOR = vec4(1.0);\n}\n";
    let editor = start_editor_with(
        Some(SHADER_PROBE_HOOK),
        "",
        &[
            ("water.gdshader", water),
            ("clean.gdshader", clean),
            ("typed.gdshader", clean),
            ("notes.txt", "hello\n"),
        ],
        "",
    )
    .await;
    let bridge = &editor.bridge;
    let file = |name: &str| std::fs::read_to_string(editor.project.join(name)).expect("file");
    let uid = |name: &str| std::fs::read_to_string(editor.project.join(format!("{name}.uid"))).ok();
    let state = |path: &'static str| async move {
        bridge
            .send_command("__shader_state", json!({"path": path}))
            .await
            .expect("shader state")
    };
    let edit = |path: &'static str, old: &str, new: &str| {
        let params = json!({"shader_path": path, "old_text": old, "new_text": new});
        async move { bridge.send_command("edit_shader", params).await }
    };

    // A read of a closed shader answers with the file and the same label as the host.
    let read = bridge
        .send_command(
            "read_shader",
            json!({"shader_path": "res://water.gdshader"}),
        )
        .await
        .expect("read water");
    assert_eq!(read["text"], water);
    assert_eq!(read["source"], "file");
    assert_eq!(read["dirty"], false);
    assert_eq!(read["version"], script_label(water));

    // A new shader is written as given, tabs and all, and gets a .uid like any new file.
    let code = "shader_type spatial;\n\nvoid vertex() {\n\tVERTEX.y += 0.1;\n}\n";
    let created = bridge
        .send_command(
            "create_shader",
            json!({"shader_path": "res://fx/ripple.gdshader", "code": code}),
        )
        .await
        .expect("create ripple");
    assert_eq!(created["created"], true);
    assert_eq!(created["version"], script_label(code));
    assert_eq!(file("fx/ripple.gdshader"), code);
    let new_uid = uid("fx/ripple.gdshader").expect("the new shader has a .uid file");
    assert_eq!(created["uid"], new_uid.trim());
    expect_error(
        bridge
            .send_command(
                "create_shader",
                json!({"shader_path": "res://fx/ripple.gdshader", "code": "x"}),
            )
            .await,
        "already exists",
    );
    assert_eq!(file("fx/ripple.gdshader"), code);
    let include = bridge
        .send_command(
            "create_shader",
            json!({"shader_path": "res://fx/common.gdshaderinc", "code": "float wave(float x) {\n\treturn sin(x);\n}\n"}),
        )
        .await
        .expect("create include");
    assert_eq!(include["created"], true);
    assert!(uid("fx/common.gdshaderinc").is_some());

    // An edit opens the closed shader in the shader editor, is one undo step in its tab, saves
    // the file, gives the editor's shader the new code and keeps the uid.
    let uid_before = uid("water.gdshader");
    assert_eq!(state("res://water.gdshader").await["found"], false);
    let edited = water.replace("depth = 1.0", "depth = 2.0");
    let done = edit("res://water.gdshader", "depth = 1.0", "depth = 2.0")
        .await
        .expect("edit water");
    assert_eq!(done["replaced"], 1);
    assert_eq!(done["source"], "editor");
    assert_eq!(done["version"], script_label(&edited));
    assert_eq!(file("water.gdshader"), edited);
    assert_eq!(
        uid("water.gdshader"),
        uid_before,
        "the shader keeps its uid"
    );
    let shown = state("res://water.gdshader").await;
    assert_eq!(shown["found"], true, "the edit opened the shader's tab");
    assert_eq!(shown["text"], edited);
    assert_eq!(shown["dirty"], false, "the edit is saved");
    assert_eq!(shown["memory_code"], edited);
    let undone = bridge
        .send_command("__shader_undo", json!({"path": "res://water.gdshader"}))
        .await
        .expect("undo");
    assert_eq!(undone["text"], water, "one undo step reverses the edit");

    // Unsaved typing is read, and never overwritten by an edit or a create.
    bridge
        .send_command("edit_shader", json!({"shader_path": "res://typed.gdshader", "old_text": "vec4(1.0)", "new_text": "vec4(0.5)"}))
        .await
        .expect("open typed by editing it");
    let saved_typed = file("typed.gdshader");
    bridge
        .send_command(
            "__shader_type",
            json!({"path": "res://typed.gdshader", "text": "// typed\n"}),
        )
        .await
        .expect("type");
    let dirty = bridge
        .send_command(
            "read_shader",
            json!({"shader_path": "res://typed.gdshader"}),
        )
        .await
        .expect("read typed");
    assert_eq!(dirty["source"], "editor");
    assert_eq!(dirty["dirty"], true);
    assert!(
        dirty["text"]
            .as_str()
            .unwrap_or("")
            .starts_with("// typed\n")
    );
    expect_error(
        edit("res://typed.gdshader", "vec4(0.5)", "vec4(0.25)").await,
        "unsaved changes",
    );
    expect_error(
        bridge
            .send_command(
                "create_shader",
                json!({"shader_path": "res://typed.gdshader", "code": "x", "overwrite": true}),
            )
            .await,
        "unsaved changes",
    );
    assert_eq!(file("typed.gdshader"), saved_typed, "the file is untouched");

    // An overwrite reloads a clean open tab and the editor's shader, and keeps the uid.
    edit("res://clean.gdshader", "vec4(1.0)", "vec4(0.9)")
        .await
        .expect("open clean by editing it");
    let clean_uid = uid("clean.gdshader");
    let replacement =
        "shader_type canvas_item;\n\nvoid fragment() {\n\tCOLOR = vec4(0.0, 0.0, 1.0, 1.0);\n}\n";
    let overwritten = bridge
        .send_command(
            "create_shader",
            json!({"shader_path": "res://clean.gdshader", "code": replacement, "overwrite": true}),
        )
        .await
        .expect("overwrite clean");
    assert_eq!(overwritten["created"], false);
    assert_eq!(overwritten["tab_reloaded"], true);
    assert_eq!(file("clean.gdshader"), replacement);
    assert_eq!(uid("clean.gdshader"), clean_uid);
    let reloaded = state("res://clean.gdshader").await;
    assert_eq!(reloaded["text"], replacement);
    assert_eq!(reloaded["dirty"], false);
    assert_eq!(reloaded["memory_code"], replacement);

    // Refusals reply with a fix.
    expect_error(
        edit("res://ripple.gdshader", "a", "b").await,
        "godot_create_shader",
    );
    expect_error(
        edit("res://fx/ripple.gdshader", "nothing", "b").await,
        "0 times",
    );
    expect_error(edit("res://notes.txt", "hello", "bye").await, ".gdshader");
    expect_error(
        edit("res://../x.gdshader", "a", "b").await,
        "inside the project",
    );
    expect_error(
        bridge
            .send_command(
                "create_shader",
                json!({"shader_path": "res://x.gdshader", "code": 5}),
            )
            .await,
        "must be of type",
    );
    assert_eq!(file("notes.txt"), "hello\n");
}

/// The shader check compiles a saved shader with the engine's compiler and names the file and
/// line of each error, including one inside an include; a clean shader passes.
#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn validate_shader_compiles_the_file_with_godot() {
    use godot_mcp::cli_runner::CliRunner;
    use godot_mcp::tools::ToolManager;
    use std::sync::Arc;

    let (godot, _) = godot_binaries();
    let project = common::project_dir();
    let write = |name: &str, text: &str| {
        let path = project.path().join(name);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("dir");
        std::fs::write(path, text).expect("write shader");
    };
    write(
        "good.gdshader",
        "shader_type spatial;\n#include \"lib/ok.gdshaderinc\"\n\nvoid fragment() {\n\tALBEDO = vec3(helper());\n}\n",
    );
    write("lib/ok.gdshaderinc", "float helper() {\n\treturn 1.0;\n}\n");
    write(
        "bad.gdshader",
        "shader_type canvas_item;\n\nvoid fragment() {\n\tCOLOR = vec4(foo);\n}\n",
    );
    write(
        "bad_include.gdshader",
        "shader_type spatial;\n#include \"lib/broken.gdshaderinc\"\nvoid fragment() {}\n",
    );
    write(
        "lib/broken.gdshaderinc",
        "float broken() {\n\treturn 1.0\n}\n",
    );
    write(
        "missing_include.gdshader",
        "shader_type spatial;\n#include \"lib/none.gdshaderinc\"\nvoid fragment() {}\n",
    );
    write("no_type.gdshader", "void fragment() {}\n");

    let tools = Arc::new(ToolManager::new(
        BridgeClient::new(common::free_port()),
        CliRunner::new(godot),
    ));
    let validate = |path: &str| {
        let tools = tools.clone();
        let args = json!({"shader_path": path, "project_path": project.path()});
        async move { tools.call_tool("godot_validate_shader", args).await }
    };

    let good = validate("res://good.gdshader").await;
    assert!(!good.is_error, "{}", tool_text(&good));
    let answer: Value = serde_json::from_str(&tool_text(&good)).expect("json answer");
    assert_eq!(answer["valid"], true);

    for (path, needles) in [
        ("res://bad.gdshader", vec!["res://bad.gdshader:4:", "foo"]),
        (
            "res://bad_include.gdshader",
            // Godot notices the missing ';' at the token after it, on the next line.
            vec!["res://lib/broken.gdshaderinc:3:", "';'"],
        ),
        (
            "res://missing_include.gdshader",
            vec!["res://missing_include.gdshader:2:", "#include path"],
        ),
        ("res://no_type.gdshader", vec!["shader_type"]),
        ("res://lib/ok.gdshaderinc", vec!["includes it"]),
        ("res://absent.gdshader", vec!["godot_create_shader"]),
    ] {
        let result = validate(path).await;
        let text = tool_text(&result);
        assert!(result.is_error, "{path} should fail: {text}");
        for needle in needles {
            assert!(
                text.contains(needle),
                "{path}: {text:?} should mention {needle:?}"
            );
        }
    }
}

#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ editor executable"]
async fn import_params_are_set_and_reimported_through_the_editor() {
    use godot_mcp::cli_runner::CliRunner;
    use godot_mcp::tools::ToolManager;
    use std::sync::Arc;

    let icon = "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"16\" height=\"16\"><rect width=\"16\" height=\"16\" fill=\"#3a3\"/></svg>\n";
    let editor = start_editor_with(None, "", &[("sky.svg", icon), ("far.svg", icon)], "").await;
    let project = editor.project.clone();
    let (godot, _) = godot_binaries();
    let tools = Arc::new(
        ToolManager::new(BridgeClient::new(editor.port), CliRunner::new(godot))
            .with_working_dir(project.clone()),
    );
    let call = |name: &'static str, args: Value| {
        let tools = Arc::clone(&tools);
        async move {
            let result = tools.call_tool(name, args).await;
            let text = tool_text(&result);
            assert!(!result.is_error, "{name}: {text}");
            serde_json::from_str::<Value>(&text).expect("JSON report")
        }
    };

    let read = call("godot_get_import_params", json!({"path": "res://sky.svg"})).await;
    assert_eq!(read["source"], "editor");
    assert_eq!(read["params"]["compress/mode"], 0, "{read}");

    let set = call(
        "godot_set_import_params",
        json!({"path": "res://sky.svg",
               "params": {"compress/mode": 2, "compress/high_quality": true, "typo/mode": 1}}),
    )
    .await;
    assert_eq!(set["source"], "editor");
    assert_eq!(set["reimported"], true, "{set}");
    assert_eq!(set["ignored_options"], json!(["typo/mode"]));
    assert_eq!(set["after"]["params"]["compress/mode"], 2);
    let sidecar = std::fs::read_to_string(project.join("sky.svg.import")).expect("sidecar");
    assert!(
        sidecar.contains("\"vram_texture\": true"),
        "the editor reimported the texture: {sidecar}"
    );
    assert!(!sidecar.contains("typo/mode"), "{sidecar}");

    let copied = call(
        "godot_set_import_params",
        json!({"path": "res://far.svg", "copy_from": "res://sky.svg"}),
    )
    .await;
    assert_eq!(copied["reimported"], true, "{copied}");
    assert_eq!(copied["after"]["params"]["compress/high_quality"], true);
    let far = std::fs::read_to_string(project.join("far.svg.import")).expect("sidecar");
    assert!(far.contains("\"vram_texture\": true"), "{far}");

    let again = call(
        "godot_set_import_params",
        json!({"path": "res://far.svg", "params": {"compress/mode": 2}}),
    )
    .await;
    assert_eq!(again["changed"], json!([]));
    assert_eq!(again["reimported"], false, "{again}");
}

#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ editor executable"]
async fn an_image_is_imported_as_a_texture_3d_through_the_editor() {
    use godot_mcp::cli_runner::CliRunner;
    use godot_mcp::tools::ToolManager;
    use std::sync::Arc;

    let strip = "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"64\" height=\"16\"><rect width=\"64\" height=\"16\" fill=\"#3a3\"/></svg>\n";
    let editor = start_editor_with(None, "", &[("strip.svg", strip)], "").await;
    let project = editor.project.clone();
    let (godot, _) = godot_binaries();
    let tools = Arc::new(
        ToolManager::new(BridgeClient::new(editor.port), CliRunner::new(godot))
            .with_working_dir(project.clone()),
    );
    let result = tools
        .call_tool(
            "godot_set_import_params",
            json!({"path": "res://strip.svg", "importer": "3d_texture",
                   "params": {"slices/horizontal": 4}}),
        )
        .await;
    let text = tool_text(&result);
    assert!(!result.is_error, "{text}");
    let set: Value = serde_json::from_str(&text).expect("JSON report");
    assert_eq!(set["source"], "editor");
    assert_eq!(set["reimported"], true, "{set}");
    assert_eq!(set["previous_importer"], "texture");
    assert_eq!(set["type"], "CompressedTexture3D", "{set}");
    assert_eq!(set["after"]["params"]["slices/horizontal"], 4);
    let sidecar = std::fs::read_to_string(project.join("strip.svg.import")).expect("sidecar");
    assert!(sidecar.contains("importer=\"3d_texture\""), "{sidecar}");
    assert!(sidecar.contains(".ctex3d"), "{sidecar}");
}

/// An editor whose addon lacks the shader commands, as one older than the server does: the
/// error names both versions, and the shader tools work on the file instead.
#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ editor executable"]
async fn an_older_addon_is_named_and_shaders_fall_back_to_their_file() {
    use godot_mcp::cli_runner::CliRunner;
    use godot_mcp::tools::ToolManager;
    use std::sync::Arc;

    let hook = "        \"read_shader\", \"edit_shader\", \"get_scene_tree\":\n            send_error(peer, cmd_id, \"Unknown command type: \" + cmd_type)\n        \"ping\":\n";
    let shader = "shader_type spatial;\n\nuniform float sway = 1.0;\n";
    let editor = start_editor_with(Some(hook), "", &[("wind.gdshader", shader)], "").await;
    let project = editor.project.clone();
    let info = editor
        .bridge
        .send_command("get_project_info", json!({}))
        .await
        .expect("project info");
    assert_eq!(info["addon_version"], env!("CARGO_PKG_VERSION"), "{info}");

    let (godot, _) = godot_binaries();
    let tools = Arc::new(
        ToolManager::new(BridgeClient::new(editor.port), CliRunner::new(godot))
            .with_working_dir(project.clone()),
    );
    let tree = Arc::clone(&tools)
        .call_tool("godot_get_scene_tree", json!({}))
        .await;
    assert!(tree.is_error);
    let message = tool_text(&tree);
    assert!(message.contains("get_scene_tree"), "{message}");
    assert!(message.contains("godot_update_addon"), "{message}");

    let read = Arc::clone(&tools)
        .call_tool(
            "godot_read_shader",
            json!({"shader_path": "res://wind.gdshader"}),
        )
        .await;
    assert!(!read.is_error, "{}", tool_text(&read));
    assert!(
        tool_text(&read).contains("uniform float sway"),
        "{}",
        tool_text(&read)
    );
    assert!(
        tool_text(&read).contains("addon_version"),
        "{}",
        tool_text(&read)
    );

    let edit = Arc::clone(&tools)
        .call_tool(
            "godot_edit_shader",
            json!({"shader_path": "res://wind.gdshader",
                   "old_text": "uniform float sway = 1.0;",
                   "new_text": "uniform float sway = 1.0;\nuniform float gust = 0.5;"}),
        )
        .await;
    assert!(!edit.is_error, "{}", tool_text(&edit));
    let saved = std::fs::read_to_string(project.join("wind.gdshader")).expect("shader");
    assert!(saved.contains("uniform float gust = 0.5;"), "{saved}");

    let update = tools.call_tool("godot_update_addon", json!({})).await;
    assert!(
        update.is_error,
        "the editor runs the addon: {}",
        tool_text(&update)
    );
    assert!(
        tool_text(&update).contains("restart_editor: true"),
        "{}",
        tool_text(&update)
    );
}
