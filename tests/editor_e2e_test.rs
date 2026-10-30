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
    let _ = dotenvy::dotenv();
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
        .env("GODOT_DEBUG_PORT", port.to_string())
        .stdout(Stdio::from(log_file.try_clone().expect("clone log")))
        .stderr(Stdio::from(log_file))
        .kill_on_drop(true)
        .spawn()
        .expect("spawn godot editor");

    let bridge = BridgeClient::new(port);
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        if bridge.send_command("ping", json!({})).await.is_ok() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "editor bridge did not come up; log:\n{}",
            std::fs::read_to_string(&log).unwrap_or_default()
        );
        sleep(Duration::from_millis(500)).await;
    }

    Editor {
        _dir: dir,
        project,
        port,
        bridge,
        log,
        _child: child,
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
        Err(McpError::ToolExecution(message)) => assert!(
            message.contains(needle),
            "error {message:?} should mention {needle:?}"
        ),
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
    let reloaded = call("open_scene", json!({"scene_path": "res://stale.tscn"}))
        .await
        .expect("reload stale");
    assert_eq!(reloaded["reloaded"], true, "{reloaded}");
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

    // Project settings: a scalar is saved and the old value comes back; a structure, or a value
    // of the wrong type for an existing setting, is refused and changes nothing.
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
        "string, number or boolean",
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

/// A scene whose own node `A` is added under an instance that already has a child `A`. Godot
/// renames the incoming node while loading and reports it only in its "Load Errors" dialog.
const CLASH_SUB: &str = "[gd_scene format=3]\n\n[node name=\"Sub\" type=\"Node2D\"]\n\n[node name=\"A\" type=\"Node\" parent=\".\"]\n";
const CLASH_SCENE: &str = "[gd_scene load_steps=2 format=3]\n\n[ext_resource type=\"PackedScene\" path=\"res://sub.tscn\" id=\"1\"]\n\n[node name=\"Clash\" type=\"Node2D\"]\n\n[node name=\"Sub\" parent=\".\" instance=ExtResource(\"1\")]\n\n[node name=\"A\" type=\"Node\" parent=\"Sub\"]\n";

#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
async fn open_scene_returns_what_the_load_errors_dialog_shows() {
    let editor = start_editor_with(
        None,
        "",
        &[("sub.tscn", CLASH_SUB), ("clash.tscn", CLASH_SCENE)],
        "",
    )
    .await;
    let bridge = &editor.bridge;

    let clean = bridge
        .send_command("open_scene", json!({"scene_path": "res://main.tscn"}))
        .await
        .expect("open main");
    assert_eq!(clean["load_errors"], json!([]), "{clean}");
    assert_eq!(clean["unsaved"], false, "{clean}");
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
    assert!(
        message.starts_with("An incoming node's name clashes with Sub/A"),
        "{clash}"
    );
    assert!(
        message.ends_with("Please fix and re-save the scene."),
        "{clash}"
    );
    assert_eq!(clash["dialog"], "Load Errors", "{clash}");
    assert_eq!(clash["unsaved"], true, "{clash}");
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
    assert_eq!(reloaded["dialog"], "Load Errors", "{reloaded}");
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
/// refused; a script with its .uid, an imported image with its .import and imported data, and an
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
    let read = props().await;
    assert_eq!(read["loose"]["value"], "[\"x\", 1.0]", "{read}");
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
