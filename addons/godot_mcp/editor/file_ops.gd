@tool
extends RefCounted
## Deletes project files the way the FileSystem dock does: the open scene and script tabs of a
## file are closed, the file leaves the editor's resource cache and file system, and its
## companions (.uid, .import and the imported data in .godot/imported) go with it. Loaded by path
## from bridge_server.gd.
##
## Nothing is deleted while any requested path is refused, so a call either deletes everything it
## names or nothing. A file that another project file still names by path or UID is refused unless
## the caller passes force.

## Text files whose content is searched for a path or UID of a file being deleted.
const TEXT_EXTENSIONS: PackedStringArray = [
    "tscn", "tres", "gd", "gdshader", "gdshaderinc", "godot", "cfg", "import", "cs", "json",
]
## Binary resources, asked for their dependencies instead of being searched.
const BINARY_EXTENSIONS: PackedStringArray = ["scn", "res"]
const MAX_SCAN_BYTES: int = 16777216
const MAX_PATHS: int = 64
## Referencing files listed per deleted file; the count is always complete.
const MAX_LISTED_REFERENCES: int = 50
const BRIDGE_ADDON: String = "res://addons/godot_mcp/"
const IMPORTED_PREFIX: String = "res://.godot/imported/"
## Characters that continue a path or UID, so "res://a.gd" is not found inside "res://a.gdshader".
const PATH_CHARS: String = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789_-./~"

## params: paths (Array), force, allow_empty_dir, dry_run, trash (bools). bridge is the bridge
## node, for its path, scene and script helpers. Returns the report; "refused" is non-empty when nothing
## was deleted because of a refusal.
func delete(bridge: Object, params: Dictionary) -> Dictionary:
    var requested: Array = params.get("paths", [])
    var force: bool = bool(params.get("force", false))
    var allow_dir: bool = bool(params.get("allow_empty_dir", false))
    var dry_run: bool = bool(params.get("dry_run", false))
    var trash: bool = bool(params.get("trash", true))
    var report: Dictionary = {
        "source": "editor",
        "dry_run": dry_run,
        "forced": force,
        "deleted": [],
        "refused": [],
        "references": [],
        "closed_tabs": [],
        "warnings": [],
    }
    if requested.is_empty():
        return {"error": "paths is empty. Pass one or more res:// paths of files to delete."}
    if requested.size() > MAX_PATHS:
        return {"error": "At most %d paths per call." % MAX_PATHS}

    var targets: Array[Dictionary] = []
    var seen: Dictionary = {}
    for item: Variant in requested:
        var res_path: String = str(bridge.call("to_res_path", str(item))) if item is String else ""
        if res_path.is_empty():
            _refuse(report, str(item), "not a path inside the project")
            continue
        res_path = res_path.trim_suffix("/")
        if seen.has(res_path):
            continue
        seen[res_path] = true
        var reason: String = _protected(res_path)
        if reason.is_empty():
            reason = _companion_owner(res_path)
        if not reason.is_empty():
            _refuse(report, res_path, reason)
            continue
        if DirAccess.dir_exists_absolute(ProjectSettings.globalize_path(res_path)):
            if not allow_dir:
                _refuse(report, res_path, "is a directory. Only an empty directory can be deleted, with allow_empty_dir: true; delete its files first")
            elif not _is_empty_dir(res_path):
                _refuse(report, res_path, "is a directory that is not empty; delete its files first")
            else:
                targets.append({"path": res_path, "kind": "directory"})
            continue
        if not FileAccess.file_exists(res_path):
            _refuse(report, res_path, "does not exist")
            continue
        targets.append(_describe_file(res_path))

    _find_references(targets, report)
    for target: Dictionary in targets:
        var users: Array = target.get("referenced_by", [])
        if users.is_empty():
            continue
        report["references"].append({"path": target["path"], "uid": target.get("uid", ""), "count": users.size(), "referenced_by": users.slice(0, MAX_LISTED_REFERENCES)})
        if not force:
            var names: PackedStringArray = PackedStringArray()
            for user: Dictionary in users.slice(0, 5):
                names.append(str(user["file"]))
            var more: String = "" if users.size() <= 5 else " and %d more" % (users.size() - 5)
            _refuse(report, str(target["path"]), "is still referenced by %d file(s): %s%s. Remove the references first, or pass force: true to delete it anyway and leave them broken" % [users.size(), ", ".join(names), more])

    var scene_tabs: Array[String] = []
    var script_tabs: Array[String] = []
    var open_scenes: PackedStringArray = EditorInterface.get_open_scenes()
    for target: Dictionary in targets:
        var res_path: String = str(target["path"])
        if open_scenes.has(res_path):
            if bool(bridge.call("_has_unsaved_changes", res_path)):
                _refuse(report, res_path, "is open in the editor with unsaved changes. Save or close the scene first")
            else:
                scene_tabs.append(res_path)
        if res_path.get_extension() == "gd":
            var tab: Dictionary = _tab_state(bridge, res_path)
            if bool(tab.get("dirty", false)):
                _refuse(report, res_path, "is open in the script editor with unsaved changes. Save or close the script first")
            elif bool(tab.get("open", false)):
                script_tabs.append(res_path)
            elif str(tab.get("editor_state", "")) == "unknown":
                report["warnings"].append("The script editor's tabs could not be matched to %s (%s); a tab of it may stay open." % [res_path, str(tab.get("reason", ""))])

    report["would_delete"] = _listing(targets)
    if not report["refused"].is_empty() or dry_run:
        if dry_run:
            report["would_close_tabs"] = scene_tabs + script_tabs
        return report
    report.erase("would_delete")

    var previous_scene: String = ""
    var edited: Node = EditorInterface.get_edited_scene_root()
    if edited != null:
        previous_scene = edited.scene_file_path
    for res_path: String in scene_tabs:
        EditorInterface.open_scene_from_path(res_path)
        var root: Node = EditorInterface.get_edited_scene_root()
        if root != null and root.scene_file_path == res_path and EditorInterface.close_scene() == OK:
            report["closed_tabs"].append(res_path)
        else:
            report["warnings"].append("The scene tab of %s could not be closed; close it by hand." % res_path)
    if not previous_scene.is_empty() and not scene_tabs.has(previous_scene) and EditorInterface.get_open_scenes().has(previous_scene):
        EditorInterface.open_scene_from_path(previous_scene)

    var fs: EditorFileSystem = EditorInterface.get_resource_filesystem()
    var dock: Object = EditorInterface.get_file_system_dock()
    var removed_dir: bool = false
    for target: Dictionary in targets:
        var res_path: String = str(target["path"])
        if str(target["kind"]) == "directory":
            var dir_err: Error = _remove(res_path, trash)
            if dir_err != OK:
                return _stopped(report, res_path, dir_err, trash)
            removed_dir = true
            report["deleted"].append({"path": res_path, "kind": "directory", "removed_by": "trash" if trash else "delete", "companions": []})
            if dock != null and dock.has_signal("folder_removed"):
                dock.emit_signal("folder_removed", res_path + "/")
            continue
        # The editor's own delete clears the path of a cached copy, so nothing saves it back.
        if ResourceLoader.has_cached(res_path):
            var cached: Resource = ResourceLoader.get_cached_ref(res_path)
            if cached != null:
                cached.resource_path = ""
        var err: Error = _remove(res_path, trash)
        if err != OK:
            return _stopped(report, res_path, err, trash)
        var companions: Array[String] = []
        for companion: String in target["companions"]:
            if _remove(companion, trash) == OK:
                companions.append(companion)
            else:
                report["warnings"].append("%s could not be removed." % companion)
        for imported: String in target["imported"]:
            if _remove(imported, false) == OK:
                companions.append(imported)
        report["deleted"].append({"path": res_path, "kind": "file", "removed_by": "trash" if trash else "delete", "companions": companions})
        fs.update_file(res_path)
        if dock != null and dock.has_signal("file_removed"):
            dock.emit_signal("file_removed", res_path)
        if script_tabs.has(res_path) and not bool(_tab_state(bridge, res_path).get("open", false)):
            report["closed_tabs"].append(res_path)
        elif script_tabs.has(res_path):
            report["warnings"].append("The script tab of %s is still open; close it by hand." % res_path)
    if removed_dir:
        fs.scan()
    report["rescanned"] = true
    return report

func _refuse(report: Dictionary, res_path: String, reason: String) -> void:
    report["refused"].append({"path": res_path, "reason": reason})

## Why a path may never be deleted through this call, or "".
func _protected(path: String) -> String:
    # Windows and macOS file systems ignore case, and every file system reads "./" and "//" as
    # nothing, so res://Addons//godot_mcp is the addon there.
    var res_path: String = path.simplify_path().to_lower()
    if _is_alias(res_path):
        return "is spelled in a way a file system may read as another file (a trailing dot or space, ':' or '~1'); use the plain name"
    if res_path == "res://project.godot":
        return "is the project file itself"
    if res_path == "res://.godot" or res_path.begins_with("res://.godot/"):
        return "is in the editor's .godot cache, which Godot manages itself"
    if res_path + "/" == BRIDGE_ADDON or res_path.begins_with(BRIDGE_ADDON):
        return "belongs to the Godot MCP Bridge addon this call runs through; disable the plugin and remove it by hand"
    return ""

## True when a segment of the path is one some file systems read as another file: a trailing dot
## or space, a stream (":") or a short name ("~1"). The comparisons in _protected cannot see
## through those.
func _is_alias(res_path: String) -> bool:
    for segment: String in res_path.trim_prefix("res://").split("/"):
        if segment.ends_with(".") or segment.ends_with(" ") or segment.contains(":"):
            return true
        var tilde: int = segment.find("~")
        while tilde != -1:
            if tilde + 1 < segment.length() and segment[tilde + 1].is_valid_int():
                return true
            tilde = segment.find("~", tilde + 1)
    return false

## Refuses a .uid or .import file whose own file still exists: it goes with that file.
func _companion_owner(res_path: String) -> String:
    for suffix: String in [".uid", ".import"]:
        if res_path.ends_with(suffix):
            var owner_path: String = res_path.trim_suffix(suffix)
            if FileAccess.file_exists(owner_path):
                return "belongs to %s and is removed with it; delete %s instead" % [owner_path, owner_path]
    return ""

func _is_empty_dir(res_path: String) -> bool:
    var dir: DirAccess = DirAccess.open(res_path)
    if dir == null:
        return false
    dir.include_hidden = true
    return dir.get_files().is_empty() and dir.get_directories().is_empty()

## A file target: its UID, its companion files and the imported data its .import names.
func _describe_file(res_path: String) -> Dictionary:
    var uid: String = ""
    var id: int = ResourceLoader.get_resource_uid(res_path)
    if id != ResourceUID.INVALID_ID:
        uid = ResourceUID.id_to_text(id)
    var companions: Array[String] = []
    var imported: Array[String] = []
    if FileAccess.file_exists(res_path + ".uid"):
        companions.append(res_path + ".uid")
        if uid.is_empty():
            uid = FileAccess.get_file_as_string(res_path + ".uid").strip_edges()
    if FileAccess.file_exists(res_path + ".import"):
        companions.append(res_path + ".import")
        var text: String = FileAccess.get_file_as_string(res_path + ".import")
        for part: String in text.split("\""):
            if not part.begins_with(IMPORTED_PREFIX) or ".." in part:
                continue
            # The importer also keeps <name>-<hash>.md5 beside the data it lists.
            for item: String in [part, part.get_basename() + ".md5"]:
                if FileAccess.file_exists(item) and not imported.has(item):
                    imported.append(item)
    return {"path": res_path, "kind": "file", "uid": uid, "companions": companions, "imported": imported, "referenced_by": []}

## Fills referenced_by of every file target with the project files that name it by path or UID.
## The targets and their companions do not count. The .godot cache, hidden folders and folders
## with a .gdignore are not searched, as Godot does not load them.
func _find_references(targets: Array[Dictionary], report: Dictionary) -> void:
    var excluded: Dictionary = {}
    var files: Array[Dictionary] = []
    for target: Dictionary in targets:
        excluded[target["path"]] = true
        if str(target["kind"]) == "file":
            files.append(target)
            for companion: String in target["companions"]:
                excluded[companion] = true
    if files.is_empty():
        return
    var candidates: Array[String] = []
    _collect("res://", candidates)
    var skipped_large: int = 0
    for candidate: String in candidates:
        if excluded.has(candidate):
            continue
        var extension: String = candidate.get_extension().to_lower()
        if extension in BINARY_EXTENSIONS:
            var parts: Array[String] = []
            for dependency: String in ResourceLoader.get_dependencies(candidate):
                for part: String in dependency.split("::"):
                    parts.append(part)
            for target: Dictionary in files:
                var by: Array[String] = []
                if parts.has(target["path"]):
                    by.append("path")
                if not str(target["uid"]).is_empty() and parts.has(target["uid"]):
                    by.append("uid")
                if not by.is_empty():
                    target["referenced_by"].append({"file": candidate, "by": by})
            continue
        if not extension in TEXT_EXTENSIONS:
            continue
        var file: FileAccess = FileAccess.open(candidate, FileAccess.READ)
        if file == null:
            continue
        if file.get_length() > MAX_SCAN_BYTES:
            skipped_large += 1
            continue
        var text: String = file.get_as_text()
        for target: Dictionary in files:
            var by: Array[String] = []
            if mentions(text, str(target["path"])):
                by.append("path")
            if not str(target["uid"]).is_empty() and mentions(text, str(target["uid"])):
                by.append("uid")
            if not by.is_empty():
                target["referenced_by"].append({"file": candidate, "by": by})
    if skipped_large > 0:
        report["warnings"].append("%d text file(s) larger than %d bytes were not searched for references." % [skipped_large, MAX_SCAN_BYTES])

func _collect(folder: String, into: Array[String]) -> void:
    var dir: DirAccess = DirAccess.open(folder)
    if dir == null:
        return
    dir.include_hidden = false
    for name: String in dir.get_files():
        into.append(folder.path_join(name))
    for name: String in dir.get_directories():
        if name.begins_with("."):
            continue
        var child: String = folder.path_join(name)
        if FileAccess.file_exists(child.path_join(".gdignore")):
            continue
        _collect(child, into)

## True when text holds needle not followed by a character that would continue it.
func mentions(text: String, needle: String) -> bool:
    var at: int = text.find(needle)
    while at != -1:
        var after: int = at + needle.length()
        if after >= text.length() or not PATH_CHARS.contains(text[after]):
            return true
        at = text.find(needle, at + 1)
    return false

## The script tab state script_ops reports for res_path.
func _tab_state(bridge: Object, res_path: String) -> Dictionary:
    var ops: Object = bridge.call("_script_ops")
    return ops.call("tab_state", res_path, false)

func _remove(res_path: String, trash: bool) -> Error:
    var absolute: String = ProjectSettings.globalize_path(res_path)
    if trash:
        return OS.move_to_trash(absolute)
    return DirAccess.remove_absolute(absolute)

## The report after a removal failed: what was deleted before it, and the failure as a refusal.
func _stopped(report: Dictionary, res_path: String, err: Error, trash: bool) -> Dictionary:
    var how: String = "moved to the trash (pass trash: false to delete it permanently)" if trash else "deleted"
    _refuse(report, res_path, "could not be %s: %s. The files listed under deleted were removed before this one" % [how, error_string(err)])
    EditorInterface.get_resource_filesystem().scan()
    report["rescanned"] = true
    return report

func _listing(targets: Array[Dictionary]) -> Array:
    var listing: Array = []
    for target: Dictionary in targets:
        var companions: Array = []
        companions.append_array(target.get("companions", []))
        companions.append_array(target.get("imported", []))
        listing.append({"path": target["path"], "kind": target["kind"], "companions": companions})
    return listing
