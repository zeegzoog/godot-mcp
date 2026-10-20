//! End-to-end tests against a real headless Godot editor with the addon enabled.
//!
//! Ignored by default because they need a Godot 4.5+ executable. Run them with:
//!
//! ```bash
//! GODOT_PATH=/path/to/godot cargo test --test editor_e2e_test -- --ignored
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
    let (godot, console) = godot_binaries();
    let dir = tempfile::tempdir().expect("project dir");
    let project = dir.path().to_path_buf();

    std::fs::write(
        project.join("project.godot"),
        "config_version=5\n\n[application]\n\nconfig/name=\"MCP E2E Fixture\"\n\
         run/main_scene=\"res://main.tscn\"\nconfig/features=PackedStringArray(\"4.5\")\n\n\
         [editor_plugins]\n\nenabled=PackedStringArray(\"res://addons/godot_mcp/plugin.cfg\")\n",
    )
    .expect("project.godot");
    std::fs::write(project.join("main.tscn"), MAIN_SCENE).expect("main.tscn");
    std::fs::write(project.join("script.gd"), "extends Node\n").expect("script.gd");

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
    if with_undo_hook {
        let script_path = addon.join("bridge_server.gd");
        let source = std::fs::read_to_string(&script_path).expect("bridge script");
        let anchor = "        \"ping\":\n";
        assert!(source.contains(anchor), "dispatch table anchor not found");
        std::fs::write(&script_path, source.replacen(anchor, UNDO_HOOK, 1))
            .expect("patch bridge script");
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

    // open_scene accepts a project file given as an OS path but nothing outside the project.
    let absolute = editor.project.join("main.tscn");
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
}
