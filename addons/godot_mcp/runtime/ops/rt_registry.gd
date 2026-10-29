extends RefCounted
## What a project tells the runtime about its game: state it can read and commands it can run,
## registered by name by the project's adapter script (`register(registry)`). The runtime knows
## nothing of the game; this is the one place the game's own names enter.
##
## The registration calls are the ones godot-dev-bridge's adapters already use, so an adapter
## written for it registers here unchanged.

## A project command refuses its arguments by returning `refuse(message)`: a dictionary whose only
## key is this one. A command calls `refuse` instead of writing the key, so a typo in the key cannot
## turn a refusal into an ordinary result.
const BAD_ARGS_KEY := "__bad_args__"

var _states: Dictionary = {}  # name -> Callable
var _commands: Dictionary = {}  # name -> {"handler": Callable, "help": String}
var _refused: Array = []


## Registers a state provider: `provider` takes no arguments and returns any value. A registration
## with an empty name, or a provider that is not valid or wants arguments, is refused: it registers
## nothing and is listed in `refused()`. A name registered again replaces the earlier provider.
func add_state(name: Variant, provider: Variant) -> bool:
    if not name is String or (name as String).is_empty():
        return _refuse(name, "state", "the name must be a non-empty string")
    if not provider is Callable or not (provider as Callable).is_valid():
        return _refuse(name, "state", "the provider must be a valid Callable")
    if (provider as Callable).get_argument_count() != 0:
        return _refuse(name, "state", "the provider must take no arguments")
    _states[name] = provider
    return true


## Registers a command: `handler` takes the call's arguments as one dictionary and returns any
## value; `help` is one line saying what it does and which arguments it takes. Refused like a state
## provider, for a handler that does not take exactly one argument.
func add_command(name: Variant, handler: Variant, help: Variant = "") -> bool:
    if not name is String or (name as String).is_empty():
        return _refuse(name, "command", "the name must be a non-empty string")
    if not handler is Callable or not (handler as Callable).is_valid():
        return _refuse(name, "command", "the handler must be a valid Callable")
    if (handler as Callable).get_argument_count() != 1:
        return _refuse(name, "command", "the handler must take exactly one argument, the arguments dictionary")
    _commands[name] = {"handler": handler, "help": str(help)}
    return true


## What a command returns to refuse the arguments it was given.
func refuse(message: Variant) -> Dictionary:
    return {BAD_ARGS_KEY: str(message)}


func state_names() -> Array:
    var names: Array = _states.keys()
    names.sort()
    return names


func has_command(name: String) -> bool:
    return _commands.has(name)


## The help line of every command, by name.
func command_help() -> Dictionary:
    var help := {}
    for name: String in _commands:
        help[name] = (_commands[name] as Dictionary)["help"]
    return help


## The registrations that were refused, in the order they were made: `{name, kind, reason}`.
func refused() -> Array:
    return _refused.duplicate(true)


## Reads state `name`: `{value}` (any value, null included), or `{error}` for a provider that is not
## registered or is no longer valid. The call is a function of its own: a script error inside the
## provider ends the provider, and one that ends this function leaves `{}`, which no outcome of
## a call is; the caller finds the error in the log.
func call_provider(name: String) -> Dictionary:
    if not _states.has(name):
        return {"error": "No state is registered under '%s'." % name}
    var provider: Callable = _states[name]
    if not provider.is_valid():
        return {"error": "The provider of state '%s' is no longer valid." % name}
    var value: Variant = provider.call()
    return {"value": value}


## Runs command `name` with `args`: `{value}` (any value, null included), `{bad_args}` (the message)
## when the command refused its arguments, or `{error}` for a command that is not registered or
## whose handler is no longer valid. Like `call_provider`, a function of its own.
func call_command(name: String, args: Dictionary) -> Dictionary:
    if not _commands.has(name):
        return {"error": "No command is registered under '%s'." % name}
    var handler: Callable = (_commands[name] as Dictionary)["handler"]
    if not handler.is_valid():
        return {"error": "The handler of command '%s' is no longer valid." % name}
    var value: Variant = handler.call(args)
    if value is Dictionary:
        var answer: Dictionary = value
        if answer.size() == 1 and answer.has(BAD_ARGS_KEY):
            return {"bad_args": str(answer[BAD_ARGS_KEY])}
    return {"value": value}


func _refuse(name: Variant, kind: String, reason: String) -> bool:
    _refused.append({"name": str(name), "kind": kind, "reason": reason})
    push_warning("godot_mcp: refused %s registration '%s' (%s)" % [kind, str(name), reason])
    return false
