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
const PROTOCOL_VERSION := 1
const MODULES: Array = [
    preload("ops/rt_live.gd"),
    preload("ops/rt_input.gd"),
    preload("ops/rt_view.gd"),
    preload("ops/rt_output.gd"),
]

var _methods: Dictionary = {}
var _registered := false
var _inbox: Array = []
var _tick: Callable = Callable()
var _job_req: Dictionary = {}


func _enter_tree() -> void:
    if not EngineDebugger.is_active():
        set_process(false)
        return
    process_mode = Node.PROCESS_MODE_ALWAYS
    for module: Variant in MODULES:
        _methods.merge((module as GDScript).call("methods"), true)
    EngineDebugger.register_message_capture("mcp", _on_message)
    _registered = true
    var output: GDScript = preload("ops/rt_output.gd")
    output.install()


func _ready() -> void:
    if _registered:
        _send_hello.call_deferred()


func _process(_delta: float) -> void:
    if _tick.is_valid():
        var running: bool = _tick.call()
        if running:
            return
        _tick = Callable()
    _drain()


func _exit_tree() -> void:
    if _registered:
        # Drop every key, button, axis and touch this runtime is still holding.
        var input_script: GDScript = preload("ops/rt_input.gd")
        input_script.call("release_all", self, {})
        input_script.call("restore_time_scale")
    if _registered and EngineDebugger.has_capture("mcp"):
        EngineDebugger.unregister_message_capture("mcp")
    if _registered:
        preload("ops/rt_output.gd").uninstall()
    _registered = false


func _send_hello() -> void:
    var scene := get_tree().current_scene
    _send("mcp:hello", {
        "protocol": PROTOCOL_VERSION,
        "godot_version": Engine.get_version_info().get("string", ""),
        "pid": OS.get_process_id(),
        "current_scene": scene.scene_file_path if scene != null else "",
        "display": DisplayServer.get_name(),
        "methods": _method_names(),
    })


func _send(message: String, payload: Dictionary) -> void:
    if EngineDebugger.is_active():
        EngineDebugger.send_message(message, [JSON.stringify(payload)])


func _on_message(message: String, data: Array) -> bool:
    if message != "request" or data.is_empty() or not (data[0] is String):
        return true
    var parser := JSON.new()
    if parser.parse(data[0]) != OK:
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
    if not _methods.has(method):
        reply = R.err(R.BAD_PARAMS, "Unknown runtime method '%s'." % method, {"methods": _method_names()})
    elif not params is Dictionary:
        reply = R.err(R.BAD_PARAMS, "%s: params must be an object." % method)
    else:
        var fn: Callable = (_methods[method] as Dictionary)["fn"]
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


func path_of(node: Node) -> String:
    var root := scene_root()
    if node == null:
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
