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
    "get_node_properties": {"node_path": TYPE_STRING},
    "set_node_properties": {"node_path": TYPE_STRING, "properties": TYPE_DICTIONARY},
    "capture_viewport": {"view": TYPE_STRING, "max_dimension": TYPE_FLOAT},
    "open_scene": {"scene_path": TYPE_STRING, "reload": TYPE_BOOL, "discard": TYPE_BOOL},
    "run_project": {"scene_path": TYPE_STRING},
    "resave_resources": {"paths": TYPE_ARRAY},
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

func start_server() -> void:
    var err: Error = server.listen(port, LOOPBACK_ADDRESS)
    if err != OK:
        printerr("[Godot MCP] Failed to start bridge server on port ", port, ": ", error_string(err))
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
        return ""
    var relative: String = path.trim_prefix("res://")
    if relative.is_empty() or ".." in relative.split("/"):
        return ""
    return "res://" + relative

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
        if not is_instance_valid(value):
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
    if bool(decision.get("reload", false)):
        EditorInterface.reload_scene_from_path(scene_path)
    else:
        EditorInterface.open_scene_from_path(scene_path)
    remember_scene_sync(scene_path)
    send_success(peer, cmd_id, {"opened": true, "reloaded": bool(decision.get("reload", false)), "scene_path": scene_path})

func save_scene(peer: WebSocketPeer, cmd_id: String) -> void:
    var err: Error = EditorInterface.save_scene()
    if err == OK:
        var root: Node = EditorInterface.get_edited_scene_root()
        if root != null and not String(root.scene_file_path).is_empty():
            remember_scene_sync(String(root.scene_file_path))
        send_success(peer, cmd_id, {"saved": true})
    else:
        send_error(peer, cmd_id, "Failed to save scene: " + error_string(err))

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

    var props: Dictionary = {}
    for prop: Dictionary in target_node.get_property_list():
        if not (int(prop["usage"]) & PROPERTY_USAGE_EDITOR):
            continue
        var prop_name: String = prop["name"]
        props[prop_name] = {
            "type": property_type_name(prop),
            "value": describe_value(target_node.get(prop_name)),
        }

    send_success(peer, cmd_id, {"node_path": node_path, "class": target_node.get_class(), "properties": props})

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
    var problems: PackedStringArray = []
    for key: Variant in properties.keys():
        var prop_name: String = str(key)
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
        undo_redo.commit_action()
    else:
        for prop_name: String in converted:
            target_node.set(prop_name, converted[prop_name])

    var applied: Dictionary = {}
    for prop_name: String in converted:
        applied[prop_name] = describe_value(target_node.get(prop_name))
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

func resave_resources(peer: WebSocketPeer, cmd_id: String, params: Dictionary) -> void:
    var fs: EditorFileSystem = EditorInterface.get_resource_filesystem()

    if is_import_running(fs):
        send_error(peer, cmd_id, "A resource import is already in progress in the editor; retry once it finishes")
        return

    var requested: Array = params.get("paths", [])
    if requested.is_empty():
        # No specific paths: rescan the whole project for new/changed/removed
        # files. This runs on a background thread, so it is not finished by
        # the time this response is sent.
        fs.scan()
        send_success(peer, cmd_id, {"mode": "full_scan", "scanning": fs.is_scanning()})
        return

    var to_reimport: PackedStringArray = PackedStringArray()
    var to_refresh: PackedStringArray = PackedStringArray()
    var missing_paths: PackedStringArray = PackedStringArray()
    var invalid_paths: PackedStringArray = PackedStringArray()
    for item: Variant in requested:
        var res_path: String = to_res_path(str(item)) if item is String else ""
        if res_path.is_empty():
            invalid_paths.append(str(item))
        elif not FileAccess.file_exists(res_path):
            missing_paths.append(res_path)
        elif is_importable(res_path):
            to_reimport.append(res_path)
        else:
            to_refresh.append(res_path)

    if to_reimport.is_empty() and to_refresh.is_empty():
        send_error(peer, cmd_id, "None of the given paths exist inside the project: " + ", ".join(missing_paths + invalid_paths))
        return

    # Unlike scan(), reimport_files() runs on the main thread and blocks
    # until it finishes, so the reimport is genuinely done by the time we
    # respond. Files that are not importable (scripts, scenes, resources) are
    # only refreshed in the editor's file system.
    if not to_reimport.is_empty():
        fs.reimport_files(to_reimport)
    for res_path in to_refresh:
        fs.update_file(res_path)

    var mode: String = "reimport"
    if to_reimport.is_empty():
        mode = "refresh"
    elif not to_refresh.is_empty():
        mode = "mixed"
    send_success(peer, cmd_id, {
        "mode": mode,
        "reimported": to_reimport,
        "refreshed": to_refresh,
        "skipped_missing": missing_paths,
        "skipped_invalid": invalid_paths
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
