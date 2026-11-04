@tool
extends RefCounted
## Deletes project files the way the FileSystem dock does: the open scene and script tabs of a
## file are closed, the file leaves the editor's resource cache and file system, and its
## companions (.uid, .import and the imported data in .godot/imported) go with it. Loaded by path
## from bridge_server.gd.
##
## Nothing is deleted while any requested path is refused, so a call either deletes everything it
## names or nothing. A file that another project file still names by path or UID is refused unless
## the caller passes force. A script that only writes the file (a generator saving its output)
## does not count: it can create the file again (classify_script).

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

## How a .gd file names a path: "written" when every place the path reaches in its code writes
## the file (so the script can create it again), "used" otherwise. The server classifies the same
## way (src/tools/reference_kind.rs); tests/script-reference-vectors.json holds the shared cases.
const KIND_WRITTEN: String = "written"
const KIND_USED: String = "used"
## How far a path is followed through constants and variables that hold it.
const MAX_CARRIERS: int = 4
## Calls that write the file named by an argument: [receiver ("" for any), method, argument].
const WRITES: Array = [
    ["ResourceSaver", "save", 1],
    ["DirAccess", "remove_absolute", 0],
    ["DirAccess", "copy_absolute", 1],
    ["DirAccess", "rename_absolute", 1],
    ["OS", "move_to_trash", 0],
    ["ProjectSettings", "save_custom", 0],
    ["", "save_png", 0],
    ["", "save_jpg", 0],
    ["", "save_webp", 0],
    ["", "save_exr", 0],
]
## FileAccess openers; they write when their second argument is one of WRITE_MODES.
const OPENERS: PackedStringArray = ["open", "open_compressed", "open_encrypted", "open_encrypted_with_pass"]
const WRITE_MODES: PackedStringArray = ["FileAccess.WRITE", "FileAccess.WRITE_READ"]
## Calls that only look at the path: [receiver ("" for any), method].
const LOOKS: Array = [
    ["FileAccess", "file_exists"], ["ResourceLoader", "exists"], ["", "print"], ["", "prints"],
    ["", "printt"], ["", "printerr"], ["", "print_rich"], ["", "print_verbose"],
    ["", "push_warning"], ["", "push_error"],
]
## Calls that only look at the folder of the path (path.get_base_dir()).
const FOLDER_LOOKS: Array = [
    ["DirAccess", "make_dir_recursive_absolute"], ["DirAccess", "make_dir_absolute"],
    ["DirAccess", "dir_exists_absolute"],
]
## Calls that hand back the same path in another form.
const SAME_PATH: Array = [["ProjectSettings", "globalize_path"], ["ProjectSettings", "localize_path"]]
const TOKEN_WORD: int = 0
const TOKEN_TEXT: int = 1
const TOKEN_MARK: int = 2
const TOKEN_NEWLINE: int = 3
const ROLE_WRITE: int = 0
const ROLE_LOOK: int = 1
const ROLE_USE: int = 2
const WORD_CHARS: String = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789_"

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
        var used: PackedStringArray = PackedStringArray()
        var writers: PackedStringArray = PackedStringArray()
        for user: Dictionary in users:
            if str(user["kind"]) == KIND_USED:
                used.append(str(user["file"]))
            else:
                writers.append(str(user["file"]))
        report["references"].append({"path": target["path"], "uid": target.get("uid", ""), "count": users.size(), "used": used.size(), "written": writers.size(), "referenced_by": users.slice(0, MAX_LISTED_REFERENCES)})
        if used.is_empty():
            report["warnings"].append("%s is named only as a file that %d script(s) write: %s%s. They can create it again, so this does not stop the deletion; run the generator to rebuild it." % [target["path"], writers.size(), ", ".join(writers.slice(0, 5)), _more(writers.size())])
        elif not force:
            _refuse(report, str(target["path"]), "is still used by %d file(s): %s%s. Remove the references first, or pass force: true to delete it anyway and leave them broken" % [used.size(), ", ".join(used.slice(0, 5)), _more(used.size())])

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

func _more(count: int) -> String:
    return "" if count <= 5 else " and %d more" % (count - 5)

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
                    target["referenced_by"].append({"file": candidate, "by": by, "kind": KIND_USED})
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
                target["referenced_by"].append({"file": candidate, "by": by, "kind": _reference_kind(extension, text, target, by)})
    if skipped_large > 0:
        report["warnings"].append("%d text file(s) larger than %d bytes were not searched for references." % [skipped_large, MAX_SCAN_BYTES])

## How a file of the given extension names target: a script that only writes it can create it
## again; every other file uses it.
func _reference_kind(extension: String, text: String, target: Dictionary, by: Array[String]) -> String:
    if extension != "gd":
        return KIND_USED
    var needles: Array[String] = []
    if by.has("path"):
        needles.append(str(target["path"]))
    if by.has("uid"):
        needles.append(str(target["uid"]))
    for needle: String in needles:
        if classify_script(text, needle) != KIND_WRITTEN:
            return KIND_USED
    return KIND_WRITTEN

## How a GDScript source names needle (a res:// path or uid://), which its text mentions:
## KIND_WRITTEN or KIND_USED.
func classify_script(source: String, needle: String) -> String:
    var kinds: PackedInt32Array = PackedInt32Array()
    var texts: PackedStringArray = PackedStringArray()
    _lex(source, kinds, texts)
    var roles: Array[int] = []
    for at: int in kinds.size():
        if kinds[at] != TOKEN_TEXT or not mentions(texts[at], needle):
            continue
        if texts[at] != needle:
            return KIND_USED
        roles.append(_role(kinds, texts, at, at + 1, 0, false))
    # A mention outside the code's strings (in a comment, say) is no place the analysis follows.
    if roles.is_empty():
        return KIND_USED
    return KIND_WRITTEN if _combine(roles) == ROLE_WRITE else KIND_USED

func _combine(roles: Array[int]) -> int:
    var result: int = ROLE_LOOK
    for role: int in roles:
        if role == ROLE_USE:
            return ROLE_USE
        if role == ROLE_WRITE:
            result = ROLE_WRITE
    return result

## Splits GDScript into words, string contents, marks and line ends; comments are dropped, and so
## are line ends inside brackets, as GDScript ignores them there.
func _lex(source: String, kinds: PackedInt32Array, texts: PackedStringArray) -> void:
    var length: int = source.length()
    var brackets: int = 0
    var at: int = 0
    while at < length:
        var c: String = source[at]
        if c == "\n":
            if brackets == 0:
                kinds.append(TOKEN_NEWLINE)
                texts.append("")
            at += 1
        elif c == " " or c == "\t" or c == "\r":
            at += 1
        elif c == "\\" and at + 1 < length and source[at + 1] == "\n":
            at += 2
        elif c == "#":
            while at < length and source[at] != "\n":
                at += 1
        elif c == "\"" or c == "'":
            var triple: bool = source.substr(at, 3) == c + c + c
            var open: int = 3 if triple else 1
            var start: int = at + open
            var end: int = start
            while end < length:
                if source[end] == "\\":
                    end += 2
                    continue
                if triple:
                    if source.substr(end, 3) == c + c + c:
                        break
                elif source[end] == c or source[end] == "\n":
                    break
                end += 1
            end = mini(end, length)
            kinds.append(TOKEN_TEXT)
            texts.append(source.substr(start, end - start))
            at = end + open if end < length and source[end] == c else end
        elif WORD_CHARS.contains(c):
            var start: int = at
            while at < length and WORD_CHARS.contains(source[at]):
                at += 1
            kinds.append(TOKEN_WORD)
            texts.append(source.substr(start, at - start))
        else:
            var pair: String = source.substr(at, 2)
            var mark: String = pair if pair in [":=", "==", "!=", "<=", ">=", "->"] else c
            if c in ["(", "[", "{"]:
                brackets += 1
            elif c in [")", "]", "}"]:
                brackets = maxi(brackets - 1, 0)
            kinds.append(TOKEN_MARK)
            texts.append(mark)
            at += mark.length()

func _is_mark(kinds: PackedInt32Array, texts: PackedStringArray, at: int, mark: String) -> bool:
    return at >= 0 and at < kinds.size() and kinds[at] == TOKEN_MARK and texts[at] == mark

func _word(kinds: PackedInt32Array, texts: PackedStringArray, at: int) -> String:
    return texts[at] if at >= 0 and at < kinds.size() and kinds[at] == TOKEN_WORD else ""

func _listed(list: Array, receiver: String, method: String) -> bool:
    for entry: Array in list:
        if str(entry[1]) == method and (str(entry[0]).is_empty() or str(entry[0]) == receiver):
            return true
    return false

## The call whose argument list holds token at: {receiver, method, open, argument}, or {}.
func _enclosing_call(kinds: PackedInt32Array, texts: PackedStringArray, at: int) -> Dictionary:
    var nested: int = 0
    var argument: int = 0
    var index: int = at
    while index > 0:
        index -= 1
        if kinds[index] != TOKEN_MARK:
            continue
        var mark: String = texts[index]
        if mark in [")", "]", "}"]:
            nested += 1
        elif mark == "(" and nested == 0:
            var method: String = _word(kinds, texts, index - 1)
            if method.is_empty():
                return {}
            var receiver: String = _word(kinds, texts, index - 3) if _is_mark(kinds, texts, index - 2, ".") else ""
            return {"receiver": receiver, "method": method, "open": index, "argument": argument}
        elif (mark == "[" or mark == "{") and nested == 0:
            return {}
        elif mark in ["(", "[", "{"]:
            nested -= 1
        elif mark == "," and nested == 0:
            argument += 1
    return {}

## The index of the ")" that closes the call opened at open, or -1.
func _call_end(kinds: PackedInt32Array, texts: PackedStringArray, open: int) -> int:
    var nested: int = 0
    for index: int in range(open + 1, kinds.size()):
        if kinds[index] != TOKEN_MARK:
            continue
        if texts[index] in ["(", "[", "{"]:
            nested += 1
        elif texts[index] in [")", "]", "}"]:
            if nested == 0:
                return index
            nested -= 1
    return -1

## The tokens of argument wanted of the call opened at open, joined.
func _argument_text(kinds: PackedInt32Array, texts: PackedStringArray, open: int, wanted: int) -> String:
    var nested: int = 0
    var argument: int = 0
    var text: String = ""
    for index: int in range(open + 1, kinds.size()):
        var kind: int = kinds[index]
        if kind == TOKEN_MARK:
            if texts[index] in ["(", "[", "{"]:
                nested += 1
            elif texts[index] in [")", "]", "}"]:
                if nested == 0:
                    break
                nested -= 1
            elif texts[index] == "," and nested == 0:
                argument += 1
                continue
        if argument == wanted:
            if kind == TOKEN_TEXT:
                text += "\""
            elif kind != TOKEN_NEWLINE:
                text += texts[index]
    return text

## What the expression in tokens [start, end) does with the path, or with its folder when folder.
func _role(kinds: PackedInt32Array, texts: PackedStringArray, start: int, end: int, carriers: int, folder: bool) -> int:
    # path.get_base_dir() is the folder: making it or checking it changes nothing.
    if not folder and _is_mark(kinds, texts, end, ".") and _word(kinds, texts, end + 1) == "get_base_dir" and _is_mark(kinds, texts, end + 2, "(") and _is_mark(kinds, texts, end + 3, ")"):
        return _role(kinds, texts, start, end + 4, carriers, true)
    var whole: bool = (_is_mark(kinds, texts, start - 1, "(") or _is_mark(kinds, texts, start - 1, ",")) and (_is_mark(kinds, texts, end, ")") or _is_mark(kinds, texts, end, ","))

    var enclosing: Dictionary = _enclosing_call(kinds, texts, start)
    if not enclosing.is_empty():
        var receiver: String = str(enclosing["receiver"])
        var method: String = str(enclosing["method"])
        var open: int = int(enclosing["open"])
        var argument: int = int(enclosing["argument"])
        if _listed(LOOKS, receiver, method):
            return ROLE_LOOK
        if not whole:
            return ROLE_USE
        if _listed(SAME_PATH, receiver, method) and argument == 0:
            var close: int = _call_end(kinds, texts, open)
            if close == -1:
                return ROLE_USE
            return _role(kinds, texts, open - 1 if receiver.is_empty() else open - 3, close + 1, carriers, folder)
        if folder:
            return ROLE_LOOK if _listed(FOLDER_LOOKS, receiver, method) and argument == 0 else ROLE_USE
        for entry: Array in WRITES:
            if str(entry[1]) == method and int(entry[2]) == argument and (str(entry[0]).is_empty() or str(entry[0]) == receiver):
                return ROLE_WRITE
        if receiver == "FileAccess" and OPENERS.has(method) and argument == 0 and WRITE_MODES.has(_argument_text(kinds, texts, open, 1)):
            return ROLE_WRITE
        return ROLE_USE

    # const NAME := <path> or var NAME: Type = <path>: follow NAME.
    var ends_statement: bool = end >= kinds.size() or kinds[end] == TOKEN_NEWLINE or _is_mark(kinds, texts, end, ";")
    if not ends_statement or not (_is_mark(kinds, texts, start - 1, "=") or _is_mark(kinds, texts, start - 1, ":=")):
        return ROLE_USE
    var assign: int = start - 1
    var keyword: int = -1
    for candidate: int in [assign - 2, assign - 4]:
        if candidate < 0 or not _word(kinds, texts, candidate) in ["var", "const"] or _word(kinds, texts, candidate + 1).is_empty():
            continue
        if candidate + 2 == assign or (_is_mark(kinds, texts, candidate + 2, ":") and not _word(kinds, texts, candidate + 3).is_empty() and candidate + 4 == assign):
            keyword = candidate
            break
    if keyword == -1 or carriers >= MAX_CARRIERS:
        return ROLE_USE
    var name: String = texts[keyword + 1]
    var roles: Array[int] = []
    for at: int in kinds.size():
        if at != keyword + 1 and kinds[at] == TOKEN_WORD and texts[at] == name and not _is_mark(kinds, texts, at - 1, "."):
            roles.append(_role(kinds, texts, at, at + 1, carriers + 1, folder))
    if roles.is_empty():
        return ROLE_USE
    return _combine(roles)

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
        # Godot does not scan these folders, nor a nested project (its own project.godot).
        if FileAccess.file_exists(child.path_join(".gdignore")) or FileAccess.file_exists(child.path_join("project.godot")):
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
