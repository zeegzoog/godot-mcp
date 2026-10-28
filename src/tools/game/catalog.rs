//! The catalog of the running-game and debugger tools, and the schema helpers it uses.

use crate::protocol::Tool;
use crate::tools::tool;
use serde_json::{Value, json};

pub fn catalog() -> Vec<Tool> {
    vec![
        tool(
            "godot_run_output",
            "Reads logs, warnings and errors (with stack traces) from a managed process run or the editor's running game.",
            json!({"run_id": s("Optional run id. Default: newest process run, else the editor game."), "since": i("Return entries with seq >= since."), "level": s("all (default), warning, or error.")}),
            &[],
        ),
        tool(
            "godot_game_tree",
            "Lists live nodes in the running game (path, class, position, visible text).",
            json!({"run_id": s(""), "root": s("Default: current scene. \"/root\" includes autoloads."), "depth": i(""), "max_nodes": i("Default 150."), "name": s(""), "class": s(""), "group": s(""), "text": s("")}),
            &[],
        ),
        tool(
            "godot_game_get",
            "Reads a live node's properties, groups and, optionally, signals and methods. A property uses '/' for a Dictionary key or an Object property, the same way godot_game_wait does: get only, no method calls, and a private name or script is refused. A missing segment anywhere in that path is an entry in property_errors that names the segment; other requested properties are still returned. null is only a value that really is null. A property whose own name contains '/' (metadata/note, theme_override_colors/font_color) is read whole. ':' steps through a Resource (material_override:shader_parameter/caustics), including a segment that starts with _. A requested script name is omitted, not rejected. A ':' step through something that is not a Resource is an error and names '/'.",
            json!({"run_id": s(""), "path": s("Node path relative to the current scene, or /root/..."), "properties": {"type": "array", "items": {"type": "string"}}, "signals": b(), "methods": b()}),
            &[],
        ),
        tool(
            "godot_game_set",
            "Sets properties on a live node. A value is converted to the property's current type: numbers, [x, y] or [x, y, z] arrays for vectors, [r, g, b, a] for colors, and any Godot literal string (\"Transform3D(1, 0, 0, 0, 1, 0, 0, 0, 1, 2, 2, 4)\", \"Basis(...)\", \"Vector3(1, 2, 3)\") for any typed property. A value of the wrong type is an error that names the property's type. '/' writes a Dictionary key or an Object property, the same way godot_game_get reads one: get only, no method calls. A missing property or key is an error that names the segment; a new Dictionary key is not created. The value is read back, and a write that did not take (a read-only property, or a setter that rejects the value) is an error, never success with null. A property whose own name contains '/' (shader_parameter/caustics, metadata/note) is written whole. ':' steps through a Resource (material_override:shader_parameter/caustics). A '/' step stays inside the object it starts from: a Node along the way is an error, and that node is named with the path parameter. Refuses script, a private segment, and a ':' step that is not a Resource. Nothing is saved to disk.",
            json!({"run_id": s(""), "path": s(""), "properties": {"type": "object"}}),
            &["properties"],
        ),
        tool(
            "godot_game_call",
            "Calls one method on a live node. The method must be listed in permit and is refused if it can quit, evaluate code, call another method by name (call, callv, call_group, propagate_call, rpc, emit_signal, notification), set a property by name (set, set_deferred, set_indexed), or free or replace a node (free, queue_free, replace_by). A JSON string is passed as a StringName or NodePath where the method asks for one. When the method takes the node out of the tree (it reloads or changes the scene), the answer has left_tree true and the path the node had before the call.",
            json!({"run_id": s(""), "path": s(""), "method": s(""), "args": {"type": "array"}, "permit": {"type": "array", "items": {"type": "string"}, "description": "Methods this call is allowed to invoke. Must include method."}}),
            &["method", "permit"],
        ),
        tool(
            "godot_game_signals",
            "Lists a live node's signals and current connections.",
            json!({"run_id": s(""), "path": s("")}),
            &[],
        ),
        tool(
            "godot_game_observe",
            "Connects to signals for a few frames and returns the ones that fired, then disconnects. Refused while the game is held: step with godot_game_physics, then read with godot_game_signals.",
            json!({"run_id": s(""), "path": s(""), "signals": {"type": "array", "items": {"type": "string"}}, "frames": i("How many frames to watch (default 1, max 600).")}),
            &["signals"],
        ),
        tool(
            "godot_game_snapshot",
            "Compact snapshot of the running game: scene path, monotonic scene_generation, scene_instance_id, frame, pause flag, children, and the loaded root script's path and SHA-256. The hash is the source Godot had loaded when this scene instance became current, not a later read of the file on disk. Reloading the same scene path increments the generation and changes the instance id.",
            json!({"run_id": s("")}),
            &[],
        ),
        tool(
            "godot_game_input",
            "Injects input into the running game. Steps: action, key, text, mouse_move, mouse_button, touch, joy_button, joy_axis, wait, click, click_node, swipe, path, circle, time_scale. action presses an Input Map action (phase tap, press, or release; strength 0 to 1). touch phase move drags a touch that is already down. phase is tap (default), press, or release. click is a mouse tap at x, y. click_node taps the center of a Control, Node2D, or Node3D. swipe presses at x, y and releases at to_x, to_y across frames (default 8, pointer mouse or touch). path presses once at the first of points ([[x, y], ...], 2 to 256), moves through them, and releases at the last; frames_per_point is the frames spent on each step between points (default 1); a path takes at most 600 frames in all (points - 1 times frames_per_point). circle is that same drag around center [x, y]: radius, turns (default 1, max 8) and points_per_turn (default 16, 4 to 64). pointer is mouse or touch, and touch may set index. Every pointer position (mouse_move, mouse_button, touch, click, swipe, path, circle) is in coordinate_space. viewport (the default) is a pixel of the root viewport's visible rectangle, the same space _input and _unhandled_input see after content scale and the space Control global positions and godot_game_screenshot use. window is a pixel of the game window, before content scale (in a 1920x1080 project shown in a 1280x720 window, window 640, 360 is viewport 960, 540); it is mapped into viewport space once when the call is parsed. Neither is an OS screen pixel or a world coordinate. The answer names the coordinate_space, both sizes and the mapping (window = viewport * window_per_viewport_pixel + window_offset); click_node also returns at, the point it clicked, in that space. A touch step, and a swipe, path or circle with pointer touch, also emits the engine's emulated mouse event when Input.emulate_mouse_from_touch is on (project setting input_devices/pointing/emulate_mouse_from_touch, on by default). The engine delivers that InputEventMouseButton or InputEventMouseMotion first, with device InputEvent.DEVICE_ID_EMULATION, then the touch, both at the same viewport position. emulate_mouse false turns that emulation off for this call and restores the previous value afterwards, including when a step fails. Mouse steps do not move the OS cursor unless warp_mouse is true. time_scale sets Engine.time_scale (0 through 8) until another call changes it. A failed step releases every button and touch this server is still holding. More than 20 steps return ok_count and failed instead of one object per step, unless verbose is true. In a held game the steps are applied at once: key, mouse_button, touch and joy_button taps stay down for hold_ticks ticks (default 2) and are released at the start of the tick they fall due on, so how the ticks are split into steps does not change what the game sees; wait, text, swipe, path and circle steps are refused while held. Leaving the hold releases everything still held.",
            json!({"run_id": s(""), "steps": {"type": "array", "items": step_schema()}, "warp_mouse": {"type": "boolean", "description": "Move the real OS cursor to each mouse event. Default false."}, "coordinate_space": {"type": "string", "enum": ["viewport", "window"], "description": "The space of every pointer x, y, to_x, to_y, points and center. viewport (default): the root viewport's visible rectangle after content scale, as _input sees it. window: game window pixels, before content scale."}, "emulate_mouse": {"type": "boolean", "description": "When false, touch steps do not also emit the engine's emulated mouse events. The previous value is restored when the call finishes. Default: leave the project setting (on unless the project turned it off)."}, "verbose": {"type": "boolean", "description": "Return every step when there are more than 20. Default false, which answers with ok_count and failed."}, "hold_ticks": ir("Held game only: how many ticks a tap stays down before it is released. 1 to 600, default 2.", 1, Some(600))}),
            &["steps"],
        ),
        tool(
            "godot_game_input_release",
            "Releases every key, mouse button, joypad button, axis, Input Map action and touch this server is still holding.",
            json!({"run_id": s("")}),
            &[],
        ),
        tool(
            "godot_game_wait",
            "Waits in the running game. frames waits process frames (default 1). node waits until that node exists. signal waits until that signal fires on node (timeout frames, default 180; not combined with a property or scene_generation). property reads a public node property, including nested Dictionary keys or Object properties separated by /, using get only: no expressions and no method calls. A missing segment anywhere in that path is an error that names the segment; null is returned only when the property exists and its value is null. op is eq, ne, lt, lte, gt, gte, or exists. eq and ne require the same type, except that null compares with any value (a JSON number that is a whole number may match an int; a fraction against an int is a type error; strings are never coerced). Ordering is int-with-int or float-with-float, and float equality is exact. Private names, script, and anything inside an Array are refused. scene_generation waits until the running generation is greater than the given one, and can be combined with node and property. The result and a timeout include the observed value and the scene context. time_scale sets Engine.time_scale from 0 through 8 and keeps it until changed again; the runtime restores the previous scale when the game exits. Refused while the game is held, because no frame would pass: step with godot_game_physics instead.",
            json!({"run_id": s(""), "frames": i("Default 1. Ignored when node, signal, property, or scene_generation is set, except as the default timeout."), "node": s("Wait until this node exists. Required when property is set."), "signal": s("Wait until this signal fires on node."), "property": s("Public property path. Use / for a Dictionary key or an Object property."), "op": s("eq, ne, lt, lte, gt, gte, or exists."), "value": {"type": ["string", "number", "integer", "boolean", "object", "array", "null"], "description": "Value to compare. Same type as the property. No string coercion. A whole JSON number may match an int. null works with eq and ne on any property: eq null holds only while the value is null."}, "scene_generation": i("Wait until the running scene generation is greater than this."), "timeout": i("Frame limit for node, signal, property, or scene_generation. Default 180, max 600."), "time_scale": n("Engine.time_scale, from 0 through 8.")}),
            &[],
        ),
        tool(
            "godot_game_physics",
            "Controls game time. action hold stops the game at the start of a physics tick: no tick, frame, timer or signal advances, even in nodes that ignore pause, while reads (tree, get, set, call with permit, signals, snapshot, output, one-frame screenshot) are still answered. While held, step ticks runs exactly that many physics ticks (1 to 600) and holds again; step with until (node, property, op, value, checked before every tick, as in godot_game_wait) and max (1 to 3600) runs until the condition holds and says until_met; resume lets the game run. Held steps are exact only when the game was started with godot_run_project held true: the answer carries deterministic and, when false, deterministic_reason (not_started_held, fixed_rate_differs or frames_did_not_follow_ticks). A hold entered later, or in the editor, still stands still but is not exact. A held game refuses what needs time to pass (godot_game_wait, godot_game_observe, a screenshot sheet or camera, godot_game_record, input wait, text, swipe, path and circle steps) and says to step instead. Without a hold, step advances 1 to 120 real engine physics iterations, then leaves the tree paused (resume clears that pause); it sets max_physics_steps_per_frame to 1 while it runs, does not call node _physics_process, does not step idle _process or nodes that ignore pause, and does not make the game deterministic. status reports held, deterministic and the timing settings",
            json!({"run_id": s(""), "action": s("step (default), hold, resume, or status. hold stops the game at the start of a physics tick until it is stepped or resumed."), "ticks": ir("Physics iterations for step: 1 to 120, or 1 to 600 while the game is held.", 1, None), "until": until_schema(), "max": ir("With until: the most ticks to run. 1 to 3600, default 600.", 1, Some(3600))}),
            &[],
        ),
        crate::tools::scenario::definition(),
        tool(
            "godot_game_screenshot",
            "Captures the running game window, a viewport, or a Camera2D/Camera3D as one PNG. frames (1 to 8) stitches shots taken every process frames into a single sheet. For a longer series saved as PNG files, use godot_game_record. The text names the godot_game_input coordinate space (the root viewport's visible width and height, after content scale) and the scale from returned image pixels to that space: multiply an image pixel by the scale. A sheet uses that scale per frame, after subtracting the cell origin. When a baseline is compared, roi is the comparison region in returned-image pixels (roi_space returned_image) and the same scale applies inside a frame; roi does not crop the image and is refused without a baseline. A camera capture uses the camera's viewport; when that viewport is not the root, the text also names the captured viewport and the scale to it. The text says how many pixels changed since the previous capture of this run, and where (a sheet compares its last frame, and only against a capture of the same size). It also names the engine's drawn-frame count and process frame, and says STALE when the window drew no new frame before the capture (a covered or minimized window on macOS stops drawing): that image is older than the game state, so bring the window forward and capture again. save_baseline stores the returned image under a name (at most 8, each at most 512×512 pixels). baseline compares the returned image with a saved one. roi is the comparison region (x, y, w, h in pixels of that returned image, including a sheet) and requires baseline; it does not crop the returned image. tolerance (0 to 255, only with baseline) is the allowed per-channel difference; a larger difference counts as changed. The text reports changed, sampled, ratio, bounds, tolerance, the baseline name, and whether the pixel count is exact or sampled. Fails instead of returning an empty image. Headless games have nothing to capture. While the game is held a single capture shows the last frame it drew (a sheet with frames above 1, or a camera switch, is refused: take one capture, step, take the next). A missing baseline, a bad name, or an roi outside the image is an error and does not replace a saved image.",
            json!({"run_id": s(""), "node": s("Viewport or node whose viewport to capture."), "camera": s("Camera2D or Camera3D to make current for the shot."), "max_dimension": i("Longest side of the returned image in pixels. Default 768."), "frames": ir("How many frames to stitch. Default 1, max 8.", 1, Some(8)), "every": ir("Process frames between shots. Default 1, max 60.", 1, Some(60)), "baseline": s("Name of a saved image to compare."), "save_baseline": s("Store the returned image under this name. Letters, digits, underscore, hyphen. At most 8 names."), "roi": roi_schema(), "tolerance": ir("Per-channel difference that still counts as unchanged, only when comparing a baseline. 0 to 255. Default 0.", 0, Some(255))}),
            &[],
        ),
        tool(
            "godot_game_record",
            "Records the game window as PNG files on disk, for motion that a sheet of 8 frames cannot cover. frames is 1 to 240 (default 60). every is the process frames between shots, 1 to 60 (default 1). frames * every must be at most 480. max_dimension is the longest side of each file (default 768, 16 to 4096). Each frame only grabs an image. The PNG files are written after the last frame, and the timings are those capture instants. frames × width × height × 4 must stay within 512 MB after max_dimension. directory is an absolute path inside the system temporary directory; omit it for a new one. The reply lists each file's path, size and timing, and does not embed image bytes. format png (default) writes only those files. gif or mp4 also builds recording.gif or recording.mp4 when ffmpeg is on PATH; without ffmpeg the PNG files are still returned. Refused while the game is held, and a headless game has nothing to record.",
            json!({
                "run_id": s(""),
                "frames": ir("How many PNG files to save. 1 to 240, default 60.", 1, Some(240)),
                "every": ir("Process frames between shots. 1 to 60, default 1. frames * every must be at most 480.", 1, Some(60)),
                "max_dimension": ir("Longest side of each PNG in pixels. 16 to 4096, default 768.", 16, Some(4096)),
                "directory": s("Absolute directory to write into, inside the system temporary directory. Created if missing. Must not already contain frame PNGs or a recording. Omit for a new one."),
                "format": {"type": "string", "enum": ["png", "gif", "mp4"], "description": "png (default) keeps the frames. gif or mp4 also runs ffmpeg when it is on PATH."}
            }),
            &[],
        ),
        tool(
            "godot_debug_breakpoints",
            "Lists, adds or removes breakpoints on a process run (Godot's core:breakpoint). Editor Play uses the editor's own debugger.",
            json!({"run_id": s(""), "action": {"type": "string", "enum": ["list", "add", "remove", "clear"]}, "breakpoint": s("res://script.gd:LINE")}),
            &[],
        ),
        tool(
            "godot_debug_control",
            "Pause, resume, or step a process run that this server is debugging. Pause is refused on a game held with godot_game_physics: it is already still.",
            json!({"run_id": s(""), "action": {"type": "string", "enum": ["pause", "resume", "step_into", "step_over", "step_out"]}, "wait_seconds": i("How long to wait for the next stop. Default 10.")}),
            &["action"],
        ),
        tool(
            "godot_debug_state",
            "Stack frames and variables of a stopped process run.",
            json!({"run_id": s(""), "frame": i("0 is the innermost frame.")}),
            &[],
        ),
    ]
}

fn s(description: &str) -> Value {
    if description.is_empty() {
        json!({"type": "string"})
    } else {
        json!({"type": "string", "description": description})
    }
}

fn i(description: &str) -> Value {
    if description.is_empty() {
        json!({"type": "integer"})
    } else {
        json!({"type": "integer", "description": description})
    }
}

fn b() -> Value {
    json!({"type": "boolean"})
}

/// The fields of one `godot_game_input` step, so a caller does not have to guess them. The
/// runtime checks each step; this describes it.
fn step_schema() -> Value {
    json!({
        "type": "object",
        "description": "One input step. type is required; the other fields depend on it (key: key, phase; text: text; mouse_move, mouse_button, touch: x, y, button, index, phase; click: x, y, button; click_node: node, pointer, button; swipe: x, y, to_x, to_y, frames, pointer, button; path: points, frames_per_point, pointer, button, index; circle: center, radius, turns, points_per_turn, frames_per_point, pointer, button, index; joy_button: button, device, phase; joy_axis: axis, value, device; wait: frames, node, signal, timeout; time_scale: scale).",
        "properties": {
            "type": {"type": "string", "enum": ["action", "key", "text", "mouse_move", "mouse_button", "touch", "click", "click_node", "swipe", "path", "circle", "joy_button", "joy_axis", "wait", "time_scale"]},
            "phase": {"type": "string", "enum": ["tap", "press", "release", "move"], "description": "Default tap. move is a touch drag and needs that index to be down."},
            "action": {"type": "string", "description": "Input Map action name, such as move_right or jump."},
            "strength": {"type": "number", "description": "action: analog strength from 0 to 1. Default 1."},
            "key": {"type": "string", "description": "An engine key name such as Right, Space or A."},
            "text": {"type": "string"},
            "x": {"type": "number"}, "y": {"type": "number"},
            "to_x": {"type": "number"}, "to_y": {"type": "number"},
            "button": {"type": "string", "description": "A mouse button (left, right, middle, ...) or a joypad button name."},
            "index": {"type": "integer"},
            "pointer": {"type": "string", "enum": ["mouse", "touch"]},
            "node": {"type": "string", "description": "A node path in the running game."},
            "signal": {"type": "string"},
            "frames": {"type": "integer", "description": "wait: frames to wait (0 to 600); swipe: frames the swipe takes (1 to 60, default 8)."},
            "points": {"type": "array", "description": "path: 2 to 256 points, each [x, y].", "items": {"type": "array", "items": {"type": "number"}}},
            "frames_per_point": {"type": "integer", "description": "path and circle: frames spent on each step between points. 1 to 60, default 1; the whole gesture takes at most 600 frames."},
            "center": {"type": "array", "description": "circle: [x, y].", "items": {"type": "number"}},
            "radius": {"type": "number", "description": "circle: greater than 0."},
            "turns": {"type": "integer", "description": "circle: 1 to 8, default 1."},
            "points_per_turn": {"type": "integer", "description": "circle: 4 to 64, default 16."},
            "timeout": {"type": "integer", "description": "wait for a node or signal: frames before giving up (1 to 600)."},
            "axis": {"type": "string"}, "value": {"type": "number", "description": "joy_axis: -1 to 1."},
            "device": {"type": "integer"},
            "scale": {"type": "number", "description": "time_scale: 0 to 8."}
        },
        "required": ["type"]
    })
}

/// The condition of a held step, as in `godot_game_wait`.
fn until_schema() -> Value {
    json!({
        "type": "object",
        "description": "Held game only. Step until a public node property satisfies a condition, checked before every tick. Use max instead of ticks.",
        "properties": {
            "node": {"type": "string", "description": "Node path; '.' is the current scene."},
            "property": {"type": "string", "description": "A public property, or a path such as stats/hp."},
            "op": {"type": "string", "enum": ["eq", "ne", "lt", "lte", "gt", "gte", "exists"]},
            "value": {"type": ["string", "number", "integer", "boolean", "object", "array", "null"], "description": "The value to compare with. null works with eq and ne on any property."}
        }
    })
}

/// Comparison region of a baseline screenshot, in the returned image's pixels. Not a crop.
fn roi_schema() -> Value {
    json!({
        "type": "object",
        "description": "Comparison region in pixels of the returned image (integer x, y, w, h). Requires baseline; it does not crop the image. w and h are at least 1.",
        "properties": {
            "x": {"type": "integer"}, "y": {"type": "integer"},
            "w": {"type": "integer", "minimum": 1}, "h": {"type": "integer", "minimum": 1}
        }
    })
}

/// An integer parameter with the range the tool documents.
fn ir(description: &str, minimum: i64, maximum: Option<i64>) -> Value {
    let mut schema = json!({"type": "integer", "description": description, "minimum": minimum});
    if let Some(maximum) = maximum {
        schema["maximum"] = json!(maximum);
    }
    schema
}

fn n(description: &str) -> Value {
    json!({"type": "number", "description": description})
}
