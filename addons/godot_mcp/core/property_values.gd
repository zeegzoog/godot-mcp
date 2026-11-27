@tool
extends RefCounted
## Converts JSON values to the type of a property, and changes and saves the properties of a
## resource file. Only engine classes that exist outside the editor, so the editor bridge and the
## server's headless runner (which copies this file next to its own script) share it.
##
## Every function is static. A conversion answers {"value": Variant} or {"error": String}.

## Text forms longer than this are shortened in answers.
const MAX_VALUE_CHARS: int = 2000

## Fields accepted when a vector-like value is given as a JSON object.
const OBJECT_FIELDS: Dictionary = {
    TYPE_VECTOR2: ["x", "y"],
    TYPE_VECTOR2I: ["x", "y"],
    TYPE_VECTOR3: ["x", "y", "z"],
    TYPE_VECTOR3I: ["x", "y", "z"],
    TYPE_VECTOR4: ["x", "y", "z", "w"],
    TYPE_VECTOR4I: ["x", "y", "z", "w"],
    TYPE_QUATERNION: ["x", "y", "z", "w"],
    TYPE_COLOR: ["r", "g", "b", "a"],
}

static func property_type_name(info: Dictionary) -> String:
    var type_id: int = info["type"]
    if type_id == TYPE_OBJECT:
        var object_class: String = info.get("class_name", "")
        return object_class if not object_class.is_empty() else "Object"
    if type_id == TYPE_ARRAY:
        var element: Dictionary = array_element_info(info)
        if not element.is_empty() and not element.has("error"):
            return "Array[%s]" % property_type_name(element)
    if type_id == TYPE_DICTIONARY:
        var entries: Dictionary = dictionary_entry_info(info)
        if not entries.is_empty() and not entries.has("error"):
            var key_name: String = "Variant" if entries["key"].is_empty() else property_type_name(entries["key"])
            var value_name: String = "Variant" if entries["value"].is_empty() else property_type_name(entries["value"])
            return "Dictionary[%s, %s]" % [key_name, value_name]
    return type_string(type_id)

## Builds a vector-like value from a list of numbers; returns null if the count is wrong.
static func build_numeric_value(expected: int, n: Array) -> Variant:
    match expected:
        TYPE_VECTOR2:
            if n.size() == 2: return Vector2(n[0], n[1])
        TYPE_VECTOR2I:
            if n.size() == 2: return Vector2i(int(n[0]), int(n[1]))
        TYPE_VECTOR3:
            if n.size() == 3: return Vector3(n[0], n[1], n[2])
        TYPE_VECTOR3I:
            if n.size() == 3: return Vector3i(int(n[0]), int(n[1]), int(n[2]))
        TYPE_VECTOR4:
            if n.size() == 4: return Vector4(n[0], n[1], n[2], n[3])
        TYPE_VECTOR4I:
            if n.size() == 4: return Vector4i(int(n[0]), int(n[1]), int(n[2]), int(n[3]))
        TYPE_QUATERNION:
            if n.size() == 4: return Quaternion(n[0], n[1], n[2], n[3])
        TYPE_COLOR:
            if n.size() == 3: return Color(n[0], n[1], n[2])
            if n.size() == 4: return Color(n[0], n[1], n[2], n[3])
        TYPE_RECT2:
            if n.size() == 4: return Rect2(n[0], n[1], n[2], n[3])
        TYPE_RECT2I:
            if n.size() == 4: return Rect2i(int(n[0]), int(n[1]), int(n[2]), int(n[3]))
    return null

## Reads a JSON array or object of numbers; returns [] if anything is not a number.
static func numbers_from(value: Variant, fields: Array) -> Array:
    var numbers: Array = []
    if value is Array:
        for item: Variant in value:
            if not (item is float or item is int):
                return []
            numbers.append(float(item))
    elif value is Dictionary:
        for field: String in fields:
            if not value.has(field):
                break
            var item: Variant = value[field]
            if not (item is float or item is int):
                return []
            numbers.append(float(item))
    return numbers

## The property info of one element type from a typed container's hint: "type/hint:hint_string"
## (PROPERTY_HINT_TYPE_STRING, exported containers) or a type or class name
## (PROPERTY_HINT_ARRAY_TYPE). Returns {} for a Variant element, else {"type", "hint",
## "hint_string", "class_name"} or {"error": String}.
static func element_info(description: String) -> Dictionary:
    if description.is_empty() or description == "Variant":
        return {}
    var colon: int = description.find(":")
    var head: String = description if colon < 0 else description.substr(0, colon)
    var head_parts: PackedStringArray = head.split("/")
    if head_parts[0].is_valid_int() and (head_parts.size() == 1 or head_parts[1].is_valid_int()):
        var type_id: int = head_parts[0].to_int()
        if type_id == TYPE_NIL:
            return {}
        var hint: int = head_parts[1].to_int() if head_parts.size() > 1 else PROPERTY_HINT_NONE
        var hint_string: String = "" if colon < 0 else description.substr(colon + 1)
        var object_class: String = ""
        if type_id == TYPE_OBJECT:
            var names_class: bool = hint in [PROPERTY_HINT_RESOURCE_TYPE, PROPERTY_HINT_NODE_TYPE] and not hint_string.is_empty()
            object_class = hint_string if names_class else "Object"
        return {"type": type_id, "hint": hint, "hint_string": hint_string, "class_name": object_class}
    for type_id: int in range(TYPE_NIL + 1, TYPE_MAX):
        if type_id != TYPE_OBJECT and type_string(type_id) == description:
            return {"type": type_id, "hint": PROPERTY_HINT_NONE, "hint_string": "", "class_name": ""}
    if ClassDB.class_exists(description) or not global_class_script(description).is_empty():
        return {"type": TYPE_OBJECT, "hint": PROPERTY_HINT_NONE, "hint_string": description, "class_name": description}
    return {"error": "cannot read the element type '%s' of this typed container" % description}

## The element type of a typed array property ({} for an untyped one); see element_info.
static func array_element_info(info: Dictionary) -> Dictionary:
    var hint: int = info.get("hint", PROPERTY_HINT_NONE)
    if hint != PROPERTY_HINT_TYPE_STRING and hint != PROPERTY_HINT_ARRAY_TYPE:
        return {}
    return element_info(str(info.get("hint_string", "")))

## The key and value types of a typed dictionary property: {} for an untyped one, else
## {"key": Dictionary, "value": Dictionary} (each as element_info) or {"error": String}.
static func dictionary_entry_info(info: Dictionary) -> Dictionary:
    var hint: int = info.get("hint", PROPERTY_HINT_NONE)
    var hint_string: String = str(info.get("hint_string", ""))
    if (hint != PROPERTY_HINT_TYPE_STRING and hint != PROPERTY_HINT_DICTIONARY_TYPE) or not hint_string.contains(";"):
        return {}
    var key: Dictionary = element_info(hint_string.get_slice(";", 0))
    if key.has("error"):
        return key
    var value: Dictionary = element_info(hint_string.substr(hint_string.find(";") + 1))
    if value.has("error"):
        return value
    if key.is_empty() and value.is_empty():
        return {}
    return {"key": key, "value": value}

## res:// path of the script of a global class_name, or "".
static func global_class_script(global_name: String) -> String:
    for entry: Dictionary in ProjectSettings.get_global_class_list():
        if entry["class"] == global_name:
            return str(entry["path"])
    return ""

## Whether an object is of a native class or a global script class (or extends it).
static func object_is_class(object: Object, wanted: String) -> bool:
    if wanted.is_empty() or wanted == "Object":
        return true
    if ClassDB.class_exists(wanted):
        return object.is_class(wanted)
    var script: Script = object.get_script() as Script
    while script != null:
        if String(script.get_global_name()) == wanted:
            return true
        script = script.get_base_script()
    return false

## Converts one element of a typed container. A value already of the element type (from a
## parsed Godot literal) is kept. Returns {"value": Variant} or {"error": String}.
static func convert_element(element: Dictionary, raw: Variant) -> Dictionary:
    if element.is_empty():
        return {"value": raw}
    var expected: int = element["type"]
    if expected != TYPE_OBJECT:
        if typeof(raw) == expected:
            return {"value": raw}
        return convert_property_value(element, raw)
    var result: Dictionary = {"value": raw} if raw == null or raw is Object else convert_property_value(element, raw)
    if result.has("error") or result["value"] == null:
        return result
    var object: Object = result["value"]
    var wanted: String = element["class_name"]
    if not object_is_class(object, wanted):
        return {"error": "%s is a %s, not a %s" % [describe_value(object), object.get_class(), wanted]}
    return result

## The arguments that type a container's elements: [type, native class, script].
static func container_typing(element: Dictionary) -> Array:
    if element.is_empty():
        return [TYPE_NIL, &"", null]
    if element["type"] != TYPE_OBJECT:
        return [element["type"], &"", null]
    var wanted: String = element["class_name"]
    if ClassDB.class_exists(wanted):
        return [TYPE_OBJECT, StringName(wanted), null]
    var script: Script = load(global_class_script(wanted)) as Script
    if script == null:
        return [TYPE_OBJECT, &"Object", null]
    return [TYPE_OBJECT, script.get_instance_base_type(), script]

## Builds a typed array from JSON or parsed items. Returns {"value": Array} or {"error": String}
## naming the first element that does not convert.
static func typed_array_from(element: Dictionary, items: Array) -> Dictionary:
    var values: Array = []
    for index: int in items.size():
        var converted: Dictionary = convert_element(element, items[index])
        if converted.has("error"):
            return {"error": "element %d: %s" % [index, converted["error"]]}
        values.append(converted["value"])
    var typing: Array = container_typing(element)
    return {"value": Array(values, typing[0], typing[1], typing[2])}

## Builds a typed dictionary from JSON or parsed entries. JSON keys are always strings, so a key
## is converted like an element. Returns {"value": Dictionary} or {"error": String}.
static func typed_dictionary_from(key_element: Dictionary, value_element: Dictionary, items: Dictionary) -> Dictionary:
    var values: Dictionary = {}
    for raw_key: Variant in items:
        var key: Dictionary = convert_element(key_element, raw_key)
        if key.has("error"):
            return {"error": "key %s: %s" % [describe_value(raw_key), key["error"]]}
        var value: Dictionary = convert_element(value_element, items[raw_key])
        if value.has("error"):
            return {"error": "value of %s: %s" % [describe_value(raw_key), value["error"]]}
        values[key["value"]] = value["value"]
    var key_typing: Array = container_typing(key_element)
    var value_typing: Array = container_typing(value_element)
    return {"value": Dictionary(values, key_typing[0], key_typing[1], key_typing[2], value_typing[0], value_typing[1], value_typing[2])}

## Converts a JSON value to what the property expects. Returns {"value": Variant} or
## {"error": String}.
static func convert_property_value(info: Dictionary, raw: Variant) -> Dictionary:
    var expected: int = info["type"]

    match expected:
        TYPE_NIL:
            return {"value": raw}
        TYPE_BOOL:
            if raw is bool:
                return {"value": raw}
        TYPE_INT:
            if raw is int:
                return {"value": raw}
            if raw is float and raw == floorf(raw):
                return {"value": int(raw)}
        TYPE_FLOAT:
            if raw is float or raw is int:
                return {"value": float(raw)}
        TYPE_STRING:
            if raw is String:
                return {"value": raw}
        TYPE_STRING_NAME:
            if raw is String:
                return {"value": StringName(raw)}
        TYPE_NODE_PATH:
            if raw is String:
                return {"value": NodePath(raw)}
        TYPE_VECTOR2, TYPE_VECTOR2I, TYPE_VECTOR3, TYPE_VECTOR3I, TYPE_VECTOR4, TYPE_VECTOR4I, TYPE_QUATERNION, TYPE_COLOR, TYPE_RECT2, TYPE_RECT2I:
            if expected == TYPE_COLOR and raw is String:
                var sentinel: Color = Color(-1.0, -1.0, -1.0, -1.0)
                var parsed_color: Color = Color.from_string(raw, sentinel)
                if parsed_color != sentinel:
                    return {"value": parsed_color}
            if raw is Array or raw is Dictionary:
                var built: Variant = build_numeric_value(expected, numbers_from(raw, OBJECT_FIELDS.get(expected, [])))
                if built != null:
                    return {"value": built}
        TYPE_OBJECT:
            if raw == null:
                return {"value": null}
            if raw is String and raw.begins_with("res://"):
                if not res_path_inside(raw):
                    return {"error": "%s is not a path inside the project: pass the res:// path of a saved resource, without .. segments" % raw}
                if not ResourceLoader.exists(raw):
                    return {"error": "Resource not found: " + raw}
                return {"value": load(raw)}
        TYPE_ARRAY:
            var element: Dictionary = array_element_info(info)
            if element.has("error"):
                return element
            if not element.is_empty():
                # A typed array keeps only elements of its type, so assigning a mismatched array
                # leaves it empty without an error; convert every element or refuse.
                var parsed_items: Dictionary = parse_value_text(raw) if raw is String else {"value": raw}
                if parsed_items.has("error"):
                    return parsed_items
                var items: Variant = parsed_items["value"]
                if not items is Array:
                    return {"error": "expected %s (a JSON array) but got %s (%s)" % [property_type_name(info), type_string(typeof(raw)), describe_value(raw)]}
                return typed_array_from(element, items)
            if raw is Array:
                return {"value": raw}
        TYPE_DICTIONARY:
            var entries: Dictionary = dictionary_entry_info(info)
            if entries.has("error"):
                return entries
            if not entries.is_empty():
                var parsed_entries: Dictionary = parse_value_text(raw) if raw is String else {"value": raw}
                if parsed_entries.has("error"):
                    return parsed_entries
                var items: Variant = parsed_entries["value"]
                if not items is Dictionary:
                    return {"error": "expected %s (a JSON object) but got %s (%s)" % [property_type_name(info), type_string(typeof(raw)), describe_value(raw)]}
                return typed_dictionary_from(entries["key"], entries["value"], items)
            if raw is Dictionary:
                return {"value": raw}
        TYPE_PACKED_BYTE_ARRAY, TYPE_PACKED_INT32_ARRAY, TYPE_PACKED_INT64_ARRAY, TYPE_PACKED_FLOAT32_ARRAY, TYPE_PACKED_FLOAT64_ARRAY, TYPE_PACKED_STRING_ARRAY, TYPE_PACKED_VECTOR2_ARRAY, TYPE_PACKED_VECTOR3_ARRAY, TYPE_PACKED_COLOR_ARRAY, TYPE_PACKED_VECTOR4_ARRAY:
            if raw is Array:
                var packed: Variant = type_convert(raw, expected)
                if typeof(packed) == expected:
                    return {"value": packed}
        _:
            if typeof(raw) == expected:
                return {"value": raw}

    # Anything else may be a Godot literal such as "Transform2D(...)".
    if raw is String:
        var parsed_text: Dictionary = parse_value_text(raw)
        if parsed_text.has("error"):
            return parsed_text
        var parsed: Variant = parsed_text["value"]
        if parsed != null and typeof(parsed) == expected:
            return {"value": parsed}

    return {"error": "expected %s but got %s (%s)" % [property_type_name(info), type_string(typeof(raw)), describe_value(raw)]}

## A value text parsed with str_to_var: {"value"}, or {"error"} when the text uses a constructor
## that is not a built-in type. Parsing Object(...) creates an Object and Resource(...) loads a
## resource (which may carry a script), even inside an array or a dictionary, so such text is
## refused before it is parsed.
static func parse_value_text(raw: String) -> Dictionary:
    if not text_builds_no_object(raw):
        return {"error": "the text %s uses a constructor that is not a built-in type (such as Object(...) or Resource(...)) or a ; comment; no Object is built from text. Pass built-in literals such as Vector3(0, 1, 0), JSON arrays and objects, and res:// paths for resources" % describe_value(raw)}
    return {"value": str_to_var(raw)}

## Whether every constructor in a value text, outside its strings, is a built-in type other than
## Object, so parsing it creates no Object and loads no Resource. The running game keeps a copy in
## runtime/ops/rt_live.gd; tests/version_sync_test.rs checks that the two stay equal.
static func text_builds_no_object(text: String) -> bool:
    var in_string: bool = false
    var escaped: bool = false
    var word: String = ""
    var last_word: String = ""
    for character: String in text:
        if in_string:
            if escaped:
                escaped = false
            elif character == "\\":
                escaped = true
            elif character == "\"":
                in_string = false
            continue
        if character == "_" or (character >= "a" and character <= "z") or (character >= "A" and character <= "Z") or (character >= "0" and character <= "9"):
            word += character
            continue
        if not word.is_empty():
            last_word = word
            word = ""
        if character == "\"":
            in_string = true
        elif character == ";":
            # The parser reads ; outside a string as a comment to the end of the line, and a quote
            # in it would make this scan see a string the parser skips. Godot never writes one.
            return false
        elif character == "(":
            if last_word.is_empty() or last_word == "Object" or not is_builtin_type_name(last_word):
                return false
            last_word = ""
        elif character != " " and character != "\t" and character != "\n" and character != "\r" and character != "[" and character != "]":
            last_word = ""
    return not in_string

## Whether a name is a built-in Variant type other than Object.
static func is_builtin_type_name(type_name: String) -> bool:
    for type: int in range(TYPE_NIL + 1, TYPE_MAX):
        if type != TYPE_OBJECT and type_string(type) == type_name:
            return true
    return false

## Whether a res:// path stays inside the project: no empty path and no .. segment.
static func res_path_inside(path: String) -> bool:
    var normalized: String = path.strip_edges().replace("\\", "/")
    if not normalized.begins_with("res://"):
        return false
    var relative: String = normalized.trim_prefix("res://")
    return not relative.is_empty() and not ".." in relative.split("/")

## Types whose components are floats, compared with their own is_equal_approx.
const APPROX_TYPES: Array[int] = [
    TYPE_VECTOR2, TYPE_VECTOR3, TYPE_VECTOR4, TYPE_RECT2, TYPE_COLOR, TYPE_QUATERNION,
    TYPE_PLANE, TYPE_AABB, TYPE_BASIS, TYPE_TRANSFORM2D, TYPE_TRANSFORM3D,
]

## Whether a value read back is the one that was set. A float property stored in single precision
## reads back slightly off, so floats compare approximately.
static func same_value(read: Variant, wanted: Variant) -> bool:
    var read_type: int = typeof(read)
    var wanted_type: int = typeof(wanted)
    if (read_type == TYPE_FLOAT or wanted_type == TYPE_FLOAT) \
            and (read_type == TYPE_FLOAT or read_type == TYPE_INT) \
            and (wanted_type == TYPE_FLOAT or wanted_type == TYPE_INT):
        return is_equal_approx(float(read), float(wanted))
    if read_type == wanted_type and APPROX_TYPES.has(read_type):
        return read.is_equal_approx(wanted)
    return read == wanted

## Text form of a value in Godot's own syntax, shortened if it is huge. A saved resource is named
## by its path.
static func describe_value(value: Variant) -> String:
    var text: String
    if typeof(value) == TYPE_OBJECT:
        if value == null:
            text = "null"
        elif not is_instance_valid(value):
            text = "<freed object>"
        elif value is Resource and not (value as Resource).resource_path.is_empty():
            text = 'Resource("%s")' % (value as Resource).resource_path
        else:
            text = "<%s>" % value.get_class()
    else:
        text = var_to_str(value)
    if text.length() > MAX_VALUE_CHARS:
        text = text.substr(0, MAX_VALUE_CHARS) + "... (%d more characters)" % (text.length() - MAX_VALUE_CHARS)
    return text

# --- Resource files --------------------------------------------------------------------

const SHADER_PARAMETER_PREFIX: String = "shader_parameter/"

## Properties of a resource that are not its data: setting one would move, re-type or renumber the
## file instead of changing what it holds.
const REFUSED_RESOURCE_PROPERTIES: PackedStringArray = ["resource_path", "script", "resource_scene_unique_id"]

## Whether a loaded resource is one of the classes an Object property's hint names
## (PROPERTY_HINT_RESOURCE_TYPE: "Texture2D" or "Texture2D,CompressedTexture3D").
static func resource_fits_hint(info: Dictionary, value: Variant) -> String:
    if int(info.get("type", TYPE_NIL)) != TYPE_OBJECT or int(info.get("hint", PROPERTY_HINT_NONE)) != PROPERTY_HINT_RESOURCE_TYPE:
        return ""
    if value == null or not value is Object:
        return ""
    var wanted: PackedStringArray = []
    for part: String in str(info.get("hint_string", "")).split(","):
        var name: String = part.strip_edges()
        if not name.is_empty() and not name.begins_with("-"):
            wanted.append(name)
    if wanted.is_empty():
        return ""
    for name: String in wanted:
        if object_is_class(value as Object, name):
            return ""
    return "%s is a %s, not a %s" % [describe_value(value), (value as Object).get_class(), " or ".join(wanted)]

## The uniform names of a ShaderMaterial's shader, for an error message.
static func shader_parameter_names(material: ShaderMaterial) -> PackedStringArray:
    var names: PackedStringArray = []
    for prop: Dictionary in material.get_property_list():
        var name: String = prop["name"]
        if name.begins_with(SHADER_PARAMETER_PREFIX):
            names.append(name.substr(SHADER_PARAMETER_PREFIX.length()))
    return names

## Why a property name of a resource is unknown, naming what it has instead.
static func unknown_resource_property(resource: Resource, name: String) -> String:
    if not name.begins_with(SHADER_PARAMETER_PREFIX):
        return "Unknown property '%s' on %s" % [name, resource.get_class()]
    if not resource is ShaderMaterial:
        return "'%s': a %s has no shader parameters; only a ShaderMaterial does" % [name, resource.get_class()]
    var material: ShaderMaterial = resource
    if material.shader == null:
        return "'%s': the ShaderMaterial has no shader, so it has no shader parameters; set shader first in its own call" % name
    var names: PackedStringArray = shader_parameter_names(material)
    var listed: String = "none" if names.is_empty() else ", ".join(names)
    return "'%s': the shader %s has no uniform '%s' (its uniforms: %s)" % [name, describe_value(material.shader), name.substr(SHADER_PARAMETER_PREFIX.length()), listed]

## Plans setting properties of the resource saved at resource_path (a res:// path of a .tres or
## .res file), without changing anything: loads it (the copy already loaded, when there is one),
## converts every value to the property's current type and sets them on a copy of it, reading each
## one back. A property that adds properties (streams_count adds stream_<n>/stream, any setter that
## calls notify_property_list_changed) is set first, and the names the resource did not have are
## checked again against the copy's refreshed list, so one call sets a count and the entries it
## adds. Returns {"resource": Resource, "values": Dictionary in the order to set them, "before":
## Dictionary that puts the resource back when set in its order} or {"error": String} naming every
## refused property.
static func plan_resource_change(resource_path: String, properties: Dictionary) -> Dictionary:
    if not (resource_path.ends_with(".tres") or resource_path.ends_with(".res")):
        return {"error": "resource_path must name a .tres or .res file: %s" % resource_path}
    if not FileAccess.file_exists(resource_path):
        return {"error": "Resource file not found: %s" % resource_path}
    if properties.is_empty():
        return {"error": "properties is empty: pass at least one property name and its value"}
    var resource: Resource = load(resource_path) as Resource
    if resource == null:
        return {"error": "Godot could not load %s as a resource" % resource_path}
    # The values are tried on a copy, so a refused call never touches the loaded resource (setting
    # a smaller count and back would drop the entries it removed).
    var trial: Resource = resource.duplicate()
    if trial == null:
        return {"error": "Godot could not copy %s to try the values on" % resource_path}
    var original_names: Dictionary = property_infos(resource)
    var values: Dictionary = {}
    var problems: PackedStringArray = []
    var pending: Array = []
    for key: Variant in properties.keys():
        var name: String = str(key)
        if name.contains(":"):
            problems.append("'%s': ':' steps are not supported here; name a property of %s itself" % [name, resource_path])
        elif REFUSED_RESOURCE_PROPERTIES.has(name) or name.begins_with("_"):
            problems.append("Refusing to set '%s': it is not part of the resource's data" % name)
        else:
            pending.append(key)
    # Each round sets the names the copy has now; a round that sets none ends the search.
    while not pending.is_empty():
        var known: Dictionary = property_infos(trial)
        var waiting: Array = []
        var progressed: bool = false
        for key: Variant in pending:
            var name: String = str(key)
            if not known.has(name):
                waiting.append(key)
                continue
            progressed = true
            var info: Dictionary = known[name]
            var result: Dictionary = convert_property_value(info, properties[key])
            if result.has("error"):
                problems.append("%s: %s" % [name, result["error"]])
                continue
            var misfit: String = resource_fits_hint(info, result["value"])
            if not misfit.is_empty():
                problems.append("%s: %s" % [name, misfit])
                continue
            values[name] = result["value"]
            trial.set(name, result["value"])
        if not progressed:
            for key: Variant in waiting:
                problems.append(unknown_resource_property(trial, str(key)))
            break
        pending = waiting
    if not problems.is_empty():
        return {"error": "No properties were changed. " + "; ".join(problems)}
    var kept: PackedStringArray = []
    for name: String in values:
        var read: Variant = trial.get(name)
        if not same_value(read, values[name]):
            kept.append("%s reads back %s after being set to %s: Godot's setter clamped, rounded or refused it" % [name, describe_value(read), describe_value(values[name])])
    if not kept.is_empty():
        return {"error": "No properties were changed. " + "; ".join(kept)}
    var before: Dictionary = {}
    if property_infos(trial).keys() != original_names.keys():
        # The change adds or removes properties: putting back only the changed names would lose the
        # entries a smaller count removes, so keep every property, in the order Godot loads them.
        for name: String in original_names:
            var usage: int = int(original_names[name].get("usage", 0))
            if usage & (PROPERTY_USAGE_STORAGE | PROPERTY_USAGE_EDITOR) and not REFUSED_RESOURCE_PROPERTIES.has(name):
                before[name] = resource.get(name)
    for name: String in values:
        if not before.has(name):
            before[name] = resource.get(name) if original_names.has(name) else null
    return {"resource": resource, "values": values, "before": before}

## Each property of an object by name, in the order of its property list.
static func property_infos(object: Object) -> Dictionary:
    var infos: Dictionary = {}
    for prop: Dictionary in object.get_property_list():
        infos[prop["name"]] = prop
    return infos

## Sets the values on the resource and saves it to its own file. On a failed save the old values
## are put back. Returns the save's error.
static func store_resource_values(resource: Resource, values: Dictionary, before: Dictionary) -> Error:
    for name: String in values:
        resource.set(name, values[name])
    var saved: Error = ResourceSaver.save(resource)
    if saved != OK:
        for name: String in before:
            resource.set(name, before[name])
    return saved

## Each changed property's value before and after, as Godot's text forms. A shader parameter the
## material did not set before reads as null: the shader's own default applied.
static func resource_change_report(resource: Resource, values: Dictionary, before: Dictionary) -> Dictionary:
    var changed: Dictionary = {}
    for name: String in values:
        changed[name] = {"before": describe_value(before[name]), "after": describe_value(resource.get(name))}
    return changed
