extends RefCounted
## Holds the running game still at the start of a physics tick.
##
## While held, no tick, frame, timer or awaited signal advances: the game stays inside the
## SceneTree.physics_frame handler, which the tree emits before any node's physics callback. The
## loop keeps the window responsive and drives the debugger channel itself with
## EngineDebugger.line_poll(), whose full poll (send queued output, dispatch received messages)
## runs once in every 2048 calls, so requests keep arriving and being answered. Only requests that
## finish at once are served: anything that needs frames or real time to pass is refused, because
## that time would never come.

const R := preload("rt_result.gd")
const Inputs := preload("rt_input.gd")

const MAX_HELD_TICKS := 600
const DEFAULT_UNTIL_MAX := 600
const MAX_UNTIL_MAX := 3600
const DEFAULT_HOLD_TICKS := 2
const MAX_HOLD_TICKS := 600
const HOLD_ARGUMENT := "--godot-mcp-hold"
const FIXED_FPS_ARGUMENT := "--godot-mcp-fixed-fps="

## One full debugger poll happens within this many consecutive line_poll() calls.
const POLL_CALLS := 2048
const IDLE_MSEC := 2
## How long a hold request waits for the game to reach a physics tick.
const ARM_TIMEOUT_MSEC := 5000
## Input steps that last over frames; a held game takes the rest at once.
const TIMED_STEPS: Array[String] = ["wait", "text", "swipe", "path"]
## The runtime methods that answer at once and are therefore safe to serve while held.
const SERVED: Array[String] = [
    "game.tree", "game.get", "game.set", "game.call", "game.signals", "game.snapshot",
    "game.physics", "rt.output", "input.release_all",
]

static var _armed := false
static var _holding := false
static var _step_active := false
static var _ticks_run := 0
static var _limit := 0
static var _cond: Dictionary = {}
static var _until_met := false
static var _total_ticks := 0
static var _frames_at_start := 0
static var _step_report: Dictionary = {}
static var _revoked := false


static func is_armed() -> bool:
    return _armed


static func is_held() -> bool:
    return _armed and _holding


## Why a request cannot be served while the game is held, with what to do instead; empty when it
## can. Everything that returns at once is served. Anything that needs frames or real time to
## pass inside the game would wait for time that never comes, so it is refused before it starts.
static func refusal(method: String, params: Dictionary) -> String:
    match method:
        "input.wait":
            return _refused("a wait needs frames to pass", "step with godot_game_physics, using until for a property, then read")
        "game.observe":
            return _refused("watching signals needs frames to pass", "step with godot_game_physics, then read with godot_game_signals")
        "game.screenshot":
            var frames: Variant = params.get("frames", 1)
            var sheet: bool = (frames is int or frames is float) and float(frames) > 1.0
            if sheet or not str(params.get("camera", "")).strip_edges().is_empty():
                return _refused("a frame sheet or a camera switch needs frames to be drawn", "take one capture, step with godot_game_physics, then take the next")
            return ""
        "game.record":
            return _refused("a recording needs frames to be drawn", "resume, or step with godot_game_physics and take screenshots")
        "input.run":
            var steps: Variant = params.get("steps", [])
            if steps is Array:
                for entry: Variant in steps:
                    if entry is Dictionary and str((entry as Dictionary).get("type", "")) in TIMED_STEPS:
                        return _refused("a %s step needs frames to pass" % str((entry as Dictionary)["type"]), "step with godot_game_physics between inputs")
            return ""
    if SERVED.has(method):
        return ""
    return _refused("%s needs time to pass" % method, "step with godot_game_physics, or resume it")


## The version of a method that works while the game is held, or an empty Callable.
static func held_fn(method: String) -> Callable:
    if method == "input.run":
        return run_input
    return Callable()


## Input for a held game: applied at once, in order. A tap is let go after `hold_ticks` ticks.
static func run_input(rt: Node, params: Dictionary) -> Dictionary:
    var hold_ticks: Variant = _whole_number(params.get("hold_ticks", DEFAULT_HOLD_TICKS))
    if hold_ticks == null or int(hold_ticks) < 1 or int(hold_ticks) > MAX_HOLD_TICKS:
        return R.err(R.BAD_PARAMS, "hold_ticks must be an integer from 1 to %d." % MAX_HOLD_TICKS)
    var parsed := Inputs.parse_steps(params)
    if R.is_err(parsed):
        return parsed
    var results: Array = []
    for step: Dictionary in parsed["steps"]:
        var applied := Inputs.apply_held_step(rt, step, _total_ticks + int(hold_ticks))
        if R.is_err(applied):
            Inputs.release_everything()
            return applied
        results.append(applied["result"])
    return R.ok({
        "steps": results,
        "held": Inputs.held_summary(),
        "warp_mouse": Inputs.warp_enabled(),
        "hold_ticks": int(hold_ticks),
        "total_ticks": _total_ticks,
    })


static func _refused(what: String, instead: String) -> String:
    return "The game is held and %s. Instead %s." % [what, instead]


## Arms a hold for a game started held: it stops at its first physics tick, with no request to
## answer.
static func arm_at_launch(rt: Node) -> void:
    if _armed or not OS.get_cmdline_user_args().has(HOLD_ARGUMENT):
        return
    _armed = true
    rt.get_tree().physics_frame.connect(rt.on_physics_frame)


static func total_ticks() -> int:
    return _total_ticks


## Asks for a hold. The answer comes from the hold point, once the game has reached it.
static func begin(rt: Node) -> Dictionary:
    if _armed:
        return R.ok(status(rt))
    _armed = true
    rt.get_tree().physics_frame.connect(rt.on_physics_frame)
    var deadline := Time.get_ticks_msec() + ARM_TIMEOUT_MSEC
    return {"_pending": true, "_tick": func() -> bool: return _await_hold(rt, deadline)}


## Leaves the hold, or cancels a request that has not been reached yet.
static func release(rt: Node) -> void:
    if _armed:
        # Ending a hold lets go of everything it held or scheduled.
        Inputs.release_everything()
    _armed = false
    var tree := rt.get_tree()
    if tree != null and tree.physics_frame.is_connected(rt.on_physics_frame):
        tree.physics_frame.disconnect(rt.on_physics_frame)


static func status(rt: Node) -> Dictionary:
    var body := {
        "held": is_held(),
        "paused": rt.get_tree().paused,
        "total_ticks": _total_ticks,
        "physics_frames": Engine.get_physics_frames(),
        "process_frames": Engine.get_process_frames(),
        "scene": rt.scene_context(),
    }
    body.merge(exactness())
    return body


## Whether stepping is exact, and why not: the game must have been started held at a fixed frame
## rate equal to its tick rate, and no step may have seen frames fail to follow its ticks.
static func exactness() -> Dictionary:
    var reason := ""
    if not OS.get_cmdline_user_args().has(HOLD_ARGUMENT):
        reason = "not_started_held"
    elif _declared_rate() != Engine.physics_ticks_per_second:
        reason = "fixed_rate_differs"
    elif _revoked:
        reason = "frames_did_not_follow_ticks"
    return {"deterministic": reason.is_empty(), "deterministic_reason": reason}


## Runs exactly the asked ticks, or until a condition holds, then holds again. The answer is sent
## from the hold point that ends the step.
static func step(rt: Node, params: Dictionary) -> Dictionary:
    if not is_held():
        return R.err(R.BAD_PARAMS, "The game is not held. Hold it first with action \"hold\".")
    if _step_active:
        return R.err(R.BAD_PARAMS, "A held step is already running.")
    var parsed := _parse_step(params)
    if R.is_err(parsed):
        return parsed
    _limit = int(parsed["limit"])
    _cond = parsed["cond"]
    _ticks_run = 0
    _until_met = false
    _frames_at_start = Engine.get_process_frames()
    _step_active = true
    return {"_pending": true, "_tick": func() -> bool: return true}


static func _parse_step(params: Dictionary) -> Dictionary:
    if params.has("until"):
        if params.has("ticks"):
            return R.err(R.BAD_PARAMS, "Pass max, not ticks, with until.")
        var parsed := Inputs.parse_condition(params["until"])
        if R.is_err(parsed):
            return parsed
        var limit := DEFAULT_UNTIL_MAX
        if params.has("max"):
            var whole: Variant = _whole_number(params["max"])
            if whole == null or int(whole) < 1 or int(whole) > MAX_UNTIL_MAX:
                return R.err(R.BAD_PARAMS, "max must be an integer from 1 to %d." % MAX_UNTIL_MAX)
            limit = int(whole)
        return {"limit": limit, "cond": parsed["cond"]}
    if params.has("max"):
        return R.err(R.BAD_PARAMS, "max needs until: a step without a condition takes ticks.")
    var ticks: Variant = _whole_number(params.get("ticks", null))
    if ticks == null or int(ticks) < 1 or int(ticks) > MAX_HELD_TICKS:
        return R.err(R.BAD_PARAMS, "ticks must be an integer from 1 to %d while the game is held." % MAX_HELD_TICKS)
    return {"limit": int(ticks), "cond": {}}


## Called at the start of every physics tick while a hold is armed. Returning lets the tick run.
static func serve(rt: Node) -> void:
    if not _armed or _holding:
        return
    Inputs.release_due(_total_ticks)
    while _armed:
        if _step_active and _step_lets_tick_run(rt):
            return
        _hold(rt)
        # A step started from the hold decides for this very tick: leaving the hold lets it run.
        if not (_armed and _step_active):
            break
    release(rt)


## Decides for the tick that is about to start. The condition is read before every tick, the
## first included, so a step whose condition already holds runs none.
static func _step_lets_tick_run(rt: Node) -> bool:
    var reading := {}
    if not _cond.is_empty():
        reading = Inputs.read_condition(rt, _cond)
        _until_met = bool(reading["holds"])
    if _until_met or _ticks_run >= _limit:
        _finish_step(reading)
        return false
    _ticks_run += 1
    _total_ticks += 1
    return true


static func _hold(rt: Node) -> void:
    _holding = true
    var reply := status(rt)
    reply.merge(_step_report)
    _step_report = {}
    rt.hold_started(reply)
    while _armed and EngineDebugger.is_active() and not _step_active:
        rt._drain()
        DisplayServer.process_events()
        for _i in POLL_CALLS:
            EngineDebugger.line_poll()
        OS.delay_msec(IDLE_MSEC)
    _holding = false


static func _finish_step(reading: Dictionary) -> void:
    var frames := Engine.get_process_frames() - _frames_at_start
    # A tick not paired with exactly one frame means this run cannot promise exact steps.
    if frames != _ticks_run:
        _revoked = true
    _step_active = false
    _step_report = {
        "action": "step",
        "ticks_run": _ticks_run,
        "frames_advanced": frames,
    }
    if not _cond.is_empty():
        _step_report["until_met"] = _until_met
        _step_report["condition"] = {
            "property": str(_cond["property"]),
            "op": str(_cond["op"]),
            "exists": bool(reading.get("exists", false)),
            "observed": reading.get("observed", null),
        }


static func _whole_number(raw: Variant) -> Variant:
    if raw is int:
        return int(raw)
    if raw is float and float(int(raw)) == float(raw):
        return int(raw)
    return null


static func _declared_rate() -> int:
    for argument: String in OS.get_cmdline_user_args():
        if argument.begins_with(FIXED_FPS_ARGUMENT):
            return argument.substr(FIXED_FPS_ARGUMENT.length()).to_int()
    return 0


static func _await_hold(rt: Node, deadline: int) -> bool:
    if Time.get_ticks_msec() < deadline:
        return true
    release(rt)
    rt.finish_job(R.err(
        R.GODOT_ERROR,
        "The game did not reach a physics tick in time, so it is not held.",
        {"scene": rt.scene_context()},
    ))
    return false
