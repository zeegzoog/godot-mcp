@tool
extends EditorPlugin

const MIN_MAJOR: int = 4
const MIN_MINOR: int = 8
const BRIDGE_SCRIPT_PATH: String = "res://addons/godot_mcp/bridge_server.gd"
const EXPORT_PLUGIN_PATH: String = "res://addons/godot_mcp/export_plugin.gd"
const RELAY_SCRIPT_PATH: String = "res://addons/godot_mcp/editor/game_relay.gd"
const OVERRIDE_SCRIPT_PATH: String = "res://addons/godot_mcp/core/runtime_override.gd"

var bridge_node: Node = null
var export_plugin: EditorExportPlugin = null
var game_relay: EditorDebuggerPlugin = null

func _enter_tree() -> void:
    var version: Dictionary = Engine.get_version_info()
    var major: int = version["major"]
    var minor: int = version["minor"]
    # Each godot-mcp release is built for one Godot minor version: an older editor may lack the
    # APIs this build calls, a newer one usually still works but has a better-matching release.
    if major < MIN_MAJOR or (major == MIN_MAJOR and minor < MIN_MINOR):
        printerr("[Godot MCP] This godot-mcp build is for Godot %d.%d, but this is Godot %s. Install the godot-mcp release for your Godot version. The bridge was not started." % [MIN_MAJOR, MIN_MINOR, version["string"]])
        return
    if major > MIN_MAJOR or minor > MIN_MINOR:
        push_warning("[Godot MCP] This godot-mcp build is for Godot %d.%d, but this is Godot %s. Install the godot-mcp release for Godot %d.%d." % [MIN_MAJOR, MIN_MINOR, version["string"], major, minor])

    # Registered before the bridge so that the addon stays out of exports even when the
    # bridge fails to start.
    var export_script: GDScript = load(EXPORT_PLUGIN_PATH)
    if export_script == null or not export_script.can_instantiate():
        printerr("[Godot MCP] Failed to load ", EXPORT_PLUGIN_PATH, ". The addon will be included in exported games.")
    else:
        export_plugin = export_script.new()
        add_export_plugin(export_plugin)

    # Loaded at runtime (not through its class_name) so that a script which fails to compile
    # reports a clear message instead of breaking this plugin as well.
    var bridge_script: GDScript = load(BRIDGE_SCRIPT_PATH)
    if bridge_script == null or not bridge_script.can_instantiate():
        printerr("[Godot MCP] Failed to load ", BRIDGE_SCRIPT_PATH, ". Check the Output panel for script errors.")
        return

    print("[Godot MCP] Initializing Godot MCP plugin...")
    bridge_node = bridge_script.new()
    bridge_node.name = "GodotMcpBridgeServer"
    bridge_node.set("undo_redo", get_undo_redo())
    add_child(bridge_node)
    _start_game_relay()
    print("[Godot MCP] Plugin activated successfully")

## Called by the editor immediately before Play. Injects the runtime autoload, then the
## debugger relay removes it once the game has read override.cfg.
## `godot_mcp/runtime_on_play` = false leaves Play alone.
func _build() -> bool:
    if not bool(ProjectSettings.get_setting("godot_mcp/runtime_on_play", true)):
        return true
    var script: GDScript = load(OVERRIDE_SCRIPT_PATH)
    if script != null:
        script.call("inject")
    return true

func _start_game_relay() -> void:
    var script: GDScript = load(RELAY_SCRIPT_PATH)
    if script == null or not script.can_instantiate():
        return
    game_relay = script.new()
    game_relay.broadcast = Callable(bridge_node, "broadcast_event")
    bridge_node.set("game_relay", game_relay)
    add_debugger_plugin(game_relay)

func _exit_tree() -> void:
    if game_relay != null:
        remove_debugger_plugin(game_relay)
        game_relay = null
    var override_script: GDScript = load(OVERRIDE_SCRIPT_PATH)
    if override_script != null:
        override_script.call("restore")
    if export_plugin != null:
        remove_export_plugin(export_plugin)
        export_plugin = null
    if bridge_node != null:
        bridge_node.call("stop_server")
        bridge_node.queue_free()
        bridge_node = null
        print("[Godot MCP] Plugin deactivated")
