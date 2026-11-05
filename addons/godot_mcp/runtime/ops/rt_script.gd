extends RefCounted
## Runs one function of a script file inside the running game, against the live scene.
##
## The file is compiled again from disk on every call, so a test changed between two calls runs
## as changed, and the scene the game already loaded stays as it is. The script must extend Node:
## its instance is a child of the runtime for the length of the call, so get_tree() is the game's
## tree and an await inside the function ends with the instance. Freeing the instance is the one
## way to end a suspended GDScript function, which is why no other base class is taken.
##
## A function that awaits keeps the request open. In a game that is not held it advances with the
## game; in a held game it runs as a held step, tick by tick, until it returns.

const R := preload("rt_result.gd")
const Hold := preload("rt_hold.gd")
const Live := preload("rt_live.gd")
const Output := preload("rt_output.gd")

const DEFAULT_TIMEOUT_SECONDS := 60
const MAX_TIMEOUT_SECONDS := 600
const DEFAULT_FUNCTION := "run"
## The most JSON text a value or the output may carry: more would fill an agent's context.
const MAX_ANSWER_BYTES := 262144
const NODE_NAME := "McpTestScript"


static func methods() -> Dictionary:
    return {"game.run_script": {"fn": run}}


static func run(rt: Node, params: Dictionary) -> Dictionary:
    if Job.running:
        return R.err(R.BAD_PARAMS, "A function started with godot_game_run_script is still running. Wait for its answer.")
    var parsed := _parse(params)
    if R.is_err(parsed):
        return parsed
    var path: String = parsed["script"]
    var function: String = parsed["function"]
    var mark := Output.mark()
    # Ignoring the cache reads the file again. A plain load would keep the first version.
    var loaded: Variant = ResourceLoader.load(path, "GDScript", ResourceLoader.CACHE_MODE_IGNORE)
    if not loaded is GDScript or not (loaded as GDScript).can_instantiate():
        var cause := _first_error(Output.lines_since(mark))
        return R.err(R.BAD_PARAMS, "The script %s did not compile%s Nothing ran, and the game is as it was." % [path, ": %s." % cause if not cause.is_empty() else "."], {"errors": _reported(Output.lines_since(mark))})
    var script: GDScript = loaded
    var base := script.get_instance_base_type()
    if not ClassDB.is_parent_class(base, "Node"):
        return R.err(R.BAD_PARAMS, "The script %s extends %s, and only a Node can run in the game: %s" % [path, base, _base_advice(base)])
    var checked := _check_function(script, path, function, parsed["args"])
    if R.is_err(checked):
        return checked
    var node: Variant = script.new()
    if not node is Node:
        return R.err(R.BAD_PARAMS, "The script %s could not be instantiated: _init() must take no required arguments." % path, {"errors": _reported(Output.lines_since(mark))})
    (node as Node).name = NODE_NAME
    rt.add_child(node)
    Job.running = true
    var job := Job.new(rt, node, path, function, mark, int(parsed["timeout"]))
    job.start(checked["args"])
    if job.finished():
        var answer := job.report()
        if Hold.is_held():
            answer.merge(Hold.status(rt))
        return R.ok(answer)
    if Hold.is_held():
        # Lambdas, not job.is_running: a method Callable does not keep a RefCounted alive.
        return Hold.run_while(func() -> bool: return job.is_running(), func() -> Dictionary: return job.report())
    return {"_pending": true, "_tick": func() -> bool: return _follow(rt, job)}


static func _follow(rt: Node, job: Job) -> bool:
    if job.is_running():
        return true
    rt.finish_job(R.ok(job.report()))
    return false


static func _parse(params: Dictionary) -> Dictionary:
    var script: Variant = params.get("script", null)
    if not script is String or not (script as String).begins_with("res://") or not (script as String).ends_with(".gd") or (script as String).contains(".."):
        return R.err(R.BAD_PARAMS, "game.run_script: 'script' must be the res:// path of a .gd file of the project.")
    var function: Variant = params.get("function", DEFAULT_FUNCTION)
    if not function is String or not (function as String).is_valid_identifier():
        return R.err(R.BAD_PARAMS, "game.run_script: 'function' must be an identifier.")
    var args: Variant = params.get("args", [])
    if args == null:
        args = []
    if not args is Array:
        return R.err(R.BAD_PARAMS, "game.run_script: 'args' must be an array.")
    var timeout: Variant = params.get("timeout_seconds", DEFAULT_TIMEOUT_SECONDS)
    if not (timeout is int or timeout is float) or float(int(timeout)) != float(timeout) or int(timeout) < 1 or int(timeout) > MAX_TIMEOUT_SECONDS:
        return R.err(R.BAD_PARAMS, "game.run_script: 'timeout_seconds' must be a whole number from 1 to %d." % MAX_TIMEOUT_SECONDS)
    if not FileAccess.file_exists(script):
        return R.err(R.NOT_FOUND, "No script exists at %s." % script)
    return {"script": script, "function": function, "args": args, "timeout": int(timeout)}


static func _base_advice(base: String) -> String:
    if ClassDB.is_parent_class(base, "MainLoop"):
        return "the extends SceneTree form is for godot_run_script. Start the file with extends Node, move the work into a function such as run(), and do not call quit(): get_tree() is the game's own tree."
    return "start the file with extends Node."


## The function's declaration, read before anything runs: the arguments it takes, each converted
## to the type it declares.
static func _check_function(script: GDScript, path: String, function: String, raw: Array) -> Dictionary:
    var names: Array = []
    for info: Dictionary in script.get_script_method_list():
        var name := str(info.get("name", ""))
        if not name.begins_with("@") and not names.has(name):
            names.append(name)
        if name != function:
            continue
        var declared: Array = info.get("args", [])
        var required: int = declared.size() - (info.get("default_args", []) as Array).size()
        var variadic: bool = int(info.get("flags", 0)) & METHOD_FLAG_VARARG != 0
        if raw.size() < required or (raw.size() > declared.size() and not variadic):
            return R.err(R.BAD_PARAMS, "%s() in %s takes %d to %d arguments, and args holds %d." % [function, path, required, declared.size(), raw.size()])
        var types: Array = []
        for argument: Dictionary in declared:
            types.append(int(argument.get("type", TYPE_NIL)))
        return Live.convert_args(raw, types, function)
    names.sort()
    return R.err(R.NOT_FOUND, "The script %s has no function %s()." % [path, function], {"functions": names})


static func _reported(lines: Array) -> Array:
    var shown: Array = []
    for line: Dictionary in lines:
        if line["level"] != "info":
            shown.append({"level": line["level"], "text": line["text"], "stack": line["stack"]})
    return shown


static func _first_error(lines: Array) -> String:
    for line: Dictionary in lines:
        if line["level"] == "error":
            return str(line["text"])
    return ""


class Job extends RefCounted:
    ## True from before a function starts until its answer exists. A debugger poll inside a
    ## function with no await can dispatch another request.
    static var running := false

    var _rt: Node
    var _node: Node
    var _path: String
    var _function: String
    var _mark: int
    var _timeout: int
    var _started_msec: int
    var _deadline_msec: int
    var _physics_at_start: int
    var _process_at_start: int
    var _done := false
    var _timed_out := false
    var _value: Variant = null
    ## Nothing else keeps the first state of a function that awaits more than once alive, and the
    ## signal comes from the first state.
    var _state: Variant = null

    func _init(rt: Node, node: Node, path: String, function: String, mark: int, timeout: int) -> void:
        _rt = rt
        _node = node
        _path = path
        _function = function
        _mark = mark
        _timeout = timeout
        _started_msec = Time.get_ticks_msec()
        _deadline_msec = _started_msec + timeout * 1000
        _physics_at_start = Engine.get_physics_frames()
        _process_at_start = Engine.get_process_frames()

    ## The call is a function of its own: a script error ends the function that raised it and
    ## leaves null here. A function that awaits answers with its state, whose signal carries the
    ## return value, and a script error after an await still fires that signal. The state's class
    ## is registered but has no name a script can use.
    func start(args: Array) -> void:
        var out: Variant = _node.callv(_function, args)
        if out is Object and is_instance_valid(out) and (out as Object).get_class() == "GDScriptFunctionState":
            _state = out
            (out as Object).connect("completed", _on_completed, CONNECT_ONE_SHOT)
        else:
            _on_completed(out)

    func _on_completed(value: Variant) -> void:
        _value = value
        _done = true

    func finished() -> bool:
        return _done

    func is_running() -> bool:
        if _done or not is_instance_valid(_node):
            return false
        if Time.get_ticks_msec() >= _deadline_msec:
            _timed_out = true
            return false
        return true

    ## Reads what the function left, then frees its node, which ends a function still suspended.
    func report() -> Dictionary:
        var answer := {
            "ok": true,
            "script": _path,
            "function": _function,
            "value": null,
            "ticks_run": Engine.get_physics_frames() - _physics_at_start,
            "frames_advanced": Engine.get_process_frames() - _process_at_start,
            "elapsed_ms": Time.get_ticks_msec() - _started_msec,
            "scene": _rt.scene_context(),
        }
        if _done:
            var value: Variant = _finite(_rt.encode(_value))
            var size := JSON.stringify(value).to_utf8_buffer().size()
            if size > MAX_ANSWER_BYTES:
                _fail(answer, "value_too_large", "The function ran, and its value is %d bytes, over the limit of %d. Return less, or print what you need." % [size, MAX_ANSWER_BYTES])
            else:
                answer["value"] = value
        elif _timed_out:
            _fail(answer, "timeout", "%s() was still waiting after %d s, so it was ended at its last await." % [_function, _timeout])
        else:
            _fail(answer, "freed", "The script's node was freed before %s() returned." % _function)
        if is_instance_valid(_node):
            _node.free()
        running = false
        # After the free: what the node logs on its way out belongs to this call.
        _add_lines(answer)
        return answer

    func _add_lines(answer: Dictionary) -> void:
        var lines := Output.lines_since(_mark)
        var logged: int = Output.mark() - _mark
        var output: Array = []
        for line: Dictionary in lines:
            if line["level"] == "info":
                output.append(line["text"])
        var dropped := logged - lines.size()
        var bytes := 0
        for text: String in output:
            bytes += text.to_utf8_buffer().size() + 1
        while bytes > MAX_ANSWER_BYTES and not output.is_empty():
            bytes -= (output.pop_front() as String).to_utf8_buffer().size() + 1
            dropped += 1
        answer["output"] = output
        if dropped > 0:
            answer["output_dropped"] = dropped
        var errors: Array = []
        for line: Dictionary in lines:
            if line["level"] != "info" and errors.size() < 64:
                errors.append({"level": line["level"], "text": line["text"], "stack": line["stack"]})
            if line["level"] == "error" and bool(line["script"]) and _is_ours(line) and bool(answer["ok"]):
                _fail(answer, "script_error", "%s() raised a script error: %s. The game may have changed before the error." % [_function, str(line["text"]).trim_suffix(".")])
        answer["errors"] = errors

    ## A script error somewhere else in the game while this function awaits is reported, and is
    ## not this function's failure.
    func _is_ours(line: Dictionary) -> bool:
        if str(line["text"]).contains(_path):
            return true
        for frame: String in line["stack"]:
            if frame.begins_with(_path + ":"):
                return true
        return false

    func _fail(answer: Dictionary, failure: String, message: String) -> void:
        answer["ok"] = false
        answer["failure"] = failure
        answer["message"] = message

    ## Replaces what JSON cannot hold: a value that is not finite would make the whole answer
    ## unreadable.
    static func _finite(value: Variant) -> Variant:
        match typeof(value):
            TYPE_FLOAT:
                return value if is_finite(value) else str(value)
            TYPE_ARRAY:
                var items: Array = []
                for item: Variant in value:
                    items.append(_finite(item))
                return items
            TYPE_DICTIONARY:
                var mapped := {}
                for key: Variant in value:
                    mapped[key] = _finite(value[key])
                return mapped
        return value
