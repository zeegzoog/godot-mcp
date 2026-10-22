extends RefCounted
## Live scene inspection. Property writes and method calls are intentionally narrow:
## `script` cannot be set, and a method runs only when the caller lists it in `permit`
## and it is not on the denylist: no eval, no quit, no calling another method by name, no setting
## a property or the script by name, no emitting a signal or sending a notification, and nothing
## that frees or replaces a node. The list must equal DENY_METHODS in src/tools/game.rs.

const R := preload("rt_result.gd")
const Hold := preload("rt_hold.gd")
const DENY: Array[String] = [
    "quit",
    "set_script",
    "call",
    "callv",
    "call_deferred",
    "call_group",
    "call_group_flags",
    "propagate_call",
    "rpc",
    "rpc_id",
    "set",
    "set_deferred",
    "set_indexed",
    "emit_signal",
    "notification",
    "propagate_notification",
    "free",
    "queue_free",
    "replace_by",
]
const MAX_NODES := 400
const MAX_DEPTH := 32


static func methods() -> Dictionary:
    return {
        "game.tree": {"fn": tree},
        "game.get": {"fn": get_node},
        "game.set": {"fn": set_props},
        "game.call": {"fn": call_method},
        "game.signals": {"fn": signals_of},
        "game.observe": {"fn": observe},
        "game.snapshot": {"fn": snapshot},
        "game.physics": {"fn": physics},
    }


static func tree(rt: Node, params: Dictionary) -> Dictionary:
    var root_path := str(params.get("root", ".")).strip_edges()
    var root: Node = rt.get_tree().root if root_path == "/root" else rt.find_node(root_path)
    if root == null:
        return R.err(R.NOT_FOUND, "No node at '%s'." % root_path)
    var limit := clampi(int(params.get("max_nodes", 150)), 1, MAX_NODES)
    var depth_limit := int(params.get("depth", MAX_DEPTH))
    var name_filter := str(params.get("name", "")).to_lower()
    var class_filter := str(params.get("class", ""))
    var group_filter := str(params.get("group", ""))
    var text_filter := str(params.get("text", "")).to_lower()
    var nodes: Array = []
    _walk(rt, root, 0, depth_limit, limit, name_filter, class_filter, group_filter, text_filter, nodes)
    return R.ok({
        "scene": rt.scene_root().scene_file_path if rt.scene_root() != null else "",
        "count": nodes.size(),
        "truncated": nodes.size() >= limit,
        "nodes": nodes,
    })


static func _walk(rt: Node, node: Node, depth: int, depth_limit: int, limit: int, name_filter: String, class_filter: String, group_filter: String, text_filter: String, into: Array) -> void:
    if into.size() >= limit or depth > depth_limit:
        return
    var include := true
    if not name_filter.is_empty() and not String(node.name).to_lower().contains(name_filter):
        include = false
    if include and not class_filter.is_empty() and not node.is_class(class_filter) and node.get_class() != class_filter:
        include = false
    if include and not group_filter.is_empty() and not node.is_in_group(group_filter):
        include = false
    var visible_text := _visible_text(node)
    if include and not text_filter.is_empty() and not visible_text.to_lower().contains(text_filter):
        include = false
    if include:
        var item := {
            "path": rt.path_of(node),
            "name": String(node.name),
            "class": node.get_class(),
            "text": visible_text,
        }
        if node.get_script() != null:
            item["script"] = (node.get_script() as Script).resource_path
        if node is Node2D:
            item["position"] = [node.position.x, node.position.y]
        elif node is Node3D:
            item["position"] = [node.position.x, node.position.y, node.position.z]
        if node is CanvasItem:
            item["visible"] = node.visible
        into.append(item)
    for child: Node in node.get_children():
        _walk(rt, child, depth + 1, depth_limit, limit, name_filter, class_filter, group_filter, text_filter, into)


static func _visible_text(node: Node) -> String:
    if node is Button or node is Label or node is LineEdit or node is TextEdit or node is RichTextLabel:
        return str(node.text)
    return ""


static func get_node(rt: Node, params: Dictionary) -> Dictionary:
    var node := _need(rt, params)
    if node == null:
        return R.err(R.NOT_FOUND, "Node not found at '%s'." % str(params.get("path", ".")))
    var wanted: Variant = params.get("properties", [])
    var props := {}
    if wanted is Array and not (wanted as Array).is_empty():
        for key: Variant in wanted:
            var name := str(key)
            var located := _locate(node, name, false)
            if bool(located.get("skip", false)):
                continue
            if located.has("error"):
                return located["error"]
            var target: Object = located["target"]
            props[name] = rt.encode(target.get(str(located["leaf"])))
    else:
        for info: Dictionary in node.get_property_list():
            if int(info.get("usage", 0)) & PROPERTY_USAGE_SCRIPT_VARIABLE == 0 and int(info.get("usage", 0)) & PROPERTY_USAGE_EDITOR == 0:
                continue
            var name := str(info.get("name", ""))
            if name.is_empty() or name == "script" or name.begins_with("_"):
                continue
            var value: Variant = node.get(name)
            var fallback: Variant = node.property_get_revert(name) if node.property_can_revert(name) else null
            if value == fallback and not (wanted is Array and (wanted as Array).has("*")):
                continue
            props[name] = rt.encode(value)
    var result := {
        "path": rt.path_of(node),
        "class": node.get_class(),
        "properties": props,
        "groups": node.get_groups(),
    }
    if node.get_script() != null:
        result["script"] = (node.get_script() as Script).resource_path
    if bool(params.get("signals", false)):
        result["signals"] = _signal_list(node)
    if bool(params.get("methods", false)):
        var methods: Array = []
        for info: Dictionary in node.get_method_list():
            var name := str(info.get("name", ""))
            if not name.begins_with("_") and not name in DENY:
                methods.append(name)
        result["methods"] = methods
    return R.ok(result)


static func set_props(rt: Node, params: Dictionary) -> Dictionary:
    var node := _need(rt, params)
    if node == null:
        return R.err(R.NOT_FOUND, "Node not found at '%s'." % str(params.get("path", ".")))
    var properties: Variant = params.get("properties", {})
    if not properties is Dictionary or (properties as Dictionary).is_empty():
        return R.err(R.BAD_PARAMS, "game.set: 'properties' must be a non-empty object.")
    var readback := {}
    for key: Variant in properties:
        var name := str(key)
        var located := _locate(node, name, true)
        if located.has("error"):
            return located["error"]
        var target: Object = located["target"]
        var leaf := str(located["leaf"])
        var current: Variant = target.get(leaf)
        var decoded: Variant = _coerce(properties[key], typeof(current) if current != null else TYPE_NIL)
        target.set(leaf, decoded)
        readback[name] = rt.encode(target.get(leaf))
    return R.ok({"path": rt.path_of(node), "properties": readback})


## A property path is a node property, then zero or more resource properties separated by `:`.
## `shader_parameter/caustics` is one Godot property name, so `/` stays inside a segment.
## Only a Resource can be stepped through, so a path cannot walk from one node to another.
## Writing refuses `script`, a segment that starts with `_`, and an empty segment. Reading
## allows a `_` segment. A requested `script` is skipped, which is what a property list did
## before paths could step into a resource.
static func _locate(root: Object, path: String, writing: bool) -> Dictionary:
    if path.is_empty() or path.begins_with(":") or path.ends_with(":") or path.contains("::"):
        return {"error": _refused_path(path, writing)}
    var segments := path.split(":", false)
    if segments.is_empty():
        return {"error": _refused_path(path, writing)}
    for segment: String in segments:
        var problem := _segment_problem(segment, writing)
        if problem == "script" and not writing:
            return {"skip": true}
        if not problem.is_empty():
            return {"error": _refused_path(path, writing)}
    var target: Object = root
    for index in segments.size() - 1:
        var current: Variant = target.get(segments[index])
        if not current is Resource:
            var verb := "changed" if writing else "read"
            return {"error": R.err(R.BAD_PARAMS, "Property '%s' is not a resource, so '%s' cannot be %s through it." % [segments[index], path, verb])}
        target = current
    return {"target": target, "leaf": segments[segments.size() - 1]}


static func _refused_path(path: String, writing: bool) -> Dictionary:
    var verb := "set" if writing else "read"
    return R.err(R.DENIED, "Refusing to %s '%s'." % [verb, path])


## "" when the segment may be used. "script" and "private" are refused on write.
## "script" on a read is reported so the caller can skip that name. "empty" is always refused.
static func _segment_problem(segment: String, writing: bool) -> String:
    if segment.is_empty() or segment.begins_with("/") or segment.ends_with("/") or segment.contains("//"):
        return "empty"
    for part: String in segment.split("/", false):
        if part.is_empty():
            return "empty"
        if part == "script":
            return "script"
        if writing and part.begins_with("_"):
            return "private"
    return ""


static func call_method(rt: Node, params: Dictionary) -> Dictionary:
    var node := _need(rt, params)
    if node == null:
        return R.err(R.NOT_FOUND, "Node not found at '%s'." % str(params.get("path", ".")))
    var method := str(params.get("method", ""))
    if not method.is_valid_identifier():
        return R.err(R.BAD_PARAMS, "Method '%s' is not an identifier." % method)
    if method in DENY or method.begins_with("_"):
        return R.err(R.DENIED, "Method '%s' is not allowed." % method)
    var permit: Variant = params.get("permit", [])
    if not permit is Array or not (permit as Array).has(method):
        return R.err(R.DENIED, "Method '%s' was not explicitly permitted. Pass permit: [\"%s\"]." % [method, method])
    if not node.has_method(method):
        return R.err(R.NOT_FOUND, "Node '%s' has no method '%s'." % [rt.path_of(node), method])
    var raw: Variant = params.get("args", [])
    if raw == null:
        raw = []
    if not raw is Array:
        return R.err(R.BAD_PARAMS, "game.call: 'args' must be an array.")
    var arg_types: Array = []
    for info: Dictionary in node.get_method_list():
        if str(info.get("name", "")) == method:
            for arg_info: Dictionary in info.get("args", []):
                arg_types.append(int(arg_info.get("type", TYPE_NIL)))
            break
    var args: Array = []
    var index := 0
    for item: Variant in raw:
        var expected := TYPE_NIL
        if index < arg_types.size():
            expected = int(arg_types[index])
        var coerced: Variant = _coerce(item, expected)
        if expected != TYPE_NIL and typeof(coerced) != expected and not _type_compatible(coerced, expected):
            return R.err(R.BAD_PARAMS, "Argument %d for '%s' is %s; expected %s." % [index, method, type_string(typeof(coerced)), type_string(expected)])
        args.append(coerced)
        index += 1
    var value: Variant = node.callv(method, args)
    return R.ok({"path": rt.path_of(node), "method": method, "value": rt.encode(value)})


static func signals_of(rt: Node, params: Dictionary) -> Dictionary:
    var node := _need(rt, params)
    if node == null:
        return R.err(R.NOT_FOUND, "Node not found at '%s'." % str(params.get("path", ".")))
    return R.ok({"path": rt.path_of(node), "signals": _signal_list(node)})


static func observe(rt: Node, params: Dictionary) -> Dictionary:
    var node := _need(rt, params)
    if node == null:
        return R.err(R.NOT_FOUND, "Node not found at '%s'." % str(params.get("path", ".")))
    var names: Variant = params.get("signals", [])
    if not names is Array or (names as Array).is_empty():
        return R.err(R.BAD_PARAMS, "game.observe: 'signals' must be a non-empty array of signal names.")
    var frames := clampi(int(params.get("frames", 1)), 0, 600)
    var events: Array = []
    var connected: Array = []
    for entry: Variant in names:
        var sig_name := str(entry)
        if not node.has_signal(sig_name):
            _disconnect_all(node, connected)
            return R.err(R.NOT_FOUND, "Node '%s' has no signal '%s'." % [rt.path_of(node), sig_name])
        var hook := func() -> void:
            events.append({"signal": sig_name, "frame": Engine.get_process_frames()})
        var err := node.connect(sig_name, hook)
        if err != OK:
            _disconnect_all(node, connected)
            return R.err(R.GODOT_ERROR, "Could not observe '%s': %s." % [sig_name, error_string(err)])
        connected.append({"signal": sig_name, "hook": hook})
    var state := {"left": frames, "node": node, "connected": connected, "events": events, "path": rt.path_of(node)}
    return {"_pending": true, "_tick": func() -> bool:
        if int(state["left"]) > 0:
            state["left"] = int(state["left"]) - 1
            return true
        _disconnect_all(state["node"], state["connected"])
        rt.finish_job(R.ok({"path": state["path"], "frames": frames, "events": state["events"]}))
        return false
    }


static func snapshot(rt: Node, _params: Dictionary) -> Dictionary:
    var root: Node = rt.scene_root() as Node
    var children: Array = []
    if root != null:
        for child: Node in root.get_children():
            children.append({"name": String(child.name), "class": child.get_class()})
    var body := {
        "scene": root.scene_file_path if root != null else "",
        "scene_name": String(root.name) if root != null else "",
        "frame": Engine.get_process_frames(),
        "paused": rt.get_tree().paused,
        "children": children,
    }
    body.merge(rt.scene_context(), true)
    return R.ok(body)


const MAX_PHYSICS_TICKS := 120

static var _physics_saved: Dictionary = {}
static var _physics_stepping := false
static var _paused_by_step := false


static func restore_physics(rt: Node) -> void:
    if _physics_saved.is_empty():
        _physics_stepping = false
        return
    _apply_physics_saved(rt, true)


static func physics(rt: Node, params: Dictionary) -> Dictionary:
    var action := str(params.get("action", "step")).strip_edges()
    if action == "hold":
        # Held ticks must reach pausable nodes, so a pause the pause-based step left is cleared.
        if _paused_by_step:
            rt.get_tree().paused = false
            _paused_by_step = false
        return Hold.begin(rt)
    if action == "status":
        return R.ok(_physics_status(rt))
    if Hold.is_armed():
        if action == "resume":
            Hold.release(rt)
            var released := _physics_status(rt)
            released["action"] = "resume"
            return R.ok(released)
        if action == "step":
            return Hold.step(rt, params)
    elif action == "step" and params.has("until"):
        return R.err(R.BAD_PARAMS, "until needs a held game. Hold it first with action \"hold\".")
    if action == "resume":
        if _physics_stepping:
            return R.err(R.BAD_PARAMS, "A physics step is still running.")
        rt.get_tree().paused = false
        _paused_by_step = false
        var resumed := _physics_status(rt)
        resumed["action"] = "resume"
        return R.ok(resumed)
    if action != "step":
        return R.err(R.BAD_PARAMS, "Unknown physics action '%s'." % action)
    var ticks: Variant = _whole_number(params.get("ticks", null))
    if ticks == null or int(ticks) < 1 or int(ticks) > MAX_PHYSICS_TICKS:
        return R.err(R.BAD_PARAMS, "ticks must be an integer from 1 to 120.")
    if _physics_stepping:
        return R.err(R.BAD_PARAMS, "A physics step is already running.")
    _arm_physics(rt)
    # Ticks come at the game's own rate, at most one per frame, so the wait is measured in time:
    # generously more than the ticks need, and less than the server waits for an answer.
    var wait_ms := mini(15000, 2000 + int(ticks) * 4000 / maxi(Engine.physics_ticks_per_second, 1))
    var state := {
        "ticks": int(ticks),
        "start": Engine.get_physics_frames(),
        "deadline": Time.get_ticks_msec() + wait_ms,
    }
    return {"_pending": true, "_tick": func() -> bool: return _tick_physics(rt, state)}


static func _arm_physics(rt: Node) -> void:
    _physics_stepping = true
    _physics_saved = {
        "paused": rt.get_tree().paused,
        "max_steps": Engine.max_physics_steps_per_frame,
    }
    # At most one physics iteration per process frame, so this runtime sees every iteration and
    # can pause the tree right after the Nth. The tick rate and the time scale stay as they are:
    # every tick has the game's normal physics delta, so the game advances by N ticks of its
    # own time. SceneTree._physics_process is not a function a script can call.
    Engine.max_physics_steps_per_frame = 1
    rt.get_tree().paused = false


static func _apply_physics_saved(rt: Node, pause_after: bool) -> void:
    var saved := _physics_saved.duplicate(true)
    _physics_saved = {}
    _physics_stepping = false
    if not saved.is_empty():
        Engine.max_physics_steps_per_frame = int(saved["max_steps"])
    if rt.get_tree() != null:
        rt.get_tree().paused = true if pause_after else bool(saved.get("paused", true))
        _paused_by_step = pause_after


static func _tick_physics(rt: Node, state: Dictionary) -> bool:
    var advanced := Engine.get_physics_frames() - int(state["start"])
    if advanced >= int(state["ticks"]):
        var wanted := int(state["ticks"])
        _apply_physics_saved(rt, true)
        rt.finish_job(R.ok(_physics_body(rt, wanted, advanced)))
        return false
    if Time.get_ticks_msec() >= int(state["deadline"]):
        var wanted := int(state["ticks"])
        _apply_physics_saved(rt, true)
        rt.finish_job(R.err(
            R.GODOT_ERROR,
            "Physics did not advance %d ticks in time." % wanted,
            {"advanced": advanced, "scene": rt.scene_context()},
        ))
        return false
    return true


static func _physics_body(rt: Node, wanted: int, advanced: int) -> Dictionary:
    var body := _physics_status(rt)
    body["action"] = "step"
    body["ticks"] = advanced
    body["requested"] = wanted
    body["physics_frames"] = Engine.get_physics_frames()
    return body


static func _physics_status(rt: Node) -> Dictionary:
    var body := {
        "held": Hold.is_held(),
        "total_ticks": Hold.total_ticks(),
        "paused": rt.get_tree().paused,
        "stepping": _physics_stepping,
        "time_scale": Engine.time_scale,
        "physics_ticks_per_second": Engine.physics_ticks_per_second,
        "max_physics_steps_per_frame": Engine.max_physics_steps_per_frame,
        "scene": rt.scene_context(),
        "limits": _physics_limits(),
    }
    body.merge(Hold.exactness())
    return body


static func _physics_limits() -> Dictionary:
    return {
        "advanced": "Engine physics iterations, with max_physics_steps_per_frame set to 1 while the step runs. Each has the normal physics delta (scaled by Engine.time_scale), so N ticks are N / physics_ticks_per_second seconds of game time. This tool does not call node _physics_process itself.",
        "still_runs": "Idle _process is not stepped. Nodes with process_mode ALWAYS still receive _process and _physics_process while the tree is paused. The engine physics-frame counter also keeps increasing while paused, so it is not a count of pausable game physics.",
        "not_covered": "Custom threads and GPU compute are not stepped or flushed, and nothing here waits for the GPU.",
        "determinism": "This does not make the game deterministic.",
    }


static func _whole_number(raw: Variant) -> Variant:
    if raw is int:
        return int(raw)
    if raw is float and float(int(raw)) == float(raw):
        return int(raw)
    return null


static func _need(rt: Node, params: Dictionary) -> Node:
    return rt.find_node(str(params.get("path", ".")))


static func _signal_list(node: Node) -> Array:
    var out: Array = []
    for info: Dictionary in node.get_signal_list():
        var name := str(info.get("name", ""))
        var connections: Array = []
        for connection: Dictionary in node.get_signal_connection_list(name):
            var callable: Callable = connection.get("callable", Callable())
            connections.append({
                "method": callable.get_method() if callable.is_valid() else "",
                "flags": connection.get("flags", 0),
            })
        out.append({"name": name, "connections": connections})
    return out


static func _disconnect_all(node: Node, connected: Array) -> void:
    for entry: Dictionary in connected:
        var hook: Callable = entry["hook"]
        if is_instance_valid(node) and node.is_connected(entry["signal"], hook):
            node.disconnect(entry["signal"], hook)


static func _type_compatible(value: Variant, expected: int) -> bool:
    if expected == TYPE_FLOAT and (value is float or value is int):
        return true
    if expected == TYPE_INT and value is int:
        return true
    return typeof(value) == expected


static func _coerce(value: Variant, expected: int) -> Variant:
    if (expected == TYPE_INT or expected == TYPE_FLOAT) and (value is float or value is int):
        return int(value) if expected == TYPE_INT else float(value)
    if value is Array:
        var items: Array = value
        if expected == TYPE_VECTOR2 and items.size() >= 2:
            return Vector2(float(items[0]), float(items[1]))
        if expected == TYPE_VECTOR3 and items.size() >= 3:
            return Vector3(float(items[0]), float(items[1]), float(items[2]))
        if expected == TYPE_COLOR and items.size() >= 3:
            var alpha := float(items[3]) if items.size() > 3 else 1.0
            return Color(float(items[0]), float(items[1]), float(items[2]), alpha)
        if expected == TYPE_NIL and items.size() == 2 and (items[0] is float or items[0] is int):
            return Vector2(float(items[0]), float(items[1]))
    if value is String and expected == TYPE_STRING_NAME:
        return StringName(value)
    if value is String and expected == TYPE_NODE_PATH:
        return NodePath(value)
    if value is String and (expected == TYPE_VECTOR2 or expected == TYPE_VECTOR3 or expected == TYPE_COLOR):
        var parsed: Variant = str_to_var(value)
        if parsed != null:
            return parsed
    return value
