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
        ).read_only(),
        tool(
            "godot_game_tree",
            "Lists live nodes in the running game (path, class, position, visible text).",
            json!({"run_id": s(""), "root": s("Default: current scene. \"/root\" includes autoloads."), "depth": i(""), "max_nodes": i("Default 150."), "name": s(""), "class": s(""), "group": s(""), "text": s("")}),
            &[],
        ).read_only(),
        tool(
            "godot_game_get",
            "Reads a live node's properties, groups and optionally signals and methods. '/' reads a Dictionary key or Object property (get only); ':' steps through a Resource (material_override:shader_parameter/x). A missing segment goes to property_errors; the rest is returned. A name containing '/' (metadata/note, shader_parameter/x) is used whole. Private names are refused, except in a ':' resource step; script is omitted.",
            json!({"run_id": s(""), "path": s("Node path relative to the current scene, or /root/..."), "properties": {"type": "array", "items": {"type": "string"}}, "signals": b(), "methods": b()}),
            &[],
        ).read_only(),
        tool(
            "godot_game_set",
            "Sets properties on a live node; nothing is saved. Values convert to the property's type: numbers, arrays for vectors and colors, Godot literal strings (\"Vector3(1, 2, 3)\"), and a res:// resource path or null for an Object property. Every value is checked and read back; an error (wrong type, missing property or key, read-only property, rejecting setter) names it and changes nothing. '/' writes a Dictionary key or Object property, never through a Node, and never creates a missing key; a name containing '/' (metadata/note) is used whole; ':' steps through a Resource (material_override:shader_parameter/x). Refuses script and private segments.",
            json!({"run_id": s(""), "path": s(""), "properties": {"type": "object"}}),
            &["properties"],
        ).destructive_idempotent(),
        tool(
            "godot_game_call",
            "Calls one method on a live node. The method must be listed in permit; refused if it can quit, evaluate code, call a method by name (call, callv, call_group, propagate_call, rpc, emit_signal, notification), set a property by name (set, set_deferred, set_indexed) or free or replace a node (free, queue_free, replace_by). JSON strings become StringName or NodePath where the method takes one. If the node leaves the tree (scene change), the answer has left_tree true.",
            json!({"run_id": s(""), "path": s(""), "method": s(""), "args": {"type": "array"}, "permit": {"type": "array", "items": {"type": "string"}, "description": "Methods this call is allowed to invoke. Must include method."}}),
            &["method", "permit"],
        ).destructive(),
        tool(
            "godot_game_state",
            "Reads, by name, the states the project's adapter script (project setting godot_mcp/adapter) registered, instead of walking the tree. No names: every state, plus the adapter's commands with help in adapter.commands. A failing state is an {error} entry; engine errors logged meanwhile are in errors. Answers over 262144 bytes are refused with each state's size. Works while held. adapter.error in godot_get_run_status says why an adapter did not load.",
            json!({"run_id": s(""), "names": {"type": "array", "items": {"type": "string", "minLength": 1, "maxLength": 256}, "maxItems": 64, "description": "State names. Default: every state."}}),
            &[],
        ).read_only(),
        tool(
            "godot_game_command",
            "Runs, by name, one command the project's adapter (project setting godot_mcp/adapter) registered, with args (numbers as floats). It does what the project wrote, which may free nodes, change the scene or write files; godot_game_call's refusals do not apply. List: adapter.commands in godot_game_state. An error entry in the value is a result, not a failure; bad args, an unknown name, a script error and a value over 262144 bytes are separate errors. A command must not await; what it defers happens when the game next advances, so step a held game before reading. Works while held.",
            json!({"run_id": s(""), "name": {"type": "string", "minLength": 1, "maxLength": 256, "description": "The registered command name."}, "args": {"type": "object", "description": "Arguments for the command."}}),
            &["name"],
        ).destructive(),
        tool(
            "godot_game_signals",
            "Lists a live node's signals and current connections.",
            json!({"run_id": s(""), "path": s("")}),
            &[],
        ).read_only(),
        tool(
            "godot_game_observe",
            "Connects to signals for a few frames and returns the ones that fired, then disconnects. Refused while the game is held: step with godot_game_physics, then read with godot_game_signals.",
            json!({"run_id": s(""), "path": s(""), "signals": {"type": "array", "items": {"type": "string"}}, "frames": i("How many frames to watch (default 1, max 600).")}),
            &["signals"],
        ).read_only(),
        tool(
            "godot_game_snapshot",
            "Scene path, scene_generation (grows on every scene change or reload), scene_instance_id, frame, paused, children, and the root script's path and SHA-256 of the loaded source (not the file).",
            json!({"run_id": s("")}),
            &[],
        ).read_only(),
        tool(
            "godot_game_input",
            "Injects input as a list of steps. phase: tap (default), press, release; touch move drags a touch already down. click_node taps the center of a Control, Node2D or Node3D and returns at. swipe, path and circle are drags of at most 600 frames. Pointer positions use coordinate_space; the answer gives window_per_viewport_pixel and window_offset. A failed step releases everything held. Over 20 steps answer ok_count and failed unless verbose. Held game: steps apply at once, taps stay down hold_ticks ticks; wait, text, swipe, path and circle are refused; leaving the hold releases everything.",
            json!({"run_id": s(""), "steps": {"type": "array", "items": step_schema()}, "warp_mouse": {"type": "boolean", "description": "Also move the OS cursor. Default false."}, "coordinate_space": {"type": "string", "enum": ["viewport", "window"], "description": "viewport (default): root viewport pixels after content scale, as _input sees them; window: window pixels."}, "emulate_mouse": {"type": "boolean", "description": "false: touch emits no emulated mouse events in this call. Default: project setting."}, "verbose": {"type": "boolean", "description": "Return every step even beyond 20. Default false."}, "hold_ticks": ir("Held game: ticks a tap stays down, default 2.", 1, Some(600))}),
            &["steps"],
        ).additive(),
        tool(
            "godot_game_input_release",
            "Releases every key, mouse button, joypad button, axis, Input Map action and touch this server is still holding.",
            json!({"run_id": s("")}),
            &[],
        ).additive_idempotent(),
        tool(
            "godot_game_wait",
            "Waits for frames, for node to exist, for signal on node, for a property of node to satisfy op and value, or for a scene_generation newer than given (combinable with node and property; signal is not). Properties are read with get only, / for Dictionary keys or Object properties; a missing segment is an error naming it, not a timeout; private names, script and Array elements are refused. eq/ne need the same type (or null; a whole number matches an int), lt/gt numbers of one type; strings are never coerced. Answers carry the observed value and scene context. Refused while held: step with godot_game_physics.",
            json!({"run_id": s(""), "frames": i("Frames to wait, default 1. Ignored when node, signal, property or scene_generation is set (then only the default of timeout)."), "node": s("Wait until this node exists. Required with property."), "signal": s("Signal of node to wait for."), "property": s("Public property path; / for a Dictionary key or Object property."), "op": s("eq, ne, lt, lte, gt, gte, or exists."), "value": {"type": ["string", "number", "integer", "boolean", "object", "array", "null"], "description": "Value to compare. null works with eq and ne on any property."}, "scene_generation": i("Wait until the running scene generation is greater than this."), "timeout": i("Frames before giving up. Default: frames when given, else 180; max 600."), "time_scale": n("Engine.time_scale, 0 to 8, kept after this call until changed (restored when the game exits).")}),
            &[],
        ).additive_idempotent(),
        tool(
            "godot_game_physics",
            "Controls game time. hold stops the game at a physics tick: nothing advances, even nodes that ignore pause; tree, get, set, call with permit, snapshot, output and one-frame screenshots still answer. While held, step runs exactly ticks ticks, or with until runs until the condition holds (until_met, at most max), then holds again; resume lets it run. Steps are exact only after godot_run_project held true: answers carry deterministic and deterministic_reason (not_started_held, fixed_rate_differs, frames_did_not_follow_ticks). A held game refuses godot_game_wait, godot_game_observe, screenshot sheets and camera switches, godot_game_record and timed input steps: step instead. Without a hold, step runs 1-120 engine physics iterations without calling _physics_process or _process (not deterministic), then leaves the tree paused until resume. status reports held, deterministic and timing.",
            json!({"run_id": s(""), "action": s("step (default), hold, resume, or status."), "ticks": ir("step: 1 to 120 ticks, or 1 to 600 while held.", 1, None), "until": until_schema(), "max": ir("With until: most ticks to run, default 600.", 1, Some(3600))}),
            &[],
        ).additive(),
        crate::tools::scenario::definition(),
        tool(
            "godot_game_run_script",
            "Runs one function of a .gd file (extends Node, not SceneTree; no quit()) inside the running game, so a heavy scene stays loaded between test scripts (unlike godot_run_script). The file is recompiled each call; scripts it loads and the game's own are not. Its node is a temporary child of the runtime (get_tree() is the game's); game changes persist. The function may await; a held game runs ticks only while it awaits, then holds again (one awaited physics frame is two ticks). Answer: ok, value, output (all game prints; oldest dropped past 262144 bytes, output_dropped), errors, ticks_run, frames_advanced, elapsed_ms, scene (same scene_instance_id: not reloaded); held games add the godot_game_physics fields. In editor Play a script error stops at the editor's debugger. On failure ok is false and failure is script_error, value_too_large (over 262144 bytes) or timeout (ended at its await); other game errors do not fail it. Other game tools wait meanwhile; a loop without await cannot be ended. For input play-throughs use godot_game_scenario; for an existing method, godot_game_call.",
            json!({
                "run_id": s(""),
                "script": s("res:// path of the .gd file."),
                "function": s("Function to run. Default run."),
                "args": {"type": "array", "description": "Arguments, converted to the declared types."},
                "timeout_seconds": ir("Seconds before the function is ended at its await, default 60.", 1, Some(600))
            }),
            &["script"],
        ).destructive(),
        tool(
            "godot_game_probe",
            "Casts rays in the game's 3D physics world, moving nothing, to measure heights, gaps or what lies below. rays: {from, to}, or {x, z} straight down. grid: up to 4096 down rays, answered as heights[row][column] (row r at z = min.z + r * step.z, column c at x = min.x + c * step.x; null = no hit) with the lowest and highest hit and collider counts. A hit has position, normal, distance, the collider's path, class and id, the shape index and shape_node. Reads physics as it stands: a node moved since the last tick may lag. Works while held.",
            json!({
                "run_id": s(""),
                "rays": {"type": "array", "maxItems": 1024, "description": "Each {from, to}, or {x, z} straight down.", "items": {"type": "object", "properties": {"from": vec3("Ray start."), "to": vec3("Ray end."), "x": n("Down ray: world x."), "z": n("Down ray: world z.")}}},
                "grid": {"type": "object", "description": "Down rays over min to max (included), spaced by step or count.", "properties": {"min": vec2("[x, z], smaller corner."), "max": vec2("[x, z], larger corner."), "step": n("Spacing in metres."), "count": {"type": "array", "items": {"type": "integer", "minimum": 1}, "minItems": 2, "maxItems": 2, "description": "[columns, rows]."}}, "required": ["min", "max"]},
                "top": n("Down ray start height. Default 1000."),
                "bottom": n("Down ray end height. Default -1000."),
                "mask": {"type": "integer", "minimum": 0, "maximum": 4294967295_u64, "description": "Collision mask, bit 0 = layer 1. Default all."},
                "exclude": {"type": "array", "items": {"type": "string"}, "description": "Paths of bodies or areas to pass through, such as the player."},
                "areas": {"type": "boolean", "description": "Also hit Area3D. Default false."},
                "bodies": {"type": "boolean", "description": "Hit physics bodies. Default true."},
                "detail": {"type": "boolean", "description": "grid: also return every full hit. Default false."},
                "world": s("Node whose 3D world to probe, such as a SubViewport. Default the main window's.")
            }),
            &[],
        ).read_only(),
        tool(
            "godot_game_screenshot",
            "Captures the game window, a viewport or a Camera2D/Camera3D as one PNG; frames above 1 makes a sheet (longer series: godot_game_record). The text gives the scale to godot_game_input coordinates (multiply an image pixel by it; per cell in a sheet), pixels changed since the previous capture, and the path (window, forced_draw, mixed; STALE: older than the game state). save_baseline stores the image by name (at most 8, each at most 512x512); baseline compares with one and reports changed, ratio and bounds: exact while the differing rows hold at most 65536 pixels, else sampled on a grid (at tolerance 0 a change the grid misses is located row by row and changed counts rows). A missing baseline or roi outside the image is an error; a headless game is an error with no image. While held, one capture shows the held state; a sheet or camera switch is refused. aim renders only the 3D world (no HUD) from a temporary copy of the game camera, moving nothing (answer: aim).",
            json!({"run_id": s(""), "node": s("Viewport, or node whose viewport to capture."), "camera": s("Camera2D or Camera3D to make current. With aim: the Camera3D to copy."), "aim": aim_schema(), "max_dimension": i("Longest image side in pixels. Default 768."), "frames": ir("Frames in the sheet, default 1.", 1, Some(8)), "every": ir("Process frames between shots, default 1.", 1, Some(60)), "baseline": s("Name of a saved image to compare."), "save_baseline": s("Save the returned image under this name (letters, digits, _, -)."), "roi": roi_schema(), "tolerance": ir("With baseline: per-channel difference still unchanged, default 0.", 0, Some(255))}),
            &[],
        ).read_only(),
        tool(
            "godot_game_record",
            "Records the game window as PNG files on disk (longer than a screenshot sheet). frames * every must be at most 480, and frames × width × height × 4 within 512 MB. The reply lists each file's path, size and capture time. Refused while held; a headless game has nothing to record.",
            json!({
                "run_id": s(""),
                "frames": ir("PNG files to save, default 60.", 1, Some(240)),
                "every": ir("Process frames between shots, default 1.", 1, Some(60)),
                "max_dimension": ir("Longest side of each PNG, default 768.", 16, Some(4096)),
                "directory": s("Absolute directory inside the system temp directory, without earlier frames or recording. Default: a new one."),
                "format": {"type": "string", "enum": ["png", "gif", "mp4"], "description": "png (default); gif or mp4 also builds recording.gif/.mp4 when ffmpeg is on PATH."}
            }),
            &[],
        ).additive(),
        tool(
            "godot_debug_breakpoints",
            "Lists, adds or removes breakpoints on a process run (Godot's core:breakpoint). Editor Play uses the editor's own debugger.",
            json!({"run_id": s(""), "action": {"type": "string", "enum": ["list", "add", "remove", "clear"]}, "breakpoint": s("res://script.gd:LINE")}),
            &[],
        ).additive_idempotent(),
        tool(
            "godot_debug_control",
            "Pause, resume, or step a process run that this server is debugging. Pause is refused on a game held with godot_game_physics: it is already still.",
            json!({"run_id": s(""), "action": {"type": "string", "enum": ["pause", "resume", "step_into", "step_over", "step_out"]}, "wait_seconds": i("How long to wait for the next stop. Default 10.")}),
            &["action"],
        ).additive(),
        tool(
            "godot_debug_state",
            "Stack frames and variables of a stopped process run.",
            json!({"run_id": s(""), "frame": i("0 is the innermost frame.")}),
            &[],
        ).read_only(),
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
        "description": "One step. Fields by type: action: action, strength; key: key; text: text; mouse_move, mouse_button, touch: x, y, index; click: x, y; click_node: node; swipe: x, y, to_x, to_y, frames; path: points, frames_per_point; circle: center, radius, turns, points_per_turn, frames_per_point; joy_button: button, device; joy_axis: axis, value, device; wait: frames, node, signal, timeout; time_scale: scale. phase, button, pointer and index apply where they fit.",
        "properties": {
            "type": {"type": "string", "enum": ["action", "key", "text", "mouse_move", "mouse_button", "touch", "click", "click_node", "swipe", "path", "circle", "joy_button", "joy_axis", "wait", "time_scale"]},
            "phase": {"type": "string", "enum": ["tap", "press", "release", "move"], "description": "Default tap. move drags a touch that is down."},
            "action": {"type": "string", "description": "Input Map action name."},
            "strength": {"type": "number", "description": "action: 0 to 1, default 1."},
            "key": {"type": "string", "description": "Engine key name: Right, Space, A."},
            "text": {"type": "string"},
            "x": {"type": "number"}, "y": {"type": "number"},
            "to_x": {"type": "number"}, "to_y": {"type": "number"},
            "button": {"type": "string", "description": "Mouse (left, right, middle) or joypad button name."},
            "index": {"type": "integer"},
            "pointer": {"type": "string", "enum": ["mouse", "touch"], "description": "click_node, swipe, path, circle: mouse (default) or touch."},
            "node": {"type": "string", "description": "Node path in the game."},
            "signal": {"type": "string"},
            "frames": {"type": "integer", "description": "wait: 0 to 600; swipe: 1 to 60, default 8."},
            "points": {"type": "array", "description": "path: 2 to 256 [x, y].", "items": {"type": "array", "items": {"type": "number"}}},
            "frames_per_point": {"type": "integer", "description": "path, circle: frames per segment, 1 to 60, default 1."},
            "center": {"type": "array", "description": "circle: [x, y].", "items": {"type": "number"}},
            "radius": {"type": "number", "description": "circle: above 0."},
            "turns": {"type": "integer", "description": "circle: 1 to 8, default 1."},
            "points_per_turn": {"type": "integer", "description": "circle: 4 to 64, default 16."},
            "timeout": {"type": "integer", "description": "wait: frames before giving up, 1 to 600."},
            "axis": {"type": "string"}, "value": {"type": "number", "description": "joy_axis: -1 to 1."},
            "device": {"type": "integer"},
            "scale": {"type": "number", "description": "time_scale: Engine.time_scale, 0 to 8, kept until changed."}
        },
        "required": ["type"]
    })
}

/// The condition of a held step, as in `godot_game_wait`.
fn until_schema() -> Value {
    json!({
        "type": "object",
        "description": "Held game only: step until a node property meets the condition (read as in godot_game_wait). Use max, not ticks.",
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
        "description": "Comparison region in returned-image pixels. Requires baseline; does not crop the image.",
        "properties": {
            "x": {"type": "integer"}, "y": {"type": "integer"},
            "w": {"type": "integer", "minimum": 1}, "h": {"type": "integer", "minimum": 1}
        }
    })
}

/// A screenshot from a temporary camera aimed at a point or placed with a look-at.
fn aim_schema() -> Value {
    json!({
        "type": "object",
        "description": "Capture from a temporary copy of the game camera: {at, distance?} or {position, look_at, up?}; size or fov sets its lens. Not with node.",
        "properties": {
            "at": vec3("World point to put in the centre of the image."),
            "distance": n("With at: distance from the point. Default: the game camera's distance to what it looks at."),
            "position": vec3("Where the camera stands. Needs look_at."),
            "look_at": vec3("The point it looks at. Needs position."),
            "up": vec3("The picture's up. Default [0, 1, 0]."),
            "size": {"type": "number", "minimum": 0.001, "maximum": 10000, "description": "Orthographic view size in metres (image height; width when the copied camera keeps width). Not with fov."},
            "fov": {"type": "number", "minimum": 1, "maximum": 179, "description": "Perspective vertical view angle in degrees (horizontal when the copied camera keeps width). Not with size."}
        },
        "additionalProperties": false
    })
}

fn vec3(description: &str) -> Value {
    json!({"type": "array", "items": {"type": "number"}, "minItems": 3, "maxItems": 3, "description": description})
}

fn vec2(description: &str) -> Value {
    json!({"type": "array", "items": {"type": "number"}, "minItems": 2, "maxItems": 2, "description": description})
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
