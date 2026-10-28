@tool
extends RefCounted
## Reads and edits a script through the script editor, so an edit is one undo step and never
## overwrites typing the person has not saved. Loaded by path from bridge_server.gd.
## Every function returns a Dictionary: {"error": message} or the answer of the command.

const MAX_BYTES: int = 1048576
## 64-bit FNV-1a offset basis (0xcbf29ce484222325) written as the signed value GDScript holds.
const FNV_OFFSET: int = -3750763034362895579
const FNV_PRIME: int = 0x100000001b3
const STALE_TAB: String = "The script tab of %s shows older text than the file on disk and has no unsaved changes. Call godot_resave_resources with paths [\"%s\"] to reload the tab, then read the script again and repeat the edit."

## Version label of a text: FNV-1a over its UTF-8 bytes with "\r\n" read as "\n", as 16 hex digits.
func label(text: String) -> String:
    var hash_value: int = FNV_OFFSET
    for byte: int in text.replace("\r\n", "\n").to_utf8_buffer():
        hash_value = (hash_value ^ byte) * FNV_PRIME
    return "%08x%08x" % [(hash_value >> 32) & 0xFFFFFFFF, hash_value & 0xFFFFFFFF]

func read(res_path: String) -> Dictionary:
    var loaded: Dictionary = _file_text(res_path)
    if loaded.has("error"):
        return loaded
    var file_text: String = str(loaded["text"])
    var pair: Dictionary = _pair_editor(res_path)
    var answer: Dictionary = {"path": res_path, "text": file_text, "source": "file", "dirty": false}
    if str(pair["state"]) == "found" and _is_dirty(pair["editor"]):
        answer["text"] = _buffer(pair["editor"]).text
        answer["source"] = "editor"
        answer["dirty"] = true
    elif str(pair["state"]) == "found" and _buffer(pair["editor"]).text != file_text:
        answer["stale_tab"] = true
        answer["hint"] = "The open script tab shows older text than the file. Call godot_resave_resources with this path to reload the tab before editing."
    elif str(pair["state"]) == "unknown":
        answer["editor_state"] = "unknown"
        answer["reason"] = str(pair["reason"])
    answer["version"] = label(str(answer["text"]))
    return answer

## The state of the script tab for res_path: {"path", "open", "dirty", "stale", "reloaded"}, plus
## "editor_state" and "reason" when the tab cannot be matched. "stale" means the tab's text differs
## from the file. With reload, a stale tab without unsaved changes is given the file's text.
func tab_state(res_path: String, reload: bool) -> Dictionary:
    var loaded: Dictionary = _file_text(res_path)
    if loaded.has("error"):
        return loaded
    var file_text: String = str(loaded["text"])
    var answer: Dictionary = {"path": res_path, "open": false, "dirty": false, "stale": false, "reloaded": false}
    var pair: Dictionary = _pair_editor(res_path)
    match str(pair["state"]):
        "none":
            return answer
        "unknown":
            answer["editor_state"] = "unknown"
            answer["reason"] = str(pair["reason"])
            return answer
    var editor: Object = pair["editor"]
    var base: CodeEdit = _buffer(editor)
    answer["open"] = true
    answer["dirty"] = _is_dirty(editor)
    answer["stale"] = base.text != file_text
    if reload and bool(answer["stale"]) and not bool(answer["dirty"]):
        _reload_tab(res_path, base, file_text)
        answer["stale"] = base.text != file_text
        answer["reloaded"] = not bool(answer["stale"])
    return answer

## Gives an unmodified tab the file's text and marks it saved, keeps the caret and scroll where they
## were, and gives the loaded script the same source so the running code matches the tab.
func _reload_tab(res_path: String, base: CodeEdit, file_text: String) -> void:
    var caret_line: int = base.get_caret_line()
    var caret_column: int = base.get_caret_column()
    var scroll: float = base.scroll_vertical
    base.text = file_text
    base.tag_saved_version()
    base.set_caret_line(mini(caret_line, base.get_line_count() - 1))
    base.set_caret_column(caret_column)
    base.scroll_vertical = scroll
    var script: Script = load(res_path) as Script
    if script != null and script.source_code != file_text:
        script.source_code = file_text
        script.reload(true)

func edit(res_path: String, old_text: String, new_text: String, version: String) -> Dictionary:
    if old_text.is_empty():
        return {"error": "old_text must not be empty. Pass the exact fragment to replace."}
    var old_lf: String = old_text.replace("\r\n", "\n")
    var new_lf: String = new_text.replace("\r\n", "\n")
    if old_lf == new_lf:
        return {"error": "old_text and new_text are identical, so nothing would change."}
    var loaded: Dictionary = _file_text(res_path)
    if loaded.has("error"):
        return loaded
    var file_lf: String = str(loaded["text"])
    var had_bom: bool = bool(loaded.get("bom", false))
    var pair: Dictionary = _pair_editor(res_path)
    var problem: String = _editor_problem(res_path, file_lf, pair)
    if not problem.is_empty():
        return {"error": problem}
    if not version.is_empty() and version != label(file_lf):
        return {"error": "The script changed since it was read (version %s, now %s). Read it again with godot_read_script and repeat the edit." % [version, label(file_lf)]}
    var count: int = _count(file_lf, old_lf)
    if count != 1:
        return {"error": "old_text was found %d times in %s; it must occur exactly once. Include more surrounding text so it identifies one place." % [count, res_path]}

    var script: Script = load(res_path) as Script
    if script == null:
        return {"error": "File is not a script that the editor can load: " + res_path}
    if str(pair["state"]) == "none":
        EditorInterface.edit_script(script)
        pair = _pair_editor(res_path)
        if str(pair["state"]) != "found":
            return {"error": "The script editor did not open %s. Open it in the editor and repeat the edit." % res_path}
    var base: CodeEdit = _buffer(pair["editor"])
    if base.text != file_lf:
        return {"error": STALE_TAB % [res_path, res_path]}

    var index: int = file_lf.find(old_lf)
    var before: String = file_lf.substr(0, index)
    var line: int = before.count("\n")
    var column: int = index - (before.rfind("\n") + 1)
    var old_lines: int = old_lf.count("\n")
    var end_line: int = line + old_lines
    var end_column: int = column + old_lf.length()
    if old_lines > 0:
        end_column = old_lf.length() - (old_lf.rfind("\n") + 1)
    base.begin_complex_operation()
    base.remove_text(line, column, end_line, end_column)
    base.insert_text(new_lf, line, column)
    base.end_complex_operation()

    script.source_code = base.text
    var save_error: Error = ResourceSaver.save(script, res_path)
    if save_error != OK:
        base.undo()
        base.tag_saved_version()
        return {"error": "Could not save %s (%s). The edit was undone." % [res_path, error_string(save_error)]}
    base.tag_saved_version()
    EditorInterface.get_resource_filesystem().update_file(res_path)
    script.reload(true)
    var answer: Dictionary = {"path": res_path, "replaced": 1, "version": label(base.text), "source": "editor"}
    if had_bom:
        # Godot saves a script without the mark; say so instead of leaving the person to find out.
        answer["bom_removed"] = not _file_starts_with_bom(res_path)
    return answer

## Why an edit must not go ahead given the editor's state for the file, or "".
func _editor_problem(res_path: String, file_lf: String, pair: Dictionary) -> String:
    match str(pair["state"]):
        "unknown":
            return "The script editor's state for %s cannot be matched (%s). Close the extra tabs in the script editor, or save the script, then read it again." % [res_path, str(pair["reason"])]
        "found":
            var editor: Object = pair["editor"]
            if _is_dirty(editor):
                return "%s has unsaved changes in the script editor. Save it in the editor, or discard the changes, then read it again and repeat the edit." % res_path
            if _buffer(editor).text != file_lf:
                return STALE_TAB % [res_path, res_path]
    return ""

## {"state": "none"} when the script is not open, {"state": "found", "editor": ScriptEditorBase}
## when exactly one editor is matched to the path, {"state": "unknown", "reason": ...} otherwise.
## The editor API has no call that names an editor's resource, so the two lists the script
## editor offers (same order) are paired. Newer editors also name the tab (the `_tab_name`
## metadata, absent in Godot 4.5.2), and then the name must start with the file name.
func _pair_editor(res_path: String) -> Dictionary:
    var script_editor: ScriptEditor = EditorInterface.get_script_editor()
    if script_editor == null:
        return {"state": "none"}
    var scripts: Array = script_editor.get_open_scripts()
    var editors: Array = script_editor.get_open_script_editors()
    var listed: bool = false
    for script: Script in scripts:
        if script.resource_path == res_path:
            listed = true
    if not listed:
        return {"state": "none"}
    if scripts.size() != editors.size():
        return {"state": "unknown", "reason": "the editor lists %d open scripts but %d script tabs" % [scripts.size(), editors.size()]}
    for index: int in range(scripts.size()):
        if (scripts[index] as Script).resource_path != res_path:
            continue
        if editors[index].has_meta("_tab_name"):
            var tab_name: String = str(editors[index].get_meta("_tab_name"))
            if not tab_name.begins_with(res_path.get_file()):
                return {"state": "unknown", "reason": "the tab '%s' does not belong to %s" % [tab_name, res_path.get_file()]}
        return {"state": "found", "editor": editors[index]}
    return {"state": "none"}

func _buffer(editor: Object) -> CodeEdit:
    return editor.call("get_base_editor") as CodeEdit

func _is_dirty(editor: Object) -> bool:
    var buffer: CodeEdit = _buffer(editor)
    return buffer.get_version() != buffer.get_saved_version()

func _count(text: String, fragment: String) -> int:
    var found: int = 0
    var from: int = 0
    while true:
        var at: int = text.find(fragment, from)
        if at < 0:
            break
        found += 1
        from = at + 1
    return found

func _file_starts_with_bom(res_path: String) -> bool:
    var file: FileAccess = FileAccess.open(res_path, FileAccess.READ)
    if file == null:
        return false
    var head: PackedByteArray = file.get_buffer(3)
    file.close()
    return head.size() == 3 and head[0] == 0xEF and head[1] == 0xBB and head[2] == 0xBF

## {"text": file text with "\n" line endings, "bom": whether it starts with a byte order mark} or
## {"error": ...}.
func _file_text(res_path: String) -> Dictionary:
    if not res_path.ends_with(".gd"):
        return {"error": "Only .gd scripts can be read or edited: " + res_path}
    if not FileAccess.file_exists(res_path):
        return {"error": "Script file does not exist: %s. Create it with godot_create_script." % res_path}
    var file: FileAccess = FileAccess.open(res_path, FileAccess.READ)
    if file == null:
        return {"error": "Could not open %s (%s)." % [res_path, error_string(FileAccess.get_open_error())]}
    if file.get_length() > MAX_BYTES:
        return {"error": "%s is larger than 1 MiB, which the script tools do not handle. Edit it with your own file tools." % res_path}
    var bytes: PackedByteArray = file.get_buffer(file.get_length())
    file.close()
    # A UTF-8 byte order mark is not part of the text (Godot's decoder skips it, so the round
    # trip below would otherwise call the file invalid).
    var has_bom: bool = bytes.size() >= 3 and bytes[0] == 0xEF and bytes[1] == 0xBB and bytes[2] == 0xBF
    if has_bom:
        bytes = bytes.slice(3)
    var text: String = bytes.get_string_from_utf8()
    if text.to_utf8_buffer() != bytes:
        return {"error": "%s is not valid UTF-8, so it is not read as text." % res_path}
    return {"text": text.replace("\r\n", "\n"), "bom": has_bom}
