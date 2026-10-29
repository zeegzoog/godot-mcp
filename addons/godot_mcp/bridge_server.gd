@tool
extends Node

## WebSocket bridge between the Godot MCP server and this editor.
##
## Deliberately has no class_name: plugin.gd loads it by path, and a global class would be
## listed in every exported game's class list even though the addon's files are left out.
##
## Listens on loopback only and requires the shared token (see prepare_token) on every
## command, so neither other machines nor web pages can drive the editor.

## Port on which the WebSocket bridge server listens.
const DEFAULT_PORT: int = 9333
const LOOPBACK_ADDRESS: String = "127.0.0.1"

## Buffer sizes are powers of two because the engine rounds them up. The defaults (64 KiB)
## are far too small for scene trees and screenshots.
const INBOUND_BUFFER_SIZE: int = 1 << 22
const OUTBOUND_BUFFER_SIZE: int = 1 << 25

const MAX_VALUE_CHARS: int = 2000
## A full scan with wait, or the first import of a file the editor has never scanned, blocks
## the call for at most this long. The MCP client waits slightly longer than this.
const RESAVE_WAIT_MSEC: int = 60000
const SCRIPT_OPS_PATH: String = "res://addons/godot_mcp/editor/script_ops.gd"
const FILE_OPS_PATH: String = "res://addons/godot_mcp/editor/file_ops.gd"
## The running game's property ops. The editor reuses their ':' resource-step resolver, so a
## property path means the same in godot_set_node_properties as in godot_game_set.
const RtLive: GDScript = preload("res://addons/godot_mcp/runtime/ops/rt_live.gd")
const MAX_CAPTURE_DIMENSION: int = 1568
const MIN_CAPTURE_SIZE: int = 16

## Extensions the editor imports (a .import file is generated next to them).
const IMPORTED_EXTENSIONS: PackedStringArray = [
    "png", "jpg", "jpeg", "webp", "svg", "bmp", "tga", "exr", "hdr", "dds", "ktx",
    "wav", "ogg", "mp3", "glb", "gltf", "fbx", "obj", "ttf", "otf", "woff", "woff2",
]

## Expected JSON type of each parameter, checked before a handler runs so that a wrongly typed
## value produces an error response instead of a script error that leaves the caller waiting.
const PARAM_TYPES: Dictionary = {
    "get_scene_tree": {"scene_path": TYPE_STRING},
    "add_node": {"parent_node_path": TYPE_STRING, "node_type": TYPE_STRING, "node_name": TYPE_STRING},
    "remove_node": {"node_path": TYPE_STRING},
    "get_node_properties": {"node_path": TYPE_STRING, "properties": TYPE_ARRAY},
    "set_node_properties": {"node_path": TYPE_STRING, "properties": TYPE_DICTIONARY},
    "capture_viewport": {"view": TYPE_STRING, "max_dimension": TYPE_FLOAT},
    "open_scene": {"scene_path": TYPE_STRING, "reload": TYPE_BOOL, "discard": TYPE_BOOL},
    "run_project": {"scene_path": TYPE_STRING},
    "resave_resources": {"paths": TYPE_ARRAY, "wait": TYPE_BOOL},
    "create_scene": {"scene_path": TYPE_STRING, "root_type": TYPE_STRING, "root_name": TYPE_STRING},
    "attach_script": {"node_path": TYPE_STRING, "script_path": TYPE_STRING},
    "read_script": {"script_path": TYPE_STRING},
    "script_tab": {"script_path": TYPE_STRING, "reload": TYPE_BOOL},
    "edit_script": {"script_path": TYPE_STRING, "old_text": TYPE_STRING, "new_text": TYPE_STRING, "version": TYPE_STRING},
    "connect_signal": {"node_path": TYPE_STRING, "signal_name": TYPE_STRING, "method": TYPE_STRING, "target_path": TYPE_STRING},
    "list_signals": {"node_path": TYPE_STRING},
    "create_resource": {"resource_path": TYPE_STRING, "class_name": TYPE_STRING},
    "set_project_setting": {"key": TYPE_STRING},
    "get_project_settings": {"prefix": TYPE_STRING},
    "get_input_map": {"action": TYPE_STRING, "include_builtin": TYPE_BOOL},
    "edit_input_action": {"action": TYPE_STRING, "add_events": TYPE_ARRAY, "remove_events": TYPE_ARRAY, "deadzone": TYPE_FLOAT, "create": TYPE_BOOL, "delete": TYPE_BOOL},
    "delete_files": {"paths": TYPE_ARRAY, "force": TYPE_BOOL, "allow_empty_dir": TYPE_BOOL, "dry_run": TYPE_BOOL, "trash": TYPE_BOOL},
    "game_send": {"command": TYPE_STRING},
}

## Fields accepted when a vector-like value is given as a JSON object.
const OBJECT_FIELDS: Dictionary = {
    TYPE_VECTOR2: ["x", "y"],
    TYPE_VECTOR2I: ["x", "y"],
    TYPE_VECTOR3: ["x", "y", "z"],
    TYPE_VECTOR3I: ["x", "y", "z"],
    TYPE_VECTOR4: ["x", "y", "z", "w"],
    TYPE_VECTOR4I: ["x", "y", "z", "w"],
    TYPE_QUATERNION: ["x", "y", "z", "w"],
    TYPE_COLOR: ["r", "g", "b", "a"],
}

var server: TCPServer = TCPServer.new()
var peers: Array[WebSocketPeer] = []
var undo_redo: EditorUndoRedoManager = null
var game_relay: EditorDebuggerPlugin = null
var port: int = DEFAULT_PORT
var verbose: bool = false
var _script_ops_instance: RefCounted = null
var _file_ops_instance: RefCounted = null
## scene res:// path -> hash of the file bytes last loaded or saved by this bridge.
## Compared by content, not mtime, so an edit in the same second is still detected.
var _scene_sync_hash: Dictionary = {}
var _open_scene_paths: PackedStringArray = []
## Scenes changed through this bridge since the last save. External disk edits of a dirty
## scene are not reloaded unless the caller passes discard.
var _scene_dirty: Dictionary = {}
## UndoRedo.get_version() captured when the scene last matched disk. Godot does not
## expose EditorUndoRedoManager.is_history_unsaved to GDScript, so a newer version
## means an edit the editor made (or this bridge made) has not been saved.
var _scene_undo_version: Dictionary = {}

var _token: String = ""
var _token_file: String = ""

func _ready() -> void:
    read_configuration()
    start_server()

func read_configuration() -> void:
    verbose = bool(ProjectSettings.get_setting("godot_mcp/verbose", false))
    # Read custom port from project settings if defined
    if ProjectSettings.has_setting("godot_mcp/bridge_port"):
        port = int(ProjectSettings.get_setting("godot_mcp/bridge_port"))
    elif OS.has_environment("GODOT_DEBUG_PORT"):
        var env_port: String = OS.get_environment("GODOT_DEBUG_PORT")
        if env_port.is_valid_int():
            port = int(env_port)

## Shows an error in the editor itself, not only in the Output panel, where a person may not look.
func _toast_error(message: String) -> void:
    if not EditorInterface.has_method("get_editor_toaster"):
        return
    var toaster: Object = EditorInterface.call("get_editor_toaster")
    if toaster != null:
        toaster.call("push_toast", message, 2)

func start_server() -> void:
    var err: Error = server.listen(port, LOOPBACK_ADDRESS)
    if err != OK:
        var failure: String = "Failed to start bridge server on port %d: %s." % [port, error_string(err)]
        if err == ERR_ALREADY_IN_USE:
            failure += " Probably another editor already uses this port; a godot-mcp server connected to it talks to that editor, not to this one. Set the project setting godot_mcp/bridge_port or the environment variable GODOT_DEBUG_PORT to a free port."
        printerr("[Godot MCP] ", failure)
        _toast_error(failure)
        return
    prepare_token()
    _index_open_scenes()
    print("[Godot MCP] Bridge server listening on ", LOOPBACK_ADDRESS, ":", port)

## The MCP server finds the token in <user data dir>/godot-mcp/bridge-<port>.token (readable
## only by the current user), or in GODOT_MCP_TOKEN if that is set for both processes.
func prepare_token() -> void:
    var env_token: String = OS.get_environment("GODOT_MCP_TOKEN")
    if not env_token.is_empty():
        _token = env_token
        return

    _token = Crypto.new().generate_random_bytes(24).hex_encode()
    var token_file: String = OS.get_data_dir().path_join("godot-mcp").path_join("bridge-%d.token" % port)
    DirAccess.make_dir_recursive_absolute(token_file.get_base_dir())
    var file: FileAccess = FileAccess.open(token_file, FileAccess.WRITE)
    if file == null:
        printerr("[Godot MCP] Cannot write the bridge token to ", token_file, ": ", error_string(FileAccess.get_open_error()))
        return
    file.store_string(_token)
    file.close()
    FileAccess.set_unix_permissions(token_file, FileAccess.UNIX_READ_OWNER | FileAccess.UNIX_WRITE_OWNER)
    _token_file = token_file

func stop_server() -> void:
    for peer in peers:
        peer.close()
    peers.clear()
    server.stop()
    if not _token_file.is_empty():
        DirAccess.remove_absolute(_token_file)
        _token_file = ""
    print("[Godot MCP] Bridge server stopped")

func _process(_delta: float) -> void:
    _track_open_scenes()
    if not server.is_listening():
        return

    # Check for incoming TCP connections and upgrade to WebSocket
    while server.is_connection_available():
        var tcp_conn: StreamPeerTCP = server.take_connection()
        if tcp_conn != null:
            var ws_peer: WebSocketPeer = WebSocketPeer.new()
            ws_peer.inbound_buffer_size = INBOUND_BUFFER_SIZE
            ws_peer.outbound_buffer_size = OUTBOUND_BUFFER_SIZE
            var err: Error = ws_peer.accept_stream(tcp_conn)
            if err == OK:
                peers.append(ws_peer)
                log_verbose("Client connected")
            else:
                printerr("[Godot MCP] Failed to accept WebSocket connection: ", error_string(err))

    # Poll connected peers
    var i: int = peers.size() - 1
    while i >= 0:
        var peer: WebSocketPeer = peers[i]
        peer.poll()
        var state: WebSocketPeer.State = peer.get_ready_state()

        if state == WebSocketPeer.STATE_OPEN:
            while peer.get_available_packet_count() > 0:
                var packet: PackedByteArray = peer.get_packet()
                handle_message(peer, packet.get_string_from_utf8())
        elif state == WebSocketPeer.STATE_CLOSED:
            peers.remove_at(i)
            log_verbose("Client disconnected")

        i -= 1

func log_verbose(message: String) -> void:
    if verbose:
        print("[Godot MCP] ", message)

func handle_message(peer: WebSocketPeer, raw_msg: String) -> void:
    var json: JSON = JSON.new()
    if json.parse(raw_msg) != OK:
        send_error(peer, "", "Invalid JSON format: " + json.get_error_message())
        return

    if not json.data is Dictionary:
        send_error(peer, "", "Message payload must be a JSON dictionary")
        return
    var message: Dictionary = json.data

    # Values are read untyped and checked by hand: assigning a wrongly typed value to a typed
    # variable raises a script error that aborts this function before any reply is sent.
    var raw_id: Variant = message.get("commandId", "")
    var cmd_id: String = raw_id if raw_id is String else str(raw_id)

    var token: Variant = message.get("token", null)
    if _token.is_empty() or not token is String or token != _token:
        send_error(peer, cmd_id, "Unauthorized: missing or invalid bridge token. Restart the MCP client so it re-reads the token, and run it as the same OS user as the editor (or set the same GODOT_MCP_TOKEN for both).")
        return

    var raw_type: Variant = message.get("type", "")
    if not raw_type is String:
        send_error(peer, cmd_id, "Field 'type' must be a string, got: " + type_string(typeof(raw_type)))
        return
    var cmd_type: String = raw_type

    var raw_params: Variant = message.get("params", {})
    if raw_params == null:
        raw_params = {}
    if not raw_params is Dictionary:
        send_error(peer, cmd_id, "Field 'params' must be a JSON object, got: " + type_string(typeof(raw_params)))
        return
    var params: Dictionary = raw_params

    var problem: String = validate_params(cmd_type, params)
    if not problem.is_empty():
        send_error(peer, cmd_id, problem)
        return

    dispatch_command(peer, cmd_id, cmd_type, params)

## Returns a description of the first wrongly typed parameter, or "" if all are fine.
## JSON null counts as "not provided".
func validate_params(cmd_type: String, params: Dictionary) -> String:
    var spec: Dictionary = PARAM_TYPES.get(cmd_type, {})
    for key: String in spec:
        if not params.has(key):
            continue
        var value: Variant = params[key]
        if value == null:
            params.erase(key)
            continue
        var expected: int = spec[key]
        var actual: int = typeof(value)
        var numeric_ok: bool = expected == TYPE_FLOAT and actual == TYPE_INT
        if actual != expected and not numeric_ok:
            return "Parameter '%s' must be of type %s, got %s" % [key, type_string(expected), type_string(actual)]
    return ""

func dispatch_command(peer: WebSocketPeer, cmd_id: String, cmd_type: String, params: Dictionary) -> void:
    match cmd_type:
        "ping":
            send_success(peer, cmd_id, {"pong": true, "timestamp": Time.get_unix_time_from_system()})
        "get_version":
            get_version(peer, cmd_id)
        "get_project_info":
            get_project_info(peer, cmd_id)
        "get_scene_tree":
            get_scene_tree(peer, cmd_id, params)
        "get_open_scenes":
            get_open_scenes(peer, cmd_id)
        "add_node":
            add_node(peer, cmd_id, params)
        "remove_node":
            remove_node(peer, cmd_id, params)
        "get_node_properties":
            get_node_properties(peer, cmd_id, params)
        "set_node_properties":
            set_node_properties(peer, cmd_id, params)
        "capture_viewport":
            capture_viewport(peer, cmd_id, params)
        "open_scene":
            open_scene(peer, cmd_id, params)
        "save_scene":
            save_scene(peer, cmd_id)
        "run_project":
            run_project(peer, cmd_id, params)
        "stop_project":
            stop_project(peer, cmd_id)
        "get_run_status":
            get_run_status(peer, cmd_id)
        "resave_resources":
            resave_resources(peer, cmd_id, params)
        "create_scene":
            create_scene(peer, cmd_id, params)
        "attach_script":
            attach_script(peer, cmd_id, params)
        "read_script":
            read_script(peer, cmd_id, params)
        "edit_script":
            edit_script(peer, cmd_id, params)
        "script_tab":
            script_tab(peer, cmd_id, params)
        "connect_signal":
            connect_signal(peer, cmd_id, params)
        "list_signals":
            list_signals(peer, cmd_id, params)
        "create_resource":
            create_resource(peer, cmd_id, params)
        "set_project_setting":
            set_project_setting(peer, cmd_id, params)
        "get_project_settings":
            get_project_settings(peer, cmd_id, params)
        "get_input_map":
            get_input_map(peer, cmd_id, params)
        "edit_input_action":
            edit_input_action(peer, cmd_id, params)
        "delete_files":
            delete_files(peer, cmd_id, params)
        "game_send":
            game_send(peer, cmd_id, params)
        "game_status":
            game_status(peer, cmd_id)
        _:
            send_error(peer, cmd_id, "Unknown command type: " + cmd_type)

# --- Helpers ---------------------------------------------------------------------------

## Converts a user-supplied path to a clean res:// path, or "" if it is not inside the project.
## Absolute OS paths inside the project are accepted and converted.
func to_res_path(raw_path: String) -> String:
    var path: String = raw_path.strip_edges().replace("\\", "/")
    if path.is_empty():
        return ""
    if not path.begins_with("res://"):
        path = ProjectSettings.localize_path(path)
    if not path.begins_with("res://"):
        # macOS: /var is a symlink to /private/var, and localize_path misses /var.
        path = _res_path_by_prefix(raw_path.strip_edges().replace("\\", "/"))
    if not path.begins_with("res://"):
        return ""
    var relative: String = path.trim_prefix("res://")
    if relative.is_empty() or ".." in relative.split("/"):
        return ""
    return "res://" + relative

func _res_path_by_prefix(absolute: String) -> String:
    if not absolute.begins_with("/"):
        return ""
    var project := ProjectSettings.globalize_path("res://").simplify_path().trim_suffix("/")
    var file := absolute.simplify_path()
    var files: Array[String] = [file]
    var roots: Array[String] = [project]
    if file.begins_with("/var/"):
        files.append("/private" + file)
    elif file.begins_with("/private/var/"):
        files.append(file.trim_prefix("/private"))
    if project.begins_with("/var/"):
        roots.append("/private" + project)
    elif project.begins_with("/private/var/"):
        roots.append(project.trim_prefix("/private"))
    for root: String in roots:
        for item: String in files:
            if item == root:
                return "res://"
            if item.begins_with(root + "/"):
                return "res://" + item.trim_prefix(root + "/")
    return ""

func scene_root_or_error(peer: WebSocketPeer, cmd_id: String) -> Node:
    var root: Node = EditorInterface.get_edited_scene_root()
    if root == null:
        send_error(peer, cmd_id, "No active scene open in the editor")
    return root

## Resolves a node inside the edited scene. Paths that leave the scene ("..", absolute paths
## into the editor UI) resolve to null.
func find_scene_node(root: Node, node_path: String) -> Node:
    if node_path.is_empty() or node_path == ".":
        return root
    var node: Node = root.get_node_or_null(NodePath(node_path))
    if node != null and (node == root or root.is_ancestor_of(node)):
        return node
    return null

func property_type_name(info: Dictionary) -> String:
    var type_id: int = info["type"]
    if type_id == TYPE_OBJECT:
        var object_class: String = info.get("class_name", "")
        return object_class if not object_class.is_empty() else "Object"
    return type_string(type_id)

## Text form of a value in Godot's own syntax, shortened if it is huge.
func describe_value(value: Variant) -> String:
    var text: String
    if typeof(value) == TYPE_OBJECT:
        if value == null:
            text = "null"
        elif not is_instance_valid(value):
            text = "<freed object>"
        elif value is Resource and not (value as Resource).resource_path.is_empty():
            text = 'Resource("%s")' % (value as Resource).resource_path
        else:
            text = "<%s>" % value.get_class()
    else:
        text = var_to_str(value)
    if text.length() > MAX_VALUE_CHARS:
        text = text.substr(0, MAX_VALUE_CHARS) + "... (%d more characters)" % (text.length() - MAX_VALUE_CHARS)
    return text

# --- Project & scenes ------------------------------------------------------------------

func get_version(peer: WebSocketPeer, cmd_id: String) -> void:
    var info: Dictionary = Engine.get_version_info()
    send_success(peer, cmd_id, {
        "version_string": info["string"],
        "major": info["major"],
        "minor": info["minor"],
        "patch": info["patch"],
        "status": info["status"]
    })

func get_project_info(peer: WebSocketPeer, cmd_id: String) -> void:
    var project_name: String = ProjectSettings.get_setting("application/config/name", "")
    var main_scene: String = ProjectSettings.get_setting("application/run/main_scene", "")
    send_success(peer, cmd_id, {
        "name": project_name,
        "main_scene": main_scene,
        "features": ProjectSettings.get_setting("application/config/features", []),
        "project_dir": ProjectSettings.globalize_path("res://").trim_suffix("/")
    })

func get_scene_tree(peer: WebSocketPeer, cmd_id: String, params: Dictionary) -> void:
    var requested: String = params.get("scene_path", "")
    var root: Node = null
    var scene_path: String = ""

    if requested.is_empty():
        root = scene_root_or_error(peer, cmd_id)
        if root == null:
            return
        scene_path = String(root.scene_file_path)
    else:
        scene_path = to_res_path(requested)
        if scene_path.is_empty():
            send_error(peer, cmd_id, "Scene path must be a res:// path inside the project: " + requested)
            return
        # get_open_scenes() and get_open_scene_roots() are parallel arrays
        # (same order, same length) per the Godot editor API.
        var open_paths: PackedStringArray = EditorInterface.get_open_scenes()
        var idx: int = open_paths.find(scene_path)
        if idx == -1:
            send_error(peer, cmd_id, "Scene is not currently open in the editor: " + scene_path + ". Use get_open_scenes to list open scenes, or open_scene to open it first.")
            return
        root = EditorInterface.get_open_scene_roots()[idx]

    # A read must not change the editor. When the file on disk differs, say so and
    # leave the reload to open_scene.
    var payload := {
        "tree": serialize_node(root, root),
        "scene_path": scene_path,
        "reloaded": false,
    }
    if not scene_path.is_empty() and FileAccess.file_exists(scene_path):
        var decision: Dictionary = _reload_decision(scene_path, false, false)
        if decision.has("error") or bool(decision.get("reload", false)):
            payload["stale"] = true
            payload["warning"] = str(decision.get("error", "The file on disk differs from the open scene. Open it with godot_open_scene to reload; this read leaves the editor unchanged."))
    send_success(peer, cmd_id, payload)

func get_open_scenes(peer: WebSocketPeer, cmd_id: String) -> void:
    var root: Node = EditorInterface.get_edited_scene_root()
    send_success(peer, cmd_id, {
        "open_scenes": EditorInterface.get_open_scenes(),
        "active_scene": root.scene_file_path if root != null else ""
    })

## Node paths are relative to the scene root ("." for the root itself).
func serialize_node(node: Node, root: Node) -> Dictionary:
    var children_data: Array[Dictionary] = []
    for child in node.get_children():
        children_data.append(serialize_node(child, root))

    return {
        "name": String(node.name),
        "class": node.get_class(),
        "path": "." if node == root else String(root.get_path_to(node)),
        "children": children_data
    }

func open_scene(peer: WebSocketPeer, cmd_id: String, params: Dictionary) -> void:
    var requested: String = params.get("scene_path", "")
    var scene_path: String = to_res_path(requested)
    if scene_path.is_empty():
        send_error(peer, cmd_id, "Scene path must be a res:// path inside the project: " + requested)
        return
    if not FileAccess.file_exists(scene_path):
        send_error(peer, cmd_id, "Scene file does not exist: " + scene_path)
        return

    var decision: Dictionary = _reload_decision(scene_path, bool(params.get("reload", false)), bool(params.get("discard", false)))
    if decision.has("error"):
        send_error(peer, cmd_id, decision["error"])
        return
    var dialogs_before: Dictionary = _visible_dialog_texts()
    if bool(decision.get("reload", false)):
        EditorInterface.reload_scene_from_path(scene_path)
    else:
        EditorInterface.open_scene_from_path(scene_path)
    remember_scene_sync(scene_path)
    var payload: Dictionary = {"opened": true, "reloaded": bool(decision.get("reload", false)), "scene_path": scene_path}
    payload.merge(await _load_report(scene_path, dialogs_before))
    send_success(peer, cmd_id, payload)

const MAX_LOAD_ERRORS: int = 50

## Godot reports what it had to fix while loading a scene, such as a node renamed because its
## name clashed with a child of an instanced scene, only in its Load Errors dialog: those
## messages reach neither the log nor a Logger. The dialog is in the tree only while it is
## shown and keeps its text until it is closed, so the messages of this load are the text the
## dialogs gained during it.
func _load_report(scene_path: String, dialogs_before: Dictionary) -> Dictionary:
    var messages: Array[String] = []
    var dialog_title: String = ""
    var dialogs_after: Dictionary = _visible_dialog_texts()
    for dialog: Variant in dialogs_after:
        var text: String = str(dialogs_after[dialog])
        var seen: String = str(dialogs_before.get(dialog, ""))
        var added: String = text.substr(seen.length()) if text.begins_with(seen) else text
        if added.strip_edges().is_empty():
            continue
        if dialog_title.is_empty():
            dialog_title = (dialog as AcceptDialog).title
        messages.append_array(_dialog_messages(added))
    if not messages.is_empty():
        # The editor marks a scene it changed while loading as unsaved a frame later.
        await get_tree().process_frame
    var unsaved: bool = EditorInterface.get_unsaved_scenes().has(scene_path)
    var report: Dictionary = {"load_errors": messages.slice(0, MAX_LOAD_ERRORS), "unsaved": unsaved}
    if messages.size() > MAX_LOAD_ERRORS:
        report["load_errors_total"] = messages.size()
    if not dialog_title.is_empty():
        report["dialog"] = dialog_title
        var warning: String = "Godot reported %d problem(s) while loading the scene and showed them in its '%s' dialog, which stays open until the person closes it." % [messages.size(), dialog_title]
        if unsaved:
            warning += " The editor changed the scene while loading it, so it now has unsaved changes."
        warning += " Fix the scene file as load_errors describe and open it again with reload: true, or save the scene to keep the editor's changes."
        report["warning"] = warning
    return report

## Text of every visible dialog, by dialog: its own text and that of its rich text labels.
func _visible_dialog_texts() -> Dictionary:
    var texts: Dictionary = {}
    for node: Node in get_tree().root.find_children("*", "AcceptDialog", true, false):
        var dialog: AcceptDialog = node as AcceptDialog
        if not dialog.visible:
            continue
        var text: String = dialog.dialog_text
        for label: Node in dialog.find_children("*", "RichTextLabel", true, false):
            text += (label as RichTextLabel).get_parsed_text()
        texts[dialog] = text
    return texts

## The Load Errors dialog writes each message after an icon, which reads as one space, and
## ends it with a newline. A message may span lines, so a line without the icon continues the
## message before it.
func _dialog_messages(text: String) -> Array[String]:
    var messages: Array[String] = []
    for line: String in text.split("\n"):
        if line.strip_edges().is_empty():
            continue
        if messages.is_empty() or line.begins_with(" "):
            messages.append(line.strip_edges())
        else:
            messages[-1] += "\n" + line.strip_edges()
    return messages

## Saving a scene also saves the script editor's tabs, and on the way the editor applies its
## convert-indent and trim settings to every open script, so a clean script indented with spaces
## came back with tabs. Those settings are off for the length of the save; a script with unsaved
## typing is still saved, as the editor's own save does, and the answer names it.
func save_scene(peer: WebSocketPeer, cmd_id: String) -> void:
    var before: Dictionary = _open_script_digests()
    var restore: Dictionary = _suspend_script_rewrites()
    var err: Error = EditorInterface.save_scene()
    _restore_editor_settings(restore)
    if err != OK:
        send_error(peer, cmd_id, "Failed to save scene: " + error_string(err) + ". Give the scene a file with godot_create_scene or save it once in the editor, then save again.")
        return
    var scene_path: String = ""
    var root: Node = EditorInterface.get_edited_scene_root()
    if root != null and not String(root.scene_file_path).is_empty():
        scene_path = String(root.scene_file_path)
        remember_scene_sync(scene_path)
    var scripts_saved: Array = []
    for path: String in before:
        if FileAccess.get_md5(path) != str(before[path]):
            scripts_saved.append(path)
    send_success(peer, cmd_id, {"saved": true, "scene_path": scene_path, "scripts_saved": scripts_saved})

const SCRIPT_REWRITE_SETTINGS: PackedStringArray = [
    "text_editor/behavior/files/convert_indent_on_save",
    "text_editor/behavior/files/trim_trailing_whitespace_on_save",
    "text_editor/behavior/files/trim_final_newlines_on_save",
]

## Turns off the script editor's rewrites on save and returns the values to restore.
func _suspend_script_rewrites() -> Dictionary:
    var settings: EditorSettings = EditorInterface.get_editor_settings()
    var restore: Dictionary = {}
    for setting: String in SCRIPT_REWRITE_SETTINGS:
        if settings.has_setting(setting) and bool(settings.get_setting(setting)):
            restore[setting] = true
            settings.set_setting(setting, false)
    _tell_script_editor_settings_changed(restore)
    return restore

func _restore_editor_settings(restore: Dictionary) -> void:
    var settings: EditorSettings = EditorInterface.get_editor_settings()
    for setting: String in restore:
        settings.set_setting(setting, restore[setting])
    _tell_script_editor_settings_changed(restore)

## The script editor keeps its own copy of these settings and refreshes it only on the editor
## settings notification, which EditorSettings does not let a script send. Send it to the script
## editor alone, so the change applies now and nothing else in the editor reacts.
func _tell_script_editor_settings_changed(changed: Dictionary) -> void:
    var script_editor: ScriptEditor = EditorInterface.get_script_editor()
    if script_editor != null and not changed.is_empty():
        script_editor.propagate_notification(EditorSettings.NOTIFICATION_EDITOR_SETTINGS_CHANGED)

## MD5 of every script file open in the script editor, by res:// path.
func _open_script_digests() -> Dictionary:
    var digests: Dictionary = {}
    var script_editor: ScriptEditor = EditorInterface.get_script_editor()
    if script_editor == null:
        return digests
    for script: Script in script_editor.get_open_scripts():
        var path: String = script.resource_path
        if path.begins_with("res://") and not path.contains("::") and FileAccess.file_exists(path):
            digests[path] = FileAccess.get_md5(path)
    return digests

# --- Nodes -----------------------------------------------------------------------------

## Creates a node from a native class or a global script class. Returns {"node": Node} or
## {"error": String}.
func instantiate_node(type_name: String) -> Dictionary:
    if ClassDB.class_exists(type_name):
        if not ClassDB.is_parent_class(type_name, "Node"):
            return {"error": "Class '%s' is not a Node type" % type_name}
        if not ClassDB.can_instantiate(type_name):
            return {"error": "Class '%s' cannot be instantiated (abstract or not available)" % type_name}
        return {"node": ClassDB.instantiate(type_name) as Node}

    for entry: Dictionary in ProjectSettings.get_global_class_list():
        if entry["class"] != type_name:
            continue
        var script: Script = load(entry["path"]) as Script
        if script == null or not script.can_instantiate():
            return {"error": "Script class '%s' cannot be instantiated" % type_name}
        var instance: Variant = script.new()
        if not instance is Node:
            if instance is Object and not instance is RefCounted:
                (instance as Object).free()
            return {"error": "Script class '%s' does not extend Node" % type_name}
        return {"node": instance as Node}

    return {"error": "Unknown Godot node type: " + type_name}

func add_node(peer: WebSocketPeer, cmd_id: String, params: Dictionary) -> void:
    var parent_path: String = params.get("parent_node_path", "")
    var node_type: String = params.get("node_type", "Node")
    var node_name: String = params.get("node_name", "")

    var root: Node = scene_root_or_error(peer, cmd_id)
    if root == null:
        return

    var parent_node: Node = find_scene_node(root, parent_path)
    if parent_node == null:
        send_error(peer, cmd_id, "Parent node not found at path: " + parent_path)
        return

    var created: Dictionary = instantiate_node(node_type)
    if created.has("error"):
        send_error(peer, cmd_id, created["error"])
        return
    var new_node: Node = created["node"]
    if not node_name.is_empty():
        new_node.name = node_name

    if undo_redo != null:
        undo_redo.create_action("Add Node " + String(new_node.name), UndoRedo.MERGE_DISABLE, root)
        undo_redo.add_do_method(parent_node, "add_child", new_node, true)
        undo_redo.add_do_property(new_node, "owner", root)
        undo_redo.add_do_reference(new_node)
        undo_redo.add_undo_method(parent_node, "remove_child", new_node)
        undo_redo.commit_action()
    else:
        parent_node.add_child(new_node, true)
        new_node.owner = root

    mark_scene_dirty()
    send_success(peer, cmd_id, {
        "name": String(new_node.name),
        "class": new_node.get_class(),
        "path": String(root.get_path_to(new_node))
    })

func collect_owned_nodes(node: Node, root: Node, into: Array[Node]) -> void:
    if node.owner == root:
        into.append(node)
    for child in node.get_children():
        collect_owned_nodes(child, root, into)

func remove_node(peer: WebSocketPeer, cmd_id: String, params: Dictionary) -> void:
    var node_path: String = params.get("node_path", "")
    var root: Node = scene_root_or_error(peer, cmd_id)
    if root == null:
        return

    var target_node: Node = find_scene_node(root, node_path)
    if target_node == null:
        send_error(peer, cmd_id, "Node not found at path: " + node_path)
        return
    if target_node == root:
        send_error(peer, cmd_id, "The scene root cannot be removed; remove its children or close the scene instead")
        return

    var parent: Node = target_node.get_parent()
    var index: int = target_node.get_index()
    var owned: Array[Node] = []
    collect_owned_nodes(target_node, root, owned)

    if undo_redo != null:
        undo_redo.create_action("Remove Node " + String(target_node.name), UndoRedo.MERGE_DISABLE, root)
        undo_redo.add_do_method(parent, "remove_child", target_node)
        undo_redo.add_undo_method(parent, "add_child", target_node, true)
        undo_redo.add_undo_method(parent, "move_child", target_node, index)
        for owned_node in owned:
            undo_redo.add_undo_property(owned_node, "owner", root)
        undo_redo.add_undo_reference(target_node)
        undo_redo.commit_action()
    else:
        parent.remove_child(target_node)
        target_node.queue_free()

    mark_scene_dirty()
    send_success(peer, cmd_id, {"removed": true, "path": node_path})

func get_node_properties(peer: WebSocketPeer, cmd_id: String, params: Dictionary) -> void:
    var node_path: String = params.get("node_path", "")
    var root: Node = scene_root_or_error(peer, cmd_id)
    if root == null:
        return

    var target_node: Node = find_scene_node(root, node_path)
    if target_node == null:
        send_error(peer, cmd_id, "Node not found at path: " + node_path)
        return

    var wanted: Array = params.get("properties", [])
    for prop_path: Variant in wanted:
        if not prop_path is String:
            send_error(peer, cmd_id, "Parameter 'properties' must be an array of property names, got %s" % describe_value(prop_path))
            return

    var props: Dictionary = {}
    if not wanted.is_empty():
        # Only the named properties, each of which may step through resources with ':'.
        var errors: Dictionary = {}
        for prop_path: String in wanted:
            var resolved: Dictionary = resolve_property(target_node, prop_path, false)
            if resolved.has("error"):
                errors[prop_path] = resolved["error"]
            elif not resolved.has("skip"):
                var holder: Object = resolved["object"]
                props[prop_path] = {
                    "type": property_type_name(resolved["info"]),
                    "value": describe_value(holder.get(str(resolved["property"]))),
                }
        var answer: Dictionary = {"node_path": node_path, "class": target_node.get_class(), "properties": props}
        if not errors.is_empty():
            answer["property_errors"] = errors
        send_success(peer, cmd_id, answer)
        return

    for prop: Dictionary in target_node.get_property_list():
        if not (int(prop["usage"]) & PROPERTY_USAGE_EDITOR):
            continue
        var prop_name: String = prop["name"]
        props[prop_name] = {
            "type": property_type_name(prop),
            "value": describe_value(target_node.get(prop_name)),
        }

    send_success(peer, cmd_id, {"node_path": node_path, "class": target_node.get_class(), "properties": props})

## Finds the object and property a name points to. A plain name is a property of the node. A name
## with ':' steps through resources the way godot_game_set does (material_override:shader_parameter/x),
## using the running game's resolver, and the last segment must be a property of the last resource.
## Writing refuses a resource saved outside the edited scene: a .tres file or another scene's
## sub-resource is shared, and changing it here would change every scene that uses it.
## Returns {"object", "property", "info"}, {"skip": true} for a requested script, or {"error": String}.
func resolve_property(node: Node, prop_path: String, writing: bool) -> Dictionary:
    var holder: Object = node
    var leaf: String = prop_path
    if prop_path.contains(":"):
        var located: Dictionary = RtLive._locate(node, prop_path, writing)
        if located.has("error"):
            var failure: Dictionary = located["error"]["error"]
            return {"error": str(failure.get("message", "Cannot resolve '%s'" % prop_path))}
        if bool(located.get("skip", false)):
            return {"skip": true}
        if writing:
            for resource: Resource in located["resources"]:
                var problem: String = foreign_resource_problem(resource)
                if not problem.is_empty():
                    return {"error": "'%s' %s" % [prop_path, problem]}
        holder = located["target"]
        leaf = str(located["leaf"])
    for prop: Dictionary in holder.get_property_list():
        if prop["name"] == leaf:
            return {"object": holder, "property": leaf, "info": prop}
    if holder == node:
        return {"error": "Unknown property '%s' on %s" % [leaf, node.get_class()]}
    return {"error": "Unknown property '%s' on %s (in '%s'); read the resource's properties with this tool to see their names" % [leaf, holder.get_class(), prop_path]}

## "" when the resource belongs to the edited scene alone: built into it, or not saved anywhere
## yet. Otherwise why changing it from this scene is refused and what to do instead.
func foreign_resource_problem(resource: Resource) -> String:
    var path: String = resource.resource_path
    if path.is_empty():
        return ""
    var root: Node = EditorInterface.get_edited_scene_root()
    var scene_path: String = "" if root == null else String(root.scene_file_path)
    var file: String = path.get_slice("::", 0)
    if path.contains("::") and file == scene_path and not scene_path.is_empty():
        return ""
    if path.contains("::"):
        return "goes through a %s built into %s, not into this scene, so changing it here would change every scene that uses that file. Open %s and change it there, or give this node its own copy of the resource first." % [resource.get_class(), file, file]
    return "goes through the resource file %s (%s), which every scene that uses it shares. Change that file instead, or give this node its own copy of the resource first." % [path, resource.get_class()]

## Builds a vector-like value from a list of numbers; returns null if the count is wrong.
func build_numeric_value(expected: int, n: Array) -> Variant:
    match expected:
        TYPE_VECTOR2:
            if n.size() == 2: return Vector2(n[0], n[1])
        TYPE_VECTOR2I:
            if n.size() == 2: return Vector2i(int(n[0]), int(n[1]))
        TYPE_VECTOR3:
            if n.size() == 3: return Vector3(n[0], n[1], n[2])
        TYPE_VECTOR3I:
            if n.size() == 3: return Vector3i(int(n[0]), int(n[1]), int(n[2]))
        TYPE_VECTOR4:
            if n.size() == 4: return Vector4(n[0], n[1], n[2], n[3])
        TYPE_VECTOR4I:
            if n.size() == 4: return Vector4i(int(n[0]), int(n[1]), int(n[2]), int(n[3]))
        TYPE_QUATERNION:
            if n.size() == 4: return Quaternion(n[0], n[1], n[2], n[3])
        TYPE_COLOR:
            if n.size() == 3: return Color(n[0], n[1], n[2])
            if n.size() == 4: return Color(n[0], n[1], n[2], n[3])
        TYPE_RECT2:
            if n.size() == 4: return Rect2(n[0], n[1], n[2], n[3])
        TYPE_RECT2I:
            if n.size() == 4: return Rect2i(int(n[0]), int(n[1]), int(n[2]), int(n[3]))
    return null

## Reads a JSON array or object of numbers; returns [] if anything is not a number.
func numbers_from(value: Variant, fields: Array) -> Array:
    var numbers: Array = []
    if value is Array:
        for item: Variant in value:
            if not (item is float or item is int):
                return []
            numbers.append(float(item))
    elif value is Dictionary:
        for field: String in fields:
            if not value.has(field):
                break
            var item: Variant = value[field]
            if not (item is float or item is int):
                return []
            numbers.append(float(item))
    return numbers

## Converts a JSON value to what the property expects. Returns {"value": Variant} or
## {"error": String}.
func convert_property_value(info: Dictionary, raw: Variant) -> Dictionary:
    var expected: int = info["type"]

    match expected:
        TYPE_NIL:
            return {"value": raw}
        TYPE_BOOL:
            if raw is bool:
                return {"value": raw}
        TYPE_INT:
            if raw is int:
                return {"value": raw}
            if raw is float and raw == floorf(raw):
                return {"value": int(raw)}
        TYPE_FLOAT:
            if raw is float or raw is int:
                return {"value": float(raw)}
        TYPE_STRING:
            if raw is String:
                return {"value": raw}
        TYPE_STRING_NAME:
            if raw is String:
                return {"value": StringName(raw)}
        TYPE_NODE_PATH:
            if raw is String:
                return {"value": NodePath(raw)}
        TYPE_VECTOR2, TYPE_VECTOR2I, TYPE_VECTOR3, TYPE_VECTOR3I, TYPE_VECTOR4, TYPE_VECTOR4I, TYPE_QUATERNION, TYPE_COLOR, TYPE_RECT2, TYPE_RECT2I:
            if expected == TYPE_COLOR and raw is String:
                var sentinel: Color = Color(-1.0, -1.0, -1.0, -1.0)
                var parsed_color: Color = Color.from_string(raw, sentinel)
                if parsed_color != sentinel:
                    return {"value": parsed_color}
            if raw is Array or raw is Dictionary:
                var built: Variant = build_numeric_value(expected, numbers_from(raw, OBJECT_FIELDS.get(expected, [])))
                if built != null:
                    return {"value": built}
        TYPE_OBJECT:
            if raw == null:
                return {"value": null}
            if raw is String and raw.begins_with("res://"):
                if not ResourceLoader.exists(raw):
                    return {"error": "Resource not found: " + raw}
                return {"value": load(raw)}
        TYPE_PACKED_BYTE_ARRAY, TYPE_PACKED_INT32_ARRAY, TYPE_PACKED_INT64_ARRAY, TYPE_PACKED_FLOAT32_ARRAY, TYPE_PACKED_FLOAT64_ARRAY, TYPE_PACKED_STRING_ARRAY, TYPE_PACKED_VECTOR2_ARRAY, TYPE_PACKED_VECTOR3_ARRAY, TYPE_PACKED_COLOR_ARRAY, TYPE_PACKED_VECTOR4_ARRAY:
            if raw is Array:
                var packed: Variant = type_convert(raw, expected)
                if typeof(packed) == expected:
                    return {"value": packed}
        _:
            if typeof(raw) == expected:
                return {"value": raw}

    # Anything else may be a Godot literal such as "Transform2D(...)".
    if raw is String:
        var parsed: Variant = str_to_var(raw)
        if parsed != null and typeof(parsed) == expected:
            return {"value": parsed}

    return {"error": "expected %s but got %s (%s)" % [property_type_name(info), type_string(typeof(raw)), describe_value(raw)]}

const META_PREFIX: String = "metadata/"

## The value of a metadata entry the node does not have yet: the JSON value as it is, except that
## a whole number becomes an int (JSON does not tell 3 from 3.0). Returns {"value": Variant} or
## {"error": String}.
func new_meta_value(meta_name: String, raw: Variant) -> Dictionary:
    # set_meta refuses a name that is not an identifier with an engine error and no reply.
    if not meta_name.is_valid_identifier():
        return {"error": "a new metadata name must be a valid identifier (letters, digits and _, not starting with a digit)"}
    if raw == null:
        return {"error": "null removes metadata and cannot create it; pass a value"}
    if typeof(raw) == TYPE_FLOAT:
        var number: float = raw
        if is_finite(number) and number == floorf(number) and absf(number) < 9.0e15:
            return {"value": int(number)}
    return {"value": raw}

func set_node_properties(peer: WebSocketPeer, cmd_id: String, params: Dictionary) -> void:
    var node_path: String = params.get("node_path", "")
    var properties: Dictionary = params.get("properties", {})
    var root: Node = scene_root_or_error(peer, cmd_id)
    if root == null:
        return

    var target_node: Node = find_scene_node(root, node_path)
    if target_node == null:
        send_error(peer, cmd_id, "Node not found at path: " + node_path)
        return

    var known: Dictionary = {}
    for prop: Dictionary in target_node.get_property_list():
        known[prop["name"]] = prop

    # Validate and convert everything first so a bad value never leaves a half-applied change.
    var converted: Dictionary = {}
    # metadata/<name> entries the node does not have yet, created as the inspector's Add Metadata does.
    var new_meta: Dictionary = {}
    # Properties of the node's own resources, named with ':' steps: name -> [resource, property, value].
    var stepped: Dictionary = {}
    var problems: PackedStringArray = []
    for key: Variant in properties.keys():
        var prop_name: String = str(key)
        if prop_name.contains(":"):
            var resolved: Dictionary = resolve_property(target_node, prop_name, true)
            if resolved.has("error"):
                problems.append(str(resolved["error"]))
                continue
            var stepped_value: Dictionary = convert_property_value(resolved["info"], properties[key])
            if stepped_value.has("error"):
                problems.append("%s: %s" % [prop_name, stepped_value["error"]])
            else:
                stepped[prop_name] = [resolved["object"], resolved["property"], stepped_value["value"]]
            continue
        if not known.has(prop_name) and prop_name.begins_with(META_PREFIX):
            var meta_result: Dictionary = new_meta_value(prop_name.substr(META_PREFIX.length()), properties[key])
            if meta_result.has("error"):
                problems.append("%s: %s" % [prop_name, meta_result["error"]])
            else:
                new_meta[prop_name] = meta_result["value"]
            continue
        if not known.has(prop_name):
            problems.append("Unknown property '%s' on %s" % [prop_name, target_node.get_class()])
            continue
        var result: Dictionary = convert_property_value(known[prop_name], properties[key])
        if result.has("error"):
            problems.append("%s: %s" % [prop_name, result["error"]])
        else:
            converted[prop_name] = result["value"]
    if not problems.is_empty():
        send_error(peer, cmd_id, "No properties were changed. " + "; ".join(problems))
        return

    if undo_redo != null:
        undo_redo.create_action("Set Properties on " + String(target_node.name), UndoRedo.MERGE_DISABLE, root)
        for prop_name: String in converted:
            undo_redo.add_do_property(target_node, prop_name, converted[prop_name])
            undo_redo.add_undo_property(target_node, prop_name, target_node.get(prop_name))
        for prop_name: String in stepped:
            var step: Array = stepped[prop_name]
            var resource: Object = step[0]
            undo_redo.add_do_property(resource, step[1], step[2])
            undo_redo.add_undo_property(resource, step[1], resource.get(step[1]))
        for prop_name: String in new_meta:
            var meta_name: StringName = StringName(prop_name.substr(META_PREFIX.length()))
            undo_redo.add_do_method(target_node, "set_meta", meta_name, new_meta[prop_name])
            undo_redo.add_undo_method(target_node, "remove_meta", meta_name)
        # The inspector lists metadata from the property list, so it must redraw.
        if not new_meta.is_empty():
            undo_redo.add_do_method(target_node, "notify_property_list_changed")
            undo_redo.add_undo_method(target_node, "notify_property_list_changed")
        undo_redo.commit_action()
    else:
        for prop_name: String in converted:
            target_node.set(prop_name, converted[prop_name])
        for prop_name: String in new_meta:
            target_node.set_meta(StringName(prop_name.substr(META_PREFIX.length())), new_meta[prop_name])
        for prop_name: String in stepped:
            var step: Array = stepped[prop_name]
            (step[0] as Object).set(step[1], step[2])

    converted.merge(new_meta)
    var applied: Dictionary = {}
    for prop_name: String in converted:
        applied[prop_name] = describe_value(target_node.get(prop_name))
    for prop_name: String in stepped:
        var step: Array = stepped[prop_name]
        applied[prop_name] = describe_value((step[0] as Object).get(step[1]))
    mark_scene_dirty()
    send_success(peer, cmd_id, {"updated": true, "node_path": node_path, "applied": applied})

# --- Viewport --------------------------------------------------------------------------

func capture_viewport(peer: WebSocketPeer, cmd_id: String, params: Dictionary) -> void:
    if DisplayServer.get_name() == "headless":
        send_error(peer, cmd_id, "The editor is running headless, so there is nothing to capture; start the editor with a window")
        return

    var root: Node = scene_root_or_error(peer, cmd_id)
    if root == null:
        return

    var view: String = params.get("view", "auto")
    if view == "auto":
        view = "3d" if root is Node3D else "2d"
    if view != "2d" and view != "3d":
        send_error(peer, cmd_id, "Parameter 'view' must be one of: auto, 2d, 3d")
        return
    var max_dimension: int = int(params.get("max_dimension", MAX_CAPTURE_DIMENSION))

    # A viewport only renders while its main screen is visible, so switch to it, let the
    # layout settle, then force a draw. Waiting for a natural frame could block forever: an
    # idle editor (low processor mode) does not redraw until something changes.
    EditorInterface.set_main_screen_editor("3D" if view == "3d" else "2D")
    await get_tree().process_frame
    await get_tree().process_frame
    RenderingServer.force_draw()
    await get_tree().process_frame

    var viewport: SubViewport = EditorInterface.get_editor_viewport_3d(0) if view == "3d" else EditorInterface.get_editor_viewport_2d()
    if viewport == null:
        send_error(peer, cmd_id, "Unable to access the editor %s viewport" % view)
        return

    var image: Image = viewport.get_texture().get_image()
    if image == null or image.get_width() < MIN_CAPTURE_SIZE or image.get_height() < MIN_CAPTURE_SIZE:
        var size_text: String = "none" if image == null else "%dx%d" % [image.get_width(), image.get_height()]
        send_error(peer, cmd_id, "The %s viewport is too small to capture (%s). Make sure the editor window is visible and not minimized." % [view, size_text])
        return

    var largest: int = maxi(image.get_width(), image.get_height())
    if max_dimension >= MIN_CAPTURE_SIZE and largest > max_dimension:
        var factor: float = float(max_dimension) / float(largest)
        image.resize(maxi(1, int(image.get_width() * factor)), maxi(1, int(image.get_height() * factor)), Image.INTERPOLATE_LANCZOS)

    send_success(peer, cmd_id, {
        "mime_type": "image/png",
        "data": Marshalls.raw_to_base64(image.save_png_to_buffer()),
        "width": image.get_width(),
        "height": image.get_height(),
        "view": view
    })

# --- Running ---------------------------------------------------------------------------

func run_project(peer: WebSocketPeer, cmd_id: String, params: Dictionary) -> void:
    var requested: String = params.get("scene_path", "")

    if not requested.is_empty():
        var scene_path: String = to_res_path(requested)
        if scene_path.is_empty():
            send_error(peer, cmd_id, "Scene path must be a res:// path inside the project: " + requested)
            return
        if not FileAccess.file_exists(scene_path):
            send_error(peer, cmd_id, "Scene file does not exist: " + scene_path)
            return
        EditorInterface.play_custom_scene(scene_path)
    else:
        EditorInterface.play_main_scene()

    send_success(peer, cmd_id, {
        "running": EditorInterface.is_playing_scene(),
        "scene": EditorInterface.get_playing_scene()
    })

func stop_project(peer: WebSocketPeer, cmd_id: String) -> void:
    EditorInterface.stop_playing_scene()
    send_success(peer, cmd_id, {"stopped": true})

func get_run_status(peer: WebSocketPeer, cmd_id: String) -> void:
    send_success(peer, cmd_id, {
        "running": EditorInterface.is_playing_scene(),
        "scene": EditorInterface.get_playing_scene()
    })

# --- Resources -------------------------------------------------------------------------

func is_importable(res_path: String) -> bool:
    return res_path.get_extension().to_lower() in IMPORTED_EXTENSIONS or FileAccess.file_exists(res_path + ".import")

func is_import_running(fs: EditorFileSystem) -> bool:
    return fs.is_importing()

## True when the editor's file system already has this file. reimport_files() on a path it
## has never scanned does not write a .import, and load() then fails.
func _filesystem_has(fs: EditorFileSystem, res_path: String) -> bool:
    var directory: EditorFileSystemDirectory = fs.get_filesystem_path(res_path.get_base_dir())
    if directory == null:
        return false
    return directory.find_file_index(res_path.get_file()) != -1

## Waits until scanning and importing have both been idle for a few frames, so a scan that
## starts the importer a moment later is not reported as finished. False on timeout.
func _wait_for_filesystem(fs: EditorFileSystem, timeout_msec: int) -> bool:
    var deadline: int = Time.get_ticks_msec() + timeout_msec
    var quiet: int = 0
    while Time.get_ticks_msec() < deadline:
        await get_tree().process_frame
        if fs.is_scanning() or is_import_running(fs):
            quiet = 0
            continue
        quiet += 1
        if quiet >= 3:
            return true
    return false

func resave_resources(peer: WebSocketPeer, cmd_id: String, params: Dictionary) -> void:
    var fs: EditorFileSystem = EditorInterface.get_resource_filesystem()

    if is_import_running(fs):
        send_error(peer, cmd_id, "A resource import is already in progress in the editor; retry once it finishes")
        return

    var requested: Array = params.get("paths", [])
    if requested.is_empty():
        # No specific paths: rescan the whole project. Without wait the scan keeps
        # running after this reply. With wait the reply is held until it finishes.
        var wait: bool = bool(params.get("wait", false))
        if not wait:
            fs.scan()
            send_success(peer, cmd_id, {"mode": "full_scan", "scanning": fs.is_scanning()})
            return
        var imported_paths: Array[String] = []
        var on_reimported := func(resources: PackedStringArray) -> void:
            for resource_path: String in resources:
                if not imported_paths.has(resource_path):
                    imported_paths.append(resource_path)
        fs.resources_reimported.connect(on_reimported)
        fs.scan()
        var finished: bool = await _wait_for_filesystem(fs, RESAVE_WAIT_MSEC)
        if fs.resources_reimported.is_connected(on_reimported):
            fs.resources_reimported.disconnect(on_reimported)
        send_success(peer, cmd_id, {
            "mode": "full_scan",
            "scanning": fs.is_scanning() or is_import_running(fs),
            "waited": true,
            "timed_out": not finished,
            "imported": imported_paths.size(),
        })
        return

    var to_reimport: PackedStringArray = PackedStringArray()
    var to_refresh: PackedStringArray = PackedStringArray()
    var unseen: PackedStringArray = PackedStringArray()
    var missing_paths: PackedStringArray = PackedStringArray()
    var invalid_paths: PackedStringArray = PackedStringArray()
    for item: Variant in requested:
        var res_path: String = to_res_path(str(item)) if item is String else ""
        if res_path.is_empty():
            invalid_paths.append(str(item))
        elif not FileAccess.file_exists(res_path):
            missing_paths.append(res_path)
        elif not _filesystem_has(fs, res_path):
            unseen.append(res_path)
        elif is_importable(res_path):
            to_reimport.append(res_path)
        else:
            to_refresh.append(res_path)

    var not_scanned: PackedStringArray = PackedStringArray()
    var timed_out: bool = false
    if not unseen.is_empty():
        for res_path: String in unseen:
            fs.update_file(res_path)
        var finished: bool = await _wait_for_filesystem(fs, RESAVE_WAIT_MSEC)
        var still: PackedStringArray = PackedStringArray()
        for res_path: String in unseen:
            if _filesystem_has(fs, res_path):
                if is_importable(res_path):
                    to_reimport.append(res_path)
                else:
                    to_refresh.append(res_path)
            else:
                still.append(res_path)
        # update_file does not see a directory the editor has never scanned. One scan does.
        if not still.is_empty():
            if not finished:
                timed_out = true
            else:
                fs.scan()
                finished = await _wait_for_filesystem(fs, RESAVE_WAIT_MSEC)
                if not finished:
                    timed_out = true
            for res_path: String in still:
                if _filesystem_has(fs, res_path):
                    if is_importable(res_path):
                        to_reimport.append(res_path)
                    else:
                        to_refresh.append(res_path)
                else:
                    not_scanned.append(res_path)

    if to_reimport.is_empty() and to_refresh.is_empty():
        if not not_scanned.is_empty():
            send_success(peer, cmd_id, {
                "mode": "not_scanned",
                "reimported": PackedStringArray(),
                "refreshed": PackedStringArray(),
                "not_scanned": not_scanned,
                "skipped_missing": missing_paths,
                "skipped_invalid": invalid_paths,
                "timed_out": timed_out,
            })
            return
        send_error(peer, cmd_id, "None of the given paths exist inside the project: " + ", ".join(missing_paths + invalid_paths))
        return

    # Unlike scan(), reimport_files() runs on the main thread and blocks
    # until it finishes, so the reimport is genuinely done by the time we
    # respond. Files that are not importable (scripts, scenes, resources) are
    # only refreshed in the editor's file system. A path is reimported only
    # once it is in the file system; otherwise it stays in not_scanned.
    if not to_reimport.is_empty():
        fs.reimport_files(to_reimport)
    for res_path in to_refresh:
        fs.update_file(res_path)
    # A script tab without unsaved changes follows its file; one with unsaved changes is kept
    # and listed, so the caller can ask the person.
    var reloaded_tabs: PackedStringArray = PackedStringArray()
    var unsaved_tabs: PackedStringArray = PackedStringArray()
    for res_path: String in to_refresh:
        if res_path.get_extension() != "gd":
            continue
        var tab: Dictionary = _script_ops().tab_state(res_path, true)
        if bool(tab.get("reloaded", false)):
            reloaded_tabs.append(res_path)
        elif bool(tab.get("dirty", false)):
            unsaved_tabs.append(res_path)

    var reimported: PackedStringArray = PackedStringArray()
    for res_path: String in to_reimport:
        if FileAccess.file_exists(res_path + ".import"):
            reimported.append(res_path)
        else:
            not_scanned.append(res_path)

    var mode: String = "reimport"
    if reimported.is_empty() and to_refresh.is_empty():
        mode = "not_scanned"
    elif reimported.is_empty():
        mode = "refresh"
    elif not to_refresh.is_empty() or not not_scanned.is_empty():
        mode = "mixed"
    send_success(peer, cmd_id, {
        "mode": mode,
        "reimported": reimported,
        "refreshed": to_refresh,
        "reloaded_tabs": reloaded_tabs,
        "unsaved_tabs": unsaved_tabs,
        "not_scanned": not_scanned,
        "skipped_missing": missing_paths,
        "skipped_invalid": invalid_paths,
        "timed_out": timed_out,
    })

# --- Scene freshness ------------------------------------------------------------------

func _track_open_scenes() -> void:
    var open: PackedStringArray = EditorInterface.get_open_scenes()
    if open == _open_scene_paths:
        return
    for scene_path: String in open:
        if not _scene_sync_hash.has(scene_path):
            _scene_sync_hash[scene_path] = _file_hash(scene_path)
        # A scene a person opened is as it was on disk when it first shows up here, so edits
        # they make afterwards move the undo version past this baseline.
        if not _scene_undo_version.has(scene_path):
            var version: int = _undo_version(scene_path)
            if version >= 0:
                _scene_undo_version[scene_path] = version
    var stale: Array = []
    for key: Variant in _scene_sync_hash:
        if not open.has(String(key)):
            stale.append(key)
    for key: Variant in stale:
        _scene_sync_hash.erase(key)
        _scene_dirty.erase(key)
        _scene_undo_version.erase(key)
    _open_scene_paths = open

func _file_hash(scene_path: String) -> int:
    if not FileAccess.file_exists(scene_path):
        return 0
    return hash(FileAccess.get_file_as_bytes(scene_path))

func _index_open_scenes() -> void:
    _track_open_scenes()

func remember_scene_sync(scene_path: String) -> void:
    _scene_sync_hash[scene_path] = _file_hash(scene_path)
    _scene_dirty.erase(scene_path)
    _scene_undo_version[scene_path] = _undo_version(scene_path)
    if not _open_scene_paths.has(scene_path):
        _open_scene_paths.append(scene_path)

func mark_scene_dirty() -> void:
    var root: Node = EditorInterface.get_edited_scene_root()
    if root != null and not String(root.scene_file_path).is_empty():
        _scene_dirty[String(root.scene_file_path)] = true

## Reloads an already-open scene when the file bytes differ from the copy the editor
## loaded. A same-second edit still counts. Unsaved editor changes, including ones made
## by hand, need discard: true. A reload drops that scene's undo history.
func _reload_decision(scene_path: String, force: bool, discard: bool) -> Dictionary:
    var open: bool = EditorInterface.get_open_scenes().has(scene_path)
    if not open and not force:
        return {"reload": false}
    var disk: int = _file_hash(scene_path)
    var known: bool = _scene_sync_hash.has(scene_path)
    var changed: bool = known and int(_scene_sync_hash[scene_path]) != disk
    if not force and not changed:
        return {"reload": false}
    if not open:
        return {"reload": false}
    if _has_unsaved_changes(scene_path) and not discard:
        return {"error": "Scene '%s' has unsaved editor changes and the file on disk has changed. Save it, or open it again with discard: true to reload from disk. Reloading drops this scene's undo history." % scene_path}
    return {"reload": true}

func _has_unsaved_changes(scene_path: String) -> bool:
    if bool(_scene_dirty.get(scene_path, false)):
        return true
    if not _scene_undo_version.has(scene_path):
        return false
    var current: int = _undo_version(scene_path)
    var saved: int = int(_scene_undo_version[scene_path])
    if current < 0 or saved < 0:
        return false
    return current != saved

func _open_scene_root(scene_path: String) -> Node:
    var open: PackedStringArray = EditorInterface.get_open_scenes()
    var idx: int = open.find(scene_path)
    if idx == -1:
        return null
    var roots: Array = EditorInterface.get_open_scene_roots()
    if idx < 0 or idx >= roots.size():
        return null
    return roots[idx]

func _undo_version(scene_path: String) -> int:
    var root: Node = _open_scene_root(scene_path)
    if root == null or undo_redo == null:
        return -1
    if not undo_redo.has_method("get_object_history_id") or not undo_redo.has_method("get_history_undo_redo"):
        return -1
    var history_id: int = undo_redo.get_object_history_id(root)
    var history: UndoRedo = undo_redo.get_history_undo_redo(history_id)
    if history == null:
        return -1
    return history.get_version()

# --- Replies ---------------------------------------------------------------------------

# --- Authoring ------------------------------------------------------------------------

## Creates the folders a new file needs. Returns "" when they exist afterwards, else the error text
## naming the folder.
func _ensure_folder(res_path: String) -> String:
    var folder: String = res_path.get_base_dir()
    var absolute: String = ProjectSettings.globalize_path(folder)
    if DirAccess.dir_exists_absolute(absolute):
        return ""
    var err: Error = DirAccess.make_dir_recursive_absolute(absolute)
    if err != OK or not DirAccess.dir_exists_absolute(absolute):
        return "Could not create the folder %s (%s)" % [folder, error_string(err)]
    return ""

func create_scene(peer: WebSocketPeer, cmd_id: String, params: Dictionary) -> void:
    var scene_path: String = to_res_path(str(params.get("scene_path", "")))
    var root_type: String = str(params.get("root_type", "Node"))
    var root_name: String = str(params.get("root_name", "Root"))
    if scene_path.is_empty() or not scene_path.ends_with(".tscn"):
        send_error(peer, cmd_id, "scene_path must be a res:// path ending in .tscn")
        return
    if FileAccess.file_exists(scene_path):
        send_error(peer, cmd_id, "Scene already exists: " + scene_path)
        return
    var made_folder: String = _ensure_folder(scene_path)
    if not made_folder.is_empty():
        send_error(peer, cmd_id, made_folder)
        return
    var created: Dictionary = instantiate_node(root_type)
    if created.has("error"):
        send_error(peer, cmd_id, created["error"])
        return
    var root: Node = created["node"]
    root.name = root_name if not root_name.is_empty() else "Root"
    var packed := PackedScene.new()
    var pack_err: Error = packed.pack(root)
    root.free()
    if pack_err != OK:
        send_error(peer, cmd_id, "Failed to pack scene: " + error_string(pack_err))
        return
    var save_err: Error = ResourceSaver.save(packed, scene_path)
    if save_err != OK:
        send_error(peer, cmd_id, "Failed to save scene: " + error_string(save_err))
        return
    EditorInterface.get_resource_filesystem().update_file(scene_path)
    EditorInterface.open_scene_from_path(scene_path)
    remember_scene_sync(scene_path)
    send_success(peer, cmd_id, {"created": true, "scene_path": scene_path, "root_type": root_type})

func attach_script(peer: WebSocketPeer, cmd_id: String, params: Dictionary) -> void:
    var root: Node = scene_root_or_error(peer, cmd_id)
    if root == null:
        return
    var node_path: String = str(params.get("node_path", "."))
    var target: Node = find_scene_node(root, node_path)
    if target == null:
        send_error(peer, cmd_id, "Node not found at path: " + node_path)
        return
    var script_path: String = str(params.get("script_path", ""))
    var script: Script = null
    if not script_path.is_empty() and script_path != "null":
        script_path = to_res_path(script_path)
        if script_path.is_empty() or not FileAccess.file_exists(script_path):
            send_error(peer, cmd_id, "Script file does not exist: " + str(params.get("script_path", "")))
            return
        script = load(script_path) as Script
        if script == null:
            send_error(peer, cmd_id, "File is not a script: " + script_path)
            return
    if undo_redo != null:
        undo_redo.create_action("Attach Script", UndoRedo.MERGE_DISABLE, root)
        undo_redo.add_do_property(target, "script", script)
        undo_redo.add_undo_property(target, "script", target.get_script())
        undo_redo.commit_action()
    else:
        target.set_script(script)
    mark_scene_dirty()
    send_success(peer, cmd_id, {"node_path": node_path, "script_path": script_path})

## The script commands keep their logic in editor/script_ops.gd; this only checks the path.
func read_script(peer: WebSocketPeer, cmd_id: String, params: Dictionary) -> void:
    var res_path: String = _script_command_path(peer, cmd_id, params)
    if res_path.is_empty():
        return
    var result: Dictionary = _script_ops().read(res_path)
    if result.has("error"):
        send_error(peer, cmd_id, str(result["error"]))
        return
    send_success(peer, cmd_id, result)

func edit_script(peer: WebSocketPeer, cmd_id: String, params: Dictionary) -> void:
    var res_path: String = _script_command_path(peer, cmd_id, params)
    if res_path.is_empty():
        return
    for key: String in ["old_text", "new_text"]:
        if not params.has(key):
            send_error(peer, cmd_id, "Missing %s. Pass the fragment to replace as old_text and its replacement as new_text." % key)
            return
    var result: Dictionary = _script_ops().edit(res_path, str(params["old_text"]), str(params["new_text"]), str(params.get("version", "")))
    if result.has("error"):
        send_error(peer, cmd_id, str(result["error"]))
        return
    send_success(peer, cmd_id, result)

## Reports the script tab of a file and, with reload, gives an unmodified stale tab the file's text.
func script_tab(peer: WebSocketPeer, cmd_id: String, params: Dictionary) -> void:
    var res_path: String = _script_command_path(peer, cmd_id, params)
    if res_path.is_empty():
        return
    var result: Dictionary = _script_ops().tab_state(res_path, bool(params.get("reload", false)))
    if result.has("error"):
        send_error(peer, cmd_id, str(result["error"]))
        return
    send_success(peer, cmd_id, result)

## The res:// path of the script_path parameter, or "" after an error reply was sent.
func _script_command_path(peer: WebSocketPeer, cmd_id: String, params: Dictionary) -> String:
    var raw: String = str(params.get("script_path", "")).strip_edges()
    if raw.is_empty():
        send_error(peer, cmd_id, "Missing script_path. Pass a res:// path to a .gd script.")
        return ""
    var res_path: String = to_res_path(raw)
    if res_path.is_empty():
        send_error(peer, cmd_id, "script_path must be a path inside the project (res://...): " + raw)
        return ""
    return res_path

func _script_ops() -> RefCounted:
    if _script_ops_instance == null:
        _script_ops_instance = (load(SCRIPT_OPS_PATH) as GDScript).new() as RefCounted
    return _script_ops_instance

## Deletes project files with their companions, closing their tabs; see editor/file_ops.gd.
func delete_files(peer: WebSocketPeer, cmd_id: String, params: Dictionary) -> void:
    if _file_ops_instance == null:
        _file_ops_instance = (load(FILE_OPS_PATH) as GDScript).new() as RefCounted
    var result: Dictionary = _file_ops_instance.call("delete", self, params)
    if result.has("error"):
        send_error(peer, cmd_id, str(result["error"]))
        return
    send_success(peer, cmd_id, result)

func connect_signal(peer: WebSocketPeer, cmd_id: String, params: Dictionary) -> void:
    var root: Node = scene_root_or_error(peer, cmd_id)
    if root == null:
        return
    var source: Node = find_scene_node(root, str(params.get("node_path", ".")))
    var target: Node = find_scene_node(root, str(params.get("target_path", ".")))
    var signal_name: String = str(params.get("signal_name", ""))
    var method_name: String = str(params.get("method", ""))
    if source == null or target == null:
        send_error(peer, cmd_id, "Signal source or target node was not found in the edited scene")
        return
    if not source.has_signal(signal_name):
        send_error(peer, cmd_id, "Node has no signal '%s'" % signal_name)
        return
    if not method_name.is_valid_identifier():
        send_error(peer, cmd_id, "Method name must be an identifier")
        return
    if not target.has_method(method_name):
        send_error(peer, cmd_id, "Node '%s' has no method '%s'; attach a script that defines it first" % [str(params.get("target_path", ".")), method_name])
        return
    var callable := Callable(target, method_name)
    if source.is_connected(signal_name, callable):
        send_error(peer, cmd_id, "Signal '%s' is already connected to '%s'" % [signal_name, method_name])
        return
    if undo_redo != null:
        undo_redo.create_action("Connect Signal", UndoRedo.MERGE_DISABLE, root)
        undo_redo.add_do_method(source, "connect", signal_name, callable, CONNECT_PERSIST)
        undo_redo.add_undo_method(source, "disconnect", signal_name, callable)
        undo_redo.commit_action()
    else:
        source.connect(signal_name, callable, CONNECT_PERSIST)
    mark_scene_dirty()
    send_success(peer, cmd_id, {"signal": signal_name, "method": method_name})

func list_signals(peer: WebSocketPeer, cmd_id: String, params: Dictionary) -> void:
    var root: Node = scene_root_or_error(peer, cmd_id)
    if root == null:
        return
    var node: Node = find_scene_node(root, str(params.get("node_path", ".")))
    if node == null:
        send_error(peer, cmd_id, "Node not found")
        return
    var signals: Array = []
    for info: Dictionary in node.get_signal_list():
        var signal_name := str(info.get("name", ""))
        var connections: Array = []
        for connection: Dictionary in node.get_signal_connection_list(signal_name):
            var callable: Callable = connection.get("callable", Callable())
            if not callable.is_valid():
                continue
            # Only connections to nodes of the edited scene; the editor's own panels listen to
            # many signals and are not the scene's business.
            var target: Object = callable.get_object()
            if not target is Node:
                continue
            if target != root and not root.is_ancestor_of(target as Node):
                continue
            connections.append({"target": str(root.get_path_to(target as Node)), "method": callable.get_method()})
        signals.append({"name": signal_name, "connections": connections})
    send_success(peer, cmd_id, {"signals": signals})

func create_resource(peer: WebSocketPeer, cmd_id: String, params: Dictionary) -> void:
    var resource_path: String = to_res_path(str(params.get("resource_path", "")))
    var resource_class: String = str(params.get("class_name", ""))
    if resource_path.is_empty():
        send_error(peer, cmd_id, "resource_path must be a res:// path")
        return
    # Saving over a file would replace it, also one the editor has open, without a way back.
    if FileAccess.file_exists(resource_path):
        send_error(peer, cmd_id, "File already exists: " + resource_path)
        return
    if not (resource_path.ends_with(".tres") or resource_path.ends_with(".res")):
        send_error(peer, cmd_id, "resource_path must end in .tres or .res")
        return
    if not ClassDB.class_exists(resource_class) or not ClassDB.can_instantiate(resource_class) or not ClassDB.is_parent_class(resource_class, "Resource"):
        send_error(peer, cmd_id, "Unknown resource class: " + resource_class)
        return
    var made_folder: String = _ensure_folder(resource_path)
    if not made_folder.is_empty():
        send_error(peer, cmd_id, made_folder)
        return
    var resource: Resource = ClassDB.instantiate(resource_class)
    var save_err: Error = ResourceSaver.save(resource, resource_path)
    if save_err != OK:
        send_error(peer, cmd_id, "Failed to save resource: " + error_string(save_err))
        return
    EditorInterface.get_resource_filesystem().update_file(resource_path)
    send_success(peer, cmd_id, {"resource_path": resource_path, "class_name": resource_class})

const SETTING_KEY_CHARS: String = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789_/.-"

func _is_setting_key(key: String) -> bool:
    if key.is_empty() or not key.contains("/"):
        return false
    for part: String in key.split("/"):
        if part.is_empty() or part == "." or part == "..":
            return false
    for character: String in key:
        if not SETTING_KEY_CHARS.contains(character):
            return false
    return true

## The value to store for a setting that already exists, or an error. The value must have the
## type the setting has: a wrong type would be written to project.godot and fail at the next load.
func _coerce_setting(current: Variant, value: Variant) -> Dictionary:
    match typeof(current):
        TYPE_STRING:
            if value is String:
                return {"value": value}
        TYPE_BOOL:
            if value is bool:
                return {"value": value}
        TYPE_INT:
            if (value is float or value is int) and not value is bool and is_equal_approx(floorf(float(value)), float(value)):
                return {"value": int(value)}
        TYPE_FLOAT:
            if (value is float or value is int) and not value is bool:
                return {"value": float(value)}
    return {"error": "This setting expects a %s; a value of another type is not stored. Change settings of other types in the editor's Project Settings." % type_string(typeof(current)).to_lower()}

func set_project_setting(peer: WebSocketPeer, cmd_id: String, params: Dictionary) -> void:
    var key: String = str(params.get("key", ""))
    if not _is_setting_key(key):
        send_error(peer, cmd_id, "key must start with a section and use only letters, digits and _ / . -, like application/config/name")
        return
    var value: Variant = params.get("value")
    if not (value is String or value is bool or value is int or value is float):
        send_error(peer, cmd_id, "value must be a string, number or boolean")
        return
    var previous: Variant = null
    if ProjectSettings.has_setting(key):
        previous = ProjectSettings.get_setting(key)
        var coerced: Dictionary = _coerce_setting(previous, value)
        if coerced.has("error"):
            send_error(peer, cmd_id, "'%s': %s" % [key, coerced["error"]])
            return
        value = coerced["value"]
    ProjectSettings.set_setting(key, value)
    var err: Error = ProjectSettings.save()
    if err != OK:
        ProjectSettings.set_setting(key, previous)
        send_error(peer, cmd_id, "Failed to save project settings: " + error_string(err))
        return
    var reported: Variant = previous
    if previous != null and not (previous is String or previous is bool or previous is int or previous is float):
        reported = str(previous)
    send_success(peer, cmd_id, {"key": key, "saved": true, "previous": reported})

func get_project_settings(peer: WebSocketPeer, cmd_id: String, params: Dictionary) -> void:
    var prefix: String = str(params.get("prefix", ""))
    var values: Dictionary = {}
    var count := 0
    for info: Dictionary in ProjectSettings.get_property_list():
        var key := str(info.get("name", ""))
        if not key.contains("/"):
            continue
        if not prefix.is_empty() and not key.begins_with(prefix):
            continue
        if not ProjectSettings.has_setting(key):
            continue
        values[key] = str(ProjectSettings.get_setting(key))
        count += 1
        if count >= 200:
            break
    send_success(peer, cmd_id, {"settings": values, "truncated": count >= 200})

# --- Input map -------------------------------------------------------------------------

const INPUT_PREFIX: String = "input/"
## What the editor's Input Map panel gives a new action.
const DEFAULT_DEADZONE: float = 0.2
## Characters the editor's Input Map panel refuses in an action name.
const ACTION_NAME_FORBIDDEN: String = "/:=\\\""
const MAX_ACTION_NAME: int = 128
const MOUSE_BUTTON_NAMES: Dictionary = {
    "left": MOUSE_BUTTON_LEFT,
    "right": MOUSE_BUTTON_RIGHT,
    "middle": MOUSE_BUTTON_MIDDLE,
    "wheel_up": MOUSE_BUTTON_WHEEL_UP,
    "wheel_down": MOUSE_BUTTON_WHEEL_DOWN,
    "wheel_left": MOUSE_BUTTON_WHEEL_LEFT,
    "wheel_right": MOUSE_BUTTON_WHEEL_RIGHT,
    "xbutton1": MOUSE_BUTTON_XBUTTON1,
    "xbutton2": MOUSE_BUTTON_XBUTTON2,
}

func _is_action_name(action: String) -> bool:
    if action.strip_edges().is_empty() or action.length() > MAX_ACTION_NAME:
        return false
    for character: String in action:
        var code: int = character.unicode_at(0)
        if ACTION_NAME_FORBIDDEN.contains(character) or code < 32 or code == 127:
            return false
    return true

## The action stored under input/<name> ({"deadzone", "events"}), or {} when there is none.
func _stored_action(action: String) -> Dictionary:
    var key: String = INPUT_PREFIX + action
    if not ProjectSettings.has_setting(key):
        return {}
    var stored: Variant = ProjectSettings.get_setting(key)
    if not stored is Dictionary:
        return {}
    return stored

func _is_number(value: Variant) -> bool:
    return (value is int or value is float) and not value is bool

func _spec_flag(spec: Dictionary, key: String, default_value: bool) -> bool:
    var value: Variant = spec.get(key, default_value)
    return value if value is bool else default_value

## An event as the tools report it. Two events are the same when their descriptions without
## "text" are equal.
func _describe_event(event: InputEvent) -> Dictionary:
    if event is InputEventKey:
        var key_event: InputEventKey = event
        var physical: bool = key_event.physical_keycode != KEY_NONE
        var code: Key = key_event.physical_keycode if physical else key_event.keycode
        return {
            "type": "key",
            "key": OS.get_keycode_string(code),
            "physical": physical,
            "shift": key_event.shift_pressed,
            "ctrl": key_event.ctrl_pressed,
            "alt": key_event.alt_pressed,
            "meta": key_event.meta_pressed,
            "text": key_event.as_text(),
        }
    if event is InputEventMouseButton:
        var mouse_event: InputEventMouseButton = event
        var button: Variant = int(mouse_event.button_index)
        for button_name: String in MOUSE_BUTTON_NAMES:
            if int(MOUSE_BUTTON_NAMES[button_name]) == int(mouse_event.button_index):
                button = button_name
        return {"type": "mouse_button", "button": button, "text": mouse_event.as_text()}
    if event is InputEventJoypadButton:
        var pad_button: InputEventJoypadButton = event
        return {"type": "joypad_button", "button": int(pad_button.button_index), "device": pad_button.device, "text": pad_button.as_text()}
    if event is InputEventJoypadMotion:
        var pad_motion: InputEventJoypadMotion = event
        return {"type": "joypad_motion", "axis": int(pad_motion.axis), "axis_value": signf(pad_motion.axis_value), "device": pad_motion.device, "text": pad_motion.as_text()}
    return {"type": event.get_class(), "text": event.as_text()}

func _event_identity(event: InputEvent) -> Dictionary:
    var described: Dictionary = _describe_event(event)
    described.erase("text")
    return described

## The InputEvent a spec asks for as {"event"}, or {"error"}. Events apply to all devices
## (device -1) unless a joypad spec names one, as the editor's Input Map panel makes them.
func _event_from_spec(spec: Variant) -> Dictionary:
    if not spec is Dictionary:
        return {"error": "each event must be an object with a type"}
    var data: Dictionary = spec
    var kind: String = str(data.get("type", ""))
    var device: int = -1
    var raw_device: Variant = data.get("device", -1)
    if _is_number(raw_device):
        device = int(raw_device)
    match kind:
        "key":
            var key_name: String = str(data.get("key", "")).strip_edges()
            var code: Key = OS.find_keycode_from_string(key_name) if not key_name.is_empty() else KEY_NONE
            if code == KEY_NONE or (code & KEY_MODIFIER_MASK) != 0:
                return {"error": "Unknown key '%s': use one key name such as E, Space, Up, Escape or F1, and pass modifiers as shift, ctrl, alt or meta" % key_name}
            var key_event := InputEventKey.new()
            key_event.device = -1
            if _spec_flag(data, "physical", true):
                key_event.physical_keycode = code
            else:
                key_event.keycode = code
            key_event.shift_pressed = _spec_flag(data, "shift", false)
            key_event.ctrl_pressed = _spec_flag(data, "ctrl", false)
            key_event.alt_pressed = _spec_flag(data, "alt", false)
            key_event.meta_pressed = _spec_flag(data, "meta", false)
            return {"event": key_event}
        "mouse_button":
            var raw_button: Variant = data.get("button", "")
            var index: int = 0
            if raw_button is String and MOUSE_BUTTON_NAMES.has(raw_button):
                index = int(MOUSE_BUTTON_NAMES[raw_button])
            elif _is_number(raw_button) and int(raw_button) >= 1 and int(raw_button) <= 9:
                index = int(raw_button)
            if index == 0:
                return {"error": "Unknown mouse button '%s': use %s, or a number from 1 to 9" % [str(raw_button), ", ".join(PackedStringArray(MOUSE_BUTTON_NAMES.keys()))]}
            var mouse_event := InputEventMouseButton.new()
            mouse_event.device = -1
            mouse_event.button_index = index as MouseButton
            return {"event": mouse_event}
        "joypad_button":
            var raw_pad: Variant = data.get("button", -1)
            if not _is_number(raw_pad) or int(raw_pad) < 0 or int(raw_pad) > 127:
                return {"error": "A joypad_button event needs button, a number from 0 to 127"}
            var pad_button := InputEventJoypadButton.new()
            pad_button.device = device
            pad_button.button_index = int(raw_pad) as JoyButton
            return {"event": pad_button}
        "joypad_motion":
            var raw_axis: Variant = data.get("axis", -1)
            var raw_value: Variant = data.get("axis_value", 0)
            if not _is_number(raw_axis) or int(raw_axis) < 0 or int(raw_axis) > 9:
                return {"error": "A joypad_motion event needs axis, a number from 0 to 9"}
            if not _is_number(raw_value) or is_zero_approx(float(raw_value)):
                return {"error": "A joypad_motion event needs axis_value, a non-zero number whose sign picks the direction"}
            var pad_motion := InputEventJoypadMotion.new()
            pad_motion.device = device
            pad_motion.axis = int(raw_axis) as JoyAxis
            pad_motion.axis_value = signf(float(raw_value))
            return {"event": pad_motion}
    return {"error": "Unknown event type '%s': use key, mouse_button, joypad_button or joypad_motion" % kind}

func _describe_action(action: String, stored: Dictionary) -> Dictionary:
    var events: Array = []
    var stored_events: Variant = stored.get("events", [])
    if stored_events is Array:
        for item: Variant in stored_events:
            if item is InputEvent:
                events.append(_describe_event(item))
    var deadzone: Variant = stored.get("deadzone", DEFAULT_DEADZONE)
    return {"name": action, "deadzone": float(deadzone) if _is_number(deadzone) else DEFAULT_DEADZONE, "events": events}

func get_input_map(peer: WebSocketPeer, cmd_id: String, params: Dictionary) -> void:
    var only: String = str(params.get("action", ""))
    var include_builtin: bool = params.get("include_builtin", false) == true
    var actions: Array = []
    var hidden: int = 0
    for info: Dictionary in ProjectSettings.get_property_list():
        var key: String = str(info.get("name", ""))
        if not key.begins_with(INPUT_PREFIX):
            continue
        var action: String = key.trim_prefix(INPUT_PREFIX)
        if not only.is_empty() and action != only:
            continue
        var stored: Dictionary = _stored_action(action)
        if stored.is_empty():
            continue
        if only.is_empty() and not include_builtin and action.begins_with("ui_"):
            hidden += 1
            continue
        actions.append(_describe_action(action, stored))
    if not only.is_empty() and actions.is_empty():
        send_error(peer, cmd_id, "Action '%s' does not exist. Call godot_get_input_map without action to list the actions, or create it with godot_edit_input_action create: true." % only)
        return
    send_success(peer, cmd_id, {"actions": actions, "builtin_hidden": hidden})

## Adds and removes events of one action, sets its deadzone, creates or deletes it. Every event
## is checked before anything changes; the change is saved and recorded as one undoable step.
func edit_input_action(peer: WebSocketPeer, cmd_id: String, params: Dictionary) -> void:
    var action: String = str(params.get("action", ""))
    if not _is_action_name(action):
        send_error(peer, cmd_id, "action '%s' is not a valid name: use 1 to %d characters without / : = \\ \" or control characters" % [action, MAX_ACTION_NAME])
        return
    var exists: bool = not _stored_action(action).is_empty()
    var create: bool = params.get("create", false) == true
    var delete: bool = params.get("delete", false) == true
    var raw_add: Variant = params.get("add_events", [])
    var raw_remove: Variant = params.get("remove_events", [])
    var to_add: Array = raw_add if raw_add is Array else []
    var to_remove: Array = raw_remove if raw_remove is Array else []
    if delete:
        if not to_add.is_empty() or not to_remove.is_empty() or params.has("deadzone") or create:
            send_error(peer, cmd_id, "delete cannot be combined with add_events, remove_events, deadzone or create")
            return
        if action.begins_with("ui_"):
            send_error(peer, cmd_id, "'%s' is a built-in action: it can be edited but not deleted" % action)
            return
        if not exists:
            send_error(peer, cmd_id, "Action '%s' does not exist, so there is nothing to delete. List the actions with godot_get_input_map." % action)
            return
        _commit_input_action(peer, cmd_id, action, null, {"deleted": true})
        return
    if not exists and not create:
        send_error(peer, cmd_id, "Action '%s' does not exist. Pass create: true to make it, or list the actions with godot_get_input_map." % action)
        return

    var current: Dictionary = _stored_action(action)
    var deadzone: float = float(_describe_action(action, current)["deadzone"])
    if params.has("deadzone"):
        var raw_deadzone: Variant = params["deadzone"]
        if not _is_number(raw_deadzone) or float(raw_deadzone) < 0.0 or float(raw_deadzone) > 1.0:
            send_error(peer, cmd_id, "deadzone must be a number from 0 to 1")
            return
        deadzone = float(raw_deadzone)
    var events: Array = []
    var stored_events: Variant = current.get("events", [])
    if stored_events is Array:
        for item: Variant in stored_events:
            if item is InputEvent:
                events.append(item)

    var removed: int = 0
    for spec: Variant in to_remove:
        var built: Dictionary = _event_from_spec(spec)
        if built.has("error"):
            send_error(peer, cmd_id, "remove_events: %s. Nothing was changed." % built["error"])
            return
        var wanted: Dictionary = _event_identity(built["event"])
        var found: int = -1
        for index: int in events.size():
            if _event_identity(events[index]) == wanted:
                found = index
                break
        if found < 0:
            send_error(peer, cmd_id, "Action '%s' does not have the event %s. Nothing was changed; read its events with godot_get_input_map." % [action, (built["event"] as InputEvent).as_text()])
            return
        events.remove_at(found)
        removed += 1

    var added: int = 0
    var already: Array = []
    for spec: Variant in to_add:
        var built: Dictionary = _event_from_spec(spec)
        if built.has("error"):
            send_error(peer, cmd_id, "add_events: %s. Nothing was changed." % built["error"])
            return
        var event: InputEvent = built["event"]
        var identity: Dictionary = _event_identity(event)
        var present: bool = false
        for existing: Variant in events:
            if _event_identity(existing) == identity:
                present = true
                break
        if present:
            already.append(_describe_event(event))
            continue
        events.append(event)
        added += 1

    var value: Dictionary = {"deadzone": deadzone, "events": events}
    _commit_input_action(peer, cmd_id, action, value, {"created": not exists, "added": added, "removed": removed, "already_present": already})

## Stores and saves the action (null deletes it), restoring the old value if the save fails, then
## records the change in the editor's global undo history without applying it a second time.
func _commit_input_action(peer: WebSocketPeer, cmd_id: String, action: String, value: Variant, report: Dictionary) -> void:
    var key: String = INPUT_PREFIX + action
    var previous: Variant = ProjectSettings.get_setting(key) if ProjectSettings.has_setting(key) else null
    ProjectSettings.set_setting(key, value)
    var err: Error = ProjectSettings.save()
    if err != OK:
        ProjectSettings.set_setting(key, previous)
        send_error(peer, cmd_id, "Failed to save project settings: %s. Nothing was changed; check that project.godot is writable." % error_string(err))
        return
    if undo_redo != null:
        undo_redo.create_action("Edit Input Action " + action, UndoRedo.MERGE_DISABLE, ProjectSettings)
        undo_redo.add_do_method(ProjectSettings, "set_setting", key, value)
        undo_redo.add_do_method(ProjectSettings, "save")
        undo_redo.add_undo_method(ProjectSettings, "set_setting", key, previous)
        undo_redo.add_undo_method(ProjectSettings, "save")
        undo_redo.commit_action(false)
    var answer: Dictionary = report.duplicate()
    answer["action"] = action
    answer["saved"] = true
    answer["undoable"] = undo_redo != null
    answer["deleted"] = value == null
    if value is Dictionary:
        var described: Dictionary = _describe_action(action, value)
        answer["deadzone"] = described["deadzone"]
        answer["events"] = described["events"]
    send_success(peer, cmd_id, answer)

# --- Running game ---------------------------------------------------------------------

func game_send(peer: WebSocketPeer, cmd_id: String, params: Dictionary) -> void:
    if game_relay == null:
        send_error(peer, cmd_id, "The game relay is not active. Update the Godot MCP addon and restart the editor.")
        return
    var command: String = str(params.get("command", ""))
    var data: Variant = params.get("data", [])
    if not data is Array:
        send_error(peer, cmd_id, "game_send data must be an array")
        return
    var session_id := int(params.get("session", -1))
    var sent: Dictionary = game_relay.call("send", session_id, command, data)
    if sent.has("error"):
        send_error(peer, cmd_id, str(sent["error"]))
        return
    send_success(peer, cmd_id, sent.get("result", {}))

func game_status(peer: WebSocketPeer, cmd_id: String) -> void:
    if game_relay == null:
        send_success(peer, cmd_id, {"sessions": []})
        return
    send_success(peer, cmd_id, game_relay.call("status"))

func broadcast_event(event_name: String, data: Dictionary) -> void:
    var payload := JSON.stringify({"type": "event", "event": event_name, "data": data})
    for peer: WebSocketPeer in peers:
        if peer.get_ready_state() == WebSocketPeer.STATE_OPEN:
            peer.send_text(payload)

func send_success(peer: WebSocketPeer, cmd_id: String, result: Dictionary) -> void:
    send_json(peer, cmd_id, {
        "commandId": cmd_id,
        "status": "success",
        "result": result
    })

func send_error(peer: WebSocketPeer, cmd_id: String, err_msg: String) -> void:
    send_json(peer, cmd_id, {
        "commandId": cmd_id,
        "status": "error",
        "error": err_msg
    })

func send_json(peer: WebSocketPeer, cmd_id: String, payload: Dictionary) -> void:
    var text: String = JSON.stringify(payload)
    var err: Error = peer.send_text(text)
    if err == OK:
        return
    printerr("[Godot MCP] Failed to send a ", text.length(), " character reply: ", error_string(err))
    # Fall back to a small reply so the caller gets an answer instead of waiting for its timeout.
    if peer.get_ready_state() == WebSocketPeer.STATE_OPEN:
        peer.send_text(JSON.stringify({
            "commandId": cmd_id,
            "status": "error",
            "error": "The reply (%d characters) could not be sent: %s" % [text.length(), error_string(err)]
        }))
