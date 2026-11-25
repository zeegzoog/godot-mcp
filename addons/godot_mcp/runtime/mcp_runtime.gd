extends Node
## In-game runtime for process runs and editor Play.
##
## Loaded only through a temporary res://override.cfg autoload while a debugger is attached.
## It does nothing unless EngineDebugger.is_active(), so a stale override cannot affect a
## normal or exported run. Requests arrive as EngineDebugger messages:
##   mcp:request [json]  {"id", "method", "params"}
##   mcp:reply   [json]  {"id", "result"} or {"id", "error"}
##   mcp:hello   [json]  sent once, when the main scene is ready

const R := preload("ops/rt_result.gd")
const Hold := preload("ops/rt_hold.gd")
const Project := preload("ops/rt_project.gd")
const PROTOCOL_VERSION := 1
const MODULES: Array = [
    preload("ops/rt_live.gd"),
    preload("ops/rt_input.gd"),
    preload("ops/rt_view.gd"),
    preload("ops/rt_output.gd"),
    preload("ops/rt_record.gd"),
    preload("ops/rt_project.gd"),
    preload("ops/rt_probe.gd"),
    preload("ops/rt_script.gd"),
]

## Expected JSON type of each parameter that a method converts (int(), float(), bool()), checked
## before the method runs: converting a wrongly typed value is a script error that sends no reply
## and leaves the caller waiting for its timeout. JSON null counts as not provided.
const PARAM_TYPES: Dictionary = {
    "game.tree": {"max_nodes": TYPE_FLOAT, "depth": TYPE_FLOAT},
    "game.get": {"signals": TYPE_BOOL, "methods": TYPE_BOOL},
    "game.observe": {"frames": TYPE_FLOAT},
    "game.screenshot": {"frames": TYPE_FLOAT, "every": TYPE_FLOAT, "max_dimension": TYPE_FLOAT},
    "game.probe": {"detail": TYPE_BOOL, "areas": TYPE_BOOL, "bodies": TYPE_BOOL},
    "rt.output": {"since": TYPE_FLOAT, "limit": TYPE_FLOAT},
    "input.wait": {"frames": TYPE_FLOAT, "timeout": TYPE_FLOAT},
}

var _methods: Dictionary = {}
var _registered := false
var _hello_sent := false
var _inbox: Array = []
var _tick: Callable = Callable()
var _job_req: Dictionary = {}
var _scene_generation := 0
var _scene_instance_id := 0
var _scene_script := {"path": "", "sha256": null, "basis": "unavailable"}

const SCRIPT_HASH_NOTE := "script_sha256 is the SHA-256 of the root script source Godot had loaded when this scene instance became current. It is not a later read of the file on disk, and it does not describe a different scene instance."


func _enter_tree() -> void:
    if not EngineDebugger.is_active():
        set_process(false)
        return
    process_mode = Node.PROCESS_MODE_ALWAYS
    for module: Variant in MODULES:
        _methods.merge((module as GDScript).call("methods"), true)
    _methods["run.end"] = {"fn": _end_run}
    EngineDebugger.register_message_capture("mcp", _on_message)
    _registered = true
    if not get_tree().node_added.is_connected(_on_node_added):
        get_tree().node_added.connect(_on_node_added)
    var output: GDScript = preload("ops/rt_output.gd")
    output.install()
    _note_scene()
    Hold.arm_at_launch(self)


func _ready() -> void:
    if _registered:
        Project.load_adapter()
        _send_hello.call_deferred()


func _process(_delta: float) -> void:
    _note_scene()
    if _tick.is_valid():
        var running: bool = _tick.call()
        if running:
            return
        _tick = Callable()
    _drain()


func _notification(what: int) -> void:
    # A person closing the window ends a hold; the game then quits as it normally would.
    if what == NOTIFICATION_WM_CLOSE_REQUEST and _registered:
        Hold.release(self)


func _exit_tree() -> void:
    if _registered and get_tree().node_added.is_connected(_on_node_added):
        get_tree().node_added.disconnect(_on_node_added)
    if _registered:
        Hold.release(self)
        # A step that is still in flight must not leave the tick rate or an unpaused tree behind.
        var live_script: GDScript = preload("ops/rt_live.gd")
        live_script.call("restore_physics", self)
        # Drop every key, button, axis and touch this runtime is still holding.
        var input_script: GDScript = preload("ops/rt_input.gd")
        input_script.call("release_all", self, {})
        input_script.call("restore_time_scale")
    if _registered and EngineDebugger.has_capture("mcp"):
        EngineDebugger.unregister_message_capture("mcp")
    if _registered:
        Project.unload()
        preload("ops/rt_output.gd").uninstall()
    _registered = false


## Quits the game the way a script's get_tree().quit() does, after this reply has gone out, so
## the engine shuts down in order: a Movie Maker recording writes its frame count and index then.
## The server asks for it before it stops a Movie Maker run, instead of killing the process.
func _end_run(_rt: Node, _params: Dictionary) -> Dictionary:
    get_tree().quit.call_deferred()
    return R.ok({"quitting": true})


func on_physics_frame() -> void:
    Hold.serve(self)


## Runs at the hold point, inside the first held tick. The hello may not have gone out yet, and
## the request that asked for the hold is answered from here.
func hold_started(status: Dictionary) -> void:
    _send_hello()
    _note_scene()
    if _tick.is_valid():
        finish_job(R.ok(status))


func _send_hello() -> void:
    if _hello_sent:
        return
    _hello_sent = true
    var scene := get_tree().current_scene
    _send("mcp:hello", {
        "protocol": PROTOCOL_VERSION,
        "godot_version": Engine.get_version_info().get("string", ""),
        "pid": OS.get_process_id(),
        "current_scene": scene.scene_file_path if scene != null else "",
        "display": DisplayServer.get_name(),
        "methods": _method_names(),
        "adapter": Project.status(),
    })


func _send(message: String, payload: Dictionary) -> void:
    if EngineDebugger.is_active():
        EngineDebugger.send_message(message, [JSON.stringify(payload)])


func _on_message(message: String, data: Array) -> bool:
    if message != "request" or data.is_empty() or not (data[0] is String):
        return true
    var parser := JSON.new()
    if parser.parse(data[0]) != OK:
        # Answered when its id can be read, so the caller does not wait for its timeout.
        var id: Variant = recovered_id(data[0])
        if id != null:
            _reply({"id": id}, R.err(R.BAD_PARAMS, "The request is not valid JSON: %s at line %d. Send one JSON object with id, method and params." % [parser.get_error_message(), parser.get_error_line()]))
        return true
    var parsed: Variant = parser.data
    if parsed is Dictionary:
        _inbox.append(parsed)
        _drain()
    return true


func _drain() -> void:
    if _tick.is_valid():
        return
    while not _inbox.is_empty() and not _tick.is_valid():
        _dispatch(_inbox.pop_front())


func _dispatch(req: Dictionary) -> void:
    var method := str(req.get("method", ""))
    var params: Variant = req.get("params", {})
    if params == null:
        params = {}
    var reply: Dictionary
    var refused := ""
    var mistyped := ""
    if params is Dictionary:
        mistyped = mistyped_param(method, params)
    if params is Dictionary and mistyped.is_empty() and Hold.is_held() and _methods.has(method):
        refused = Hold.refusal(method, params)
    if not _methods.has(method):
        reply = R.err(R.BAD_PARAMS, "Unknown runtime method '%s'." % method, {"methods": _method_names()})
    elif not params is Dictionary:
        reply = R.err(R.BAD_PARAMS, "%s: params must be an object." % method)
    elif not mistyped.is_empty():
        reply = R.err(R.BAD_PARAMS, mistyped)
    elif not refused.is_empty():
        reply = R.err(R.BAD_PARAMS, refused)
    else:
        var fn: Callable = (_methods[method] as Dictionary)["fn"]
        if Hold.is_held():
            var held_fn := Hold.held_fn(method)
            if held_fn.is_valid():
                fn = held_fn
        var out: Variant = fn.call(self, params)
        if out is Dictionary and bool((out as Dictionary).get("_pending", false)):
            _job_req = req
            _tick = (out as Dictionary).get("_tick", Callable())
            return
        if out is Dictionary and ((out as Dictionary).has("result") or (out as Dictionary).has("error")):
            reply = out
        else:
            reply = R.err("INTERNAL", "%s returned an unexpected value." % method)
    _reply(req, reply)


## The first wrongly typed parameter of a method (PARAM_TYPES), or "". A null is removed, as if
## it had not been passed.
func mistyped_param(method: String, params: Dictionary) -> String:
    var spec: Dictionary = PARAM_TYPES.get(method, {})
    for key: String in spec:
        if not params.has(key):
            continue
        var value: Variant = params[key]
        if value == null:
            params.erase(key)
            continue
        var expected: int = spec[key]
        var actual: int = typeof(value)
        if actual != expected and not (expected == TYPE_FLOAT and actual == TYPE_INT):
            var wanted := "a number" if expected == TYPE_FLOAT else type_string(expected)
            return "%s: '%s' must be %s, got %s. Pass it as %s or leave it out." % [method, key, wanted, type_string(actual), wanted]
    return ""


## The id of a request whose JSON does not parse, read from its text: a number or a string
## after "id":, or null when there is none.
func recovered_id(text: String) -> Variant:
    var pattern := RegEx.new()
    if pattern.compile("\"id\"\\s*:\\s*(-?[0-9]+|\"(?:[^\"\\\\]|\\\\.)*\")") != OK:
        return null
    var found := pattern.search(text)
    if found == null:
        return null
    var raw := found.get_string(1)
    if raw.begins_with("\""):
        return raw.substr(1, raw.length() - 2)
    return int(raw)


func _reply(req: Dictionary, reply: Dictionary) -> void:
    reply["id"] = req.get("id")
    _send("mcp:reply", reply)


func finish_job(reply: Dictionary) -> void:
    var req := _job_req
    _job_req = {}
    _tick = Callable()
    if not req.is_empty():
        _reply(req, reply)


func _method_names() -> Array:
    var names: Array = _methods.keys()
    names.sort()
    return names


func _on_node_added(node: Node) -> void:
    var scene := get_tree().current_scene
    if scene != null and node == scene:
        _note_scene()


func _note_scene() -> void:
    var scene := get_tree().current_scene
    # A gap with no current scene is not a new instance. Counting it would make one reload
    # look like two generations.
    if scene == null:
        return
    var id := scene.get_instance_id()
    if id == _scene_instance_id and _scene_generation > 0:
        return
    _scene_instance_id = id
    _scene_generation += 1
    _scene_script = _script_stamp(scene)
    preload("ops/rt_output.gd").note_scene(scene_context())


func scene_context() -> Dictionary:
    return {
        "path": get_tree().current_scene.scene_file_path if get_tree().current_scene != null else "",
        "name": String(get_tree().current_scene.name) if get_tree().current_scene != null else "",
        "scene_generation": _scene_generation,
        "scene_instance_id": _scene_instance_id,
        "script_path": str(_scene_script.get("path", "")),
        "script_sha256": _scene_script.get("sha256", null),
        "script_sha256_basis": str(_scene_script.get("basis", "unavailable")),
        "script_sha256_note": SCRIPT_HASH_NOTE,
    }


func _script_stamp(scene: Node) -> Dictionary:
    if scene == null or scene.get_script() == null:
        return {"path": "", "sha256": null, "basis": "unavailable"}
    var script: Script = scene.get_script()
    var path := script.resource_path
    var source := script.source_code
    if source.is_empty():
        return {"path": path, "sha256": null, "basis": "unavailable"}
    var ctx := HashingContext.new()
    if ctx.start(HashingContext.HASH_SHA256) != OK:
        return {"path": path, "sha256": null, "basis": "unavailable"}
    ctx.update(source.to_utf8_buffer())
    return {
        "path": path,
        "sha256": ctx.finish().hex_encode(),
        "basis": "loaded_source_at_scene_enter",
    }


## Whether a node is this runtime or under it: game.set and game.call never act on it.
func is_own(node: Node) -> bool:
    return node == self or is_ancestor_of(node)


func scene_root() -> Node:
    var scene := get_tree().current_scene
    return scene if scene != null else get_tree().root


func find_node(path: String) -> Node:
    var cleaned := path.strip_edges()
    if cleaned.is_empty() or cleaned == ".":
        return scene_root()
    if cleaned.begins_with("/root"):
        return get_tree().root.get_node_or_null(NodePath(cleaned))
    var root := scene_root()
    if root == null:
        return null
    return root.get_node_or_null(NodePath(cleaned))


## "" for a node that is freed or outside the tree: asking such a node for its path is an engine
## error that would show up in the game's own output.
func path_of(node: Node) -> String:
    var root := scene_root()
    if node == null or not is_instance_valid(node) or not node.is_inside_tree():
        return ""
    if root != null and (node == root or root.is_ancestor_of(node)):
        return "." if node == root else String(root.get_path_to(node))
    return String(node.get_path())


func encode(value: Variant) -> Variant:
    match typeof(value):
        TYPE_NIL, TYPE_BOOL, TYPE_INT, TYPE_FLOAT, TYPE_STRING:
            return value
        TYPE_VECTOR2:
            return [value.x, value.y]
        TYPE_VECTOR2I:
            return [value.x, value.y]
        TYPE_VECTOR3:
            return [value.x, value.y, value.z]
        TYPE_VECTOR3I:
            return [value.x, value.y, value.z]
        TYPE_COLOR:
            return [value.r, value.g, value.b, value.a]
        TYPE_ARRAY:
            var items: Array = []
            for item: Variant in value:
                items.append(encode(item))
            return items
        TYPE_DICTIONARY:
            var mapped := {}
            for key: Variant in value:
                mapped[str(key)] = encode(value[key])
            return mapped
        TYPE_OBJECT:
            if value is Node:
                return path_of(value)
            return str(value)
        _:
            return str(value)
