@tool
extends RefCounted
## Reads and changes the import parameters of a file: the [params] section of its .import
## sidecar, the way the Import dock stores them. Only ConfigFile work, with no editor class, so
## the bridge and the server's headless runner (which copies this file into a throw-away
## project) share it. The caller reimports the file afterwards; the importer then rewrites the
## sidecar with the options it has, so a name it does not know is dropped.
##
## Every answer is a Dictionary. A refusal has "error" (a code) and writes nothing.

## Types a string value is parsed into with str_to_var. Their text forms hold only numbers or
## strings, so parsing one never creates an object.
const PARSED_TYPES: Array[int] = [
    TYPE_VECTOR2, TYPE_VECTOR2I, TYPE_RECT2, TYPE_RECT2I, TYPE_VECTOR3, TYPE_VECTOR3I,
    TYPE_TRANSFORM2D, TYPE_VECTOR4, TYPE_VECTOR4I, TYPE_PLANE, TYPE_QUATERNION, TYPE_AABB,
    TYPE_BASIS, TYPE_TRANSFORM3D, TYPE_PROJECTION, TYPE_COLOR,
    TYPE_PACKED_INT32_ARRAY, TYPE_PACKED_INT64_ARRAY, TYPE_PACKED_FLOAT32_ARRAY,
    TYPE_PACKED_FLOAT64_ARRAY, TYPE_PACKED_STRING_ARRAY, TYPE_PACKED_VECTOR2_ARRAY,
    TYPE_PACKED_VECTOR3_ARRAY, TYPE_PACKED_COLOR_ARRAY, TYPE_PACKED_VECTOR4_ARRAY,
]

## The importer, the imported type and the parameters of import_file, each value as JSON can
## carry it (other types as their Godot text form), with its Godot type under "types".
static func describe(import_file: String) -> Dictionary:
    var config: ConfigFile = ConfigFile.new()
    var loaded: Error = config.load(import_file)
    if loaded != OK:
        return {"error": "UNREADABLE", "file": import_file, "message": error_string(loaded)}
    return _describe(config)

## request: importer (the importer to switch to, as the Import dock's "Import As"), params
## (Dictionary of option name to value) and copy_from (another .import file, whose parameters are
## copied first). A new importer starts from its own defaults, as in the dock: the old [params]
## are dropped. An option keeps the type it holds. Saves import_file only when something changed
## and answers with changed, new_options (names it did not hold before), copied (how many
## parameters copy_from gave), previous_importer and importer_changed.
static func change(import_file: String, request: Dictionary) -> Dictionary:
    var config: ConfigFile = ConfigFile.new()
    var loaded: Error = config.load(import_file)
    if loaded != OK:
        return {"error": "UNREADABLE", "file": import_file, "message": error_string(loaded)}
    var previous_importer: String = str(config.get_value("remap", "importer", ""))
    var importer: String = previous_importer
    var before: Dictionary = _params(config)
    var requested_importer: String = str(request.get("importer", ""))
    var importer_changed: bool = not requested_importer.is_empty() and requested_importer != importer
    if importer_changed:
        importer = requested_importer
        config.set_value("remap", "importer", importer)
        if config.has_section("params"):
            config.erase_section("params")
        before = {}
    var copied: int = 0
    var copy_from: String = str(request.get("copy_from", ""))
    if not copy_from.is_empty():
        var source: ConfigFile = ConfigFile.new()
        var source_loaded: Error = source.load(copy_from)
        if source_loaded != OK:
            return {"error": "UNREADABLE", "file": copy_from, "message": error_string(source_loaded)}
        var source_importer: String = str(source.get_value("remap", "importer", ""))
        if source_importer != importer:
            return {"error": "IMPORTER", "importer": importer, "source_importer": source_importer}
        var source_params: Dictionary = _params(source)
        for key: String in source_params:
            config.set_value("params", key, source_params[key])
        copied = source_params.size()
    var requested: Variant = request.get("params", {})
    if typeof(requested) == TYPE_DICTIONARY:
        for key: Variant in requested:
            var name: String = str(key)
            var had: bool = config.has_section_key("params", name)
            var old: Variant = config.get_value("params", name) if had else null
            var typed: Dictionary = _typed(requested[key], old, had)
            if typed.has("expected"):
                return {"error": "TYPE", "option": name, "expected": typed["expected"], "current": _plain(old)}
            config.set_value("params", name, typed["value"])
    var after: Dictionary = _params(config)
    var changed: Array = []
    var new_options: Array = []
    for key: String in after:
        if not before.has(key):
            new_options.append(key)
            changed.append(key)
        elif typeof(before[key]) != typeof(after[key]) or before[key] != after[key]:
            changed.append(key)
    if importer_changed or not changed.is_empty():
        var saved: Error = config.save(import_file)
        if saved != OK:
            return {"error": "UNWRITABLE", "file": import_file, "message": error_string(saved)}
    return {
        "importer": importer, "previous_importer": previous_importer,
        "importer_changed": importer_changed, "changed": changed, "new_options": new_options,
        "copied": copied,
    }

static func _params(config: ConfigFile) -> Dictionary:
    var params: Dictionary = {}
    if config.has_section("params"):
        for key: String in config.get_section_keys("params"):
            params[key] = config.get_value("params", key)
    return params

static func _describe(config: ConfigFile) -> Dictionary:
    var params: Dictionary = {}
    var types: Dictionary = {}
    var stored: Dictionary = _params(config)
    for key: String in stored:
        params[key] = _plain(stored[key])
        types[key] = type_string(typeof(stored[key]))
    return {
        "importer": str(config.get_value("remap", "importer", "")),
        "type": str(config.get_value("remap", "type", "")),
        "params": params,
        "types": types,
    }

static func _plain(value: Variant) -> Variant:
    match typeof(value):
        TYPE_NIL, TYPE_BOOL, TYPE_INT, TYPE_FLOAT, TYPE_STRING:
            return value
        TYPE_STRING_NAME:
            return str(value)
    return var_to_str(value)

## {"value": the value to store} or {"expected": the type the option holds}. JSON has one number
## type, so a whole number becomes an int unless the option holds a float; a string becomes the
## option's type when that type has a plain text form, such as Vector2(1, 1).
static func _typed(value: Variant, old: Variant, had: bool) -> Dictionary:
    var kind: int = typeof(value)
    var whole: bool = kind == TYPE_FLOAT and value == floorf(value) and absf(value) < 9.0e15
    if not had or typeof(old) == TYPE_NIL:
        if whole:
            return {"value": int(value)}
        return {"value": value}
    var old_kind: int = typeof(old)
    match old_kind:
        TYPE_INT:
            if kind == TYPE_INT:
                return {"value": value}
            if whole:
                return {"value": int(value)}
        TYPE_FLOAT:
            if kind == TYPE_INT or kind == TYPE_FLOAT:
                return {"value": float(value)}
        TYPE_BOOL, TYPE_STRING:
            if kind == old_kind:
                return {"value": value}
        TYPE_STRING_NAME:
            if kind == TYPE_STRING:
                return {"value": StringName(value)}
        _:
            if kind == TYPE_STRING and old_kind in PARSED_TYPES:
                var text: String = value
                if text.strip_edges().begins_with(type_string(old_kind) + "("):
                    var parsed: Variant = str_to_var(text)
                    if typeof(parsed) == old_kind:
                        return {"value": parsed}
    return {"expected": type_string(old_kind)}
