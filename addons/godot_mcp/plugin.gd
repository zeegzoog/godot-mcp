@tool
extends EditorPlugin

const MIN_MAJOR: int = 4
const MIN_MINOR: int = 8
const BRIDGE_SCRIPT_PATH: String = "res://addons/godot_mcp/bridge_server.gd"
const EXPORT_PLUGIN_PATH: String = "res://addons/godot_mcp/export_plugin.gd"

var bridge_node: Node = null
var export_plugin: EditorExportPlugin = null

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
    print("[Godot MCP] Plugin activated successfully")

func _exit_tree() -> void:
    if export_plugin != null:
        remove_export_plugin(export_plugin)
        export_plugin = null
    if bridge_node != null:
        bridge_node.call("stop_server")
        bridge_node.queue_free()
        bridge_node = null
        print("[Godot MCP] Plugin deactivated")
