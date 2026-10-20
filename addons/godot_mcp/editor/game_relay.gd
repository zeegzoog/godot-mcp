@tool
extends EditorDebuggerPlugin
## Relays mcp:* debugger messages between a game started from the editor and the MCP server.
## The editor's own debugger keeps every other prefix.

var broadcast: Callable
var _hello: Dictionary = {}


func _has_capture(prefix: String) -> bool:
    return prefix == "mcp"


func _capture(message: String, data: Array, session_id: int) -> bool:
    if message == "mcp:hello" and not data.is_empty():
        _hello[session_id] = str(data[0])
    if broadcast.is_valid():
        broadcast.call("game.message", {"session": session_id, "message": message, "data": data})
    return true


func _setup_session(session_id: int) -> void:
    var session := get_session(session_id)
    session.started.connect(_on_started.bind(session_id))
    session.stopped.connect(_on_stopped.bind(session_id))


func _on_started(session_id: int) -> void:
    var override_script: GDScript = load("res://addons/godot_mcp/core/runtime_override.gd")
    if override_script != null:
        override_script.call("restore")
    if broadcast.is_valid():
        broadcast.call("game.started", {"session": session_id})


func _on_stopped(session_id: int) -> void:
    _hello.erase(session_id)
    if broadcast.is_valid():
        broadcast.call("game.stopped", {"session": session_id})


func send(session_id: int, command: String, data: Array) -> Dictionary:
    var session := _session(session_id)
    if session == null:
        return {"error": "No game started from the editor is running."}
    session.send_message(command, data)
    return {"result": {"sent": true}}


func status() -> Dictionary:
    var sessions: Array = []
    var all: Array = get_sessions()
    for index in range(all.size() - 1, -1, -1):
        var session: EditorDebuggerSession = all[index]
        if session == null or not session.is_active():
            continue
        sessions.append({"session": index, "breaked": session.is_breaked(), "hello": _hello.get(index, "")})
    return {"sessions": sessions}


func _session(session_id: int) -> EditorDebuggerSession:
    var all: Array = get_sessions()
    if session_id >= 0 and session_id < all.size():
        var session: EditorDebuggerSession = all[session_id]
        if session != null and session.is_active():
            return session
        return null
    for index in range(all.size() - 1, -1, -1):
        var session: EditorDebuggerSession = all[index]
        if session != null and session.is_active():
            return session
    return null
