extends RefCounted
## The project's own states and commands: loads the adapter script the project names in the
## setting `godot_mcp/adapter`, lets it register what the game can show and do, and keeps the
## outcome as a status the server shows next to the run.
##
## Nothing here names a project class or path. An adapter is development code the project owns:
## the setting is read once, when the runtime starts, and the runtime never writes it.

const R := preload("rt_result.gd")
const Registry := preload("rt_registry.gd")
const Output := preload("rt_output.gd")

const SETTING := "godot_mcp/adapter"
## The most JSON text an answer may carry: more would fill an agent's context with one call.
const MAX_ANSWER_BYTES := 262144

static var _registry: RefCounted = null
static var _adapter: Variant = null
static var _status: Dictionary = {"configured": false}


static func methods() -> Dictionary:
    return {
        "project.state": {"fn": state},
        "project.command": {"fn": command},
    }


## Reads the states `names` (every state when it is missing or empty): `{adapter, values, errors}`
## where each value is what its provider returned, or `{error}` for a provider that failed. A
## provider that raised a script error fails alone.
static func state(rt: Node, params: Dictionary) -> Dictionary:
    var unavailable := _unavailable()
    if not unavailable.is_empty():
        return unavailable
    var names: Array = _registry.state_names()
    var asked: Variant = params.get("names", null)
    if asked != null:
        if not asked is Array:
            return R.err(R.BAD_PARAMS, "project.state: 'names' must be an array of state names.")
        for item: Variant in asked:
            if not item is String or (item as String).is_empty():
                return R.err(R.BAD_PARAMS, "project.state: every entry of 'names' must be a non-empty string.")
        if not (asked as Array).is_empty():
            names = asked
    var values := {}
    var sizes := {}
    var errors: Array = []
    var total := 0
    for name: String in names:
        var mark := Output.mark()
        var answer: Dictionary = _registry.call_provider(name)
        var lines := Output.lines_since(mark)
        errors.append_array(_reported(lines))
        var failure := _first_error(lines, true)
        if answer.has("error"):
            values[name] = {"error": str(answer["error"])}
        elif not answer.has("value"):
            values[name] = {"error": _described("the provider did not answer", failure)}
        elif not failure.is_empty():
            values[name] = {"error": "the provider raised a script error: %s" % failure}
        else:
            var encoded: Variant = rt.encode(answer["value"])
            sizes[name] = JSON.stringify(encoded).to_utf8_buffer().size()
            total += int(sizes[name])
            values[name] = encoded
    if total > MAX_ANSWER_BYTES:
        return R.err(R.BAD_PARAMS, "The states are %d bytes together, over the limit of %d. Ask for fewer names." % [total, MAX_ANSWER_BYTES], {"sizes": sizes, "limit": MAX_ANSWER_BYTES})
    return R.ok({"adapter": status(), "values": values, "errors": errors})


## Runs the command `name` with the dictionary `args`: `{adapter, name, value, errors}`. The
## command does what the project wrote; a script error inside it fails the call, and the failure
## says the command may have changed the game before it.
static func command(rt: Node, params: Dictionary) -> Dictionary:
    var unavailable := _unavailable()
    if not unavailable.is_empty():
        return unavailable
    var name: Variant = params.get("name", null)
    if not name is String or (name as String).is_empty():
        return R.err(R.BAD_PARAMS, "project.command: 'name' must be the name of a command.")
    var args: Variant = params.get("args", null)
    if args == null:
        args = {}
    if not args is Dictionary:
        return R.err(R.BAD_PARAMS, "project.command: 'args' must be an object.")
    if not _registry.has_command(name):
        var known: Array = _registry.command_help().keys()
        known.sort()
        return R.err(R.NOT_FOUND, "The project registered no command named '%s'." % name, {"commands": known})
    var mark := Output.mark()
    var outcome: Dictionary = _registry.call_command(name, args)
    var lines := Output.lines_since(mark)
    var errors := _reported(lines)
    var failure := _first_error(lines, true)
    if not failure.is_empty():
        return R.err(R.GODOT_ERROR, "Command '%s' raised a script error: %s. It may have changed the game before the error. A handler must not await." % [name, failure], {"errors": errors})
    if outcome.has("bad_args"):
        return R.err(R.BAD_PARAMS, "Command '%s' refused its arguments: %s" % [name, outcome["bad_args"]], {"help": _registry.command_help().get(name, "")})
    if outcome.has("error"):
        return R.err(R.GODOT_ERROR, str(outcome["error"]))
    if not outcome.has("value"):
        return R.err(R.GODOT_ERROR, "Command '%s' did not answer." % name, {"errors": errors})
    var value: Variant = rt.encode(outcome["value"])
    var size := JSON.stringify(value).to_utf8_buffer().size()
    if size > MAX_ANSWER_BYTES:
        return R.err(R.GODOT_ERROR, "Command '%s' ran, and its value is %d bytes, over the limit of %d. Have the command return less." % [name, size, MAX_ANSWER_BYTES], {"sizes": {"value": size}, "limit": MAX_ANSWER_BYTES})
    return R.ok({"adapter": status(), "name": name, "value": value, "errors": errors})


## Loads the adapter the setting names, if there is one, and lets it register. What happened is in
## `status()`; a failure never raises.
static func load_adapter() -> void:
    unload()
    var path := str(ProjectSettings.get_setting(SETTING, ""))
    if path.is_empty():
        return
    _registry = Registry.new()
    _status = {"configured": true, "path": path, "loaded": false}
    var resolved := _plain_path(path)
    if not resolved.begins_with("res://"):
        _status["error"] = "the adapter must be a script of the project: a res:// path or the uid:// of one, not '%s'" % path
        return
    if not ResourceLoader.exists(resolved):
        _status["error"] = "no script exists at %s" % resolved
        return
    var mark := Output.mark()
    var script: Variant = load(resolved)
    if not script is GDScript or not (script as GDScript).can_instantiate():
        _status["error"] = _described("the script cannot be loaded", _first_error(Output.lines_since(mark), false))
        return
    _adapter = (script as GDScript).new()
    if not _adapter.has_method("register"):
        _status["error"] = "the script has no register(registry) method"
        return
    mark = Output.mark()
    _register(_adapter, _registry)
    var cause := _first_error(Output.lines_since(mark), true)
    if cause.is_empty():
        _status["loaded"] = true
    else:
        _status["error"] = _described("register raised a script error", cause)


## Lets go of the adapter. A node that was never added to the tree has no owner to free it.
static func unload() -> void:
    if _adapter is Node and is_instance_valid(_adapter) and not (_adapter as Node).is_inside_tree():
        (_adapter as Node).free()
    _adapter = null
    _registry = null
    _status = {"configured": false}


## What the server shows for the run: whether an adapter was named, loaded and registered, what it
## registered and what the registry refused.
static func status() -> Dictionary:
    var shown := _status.duplicate(true)
    if _registry != null:
        shown["states"] = _registry.state_names()
        shown["commands"] = _registry.command_help()
        shown["refused"] = _registry.refused()
    return shown


## The error to answer with when there is no adapter to ask: none named, or one that did not load.
static func _unavailable() -> Dictionary:
    if _registry != null and bool(_status.get("loaded", false)):
        return {}
    if not bool(_status.get("configured", false)):
        return R.err(R.UNSUPPORTED, "The project names no adapter: the project setting %s is empty." % SETTING, status())
    return R.err(R.UNSUPPORTED, "The adapter did not load: %s" % str(_status.get("error", "unknown cause")), status())


## What the engine logged while a call ran, apart from plain output: `{level, text}` per error and
## warning.
static func _reported(lines: Array) -> Array:
    var shown: Array = []
    for line: Dictionary in lines:
        if line["level"] != "info":
            shown.append({"level": line["level"], "text": line["text"]})
    return shown


## The call is a function of its own: an adapter whose `register` awaits makes the engine end the
## function that called it, and this one has nothing more to do.
static func _register(adapter: Variant, registry: RefCounted) -> void:
    adapter.register(registry)


## The text of the first line the engine logged as an error (as a script error when `script_only`),
## or nothing.
static func _first_error(lines: Array, script_only: bool) -> String:
    for line: Dictionary in lines:
        if line["level"] == "error" and (bool(line["script"]) or not script_only):
            return str(line["text"])
    return ""


static func _described(lead: String, cause: String) -> String:
    return lead if cause.is_empty() else "%s: %s" % [lead, cause]


## A `uid://` the engine knows resolves to the script's path; anything else stays as it is.
static func _plain_path(path: String) -> String:
    if not path.begins_with("uid://"):
        return path
    var id := ResourceUID.text_to_id(path)
    return ResourceUID.get_id_path(id) if ResourceUID.has_id(id) else path
