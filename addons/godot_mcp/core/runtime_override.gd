@tool
extends RefCounted
## Writes a temporary autoload into res://override.cfg before the editor runs the project,
## then restores the user's file. The marker matches the Rust server so either side can
## clean up after a crash.

const MARKER := "; godot_mcp: temporary runtime autoload (removed automatically)"
const AUTOLOAD_NAME := "GodotMcpRuntime"
const RUNTIME_SCRIPT := "res://addons/godot_mcp/runtime/mcp_runtime.gd"
const OVERRIDE_PATH := "res://override.cfg"
const BACKUP_PATH := "res://.godot/godot_mcp/override.cfg.user-backup"


static func inject() -> int:
    restore_if_stale()
    if not FileAccess.file_exists(RUNTIME_SCRIPT):
        return ERR_FILE_NOT_FOUND
    var original := ""
    if FileAccess.file_exists(OVERRIDE_PATH):
        original = FileAccess.get_file_as_string(OVERRIDE_PATH)
        DirAccess.make_dir_recursive_absolute(ProjectSettings.globalize_path(BACKUP_PATH.get_base_dir()))
        var backup := FileAccess.open(BACKUP_PATH, FileAccess.WRITE)
        if backup == null:
            return FileAccess.get_open_error()
        backup.store_string(original)
        backup.close()
    var ours := "%s\n[autoload]\n\n%s=\"%s\"\n" % [MARKER, AUTOLOAD_NAME, RUNTIME_SCRIPT]
    var file := FileAccess.open(OVERRIDE_PATH, FileAccess.WRITE)
    if file == null:
        return FileAccess.get_open_error()
    if original.is_empty():
        file.store_string(ours)
    else:
        file.store_string(original.strip_edges(false, true) + "\n\n" + ours)
    file.close()
    return OK


static func restore() -> void:
    var backup_abs := ProjectSettings.globalize_path(BACKUP_PATH)
    var override_abs := ProjectSettings.globalize_path(OVERRIDE_PATH)
    if FileAccess.file_exists(BACKUP_PATH):
        DirAccess.remove_absolute(override_abs)
        DirAccess.rename_absolute(backup_abs, override_abs)
    elif FileAccess.file_exists(OVERRIDE_PATH) and FileAccess.get_file_as_string(OVERRIDE_PATH).contains(MARKER):
        DirAccess.remove_absolute(override_abs)


static func restore_if_stale() -> void:
    if FileAccess.file_exists(OVERRIDE_PATH) and FileAccess.get_file_as_string(OVERRIDE_PATH).contains(MARKER):
        restore()
