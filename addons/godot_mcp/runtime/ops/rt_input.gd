extends RefCounted
## Input injection for a running game.
##
## Events go through Input.parse_input_event and Input.flush_buffered_events, which is the
## same path a real device uses (including the headless display server). Mouse events do
## not move the OS cursor unless the call passes warp_mouse. Presses are
## remembered so input.release_all can clear them. A tap is delivered at the start of a
## frame, held for one process frame, then released. click, click_node, swipe, and path are
## gestures on that same path. path presses at the first point, moves through the rest, and
## releases at the last. A step that fails releases every held button and touch.

const R := preload("rt_result.gd")

## The most frames one path or circle step may take; the same bound as a wait step.
const MAX_PATH_FRAMES := 600

const MOUSE_BUTTONS := {
    "left": MOUSE_BUTTON_LEFT,
    "right": MOUSE_BUTTON_RIGHT,
    "middle": MOUSE_BUTTON_MIDDLE,
}
const JOY_BUTTONS := {
    "a": JOY_BUTTON_A, "b": JOY_BUTTON_B, "x": JOY_BUTTON_X, "y": JOY_BUTTON_Y,
    "back": JOY_BUTTON_BACK, "start": JOY_BUTTON_START, "guide": JOY_BUTTON_GUIDE,
    "left_stick": JOY_BUTTON_LEFT_STICK, "right_stick": JOY_BUTTON_RIGHT_STICK,
    "left_shoulder": JOY_BUTTON_LEFT_SHOULDER, "right_shoulder": JOY_BUTTON_RIGHT_SHOULDER,
    "dpad_up": JOY_BUTTON_DPAD_UP, "dpad_down": JOY_BUTTON_DPAD_DOWN,
    "dpad_left": JOY_BUTTON_DPAD_LEFT, "dpad_right": JOY_BUTTON_DPAD_RIGHT,
}
const JOY_AXES := {
    "left_x": JOY_AXIS_LEFT_X, "left_y": JOY_AXIS_LEFT_Y,
    "right_x": JOY_AXIS_RIGHT_X, "right_y": JOY_AXIS_RIGHT_Y,
    "trigger_left": JOY_AXIS_TRIGGER_LEFT, "trigger_right": JOY_AXIS_TRIGGER_RIGHT,
}

static var _keys: Dictionary = {}
static var _mouse: Dictionary = {}
static var _joy_buttons: Dictionary = {}
static var _joy_axes: Dictionary = {}
static var _touches: Dictionary = {}
static var _mouse_pos := Vector2.ZERO
static var _warp_mouse := false
static var _saved_time_scale := -1.0
static var _scheduled: Array = []


static func methods() -> Dictionary:
    return {
        "input.run": {"fn": run_steps},
        "input.release_all": {"fn": release_all},
        "input.wait": {"fn": wait_frames},
    }


static func run_steps(rt: Node, params: Dictionary) -> Dictionary:
    var parsed := parse_steps(params)
    if R.is_err(parsed):
        return parsed
    var state := {"steps": parsed["steps"], "index": 0, "mode": "lead", "results": [], "hold": 0, "release": {}}
    return {"_pending": true, "_tick": func() -> bool: return _tick_steps(rt, state)}


## Validates the steps of an input call and notes its warp_mouse choice. Returns {"steps"}.
static func parse_steps(params: Dictionary) -> Dictionary:
    var raw: Variant = params.get("steps", [])
    if params.has("warp_mouse") and not params["warp_mouse"] is bool:
        return R.err(R.BAD_PARAMS, "input.run: warp_mouse must be a boolean.")
    _warp_mouse = bool(params.get("warp_mouse", false))
    if not raw is Array or (raw as Array).is_empty():
        return R.err(R.BAD_PARAMS, "input.run: 'steps' must be a non-empty array.")
    var steps: Array = []
    var index := 0
    for entry: Variant in raw:
        if not entry is Dictionary:
            return R.err(R.BAD_PARAMS, "steps[%d] must be an object." % index)
        var parsed := _parse(entry, index)
        if R.is_err(parsed):
            return parsed
        steps.append(parsed["step"])
        index += 1
    return {"steps": steps}


## One step of a held game's input, applied now and never waiting for a frame. A tap is pressed
## now and let go by release_due() once the game's tick count reaches `release_at`.
static func apply_held_step(rt: Node, step: Dictionary, release_at: int) -> Dictionary:
    var kind := str(step["type"])
    if kind == "click_node":
        var resolved := _resolve_click_node(rt, step)
        if R.is_err(resolved):
            return resolved
        step = resolved["step"]
        kind = str(step["type"])
    if kind == "time_scale":
        var note := _set_time_scale(float(step["scale"]))
        return {"result": {
            "type": "time_scale",
            "ok": true,
            "time_scale": note["time_scale"],
            "previous_time_scale": note["previous_time_scale"],
        }}
    if kind == "key" or kind == "mouse_button" or kind == "touch" or kind == "joy_button":
        var phase := str(step["phase"])
        if phase == "tap":
            _apply_phase(step, "press")
            _scheduled.append({"due": release_at, "step": step})
        else:
            _apply_phase(step, phase)
        return {"result": _done(step, {"type": str(step.get("label", kind)), "ok": true})}
    if kind == "mouse_move":
        _mouse_motion(float(step["x"]), float(step["y"]))
        Input.flush_buffered_events()
    elif kind == "joy_axis":
        _joy_axis(str(step["axis"]), float(step["value"]), int(step["device"]))
        Input.flush_buffered_events()
    else:
        return R.err(R.BAD_PARAMS, "A held game cannot apply a '%s' step." % kind)
    return {"result": _done(step, {"type": kind, "ok": true})}


## Lets go of the taps whose hold ticks are over, before the tick that is about to start runs.
static func release_due(total_ticks: int) -> void:
    var remaining: Array = []
    for entry: Dictionary in _scheduled:
        if total_ticks >= int(entry["due"]):
            _release_tap(entry["step"])
        else:
            remaining.append(entry)
    _scheduled = remaining


## Lets go of everything held or scheduled, as input.release_all does.
static func release_everything() -> void:
    _release_everything()
    Input.flush_buffered_events()


static func held_summary() -> Dictionary:
    return _held_summary()


static func warp_enabled() -> bool:
    return _warp_mouse


static func release_all(rt: Node, _params: Dictionary) -> Dictionary:
    var released := _release_everything()
    Input.flush_buffered_events()
    return R.ok({"released": released, "held": _held_summary()})


static func wait_frames(rt: Node, params: Dictionary) -> Dictionary:
    var scaled := _note_time_scale(params)
    if R.is_err(scaled):
        return scaled
    var note: Dictionary = scaled["note"]
    var node_path := str(params.get("node", "")).strip_edges()
    var sig := str(params.get("signal", "")).strip_edges()
    var property := str(params.get("property", "")).strip_edges()
    var has_generation := params.has("scene_generation")
    if not sig.is_empty() and (not property.is_empty() or has_generation):
        return R.err(R.BAD_PARAMS, "Wait for a signal on its own. A property or scene_generation wait cannot include a signal.")
    if not property.is_empty() or has_generation:
        return _begin_condition(rt, params, node_path, property, has_generation, note)
    if not sig.is_empty() and node_path.is_empty():
        return R.err(R.BAD_PARAMS, "input.wait: a signal needs a node path.")
    if not node_path.is_empty() or not sig.is_empty():
        var timeout := clampi(int(params.get("timeout", params.get("frames", 180))), 1, 600)
        var state := {
            "until": _until_state(node_path, sig, timeout),
            "note": note,
            "results": [],
            "index": 0,
            "mode": "until",
            "tool_wait": true,
        }
        return {"_pending": true, "_tick": func() -> bool: return _tick_until(rt, state)}
    var frames := clampi(int(params.get("frames", 1)), 0, 600)
    var state := {"left": frames, "start": Engine.get_process_frames(), "note": note}
    return {"_pending": true, "_tick": func() -> bool:
        if int(state["left"]) > 0:
            state["left"] = int(state["left"]) - 1
            return true
        var body := {"frames": Engine.get_process_frames() - int(state["start"])}
        body.merge(state["note"])
        rt.finish_job(R.ok(body))
        return false
    }


static func restore_time_scale() -> void:
    if _saved_time_scale >= 0.0:
        Engine.time_scale = _saved_time_scale
        _saved_time_scale = -1.0


static func _tick_steps(rt: Node, state: Dictionary) -> bool:
    var steps: Array = state["steps"]
    var index := int(state["index"])
    var mode := str(state["mode"])
    if mode == "lead":
        state["mode"] = "body"
        return true
    if mode == "hold":
        _release_tap(state["release"])
        state["release"] = {}
        var done: Dictionary = steps[index]
        (state["results"] as Array).append(_done(done, {"type": str(done.get("label", done["type"])), "ok": true}))
        state["index"] = index + 1
        state["mode"] = "body"
        index += 1
        mode = "body"
    if mode == "wait":
        state["hold"] = int(state["hold"]) - 1
        if int(state["hold"]) > 0:
            return true
        (state["results"] as Array).append({"type": "wait", "ok": true})
        state["index"] = index + 1
        state["mode"] = "body"
        index += 1
    if mode == "until":
        return _tick_until(rt, state)
    if mode == "gesture":
        return _tick_gesture(rt, state)
    if mode == "text":
        return _tick_text(rt, state)
    if index >= steps.size():
        rt.finish_job(R.ok({"steps": state["results"], "held": _held_summary(), "warp_mouse": _warp_mouse}))
        return false
    var step: Dictionary = steps[index]
    var kind := str(step["type"])
    if kind == "click_node":
        var resolved := _resolve_click_node(rt, step)
        if R.is_err(resolved):
            return _fail(rt, state, resolved)
        step = resolved["step"]
        steps[index] = step
        kind = str(step["type"])
    if kind == "swipe" or kind == "path":
        var origin := Vector2.ZERO
        if kind == "swipe":
            origin = Vector2(float(step["x"]), float(step["y"]))
        else:
            origin = (step["points"] as Array)[0]
        state["gesture"] = {"step": step, "i": 0, "at": origin}
        state["mode"] = "gesture"
        return _tick_gesture(rt, state)
    if kind == "time_scale":
        var note := _set_time_scale(float(step["scale"]))
        (state["results"] as Array).append({
            "type": "time_scale",
            "ok": true,
            "time_scale": note["time_scale"],
            "previous_time_scale": note["previous_time_scale"],
        })
        state["index"] = index + 1
        return true
    if kind == "wait":
        if not str(step.get("node", "")).is_empty() or not str(step.get("signal", "")).is_empty():
            state["until"] = _until_state(str(step["node"]), str(step["signal"]), int(step["timeout"]))
            state["mode"] = "until"
            state["tool_wait"] = false
            return _tick_until(rt, state)
        var frames := int(step["frames"])
        if frames <= 0:
            (state["results"] as Array).append({"type": "wait", "ok": true})
            state["index"] = index + 1
            return true
        state["hold"] = frames
        state["mode"] = "wait"
        return true
    if kind == "text":
        var chars: Array = []
        for character in str(step["text"]):
            chars.append(character.unicode_at(0))
        state["chars"] = chars
        state["char_i"] = 0
        state["char_down"] = false
        state["mode"] = "text"
        return _tick_text(rt, state)
    if kind == "key" or kind == "mouse_button" or kind == "touch" or kind == "joy_button":
        var phase := str(step["phase"])
        if phase == "tap":
            _apply_phase(step, "press")
            state["release"] = step
            state["mode"] = "hold"
            return true
        _apply_phase(step, phase)
        (state["results"] as Array).append(_done(step, {"type": str(step.get("label", kind)), "ok": true}))
        state["index"] = index + 1
        return true
    if kind == "mouse_move":
        _mouse_motion(float(step["x"]), float(step["y"]))
        Input.flush_buffered_events()
    elif kind == "joy_axis":
        _joy_axis(str(step["axis"]), float(step["value"]), int(step["device"]))
        Input.flush_buffered_events()
    (state["results"] as Array).append(_done(step, {"type": kind, "ok": true}))
    state["index"] = index + 1
    return true


static func _tick_text(rt: Node, state: Dictionary) -> bool:
    var chars: Array = state["chars"]
    var char_i := int(state["char_i"])
    if bool(state["char_down"]):
        _unicode(int(chars[char_i]), false)
        Input.flush_buffered_events()
        state["char_down"] = false
        state["char_i"] = char_i + 1
        char_i += 1
    if char_i >= chars.size():
        (state["results"] as Array).append({"type": "text", "ok": true})
        state["index"] = int(state["index"]) + 1
        state["mode"] = "body"
        return true
    _unicode(int(chars[char_i]), true)
    Input.flush_buffered_events()
    state["char_down"] = true
    return true


static func _apply_phase(step: Dictionary, phase: String) -> void:
    var kind := str(step["type"])
    if kind == "key":
        var code := int(step["code"])
        if phase == "release":
            _key_event(code, false)
            _keys.erase(code)
        else:
            _key_event(code, true)
            _keys[code] = true
        Input.flush_buffered_events()
    elif kind == "mouse_button":
        var button := str(step["button"])
        var index := int(MOUSE_BUTTONS[button])
        if phase == "release":
            _mouse_event(index, false, float(step["x"]), float(step["y"]))
            _mouse.erase(button)
        else:
            _mouse_motion(float(step["x"]), float(step["y"]))
            _mouse_event(index, true, float(step["x"]), float(step["y"]))
            _mouse[button] = true
        Input.flush_buffered_events()
    elif kind == "touch":
        var touch_index := int(step["index"])
        if phase == "release":
            _touch_event(touch_index, false, float(step["x"]), float(step["y"]))
            _touches.erase(touch_index)
        else:
            _touch_event(touch_index, true, float(step["x"]), float(step["y"]))
            _touches[touch_index] = Vector2(float(step["x"]), float(step["y"]))
        Input.flush_buffered_events()
    elif kind == "joy_button":
        var button_name := str(step["button"])
        var joy_index := int(JOY_BUTTONS[button_name])
        var device := int(step["device"])
        var key := "%d:%s" % [device, button_name]
        if phase == "release":
            _joy_button_event(device, joy_index, false)
            _joy_buttons.erase(key)
        else:
            _joy_button_event(device, joy_index, true)
            _joy_buttons[key] = true
        Input.flush_buffered_events()


static func _fail(rt: Node, state: Dictionary, error: Dictionary) -> bool:
    if state.has("until"):
        _disconnect_wait(state["until"])
    _release_everything()
    Input.flush_buffered_events()
    rt.finish_job(error)
    return false


static func _note_time_scale(params: Dictionary) -> Dictionary:
    if not params.has("time_scale"):
        return {"note": {}}
    var parsed := _parse_time_scale(params.get("time_scale"))
    if R.is_err(parsed):
        return parsed
    return {"note": _set_time_scale(float(parsed["scale"]))}


static func _parse_time_scale(raw: Variant) -> Dictionary:
    if not (raw is float or raw is int):
        return R.err(R.BAD_PARAMS, "time_scale must be a number from 0 to 8.")
    var scale := float(raw)
    if scale < 0.0 or scale > 8.0:
        return R.err(R.BAD_PARAMS, "time_scale must be a number from 0 to 8.")
    return {"scale": scale}


static func _set_time_scale(scale: float) -> Dictionary:
    var previous := Engine.time_scale
    if _saved_time_scale < 0.0:
        _saved_time_scale = previous
    Engine.time_scale = scale
    return {"previous_time_scale": previous, "time_scale": scale}


const _WAIT_OPS: Array[String] = ["eq", "ne", "lt", "lte", "gt", "gte", "exists"]


static func _begin_condition(rt: Node, params: Dictionary, node_path: String, property: String, has_generation: bool, note: Dictionary) -> Dictionary:
    if not property.is_empty() and node_path.is_empty():
        return R.err(R.BAD_PARAMS, "input.wait: a property needs a node path.")
    var op := ""
    if not property.is_empty():
        op = str(params.get("op", "")).strip_edges()
        if not op in _WAIT_OPS:
            return R.err(R.BAD_PARAMS, "op must be eq, ne, lt, lte, gt, gte, or exists.")
        if op != "exists" and not params.has("value"):
            return R.err(R.BAD_PARAMS, "op '%s' needs a value." % op)
    var generation := 0
    if has_generation:
        var whole: Variant = _whole_number(params.get("scene_generation"))
        if whole == null or int(whole) < 0:
            return R.err(R.BAD_PARAMS, "scene_generation must be a whole number from 0 up.")
        generation = int(whole)
    var timeout := clampi(int(params.get("timeout", params.get("frames", 180))), 1, 600)
    var state := {
        "node": node_path,
        "property": property,
        "op": op,
        "has_property": not property.is_empty(),
        "has_generation": has_generation,
        "generation": generation,
        "value": params.get("value", null),
        "left": timeout,
        "start": Engine.get_process_frames(),
        "note": note,
    }
    return {"_pending": true, "_tick": func() -> bool: return _tick_condition(rt, state)}


static func _tick_condition(rt: Node, state: Dictionary) -> bool:
    var scene: Dictionary = rt.scene_context()
    var generation_ok := true
    if bool(state["has_generation"]):
        generation_ok = int(scene.get("scene_generation", 0)) > int(state["generation"])
    var path := str(state["node"])
    var node: Node = rt.find_node(path) if not path.is_empty() else null
    var node_ok := path.is_empty() or (node != null and is_instance_valid(node))
    var property_ok := not bool(state["has_property"])
    var exists := false
    var observed: Variant = null
    if node_ok and bool(state["has_property"]) and node != null:
        var looked := _lookup(node, str(state["property"]))
        if R.is_err(looked):
            return _fail_condition(rt, _with_scene(looked, scene))
        exists = bool(looked.get("exists", false))
        if exists:
            observed = looked.get("value", null)
            if str(state["op"]) == "exists":
                property_ok = true
            else:
                var compared := _compare(str(state["op"]), observed, state.get("value", null))
                if R.is_err(compared):
                    return _fail_condition(rt, _with_scene(compared, scene, rt.encode(observed), true))
                property_ok = bool(compared.get("match", false))
        else:
            property_ok = false
    if generation_ok and node_ok and property_ok:
        var body := {
            "ok": true,
            "frames": Engine.get_process_frames() - int(state["start"]),
            "scene": scene,
        }
        if not path.is_empty():
            body["path"] = path
        if bool(state["has_property"]):
            body["property"] = str(state["property"])
            body["op"] = str(state["op"])
            body["exists"] = true
            body["observed"] = rt.encode(observed)
        if bool(state["has_generation"]):
            body["scene_generation"] = int(scene.get("scene_generation", 0))
        body.merge(state.get("note", {}), true)
        rt.finish_job(R.ok(body))
        return false
    state["left"] = int(state["left"]) - 1
    if int(state["left"]) > 0:
        return true
    var what := str(state["property"]) if bool(state["has_property"]) else "scene generation newer than %d" % int(state["generation"])
    return _fail_condition(rt, R.err(R.NOT_FOUND, "Timed out waiting for '%s'." % what, {
        "observed": rt.encode(observed) if exists else null,
        "exists": exists,
        "property": str(state["property"]),
        "op": str(state["op"]),
        "scene": scene,
    }))


## Parses the condition of a held step: the property triple of a wait, checked before any tick runs.
static func parse_condition(raw: Variant) -> Dictionary:
    if not raw is Dictionary:
        return R.err(R.BAD_PARAMS, "until must be an object with node, property, op and value.")
    var spec: Dictionary = raw
    var node_path := str(spec.get("node", "")).strip_edges()
    var property := str(spec.get("property", "")).strip_edges()
    if node_path.is_empty() or property.is_empty():
        return R.err(R.BAD_PARAMS, "until needs a node and a property.")
    var op := str(spec.get("op", "")).strip_edges()
    if not op in _WAIT_OPS:
        return R.err(R.BAD_PARAMS, "op must be eq, ne, lt, lte, gt, gte, or exists.")
    if op != "exists" and not spec.has("value"):
        return R.err(R.BAD_PARAMS, "op '%s' needs a value." % op)
    for part: String in property.split("/", false):
        var named := _public_segment(part)
        if R.is_err(named):
            return named
    return {"cond": {"node": node_path, "property": property, "op": op, "value": spec.get("value", null)}}


## Reads a parsed condition now. A node or property that cannot be read yet, or values that
## cannot be compared, make it not hold: the step goes on and is bounded by its maximum.
static func read_condition(rt: Node, cond: Dictionary) -> Dictionary:
    var node: Node = rt.find_node(str(cond["node"]))
    if node == null or not is_instance_valid(node):
        return {"holds": false, "exists": false, "observed": null}
    var looked := _lookup(node, str(cond["property"]))
    if R.is_err(looked) or not bool(looked.get("exists", false)):
        return {"holds": false, "exists": false, "observed": null}
    var observed: Variant = looked.get("value", null)
    if str(cond["op"]) == "exists":
        return {"holds": true, "exists": true, "observed": rt.encode(observed)}
    var compared := _compare(str(cond["op"]), observed, cond.get("value", null))
    return {
        "holds": not R.is_err(compared) and bool(compared.get("match", false)),
        "exists": true,
        "observed": rt.encode(observed),
    }


static func _fail_condition(rt: Node, result: Dictionary) -> bool:
    rt.finish_job(result)
    return false


static func _with_scene(result: Dictionary, scene: Dictionary, observed: Variant = null, exists: bool = false) -> Dictionary:
    if result.has("error") and result["error"] is Dictionary:
        var data: Dictionary = {}
        var existing: Variant = (result["error"] as Dictionary).get("data", null)
        if existing is Dictionary:
            data = (existing as Dictionary).duplicate(true)
        data["scene"] = scene
        if exists:
            data["observed"] = observed
            data["exists"] = true
        result["error"]["data"] = data
    return result


static func _lookup(node: Node, path: String) -> Dictionary:
    var parts: PackedStringArray = path.split("/", false)
    if parts.is_empty():
        return R.err(R.BAD_PARAMS, "property must name a public property.")
    # Some properties carry the slash in their own name (metadata/note,
    # theme_override_colors/font_color). Read those whole before walking the path.
    if parts.size() > 1 and _has_public_property(node, path):
        for part: String in parts:
            var whole := _public_segment(part)
            if R.is_err(whole):
                return whole
        return {"exists": true, "value": node.get(path)}
    var current: Variant = node
    var index := 0
    for part: String in parts:
        var named := _public_segment(part)
        if R.is_err(named):
            return named
        if index == 0:
            if not _has_public_property(node, part):
                return {"exists": false}
            current = node.get(part)
        elif current is Dictionary:
            if not (current as Dictionary).has(part):
                return {"exists": false}
            current = (current as Dictionary)[part]
        elif current is Array:
            return R.err(R.BAD_PARAMS, "Cannot read '%s' inside an Array." % part)
        elif current is Object and current != null:
            var object := current as Object
            if not _has_public_property(object, part):
                return {"exists": false}
            current = object.get(part)
        else:
            return R.err(R.BAD_PARAMS, "Cannot read '%s' inside a %s." % [part, type_string(typeof(current))])
        index += 1
    return {"exists": true, "value": current}


static func _public_segment(part: String) -> Dictionary:
    if part.is_empty() or part.begins_with("_") or part.contains(":"):
        return R.err(R.DENIED, "Refusing private property '%s'." % part)
    if part == "script":
        return R.err(R.DENIED, "Refusing property 'script'.")
    return {}


static func _has_public_property(object: Object, part: String) -> bool:
    for info: Dictionary in object.get_property_list():
        if str(info.get("name", "")) != part:
            continue
        var usage := int(info.get("usage", 0))
        if usage & PROPERTY_USAGE_CATEGORY != 0 or usage & PROPERTY_USAGE_GROUP != 0 or usage & PROPERTY_USAGE_SUBGROUP != 0:
            continue
        return true
    return false


static func _compare(op: String, observed: Variant, expected: Variant) -> Dictionary:
    # bool passes `is int` in GDScript. Handle it before the numeric path so true does not
    # become 1 and compare equal to a JSON number.
    if observed is bool or expected is bool:
        if not (observed is bool and expected is bool):
            return R.err(R.BAD_PARAMS, "Type mismatch: observed is %s and the value is %s." % [type_string(typeof(observed)), type_string(typeof(expected))])
        if op != "eq" and op != "ne":
            return R.err(R.BAD_PARAMS, "Ordering needs both sides to be int or both to be float.")
        var same_bool := bool(observed) == bool(expected)
        return {"match": same_bool if op == "eq" else not same_bool}
    if observed is String or observed is StringName:
        if not (expected is String or expected is StringName):
            return R.err(R.BAD_PARAMS, "Type mismatch: observed is text and the value is %s." % type_string(typeof(expected)))
        if op != "eq" and op != "ne":
            return R.err(R.BAD_PARAMS, "Ordering needs both sides to be int or both to be float.")
        var same := str(observed) == str(expected)
        return {"match": same if op == "eq" else not same}
    if observed is int or observed is float or expected is int or expected is float:
        return _compare_numbers(op, observed, expected)
    if op != "eq" and op != "ne":
        return R.err(R.BAD_PARAMS, "Ordering needs both sides to be int or both to be float.")
    if typeof(observed) != typeof(expected):
        return R.err(R.BAD_PARAMS, "Type mismatch: observed is %s and the value is %s." % [type_string(typeof(observed)), type_string(typeof(expected))])
    var same_value := bool(observed == expected)
    return {"match": same_value if op == "eq" else not same_value}


static func _compare_numbers(op: String, observed: Variant, expected: Variant) -> Dictionary:
    if not (observed is int or observed is float) or not (expected is int or expected is float):
        return R.err(R.BAD_PARAMS, "Type mismatch: observed is %s and the value is %s." % [type_string(typeof(observed)), type_string(typeof(expected))])
    var as_ints := false
    var as_floats := false
    if observed is int and expected is int:
        as_ints = true
    elif observed is float and expected is float:
        as_floats = true
    elif observed is int and _whole_number(expected) != null:
        # JSON has one number type, so Godot delivers a whole number as a float.
        as_ints = true
    elif observed is float and expected is int:
        as_floats = true
    elif observed is int and expected is float:
        return R.err(R.BAD_PARAMS, "Type mismatch: a fractional number cannot be compared with an int.")
    else:
        return R.err(R.BAD_PARAMS, "Type mismatch: observed is %s and the value is %s." % [type_string(typeof(observed)), type_string(typeof(expected))])
    var match_value := false
    if as_ints:
        var left := int(observed)
        var right := int(expected) if expected is int else int(_whole_number(expected))
        match op:
            "eq":
                match_value = left == right
            "ne":
                match_value = left != right
            "lt":
                match_value = left < right
            "lte":
                match_value = left <= right
            "gt":
                match_value = left > right
            "gte":
                match_value = left >= right
            _:
                return R.err(R.BAD_PARAMS, "op must be eq, ne, lt, lte, gt, gte, or exists.")
    elif as_floats:
        var left_f := float(observed)
        var right_f := float(expected)
        match op:
            "eq":
                match_value = left_f == right_f
            "ne":
                match_value = left_f != right_f
            "lt":
                match_value = left_f < right_f
            "lte":
                match_value = left_f <= right_f
            "gt":
                match_value = left_f > right_f
            "gte":
                match_value = left_f >= right_f
            _:
                return R.err(R.BAD_PARAMS, "op must be eq, ne, lt, lte, gt, gte, or exists.")
    return {"match": match_value}


static func _whole_number(raw: Variant) -> Variant:
    if raw is int:
        return int(raw)
    if raw is float and float(int(raw)) == float(raw):
        return int(raw)
    return null


static func _until_state(path: String, sig: String, timeout: int) -> Dictionary:
    return {
        "node_path": path,
        "signal_name": sig,
        "left": timeout,
        "node": null,
        "hook": Callable(),
        "connected": false,
        "fired": false,
        "start": Engine.get_process_frames(),
    }


static func _tick_until(rt: Node, state: Dictionary) -> bool:
    var watching: Dictionary = state["until"]
    var path := str(watching["node_path"])
    var sig := str(watching["signal_name"])
    var node: Node = watching.get("node")
    if node == null or not is_instance_valid(node):
        node = rt.find_node(path) if not path.is_empty() else null
        watching["node"] = node
    if not sig.is_empty():
        if node != null and not bool(watching["connected"]):
            if not node.has_signal(sig):
                return _fail(rt, state, R.err(R.NOT_FOUND, "Node '%s' has no signal '%s'." % [path, sig]))
            # The node itself stores the flag. A lambda that writes a captured Dictionary
            # can copy that Dictionary, so the tick would never see the write.
            # A zero-argument lambda also never runs when the signal carries arguments.
            var argc := _signal_argc(node, sig)
            var target := node
            var hook := Callable()
            match argc:
                0:
                    hook = func() -> void: _mark_wait(target)
                1:
                    hook = func(_a) -> void: _mark_wait(target)
                2:
                    hook = func(_a, _b) -> void: _mark_wait(target)
                3:
                    hook = func(_a, _b, _c) -> void: _mark_wait(target)
                _:
                    return _fail(rt, state, R.err(R.BAD_PARAMS, "Signal '%s' has too many arguments to wait for." % sig))
            var err := node.connect(sig, hook)
            if err != OK:
                return _fail(rt, state, R.err(R.GODOT_ERROR, "Could not wait for '%s': %s." % [sig, error_string(err)]))
            watching["hook"] = hook
            watching["connected"] = true
        if node != null and bool(node.get_meta(&"godot_mcp_wait_fired", false)):
            return _finish_until(rt, state, sig, path)
    elif node != null:
        return _finish_until(rt, state, "", path)
    watching["left"] = int(watching["left"]) - 1
    if int(watching["left"]) > 0:
        return true
    var found := node != null and is_instance_valid(node)
    var was_connected := bool(watching.get("connected", false))
    _disconnect_wait(watching)
    var what := sig if not sig.is_empty() else path
    return _fail(rt, state, R.err(R.NOT_FOUND, "Timed out waiting for '%s'." % what, {
        "connected": was_connected,
        "found": found,
    }))


static func _finish_until(rt: Node, state: Dictionary, sig: String, path: String) -> bool:
    var watching: Dictionary = state["until"]
    var waited := Engine.get_process_frames() - int(watching["start"])
    _disconnect_wait(watching)
    var body := {"ok": true, "path": path, "frames": waited}
    if not sig.is_empty():
        body["signal"] = sig
    if bool(state.get("tool_wait", false)):
        body.merge(state.get("note", {}))
        rt.finish_job(R.ok(body))
        return false
    body["type"] = "wait"
    (state["results"] as Array).append(body)
    state["index"] = int(state["index"]) + 1
    state["mode"] = "body"
    return true


static func _signal_argc(node: Node, sig: String) -> int:
    for info: Dictionary in node.get_signal_list():
        if str(info.get("name", "")) == sig:
            var args: Array = info.get("args", [])
            return args.size()
    return -1


static func _mark_wait(target: Node) -> void:
    if is_instance_valid(target):
        target.set_meta(&"godot_mcp_wait_fired", true)


static func _disconnect_wait(watching: Dictionary) -> void:
    var node: Node = watching.get("node")
    var hook: Callable = watching.get("hook", Callable())
    var sig := str(watching.get("signal_name", ""))
    if node != null and is_instance_valid(node):
        if hook.is_valid() and not sig.is_empty() and node.is_connected(sig, hook):
            node.disconnect(sig, hook)
        if node.has_meta(&"godot_mcp_wait_fired"):
            node.remove_meta(&"godot_mcp_wait_fired")
    watching["connected"] = false


static func _resolve_click_node(rt: Node, step: Dictionary) -> Dictionary:
    var path := str(step["node"])
    var node: Node = rt.find_node(path)
    if node == null:
        return R.err(R.NOT_FOUND, "No node at '%s' to click." % path)
    var point = _viewport_point(node)
    if point is Dictionary:
        return point
    var at: Vector2 = point
    var pointer := str(step["pointer"])
    if pointer == "touch":
        return {"step": {
            "type": "touch", "phase": "tap", "label": "click_node",
            "x": at.x, "y": at.y, "button": "left", "index": int(step["index"]),
        }}
    return {"step": {
        "type": "mouse_button", "phase": "tap", "label": "click_node",
        "x": at.x, "y": at.y, "button": str(step["button"]), "index": 0,
    }}


static func _viewport_point(node: Node) -> Variant:
    var viewport := node.get_viewport()
    if viewport == null:
        return R.err(R.NOT_FOUND, "Node '%s' has no viewport." % node.name)
    # The canvas transform is part of where a canvas item is drawn: a camera that scrolls, or a
    # CanvasLayer, moves the item on screen but not its global position.
    if node is Control:
        var control := node as Control
        return control.get_global_transform_with_canvas() * (control.size / 2.0)
    if node is Node2D:
        return (node as Node2D).get_global_transform_with_canvas().origin
    if node is Node3D:
        var camera := viewport.get_camera_3d()
        if camera == null:
            return R.err(R.NOT_FOUND, "No Camera3D to project '%s'." % node.name)
        var at: Vector3 = (node as Node3D).global_position
        if camera.is_position_behind(at):
            return R.err(R.BAD_PARAMS, "Node '%s' is behind the camera." % node.name)
        return camera.unproject_position(at)
    return R.err(R.BAD_PARAMS, "click_node needs a Control, Node2D, or Node3D.")


## The step's result with a warning when a pointer lands outside the root viewport (RO-11).
static func _done(step: Dictionary, result: Dictionary) -> Dictionary:
    var note := _pointer_note(step)
    if not note.is_empty():
        result["warning"] = note
    return result


## Controls outside the viewport do not react to the pointer, although `_input` still sees the
## event. A game without a window has a 64 x 64 viewport whatever the project sets, so most of a
## screen lies outside it. Empty when the step is not a pointer step or lands inside.
static func _pointer_note(step: Dictionary) -> String:
    var kind := str(step.get("type", ""))
    if not (kind == "mouse_button" or kind == "mouse_move" or kind == "touch" or kind == "swipe"):
        return ""
    var tree := Engine.get_main_loop() as SceneTree
    if tree == null:
        return ""
    var rect: Rect2 = tree.root.get_visible_rect()
    var points: Array[Vector2] = [Vector2(float(step["x"]), float(step["y"]))]
    if kind == "swipe":
        points.append(Vector2(float(step["to_x"]), float(step["to_y"])))
    for point: Vector2 in points:
        if not rect.has_point(point):
            var headless := DisplayServer.get_name() == "headless"
            return "The point (%d, %d) is outside the game's %d x %d viewport%s. Controls there do not react to the pointer, although _input and _unhandled_input still receive the event." % [
                int(point.x), int(point.y), int(rect.size.x), int(rect.size.y),
                " (a game without a window has a 64 x 64 viewport whatever the project sets; run it with a window to click controls elsewhere)" if headless else "",
            ]
    return ""


static func _tick_gesture(_rt: Node, state: Dictionary) -> bool:
    var gesture: Dictionary = state["gesture"]
    var step: Dictionary = gesture["step"]
    if str(step["type"]) == "path":
        return _tick_path(state)
    var frames := int(step["frames"])
    var i := int(gesture["i"])
    var from := Vector2(float(step["x"]), float(step["y"]))
    var to := Vector2(float(step["to_x"]), float(step["to_y"]))
    var at := from.lerp(to, clampf(float(i) / float(frames), 0.0, 1.0))
    if i == 0:
        _gesture_press(step, from)
    elif i < frames:
        _gesture_move(step, gesture["at"], at)
    else:
        _gesture_move(step, gesture["at"], to)
        _gesture_release(step, to)
        (state["results"] as Array).append(_done(step, {"type": "swipe", "ok": true}))
        state["index"] = int(state["index"]) + 1
        state["mode"] = "body"
        state["gesture"] = {}
        return true
    gesture["at"] = at if i > 0 else from
    gesture["i"] = i + 1
    return true


static func _tick_path(state: Dictionary) -> bool:
    var gesture: Dictionary = state["gesture"]
    var step: Dictionary = gesture["step"]
    var points: Array = step["points"]
    var per := int(step["frames_per_point"])
    var segments := points.size() - 1
    var total := segments * per
    var i := int(gesture["i"])
    if i == 0:
        var start: Vector2 = points[0]
        _gesture_press(step, start)
        gesture["at"] = start
        gesture["i"] = 1
        return true
    if i < total:
        var along := i
        var segment := mini(int((along - 1) / per), segments - 1)
        var within := (along - 1) - segment * per
        var from: Vector2 = points[segment]
        var to: Vector2 = points[segment + 1]
        var at := from.lerp(to, float(within + 1) / float(per))
        _gesture_move(step, gesture["at"], at)
        gesture["at"] = at
        gesture["i"] = i + 1
        return true
    var last: Vector2 = points[points.size() - 1]
    _gesture_move(step, gesture["at"], last)
    _gesture_release(step, last)
    (state["results"] as Array).append({"type": "path", "ok": true, "points": points.size()})
    state["index"] = int(state["index"]) + 1
    state["mode"] = "body"
    state["gesture"] = {}
    return true


static func _gesture_press(step: Dictionary, at: Vector2) -> void:
    if str(step["pointer"]) == "touch":
        var touch_index := int(step["index"])
        _touch_event(touch_index, true, at.x, at.y)
        _touches[touch_index] = at
    else:
        var button := str(step["button"])
        _mouse_motion(at.x, at.y)
        _mouse_event(int(MOUSE_BUTTONS[button]), true, at.x, at.y)
        _mouse[button] = true
    Input.flush_buffered_events()


static func _gesture_move(step: Dictionary, previous: Vector2, at: Vector2) -> void:
    if str(step["pointer"]) == "touch":
        var event := InputEventScreenDrag.new()
        event.index = int(step["index"])
        event.position = at
        event.relative = at - previous
        _touches[int(step["index"])] = at
        Input.parse_input_event(event)
    else:
        _mouse_motion(at.x, at.y)
    Input.flush_buffered_events()


static func _gesture_release(step: Dictionary, at: Vector2) -> void:
    if str(step["pointer"]) == "touch":
        var touch_index := int(step["index"])
        _touch_event(touch_index, false, at.x, at.y)
        _touches.erase(touch_index)
    else:
        var button := str(step["button"])
        _mouse_event(int(MOUSE_BUTTONS[button]), false, at.x, at.y)
        _mouse.erase(button)
    Input.flush_buffered_events()


static func _release_tap(step: Dictionary) -> void:
    if step.is_empty():
        return
    _apply_phase(step, "release")


static func _parse(step: Dictionary, index: int) -> Dictionary:
    var kind := str(step.get("type", ""))
    var phase := str(step.get("phase", "tap"))
    if not phase in ["tap", "press", "release"]:
        return R.err(R.BAD_PARAMS, "steps[%d]: phase must be tap, press, or release." % index)
    match kind:
        "key":
            var key := str(step.get("key", ""))
            var code := OS.find_keycode_from_string(key)
            if key.is_empty() or (code & KEY_CODE_MASK) == 0:
                return R.err(R.BAD_PARAMS, "steps[%d]: unknown key '%s'." % [index, key])
            return {"step": {"type": kind, "phase": phase, "code": code, "key": key}}
        "text":
            if str(step.get("text", "")).is_empty():
                return R.err(R.BAD_PARAMS, "steps[%d]: text is empty." % index)
            return {"step": {"type": kind, "text": str(step["text"])}}
        "mouse_move", "mouse_button", "touch":
            if not step.has("x") or not step.has("y"):
                return R.err(R.BAD_PARAMS, "steps[%d]: %s needs x and y." % [index, kind])
            var button := str(step.get("button", "left"))
            if kind == "mouse_button" and not MOUSE_BUTTONS.has(button):
                return R.err(R.BAD_PARAMS, "steps[%d]: unknown mouse button '%s'." % [index, button])
            return {"step": {
                "type": kind, "phase": phase, "x": float(step["x"]), "y": float(step["y"]),
                "button": button, "index": int(step.get("index", 0)),
            }}
        "click":
            if not step.has("x") or not step.has("y"):
                return R.err(R.BAD_PARAMS, "steps[%d]: click needs x and y." % index)
            var click_button := str(step.get("button", "left"))
            if not MOUSE_BUTTONS.has(click_button):
                return R.err(R.BAD_PARAMS, "steps[%d]: unknown mouse button '%s'." % [index, click_button])
            return {"step": {
                "type": "mouse_button", "phase": "tap", "label": "click",
                "x": float(step["x"]), "y": float(step["y"]), "button": click_button, "index": 0,
            }}
        "click_node":
            var node_path := str(step.get("node", "")).strip_edges()
            if node_path.is_empty():
                return R.err(R.BAD_PARAMS, "steps[%d]: click_node needs a node path." % index)
            var pointer := str(step.get("pointer", "mouse"))
            if not pointer in ["mouse", "touch"]:
                return R.err(R.BAD_PARAMS, "steps[%d]: pointer must be mouse or touch." % index)
            var node_button := str(step.get("button", "left"))
            if pointer == "mouse" and not MOUSE_BUTTONS.has(node_button):
                return R.err(R.BAD_PARAMS, "steps[%d]: unknown mouse button '%s'." % [index, node_button])
            return {"step": {
                "type": kind, "node": node_path, "pointer": pointer,
                "button": node_button, "index": int(step.get("index", 0)),
            }}
        "swipe":
            for key: String in ["x", "y", "to_x", "to_y"]:
                if not step.has(key):
                    return R.err(R.BAD_PARAMS, "steps[%d]: swipe needs x, y, to_x, and to_y." % index)
            var swipe_frames := int(step.get("frames", 8))
            if swipe_frames < 1 or swipe_frames > 60:
                return R.err(R.BAD_PARAMS, "steps[%d]: swipe frames must be from 1 to 60." % index)
            var swipe_pointer := str(step.get("pointer", "mouse"))
            if not swipe_pointer in ["mouse", "touch"]:
                return R.err(R.BAD_PARAMS, "steps[%d]: pointer must be mouse or touch." % index)
            var swipe_button := str(step.get("button", "left"))
            if swipe_pointer == "mouse" and not MOUSE_BUTTONS.has(swipe_button):
                return R.err(R.BAD_PARAMS, "steps[%d]: unknown mouse button '%s'." % [index, swipe_button])
            return {"step": {
                "type": kind, "pointer": swipe_pointer, "button": swipe_button,
                "index": int(step.get("index", 0)), "frames": swipe_frames,
                "x": float(step["x"]), "y": float(step["y"]),
                "to_x": float(step["to_x"]), "to_y": float(step["to_y"]),
            }}
        "path":
            var points: Variant = step.get("points", [])
            if not points is Array or (points as Array).size() < 2 or (points as Array).size() > 256:
                return R.err(R.BAD_PARAMS, "steps[%d]: path needs 2 to 256 points, each [x, y]." % index)
            var per := int(step.get("frames_per_point", 1))
            if per < 1 or per > 60:
                return R.err(R.BAD_PARAMS, "steps[%d]: frames_per_point must be from 1 to 60." % index)
            # A gesture lasts as long as the longest wait, so the call returns before the server gives up.
            if ((points as Array).size() - 1) * per > MAX_PATH_FRAMES:
                return R.err(R.BAD_PARAMS, "steps[%d]: a path may take at most %d frames (points - 1 times frames_per_point)." % [index, MAX_PATH_FRAMES])
            var parsed_points: Array = []
            for item: Variant in points:
                if not item is Array or (item as Array).size() < 2:
                    return R.err(R.BAD_PARAMS, "steps[%d]: each path point must be [x, y]." % index)
                var pair: Array = item
                parsed_points.append(Vector2(float(pair[0]), float(pair[1])))
            var path_pointer := str(step.get("pointer", "mouse"))
            if not path_pointer in ["mouse", "touch"]:
                return R.err(R.BAD_PARAMS, "steps[%d]: pointer must be mouse or touch." % index)
            var path_button := str(step.get("button", "left"))
            if path_pointer == "mouse" and not MOUSE_BUTTONS.has(path_button):
                return R.err(R.BAD_PARAMS, "steps[%d]: unknown mouse button '%s'." % [index, path_button])
            return {"step": {
                "type": kind, "pointer": path_pointer, "button": path_button,
                "index": int(step.get("index", 0)), "frames_per_point": per,
                "points": parsed_points,
            }}
        "time_scale":
            var scale_value: Variant = step.get("scale", step.get("time_scale", null))
            var parsed_scale := _parse_time_scale(scale_value)
            if R.is_err(parsed_scale):
                return R.err(R.BAD_PARAMS, "steps[%d]: %s" % [index, parsed_scale["error"]["message"]])
            return {"step": {"type": kind, "scale": parsed_scale["scale"]}}
        "joy_button":
            var button_name := str(step.get("button", ""))
            if not JOY_BUTTONS.has(button_name):
                return R.err(R.BAD_PARAMS, "steps[%d]: unknown joypad button '%s'." % [index, button_name])
            return {"step": {"type": kind, "phase": phase, "button": button_name, "device": int(step.get("device", 0))}}
        "joy_axis":
            var axis := str(step.get("axis", ""))
            if not JOY_AXES.has(axis):
                return R.err(R.BAD_PARAMS, "steps[%d]: unknown joypad axis '%s'." % [index, axis])
            return {"step": {"type": kind, "axis": axis, "value": clampf(float(step.get("value", 0.0)), -1.0, 1.0), "device": int(step.get("device", 0))}}
        "wait":
            var node_path := str(step.get("node", "")).strip_edges()
            var sig := str(step.get("signal", "")).strip_edges()
            if not sig.is_empty() and node_path.is_empty():
                return R.err(R.BAD_PARAMS, "steps[%d]: a signal wait needs a node path." % index)
            var waiting := not node_path.is_empty() or not sig.is_empty()
            return {"step": {
                "type": kind,
                "frames": clampi(int(step.get("frames", 1)), 0, 600),
                "node": node_path,
                "signal": sig,
                "timeout": clampi(int(step.get("timeout", (180 if waiting else 1))), 1, 600),
            }}
        _:
            return R.err(R.BAD_PARAMS, "steps[%d]: unknown type '%s'." % [index, kind])


static func _run(rt: Node, step: Dictionary) -> Dictionary:
    var kind := str(step["type"])
    match kind:
        "key":
            await _key(rt, int(step["code"]), str(step["phase"]))
        "text":
            for character in str(step["text"]):
                _unicode(character.unicode_at(0), true)
                Input.flush_buffered_events()
                await rt.get_tree().process_frame
                _unicode(character.unicode_at(0), false)
                Input.flush_buffered_events()
        "mouse_move":
            _mouse_motion(float(step["x"]), float(step["y"]))
            Input.flush_buffered_events()
        "mouse_button":
            await _mouse_button(rt, str(step["button"]), str(step["phase"]), float(step["x"]), float(step["y"]))
        "touch":
            await _touch(rt, int(step["index"]), str(step["phase"]), float(step["x"]), float(step["y"]))
        "joy_button":
            await _joy_button(rt, str(step["button"]), str(step["phase"]), int(step["device"]))
        "joy_axis":
            _joy_axis(str(step["axis"]), float(step["value"]), int(step["device"]))
            Input.flush_buffered_events()
        "wait":
            for _i in int(step["frames"]):
                await rt.get_tree().process_frame
    return {"step": {"type": kind, "ok": true}}


static func _key(rt: Node, code: int, phase: String) -> void:
    if phase == "release":
        _key_event(code, false)
        _keys.erase(code)
        Input.flush_buffered_events()
        return
    _key_event(code, true)
    _keys[code] = true
    Input.flush_buffered_events()
    if phase == "tap":
        await rt.get_tree().process_frame
        _key_event(code, false)
        _keys.erase(code)
        Input.flush_buffered_events()


static func _key_event(code: int, pressed: bool) -> void:
    var event := InputEventKey.new()
    event.keycode = code & KEY_CODE_MASK
    event.physical_keycode = event.keycode
    event.pressed = pressed
    event.echo = false
    Input.parse_input_event(event)


static func _unicode(code: int, pressed: bool) -> void:
    var event := InputEventKey.new()
    event.unicode = code
    event.keycode = code
    event.pressed = pressed
    Input.parse_input_event(event)


static func _mouse_motion(x: float, y: float) -> void:
    var event := InputEventMouseMotion.new()
    event.position = Vector2(x, y)
    event.global_position = event.position
    event.relative = event.position - _mouse_pos
    _mouse_pos = event.position
    # warp_mouse moves the real cursor. The parsed event is enough for _input and
    # Viewport.get_mouse_position, and it leaves the user's pointer where it was.
    if _warp_mouse:
        Input.warp_mouse(event.position)
    Input.parse_input_event(event)


static func _mouse_button(rt: Node, button: String, phase: String, x: float, y: float) -> void:
    _mouse_motion(x, y)
    var index := int(MOUSE_BUTTONS[button])
    if phase == "release":
        _mouse_event(index, false, x, y)
        _mouse.erase(button)
        Input.flush_buffered_events()
        return
    _mouse_event(index, true, x, y)
    _mouse[button] = true
    Input.flush_buffered_events()
    if phase == "tap":
        await rt.get_tree().process_frame
        _mouse_event(index, false, x, y)
        _mouse.erase(button)
        Input.flush_buffered_events()


static func _mouse_event(index: int, pressed: bool, x: float, y: float) -> void:
    var event := InputEventMouseButton.new()
    event.button_index = index
    event.pressed = pressed
    event.position = Vector2(x, y)
    event.global_position = event.position
    Input.parse_input_event(event)


static func _touch(rt: Node, index: int, phase: String, x: float, y: float) -> void:
    if phase == "release":
        _touch_event(index, false, x, y)
        _touches.erase(index)
        Input.flush_buffered_events()
        return
    _touch_event(index, true, x, y)
    _touches[index] = Vector2(x, y)
    Input.flush_buffered_events()
    if phase == "tap":
        await rt.get_tree().process_frame
        _touch_event(index, false, x, y)
        _touches.erase(index)
        Input.flush_buffered_events()


static func _touch_event(index: int, pressed: bool, x: float, y: float) -> void:
    var event := InputEventScreenTouch.new()
    event.index = index
    event.pressed = pressed
    event.position = Vector2(x, y)
    Input.parse_input_event(event)


static func _joy_button(rt: Node, button: String, phase: String, device: int) -> void:
    var index := int(JOY_BUTTONS[button])
    var key := "%d:%s" % [device, button]
    if phase == "release":
        _joy_button_event(device, index, false)
        _joy_buttons.erase(key)
        Input.flush_buffered_events()
        return
    _joy_button_event(device, index, true)
    _joy_buttons[key] = true
    Input.flush_buffered_events()
    if phase == "tap":
        await rt.get_tree().process_frame
        _joy_button_event(device, index, false)
        _joy_buttons.erase(key)
        Input.flush_buffered_events()


static func _joy_button_event(device: int, button: int, pressed: bool) -> void:
    var event := InputEventJoypadButton.new()
    event.device = device
    event.button_index = button
    event.pressed = pressed
    Input.parse_input_event(event)


static func _joy_axis(axis: String, value: float, device: int) -> void:
    var event := InputEventJoypadMotion.new()
    event.device = device
    event.axis = int(JOY_AXES[axis])
    event.axis_value = value
    Input.parse_input_event(event)
    var key := "%d:%s" % [device, axis]
    if is_zero_approx(value):
        _joy_axes.erase(key)
    else:
        _joy_axes[key] = value


static func _release_everything() -> Dictionary:
    _scheduled.clear()
    var keys: Array = _keys.keys()
    for code: Variant in keys:
        _key_event(int(code), false)
    _keys.clear()
    var buttons: Array = _mouse.keys()
    for button: Variant in buttons:
        _mouse_event(int(MOUSE_BUTTONS[str(button)]), false, _mouse_pos.x, _mouse_pos.y)
    _mouse.clear()
    var joy: Array = _joy_buttons.keys()
    for key: Variant in joy:
        var parts := str(key).split(":")
        _joy_button_event(int(parts[0]), int(JOY_BUTTONS[parts[1]]), false)
    _joy_buttons.clear()
    var axes: Array = _joy_axes.keys()
    for key: Variant in axes:
        var parts := str(key).split(":")
        _joy_axis(parts[1], 0.0, int(parts[0]))
    _joy_axes.clear()
    var touches: Array = _touches.keys()
    for index: Variant in touches:
        var at: Vector2 = _touches[index]
        _touch_event(int(index), false, at.x, at.y)
    _touches.clear()
    return {"keys": keys.size(), "mouse": buttons.size(), "joy_buttons": joy.size(), "joy_axes": axes.size(), "touches": touches.size()}


static func _held_summary() -> Dictionary:
    return {
        "keys": _keys.size(),
        "mouse": _mouse.size(),
        "joy_buttons": _joy_buttons.size(),
        "joy_axes": _joy_axes.size(),
        "touches": _touches.size(),
    }
