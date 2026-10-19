extends RefCounted
## Input injection for a running game.
##
## Events go through Input.parse_input_event and Input.flush_buffered_events, which is the
## same path a real device uses (including the headless display server). Presses are
## remembered so input.release_all can clear them. A tap is delivered at the start of a
## frame, held for one process frame, then released.

const R := preload("rt_result.gd")

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


static func methods() -> Dictionary:
    return {
        "input.run": {"fn": run_steps},
        "input.release_all": {"fn": release_all},
        "input.wait": {"fn": wait_frames},
    }


static func run_steps(rt: Node, params: Dictionary) -> Dictionary:
    var raw: Variant = params.get("steps", [])
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
    var state := {"steps": steps, "index": 0, "mode": "lead", "results": [], "hold": 0, "release": {}}
    return {"_pending": true, "_tick": func() -> bool: return _tick_steps(rt, state)}


static func release_all(rt: Node, _params: Dictionary) -> Dictionary:
    var released := _release_everything()
    Input.flush_buffered_events()
    return R.ok({"released": released, "held": _held_summary()})


static func wait_frames(rt: Node, params: Dictionary) -> Dictionary:
    var frames := clampi(int(params.get("frames", 1)), 0, 600)
    var state := {"left": frames, "start": Engine.get_process_frames()}
    return {"_pending": true, "_tick": func() -> bool:
        if int(state["left"]) > 0:
            state["left"] = int(state["left"]) - 1
            return true
        rt.finish_job(R.ok({"frames": Engine.get_process_frames() - int(state["start"])}))
        return false
    }


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
        (state["results"] as Array).append({"type": str((steps[index] as Dictionary)["type"]), "ok": true})
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
    if mode == "text":
        return _tick_text(rt, state)
    if index >= steps.size():
        rt.finish_job(R.ok({"steps": state["results"], "held": _held_summary()}))
        return false
    var step: Dictionary = steps[index]
    var kind := str(step["type"])
    if kind == "wait":
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
        (state["results"] as Array).append({"type": kind, "ok": true})
        state["index"] = index + 1
        return true
    if kind == "mouse_move":
        _mouse_motion(float(step["x"]), float(step["y"]))
        Input.flush_buffered_events()
    elif kind == "joy_axis":
        _joy_axis(str(step["axis"]), float(step["value"]), int(step["device"]))
        Input.flush_buffered_events()
    (state["results"] as Array).append({"type": kind, "ok": true})
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
            return {"step": {"type": kind, "frames": clampi(int(step.get("frames", 1)), 0, 600)}}
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
