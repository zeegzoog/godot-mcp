extends RefCounted
## Captures print(), push_warning() and push_error() while a debugger is attached.
## Godot 4.5+ OS.add_logger. The buffer is capped so a chatty game cannot grow without bound.

const R := preload("rt_result.gd")
const MAX_LINES := 2000
const MAX_TEXT := 2000

static var _logger: OutputLogger = null
static var _pending_scene: Dictionary = {}


static func methods() -> Dictionary:
    return {"rt.output": {"fn": output}}


## The number the next logged line will have. Lines logged after this call come back from
## `lines_since` with this number; -1 when output capture is not active.
static func mark() -> int:
    return _logger.next_seq() if _logger != null else -1


## The lines logged since `first_seq` (a number from `mark`), oldest first, each `{level, text, stack,
## script}`; `script` is true for an error the engine raised in a script, as against one a script
## pushed. Empty when output capture is not active.
static func lines_since(first_seq: int) -> Array:
    return _logger.entries_since(first_seq) if _logger != null else []


static func note_scene(context: Dictionary) -> void:
    _pending_scene = context.duplicate(true)
    if _logger != null:
        _logger.set_scene(_pending_scene)


static func install() -> void:
    if _logger != null:
        return
    _logger = OutputLogger.new()
    if not _pending_scene.is_empty():
        _logger.set_scene(_pending_scene)
    OS.add_logger(_logger)


static func uninstall() -> void:
    if _logger == null:
        return
    OS.remove_logger(_logger)
    _logger = null


static func output(_rt: Node, params: Dictionary) -> Dictionary:
    if _logger == null:
        return R.err(R.UNSUPPORTED, "Output capture is not active.")
    var since := int(params.get("since", 0))
    var limit := clampi(int(params.get("limit", 200)), 1, MAX_LINES)
    var level := str(params.get("level", "all"))
    var snap: Dictionary = _logger.snapshot()
    var page: Array = []
    for entry: Dictionary in snap["entries"]:
        if int(entry["seq"]) < since:
            continue
        if level == "error" and entry["level"] != "error":
            continue
        if level == "warning" and entry["level"] == "info":
            continue
        entry.erase("script")
        page.append(entry)
        if page.size() >= limit:
            break
    var next_seq := since
    if not page.is_empty():
        next_seq = int(page.back()["seq"]) + 1
    # Every line's stamp carries the same long explanation of script_sha256; say it once.
    var note: Variant = null
    for entry: Dictionary in page:
        var stamp: Dictionary = entry.get("scene", {})
        if stamp.has("script_sha256_note"):
            note = stamp["script_sha256_note"]
            stamp.erase("script_sha256_note")
    var answer := {
        "lines": page,
        "next": next_seq,
        "error_count": snap["error_count"],
        "warning_count": snap["warning_count"],
    }
    if note != null:
        answer["script_sha256_note"] = note
    return R.ok(answer)


class OutputLogger extends Logger:
    var _mutex := Mutex.new()
    var _entries: Array = []
    var _seq := 0
    var _errors := 0
    var _warnings := 0
    var _scene: Dictionary = {}

    func set_scene(context: Dictionary) -> void:
        _mutex.lock()
        _scene = context.duplicate(true)
        _mutex.unlock()

    func _log_message(message: String, error: bool) -> void:
        _add("info", ("[stderr] " if error else "") + message.trim_suffix("\n"), [])

    func _log_error(function: String, file: String, line: int, code: String, rationale: String, _editor_notify: bool, error_type: int, script_backtraces: Array[ScriptBacktrace]) -> void:
        var warning := error_type == ERROR_TYPE_WARNING
        var text := rationale if not rationale.is_empty() else code
        if not rationale.is_empty() and not code.is_empty() and code != rationale:
            text = "%s: %s" % [code, rationale]
        var frames: Array = []
        for backtrace: ScriptBacktrace in script_backtraces:
            for index in backtrace.get_frame_count():
                frames.append("%s:%d in %s()" % [backtrace.get_frame_file(index), backtrace.get_frame_line(index), backtrace.get_frame_function(index)])
        if frames.is_empty() and not file.is_empty():
            frames.append("%s:%d in %s()" % [file, line, function])
        _add("warning" if warning else "error", text, frames, error_type == ERROR_TYPE_SCRIPT)

    func _add(level: String, text: String, frames: Array, script: bool = false) -> void:
        if text.length() > MAX_TEXT:
            text = text.substr(0, MAX_TEXT)
        _mutex.lock()
        # Copy the stamp from this moment. A later scene must not rewrite an old line.
        _entries.append({
            "seq": _seq,
            "level": level,
            "text": text,
            "stack": frames,
            "script": script,
            "scene": _scene.duplicate(true),
        })
        _seq += 1
        if level == "error":
            _errors += 1
        elif level == "warning":
            _warnings += 1
        while _entries.size() > MAX_LINES:
            _entries.pop_front()
        _mutex.unlock()

    func next_seq() -> int:
        _mutex.lock()
        var next := _seq
        _mutex.unlock()
        return next

    func entries_since(first_seq: int) -> Array:
        var found: Array = []
        _mutex.lock()
        for index in range(_entries.size() - 1, -1, -1):
            var entry: Dictionary = _entries[index]
            if int(entry["seq"]) < first_seq:
                break
            found.append({
                "level": entry["level"],
                "text": entry["text"],
                "stack": (entry["stack"] as Array).duplicate(),
                "script": entry["script"],
            })
        _mutex.unlock()
        found.reverse()
        return found

    func snapshot() -> Dictionary:
        _mutex.lock()
        var copy := _entries.duplicate(true)
        var errors := _errors
        var warnings := _warnings
        _mutex.unlock()
        return {"entries": copy, "error_count": errors, "warning_count": warnings}
