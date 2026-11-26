# Running game

The tools in this document work on a game started with `godot_run_project`: in the editor's Play mode or as a managed process. The list of tools is in [tools.md](tools.md).

## Checking a running game

These calls work on a managed process and on editor Play. A property wait reads with `get` only. It does not evaluate an expression or call a method. A name that starts with `_`, the name `script`, and a path that goes through an Array are refused. `eq` and `ne` need the same type, except `null`: it compares with any value, so `eq null` waits for a property to be cleared. JSON has one number type, so a whole number such as `1` may match an int property; `1.5` against an int is a type error. Strings are not turned into numbers. Float equality is exact.

```json
{"node": ".", "property": "stats/hp", "op": "gte", "value": 1, "timeout": 60}
```

`scene_generation` on a snapshot increases when the current scene instance changes, including a reload of the same path. Wait until the running generation is newer, optionally also until a node exists:

```json
{"scene_generation": 3, "node": "Player", "timeout": 180}
```

`script_sha256` is the SHA-256 of the root script source Godot had loaded when that scene instance became current. It is not a later read of the file on disk. Reloading the same scene keeps that hash when Godot reuses the script it already loaded (`Resource.reload` on a script that is in use returns "Already in use"). The hash changes when a new scene instance loads different source, for example a scene whose script was not loaded yet. Log lines copy that stamp when they are written, so an older error keeps the hash from its own scene. `script_sha256_basis` is `loaded_source_at_scene_enter` when the source was available, and `unavailable` when it was not. A line recovered only from the debugger buffer after the runtime logger was gone is also `unavailable`; that is not the file currently on disk. `StringName` values, such as a node's `name`, are compared as text. A bool is not a number.

A named baseline is the returned image, so `roi` uses that image's pixels (`roi_space` is `returned_image`), including a sheet. `tolerance` is the per-channel difference that still counts as unchanged, from 0 to 255. The text reports `changed`, `sampled`, `ratio`, `bounds`, `tolerance`, and whether the count is `exact` or `sampled`. At most 8 baselines are kept, each at most 512×512 pixels. A missing name, a bad name, or an `roi` outside the image is an error and does not replace a saved image. The previous-capture comparison is unchanged: a sheet compares its last frame, and only with a capture of the same size. A screenshot travels in one debugger message, which Godot limits to 8 MiB: an image whose base64 PNG is larger (a large `max_dimension` sheet of a detailed scene) is refused with its size, and a smaller `max_dimension` or fewer `frames` fits. Both comparisons run on the game's main thread, so they look at no more than 65536 pixels one by one: rows that are identical are skipped at once, the count is `exact` while the rows that differ hold at most 65536 pixels, and otherwise it is `sampled` on an even grid of at most 65536 pixels (`changed` then counts changed samples, and `ratio` is their share). At tolerance 0 a change the grid misses is then located row by row with native comparisons: `changed` is the number of rows that differ (at least that many pixels changed), `bounds` hold every change, and a changed pixel is never reported as no change.

```json
{"save_baseline": "menu", "max_dimension": 256}
{"baseline": "menu", "roi": {"x": 0, "y": 0, "w": 80, "h": 24}, "tolerance": 8}
```

`godot_game_screenshot` `aim` looks at a point in the 3D world without moving the player or the game's camera rig. A temporary camera copies the game's current Camera3D (or the one named by `camera`): projection, fov or size, near, far, cull mask, environment and attributes. It renders in a temporary viewport that shares the game's world, captures once and is freed; the game's own camera stays current and unmoved. `at` puts a point in the centre of the image, seen from the game camera's direction; `distance` defaults to how far ahead of the game camera its centre ray hits something, else the distance to the point. `position` and `look_at` (with `up`, default `[0, 1, 0]`) place the camera instead. `size` (0.001 to 10000 metres) makes the copy orthographic, `fov` (1 to 179 degrees) makes it perspective, both along the copied camera's `keep_aspect` axis (height by default); one of them, not both. The answer's `aim` field says where the camera was and which lens it used, with `lens_from` (`given`, `game_camera` or `default`) and `projection_replaced`. Only the 3D world is drawn: the HUD and other 2D canvas items are not in the picture, and a `VisibleOnScreenNotifier3D` in view may report entering the screen. The fresh viewport is initialized before readback, with a second render-only pass in Compatibility so per-instance shader effects are present.

`godot_game_input` coordinates: `viewport` (the default) is a pixel of the root viewport's visible rectangle, the space `_input` sees after content scale. `window` is a pixel of the game window before content scale: in a 1920x1080 project shown in a 1280x720 window, window `640, 360` is viewport `960, 540`. It is mapped into viewport space once when the call is parsed. A touch with `emulate_mouse_from_touch` on also delivers the emulated `InputEventMouseButton` or `InputEventMouseMotion` first, then the touch, at the same position; `emulate_mouse: false` restores the previous value afterwards, also when a step fails.

`godot_game_set` writes a property of the live node. A value is converted to the property's current type: numbers, `[x, y]` / `[x, y, z]` arrays for vectors, `[r, g, b, a]` for colors, and a Godot literal string (`"Transform3D(1, 0, 0, 0, 1, 0, 0, 0, 1, 2, 2, 4)"`, `"Basis(...)"`) for any typed property, as `godot_set_node_properties` parses them in the editor. A Basis, Transform3D or Transform2D also accepts vector axes as GDScript constructs them: `"Basis(Vector3(x axis), Vector3(y axis), Vector3(z axis))"`, with the origin last for a transform. An Object property (`mesh`, `material_override`, a script's `Resource` variable) accepts the `res://` path of a saved resource, which the game loads and assigns when its class is one the property takes, or `null` to clear it; a missing file, a resource of another class or text that is not a `res://` path is an error that names the path and the class expected. A value of the wrong type is an error that names the property's type. Every value is checked before any is written, and a refused value or a write that did not take leaves every property of the call unchanged. `/` writes a field of an Object or a key of a Dictionary, the same way `godot_game_get` reads one. A missing property or key is an error that names the segment, and a new Dictionary key is not created. The value is read back; a write that did not take is an error, not success with `null`. Floats, vectors, colors and transforms are compared approximately, because many engine floats are 32-bit. `:` steps through a Resource. A `/` that belongs to one Godot property name stays that name, which is how a shader uniform is spelled. A `/` step stays inside the object it starts from, so a Node along the way is an error and that node is named with `path`.

```json
{"path": ".", "properties": {"round_state/letter_index": 22}}
{"path": "WaterPaper", "properties": {"material_override:shader_parameter/caustics": true}}
```

`script`, a segment that starts with `_`, and a `:` step through something that is not a resource are refused. The change lasts until the game exits. It is not saved. A shader uniform that nothing has written yet reads as null; Godot does not return the default declared in the shader until a value is stored.

`godot_game_record` writes that motion to disk when eight screenshot frames are not enough. `frames` is 1 to 240, `every` is the process frames between shots, and `frames * every` is at most 480. Each frame only grabs an image. The PNG files are written after the last frame, so the timings are the capture times, and `frames × width × height × 4` must stay within 512 MB after `max_dimension`. `directory` must be an absolute path below the system temporary directory (nothing is written elsewhere); omit it for a new one. The reply has no image bytes. `format` `gif` or `mp4` runs `ffmpeg` when it is installed; otherwise the PNG files are still there.

```json
{"frames": 180, "every": 1, "max_dimension": 480, "format": "png"}
```

`user_args` on `godot_run_project` is only for `mode: "process"`. The server places them after `--`, so `--caustics` is a game flag and not a Godot option. `godot_get_run_status` lists them on the process run.

### Recording a video with Movie Maker

`godot_run_project` with `mode: "process"` and `movie` records the run with Godot's [Movie Maker](https://docs.godotengine.org/en/stable/tutorials/animation/creating_movies.html):

```json
{"mode": "process", "user_args": ["--save-profile=trailer"], "movie": {"path": "res://movies/take1.avi", "fps": 60, "quit_after_frames": 900}}
```

- `.avi` writes MJPEG video with the game's audio in one file; `.png` writes `take1########.png`, one image per frame, with the audio in `take1.wav`.
- The game runs at a fixed frame rate: every frame advances game time by exactly `1/fps`, however long it takes to draw and encode. The run is usually slower than real time on screen, but physics, timers and animations follow game time, so the same input gives the same frames.
- The running-game tools still work: drive the player with `godot_game_input` or `godot_game_scenario`, read with `godot_game_tree`. Timed input follows game time.
- `quit_after_frames` (seconds times `fps`) quits the game, which finishes the file. Without it the recording runs until the game quits; `godot_stop_project` asks the game to quit first, so Godot still writes the frame count and index of the `.avi`. A recording killed some other way is reported with `finished: false`.
- `godot_get_run_status` reports `movie` on the run: `path`, `size_bytes`, `frames`, `duration_seconds` and `finished` (and for a PNG sequence the first and last frame and the `.wav`).
- The path stays inside the project, or inside the folder set with `GODOT_MCP_MOVIE_DIR`. A new folder inside the project gets a `.gdignore`, so the editor does not import the frames. An earlier recording at the same path is replaced only with `overwrite: true`.
- Not with `held` (a held game draws no frames) or `headless` (nothing is rendered). Quality, audio mix rate and vsync are project settings (`editor/movie_writer/mjpeg_quality`, `editor/movie_writer/mix_rate`, `editor/movie_writer/disable_vsync`), set with `godot_set_project_setting`.
- Sound is recorded unless the game is started with its own option to mute it: do not pass `--no-sound` in `user_args` for a clip that must be heard. An earlier recording replaced with `overwrite: true` is deleted first.

`godot_game_physics` with `{"action": "step", "ticks": 4}` advances four real engine physics iterations and then pauses the tree. `{"action": "resume"}` continues. Each tick has the normal physics delta, so four ticks are four times `1 / physics_ticks_per_second` seconds of game time, multiplied by `Engine.time_scale`. The step allows at most one physics iteration per process frame while it runs, then restores `max_physics_steps_per_frame`; the tick rate and the time scale are not changed. It does not call `_physics_process` on your nodes. It does not step idle `_process`, nodes with `process_mode` `ALWAYS` (those keep running while the tree is paused), custom threads, or GPU compute, and it does not make the game deterministic. The engine physics-frame counter also keeps moving while the tree is paused, so it is not a count of pausable game physics. If the step fails, or the runtime is removed while a step is still in flight, `max_physics_steps_per_frame` is restored and the tree is left paused.

`godot_game_probe` measures the level without moving anything. A ray straight down at a point, a ray between two points, or a grid of ground heights over an area:

```json
{"rays": [{"x": 4.5, "z": -2}, {"from": [0, 1, 0], "to": [8, 1, 0]}], "exclude": ["Frog"]}
{"grid": {"min": [-5, -5], "max": [5, 5], "step": 0.5}, "mask": 1}
```

A grid answers `heights[row][column]` (row `r` is `z = min.z + r * step`, column `c` is `x = min.x + c * step`, `null` where nothing was hit), the lowest and highest hit and the colliders it met. `godot_game_screenshot` with `aim` looks at a point the same way: `{"aim": {"at": [12, 1.5, -3]}}` draws the world from a temporary camera that copies the game camera's angle, distance, projection and environment and puts that point in the centre; `{"aim": {"position": [...], "look_at": [...]}}` places it. The game's camera stays current and unmoved, and the temporary nodes are freed after the capture. The fresh viewport is initialized before readback, with a second render-only pass in Compatibility so per-instance shader effects are included; the scene does not advance for those passes.

## Holding a game

`godot_game_physics` with `{"action": "hold"}` stops the running game at the start of a physics tick. Nothing advances while it is held: no tick, frame, timer or awaited signal, including nodes whose `process_mode` ignores pause, and the game's own pause cannot release it. The window stays responsive. Reads are still answered (`godot_game_tree`, `godot_game_get`, `godot_game_set`, `godot_game_call` with `permit`, `godot_game_signals`, `godot_game_snapshot`, `godot_run_output`, and a single `godot_game_screenshot` of the last frame the game drew).

```json
{"action": "step", "ticks": 5}
{"action": "step", "until": {"node": ".", "property": "stats/hp", "op": "lte", "value": 0}, "max": 600}
{"action": "resume"}
```

A step runs exactly that many physics ticks (1 to 600) and holds again; the answer says how many ran. With `until` the condition is read before every tick, the first included, so one that already holds runs no tick, and the step stops at `max` (default 600, at most 3600) with `until_met` saying which happened. `until` reads a public property with `get`, like a `godot_game_wait` property wait: it never evaluates an expression. A condition that cannot be read yet does not hold; one that does not parse is refused before any tick runs.

Input is applied by tick while held. `godot_game_input` takes key, mouse button, touch and joypad button taps, clicks, mouse moves, axis values and `time_scale` at once; a tap stays down for `hold_ticks` ticks (default 2, 1 to 600) and is let go at the start of the tick it falls due on, so splitting a step in two does not change what the game sees. A press made while held is down from the next tick and is reported as just pressed on the second one. Leaving the hold, with `resume` or by closing the game, releases everything still held.

`godot_game_run_script` runs in a held game too: a function that awaits runs ticks until it returns, then holds again. A held game refuses what needs time to pass, and says to step instead: `godot_game_wait`, `godot_game_observe`, a screenshot sheet or camera switch (an `aim` capture is drawn at once), `godot_game_record`, and input `wait`, `text`, `swipe`, `path` and `circle` steps. `godot_debug_control` `pause` is refused too, since the game is already still; a breakpoint hit during a step stops the game as usual.

Steps are **exact** only for a game started with `godot_run_project` `{"mode": "process", "held": true}`. That start fixes the engine's frame rate to the project's tick rate (`physics/common/physics_ticks_per_second`, 60 when unset), so every tick is paired with one frame, and the answers say `"deterministic": true`. A hold entered later, or on the editor's Play, still stands still but is reported as `"deterministic": false` with a `deterministic_reason`: `not_started_held`, `fixed_rate_differs` (the game changed its tick rate) or `frames_did_not_follow_ticks` (a step saw frames fail to follow its ticks, which ends the claim for the rest of the run). A step runs its ticks as fast as the game can, so `max` bounds ticks, not seconds. Work on other threads and file loading takes real time, so after starting a game repeat short `until` steps instead of guessing a tick count. A held step that takes longer than 60 seconds is not cancelled: the call returns an error saying the game finishes it and holds again, and `status` shows where it is.

Without a hold, `step` advances 1 to 120 real engine physics iterations and then leaves the tree paused (`resume` clears that pause). It sets `max_physics_steps_per_frame` to 1 while it runs, does not call node `_physics_process`, does not step idle `_process` or nodes that ignore pause, and does not make the game deterministic.

Closing the game's window ends a hold. The server stopping the run does too. A server that disappears without stopping the run leaves its game held, with a window that can still be closed.

## The project adapter

A game has its own vocabulary: the run, the level, the balls still in play. `godot_game_state` and `godot_game_command` let an agent read and act in those terms instead of walking the node tree. The project teaches them through one script it owns, the adapter, named in the project setting `godot_mcp/adapter` (a `res://` path or the `uid://` the file chooser stores). The setting is read once, when the game starts; set it with `godot_set_project_setting` or in Project Settings, then start the game again. The adapter is development code: nothing in the addon names it, and the caller can only ask for what it registered.

```gdscript
extends RefCounted

var _registry

func register(registry) -> void:
    _registry = registry
    registry.add_state("level", _level)
    registry.add_command("grant", _grant, "Grants an effect: {name}.")

func _level() -> Dictionary:
    return {"index": 3}

func _grant(args: Dictionary) -> Dictionary:
    if not args.has("name"):
        return _registry.refuse("name is required")
    return {"granted": args["name"]}
```

- A state provider takes no arguments and returns any value. A command takes one dictionary (the call's arguments, numbers as floats) and returns any value. A command refuses its arguments with `refuse(message)` on the registry it was given in `register`; the caller gets the message as bad arguments. A returned dictionary with an `error` entry is just a result.
- Reading is `godot_game_state` with `names` (all states when omitted); each state that fails is its own `{error}` entry, and the errors and warnings the engine logged meanwhile come back in `errors`. An answer larger than 262144 bytes is refused with the size of each state.
- A command is `godot_game_command` with `name` and `args`. It is not limited by the refusals of `godot_game_call`: it may free nodes, change the scene or write files, whatever the project wrote. A script error inside it fails the call and may leave the game changed. A command and a state must not `await`; what a command defers (`call_deferred`, a timer) takes effect when the game next advances, so step a held game before reading the result.
- Both tools work in a managed run, in editor Play and in a held game. `godot_get_run_status` shows each run's adapter: whether it loaded, what it registered and why a registration or the whole adapter was refused.
- Values with engine types (vectors, colors) come back as `godot_game_get` shows them.

## Playing a scenario

`godot_game_scenario` is one call that presses Input Map actions and pointers, advances physics ticks, and reads the result. It does not run GDScript from the caller. Start the game with `godot_run_project` `{"mode": "process", "held": true}` when the tick count must be exact. The call leaves the game held; `godot_game_physics` `{"action": "resume"}` lets it run, and `godot_stop_project` stops it. The arguments, what is and is not deterministic, and a worked example are in [docs/game-checks.md](game-checks.md).

```json
{"steps": [
  {"frames": 20, "actions": [{"action": "move_right", "phase": "press", "strength": 1}, {"action": "jump", "phase": "tap"}]},
  {"frames": 40}
], "bodies": ["Player"], "nodes": [{"path": "Player", "properties": ["jump_count"]}]}
```

## Keeping a scene loaded between test scripts

`godot_run_script` starts a new process for every script, so a test of a large streamed level pays for loading it on every run. `godot_game_run_script` runs the script inside a game that is already running, so the scene is loaded once and a debugging session changes one line and runs again in milliseconds.

Start the game held and headless: it runs at a fixed step, as fast as it can, and only when asked.

```json
{"mode": "process", "held": true, "headless": true, "scene": "res://levels/old_pond.tscn"}
```

Write the test as a Node with a function. It is not `extends SceneTree` and it does not call `quit()`: `get_tree()` is the game's own tree. A first call can do the setup, such as awaiting frames until the level has finished streaming:

```gdscript
# res://tests/frog_lands.gd
extends Node

func run(height: float) -> Dictionary:
    var frog: Node3D = get_tree().current_scene.get_node("Frog")
    frog.global_position.y = height
    frog.velocity = Vector3.ZERO
    for _i in 60:
        await get_tree().physics_frame
    print("landed: ", frog.is_on_floor())
    return {"y": frog.global_position.y, "on_floor": frog.is_on_floor()}
```

Call it with `godot_game_run_script`:

```json
{"script": "res://tests/frog_lands.gd", "args": [5.0], "timeout_seconds": 30}
```

```json
{
  "ok": true,
  "value": {"y": 0.48, "on_floor": true},
  "output": ["landed: true"],
  "errors": [],
  "ticks_run": 61,
  "frames_advanced": 61,
  "elapsed_ms": 118,
  "held": true,
  "deterministic": true,
  "scene": {"path": "res://levels/old_pond.tscn", "scene_instance_id": 33386661368, "scene_generation": 1}
}
```

Change the file and call again. The file is read and compiled on every call, `scene_instance_id` stays the same, and the game holds between calls, so `godot_game_get`, `godot_game_probe` and `godot_game_screenshot` see it as the function left it.

- `function` names another function of the file (default `run`), and `args` are converted to the types it declares. `_ready()` runs first, and the script's node is freed when the function ends.
- While held, a function with no await runs with no tick passing. One that awaits runs ticks until it returns: it resumes at the start of a tick and that tick finishes before the game holds again, so N awaited physics frames take N + 1 ticks from a hold, the same on every call. Started with `held: true`, the steps are as exact as `godot_game_physics` steps. A game that is not held simply runs on while the function waits.
- A call does not undo what the last one changed: a test that moves the frog leaves it moved. Put the state back in the test, or restart the game when it matters.
- Only the named file is read again. Scripts it preloads and the game's own scripts stay as the game loaded them; restart the game to run changed game code.
- A script error comes back as an error with the same answer (`ok` false, `failure` `script_error`, what was printed before it, the stack) and the scene stays loaded. An error elsewhere in the game meanwhile is listed in `errors` without failing the call. A function still waiting after `timeout_seconds` is ended at its await (`failure` `timeout`). A loop with no await cannot be ended from outside: stop the run.
