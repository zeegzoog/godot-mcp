//! A project adapter for the end-to-end tests: the script a project hands to the runtime to
//! register its own states and commands. It uses every kind of registration, the valid ones and
//! the ones the registry must refuse, and a few that misbehave, so one file serves the registry,
//! the loader and the tools.

/// The states the fixture registers, by name, in the order the registry lists them.
pub const STATES: [&str; 7] = [
    "balls", "big", "broken", "deferred", "level", "nothing", "position",
];

/// The commands the fixture registers, by name, in the order the registry lists them.
pub const COMMANDS: [&str; 9] = [
    "big_value",
    "clear",
    "defer",
    "fail",
    "float_arg",
    "free_a_node",
    "grant",
    "grow",
    "wait",
];

/// GDScript source of the fixture adapter, written into a test project as `res://adapter.gd`.
pub const FIXTURE_ADAPTER: &str = r#"extends RefCounted

var _registry
var _deferred := 0
var _big_size := 10

func register(registry) -> void:
    _registry = registry
    registry.add_state("level", _level)
    registry.add_state("balls", _balls)
    registry.add_state("balls", _balls_again)
    registry.add_state("takes_argument", _takes_argument)
    registry.add_state("", _level)
    registry.add_state("nothing", _nothing)
    registry.add_state("broken", _broken)
    registry.add_state("big", _big)
    registry.add_state("position", _position)
    registry.add_state("deferred", _deferred_state)
    registry.add_command("grant", _grant, "Grants {name}.")
    registry.add_command("clear", _clear, "Clears the level.")
    registry.add_command("float_arg", _float_arg, "Says whether {n} arrived as a float.")
    registry.add_command("free_a_node", _free_a_node, "Frees a node, which godot_game_call refuses.")
    registry.add_command("no_argument", _no_argument)
    registry.add_command("fail", _fail, "Raises a script error.")
    registry.add_command("wait", _wait, "Awaits a frame, which a handler must not do.")
    registry.add_command("defer", _defer, "Raises the deferred state at the next idle time.")
    registry.add_command("grow", _grow, "Makes the big state larger than an answer may be.")
    registry.add_command("big_value", _big_value, "Returns a value larger than an answer may be.")

func _level() -> Dictionary:
    return {"index": 3, "kinds": {"standard": 12}}

func _balls() -> Array:
    return [1]

func _balls_again() -> Array:
    return [1, 2]

func _takes_argument(_unused) -> int:
    return 0

func _nothing() -> Variant:
    return null

func _broken() -> Variant:
    var missing: Variant = {}.get("none")
    return missing.name

func _big() -> String:
    return "x".repeat(_big_size)

func _position() -> Vector2:
    return Vector2(1, 2)

func _deferred_state() -> int:
    return _deferred

func _grant(args: Dictionary) -> Dictionary:
    if not args.has("name"):
        return _registry.refuse("name is required")
    return {"granted": args["name"]}

func _clear(_args: Dictionary) -> Dictionary:
    return {"error": "an entry named error is a result, not a refusal"}

func _float_arg(args: Dictionary) -> Dictionary:
    return {"is_float": typeof(args.get("n")) == TYPE_FLOAT}

func _free_a_node(_args: Dictionary) -> Dictionary:
    var node := Node.new()
    node.free()
    return {"freed": not is_instance_valid(node)}

func _no_argument() -> void:
    pass

func _fail(_args: Dictionary) -> Variant:
    var missing: Variant = {}.get("none")
    return missing.name

func _wait(_args: Dictionary) -> Variant:
    await (Engine.get_main_loop() as SceneTree).process_frame
    return "waited"

func _defer(_args: Dictionary) -> Dictionary:
    call_deferred("_raise")
    return {"scheduled": true}

func _raise() -> void:
    _deferred += 1

func _grow(_args: Dictionary) -> Dictionary:
    _big_size = 300000
    return {"size": _big_size}

func _big_value(_args: Dictionary) -> String:
    return "x".repeat(300000)
"#;
