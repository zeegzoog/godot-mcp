extends RefCounted
## Live scene inspection. Property writes and method calls are intentionally narrow:
## `script` cannot be set, and a method runs only when the caller lists it in `permit`
## and it is not on the denylist (no eval, no quit, no trampoline through call/rpc).

const R := preload("rt_result.gd")
const DENY: Array[String] = [
    "quit", "set_script", "call", "callv", "call_deferred", "rpc", "rpc_id", "free",
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
            if name == "script":
                continue
            props[name] = rt.encode(node.get(name))
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
        if name == "script" or name.begins_with("_") or name.contains(":"):
            return R.err(R.DENIED, "Refusing to set '%s'." % name)
        var current: Variant = node.get(name)
        var decoded: Variant = _coerce(properties[key], typeof(current) if current != null else TYPE_NIL)
        node.set(name, decoded)
        readback[name] = rt.encode(node.get(name))
    return R.ok({"path": rt.path_of(node), "properties": readback})


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
    if method == "queue_free" and (node == rt.get_tree().root or node == rt or node == rt.scene_root()):
        return R.err(R.DENIED, "Refusing to free the scene root or the runtime.")
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
    return R.ok({
        "scene": root.scene_file_path if root != null else "",
        "scene_name": String(root.name) if root != null else "",
        "frame": Engine.get_process_frames(),
        "paused": rt.get_tree().paused,
        "children": children,
    })


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
    if value is String and (expected == TYPE_VECTOR2 or expected == TYPE_VECTOR3 or expected == TYPE_COLOR):
        var parsed: Variant = str_to_var(value)
        if parsed != null:
            return parsed
    return value
