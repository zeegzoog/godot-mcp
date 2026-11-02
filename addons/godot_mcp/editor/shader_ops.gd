@tool
extends RefCounted
## Creates, reads and edits a text shader (.gdshader, .gdshaderinc) through the shader editor and
## the shader the editor holds in memory, so an edit is one undo step in the shader's tab, the
## materials using the shader get the new code, and typing the person has not saved is never
## overwritten. Loaded by path from bridge_server.gd.
## Every function returns a Dictionary: {"error": message} or the answer of the command.

const MAX_BYTES: int = 1048576
## 64-bit FNV-1a offset basis (0xcbf29ce484222325) written as the signed value GDScript holds.
const FNV_OFFSET: int = -3750763034362895579
const FNV_PRIME: int = 0x100000001b3
const EXTENSIONS: PackedStringArray = ["gdshader", "gdshaderinc"]
const STALE_TAB: String = "The shader tab of %s shows older text than the file on disk and has no unsaved changes. Ask the person to reload it in the shader editor, or replace the whole file with godot_create_shader and overwrite: true, then read the shader again."

## Version label of a text: FNV-1a over its UTF-8 bytes with "\r\n" read as "\n", as 16 hex digits.
## The same label as the script tools give.
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
        answer["hint"] = "The open shader tab shows older text than the file. Reload it in the shader editor before editing."
    elif str(pair["state"]) == "unknown":
        answer["editor_state"] = "unknown"
        answer["reason"] = str(pair["reason"])
    answer["version"] = label(str(answer["text"]))
    return answer

## Writes the .uid file of a shader the editor's file system did not give one (ResourceSaver
## writes none for a text shader and set_uid refuses it), registers the id and tells the file
## system. Returns the id, or INVALID_ID when the file cannot be written.
func _give_uid_file(res_path: String) -> int:
    var sidecar_path: String = res_path + ".uid"
    if FileAccess.file_exists(sidecar_path):
        return ResourceLoader.get_resource_uid(res_path)
    var id: int = ResourceUID.create_id()
    var sidecar: FileAccess = FileAccess.open(sidecar_path, FileAccess.WRITE)
    if sidecar == null:
        return ResourceUID.INVALID_ID
    sidecar.store_string(ResourceUID.id_to_text(id) + "\n")
    sidecar.close()
    if not ResourceUID.has_id(id):
        ResourceUID.add_id(id, res_path)
    EditorInterface.get_resource_filesystem().update_file(res_path)
    var registered: int = ResourceLoader.get_resource_uid(res_path)
    return registered if registered != ResourceUID.INVALID_ID else id


## Frames an edit waits for the shader editor to open a tab for the shader.
const TAB_OPEN_FRAMES: int = 30


func create(res_path: String, code: String, overwrite: bool) -> Dictionary:
    if not EXTENSIONS.has(res_path.get_extension()):
        return {"error": "Only .gdshader and .gdshaderinc files are handled by the shader tools: " + res_path}
    if code.to_utf8_buffer().size() > MAX_BYTES:
        return {"error": "code is larger than 1 MiB, which the shader tools do not handle. Split the shader into a shader and .gdshaderinc includes."}
    var exists: bool = FileAccess.file_exists(res_path)
    if exists and not overwrite:
        return {"error": "File already exists: %s. Pass overwrite: true to replace it, or change part of it with godot_edit_shader." % res_path}
    if not exists and DirAccess.dir_exists_absolute(res_path):
        return {"error": "%s is a folder. Choose another path." % res_path}
    var pair: Dictionary = {"state": "none"}
    if exists:
        pair = _pair_editor(res_path)
        match str(pair["state"]):
            "unknown":
                return {"error": "The shader editor's state for %s cannot be matched (%s), so it was not overwritten. Close the shader's extra tabs in the shader editor, then repeat the call." % [res_path, str(pair["reason"])]}
            "found":
                if _is_dirty(pair["editor"]):
                    return {"error": "%s has unsaved changes in the shader editor, so it was not overwritten. Ask the person to save or discard them, then repeat the call." % res_path}

    var folder: String = res_path.get_base_dir()
    if not DirAccess.dir_exists_absolute(folder):
        var made: Error = DirAccess.make_dir_recursive_absolute(folder)
        if made != OK:
            return {"error": "Could not create the folder %s (%s). Check that the project directory is writable." % [folder, error_string(made)]}
    var shader: Resource = null
    if exists and ResourceLoader.has_cached(res_path):
        # The editor's own copy: giving it the code updates every material that uses it.
        shader = load(res_path)
    if shader == null or not ("code" in shader):
        if res_path.get_extension() == "gdshaderinc":
            shader = ShaderInclude.new()
        else:
            shader = Shader.new()
    # A change of the shader's code makes its tab apply the tab's own text to the shader, so the
    # tab is given the new text first.
    var base: CodeEdit = null
    var tab_text: String = ""
    if str(pair["state"]) == "found":
        base = _buffer(pair["editor"])
        tab_text = base.text
        _reload_tab(base, code)
    var previous: String = str(shader.get("code"))
    shader.set("code", code)
    var save_error: Error = ResourceSaver.save(shader, res_path)
    if save_error != OK:
        if base != null:
            _reload_tab(base, tab_text)
        shader.set("code", previous)
        return {"error": "Could not save %s (%s). Check that the folder is writable." % [res_path, error_string(save_error)]}
    var answer: Dictionary = {"path": res_path, "created": not exists, "version": label(code), "source": "editor"}
    if base != null:
        answer["tab_reloaded"] = base.text == code
    # The file system scan gives a new file its .uid, as for a file the editor saved itself.
    EditorInterface.get_resource_filesystem().update_file(res_path)
    var uid: int = ResourceLoader.get_resource_uid(res_path)
    if uid == ResourceUID.INVALID_ID:
        uid = _give_uid_file(res_path)
    if uid != ResourceUID.INVALID_ID:
        answer["uid"] = ResourceUID.id_to_text(uid)
    return answer

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
        return {"error": "The shader changed since it was read (version %s, now %s). Read it again with godot_read_shader and repeat the edit." % [version, label(file_lf)]}
    var count: int = _count(file_lf, old_lf)
    if count != 1:
        return {"error": "old_text was found %d times in %s; it must occur exactly once. Include more surrounding text so it identifies one place." % [count, res_path]}

    var shader: Resource = load(res_path)
    if shader == null or not ("code" in shader):
        return {"error": "File is not a shader that the editor can load: " + res_path}
    if str(pair["state"]) == "none":
        EditorInterface.edit_resource(shader)
        pair = _pair_editor(res_path)
        # An older editor connects the new tab to the shader on a later frame.
        var frames: int = 0
        while str(pair["state"]) == "none" and frames < TAB_OPEN_FRAMES:
            await (Engine.get_main_loop() as SceneTree).process_frame
            pair = _pair_editor(res_path)
            frames += 1
        if str(pair["state"]) != "found":
            return {"error": "The shader editor did not open %s (%s). Open it in the shader editor and repeat the edit." % [res_path, str(pair.get("reason", pair["state"]))]}
    var base: CodeEdit = _buffer(pair["editor"])
    if base.text != file_lf:
        return {"error": STALE_TAB % res_path}

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

    var previous: String = str(shader.get("code"))
    shader.set("code", base.text)
    var save_error: Error = ResourceSaver.save(shader, res_path)
    if save_error != OK:
        base.undo()
        base.tag_saved_version()
        shader.set("code", previous)
        return {"error": "Could not save %s (%s). The edit was undone." % [res_path, error_string(save_error)]}
    base.tag_saved_version()
    EditorInterface.get_resource_filesystem().update_file(res_path)
    var answer: Dictionary = {"path": res_path, "replaced": 1, "version": label(base.text), "source": "editor"}
    if had_bom:
        # Godot saves a shader without the mark; say so instead of leaving the person to find out.
        answer["bom_removed"] = not _file_starts_with_bom(res_path)
    return answer

## Gives an unmodified tab the new text and marks it saved, keeping the caret and scroll.
func _reload_tab(base: CodeEdit, text: String) -> void:
    var caret_line: int = base.get_caret_line()
    var caret_column: int = base.get_caret_column()
    var scroll: float = base.scroll_vertical
    base.text = text
    base.tag_saved_version()
    base.set_caret_line(mini(caret_line, base.get_line_count() - 1))
    base.set_caret_column(caret_column)
    base.scroll_vertical = scroll

## Why an edit must not go ahead given the editor's state for the file, or "".
func _editor_problem(res_path: String, file_lf: String, pair: Dictionary) -> String:
    match str(pair["state"]):
        "unknown":
            return "The shader editor's state for %s cannot be matched (%s). Close the shader's extra tabs in the shader editor, then read it again." % [res_path, str(pair["reason"])]
        "found":
            var editor: Object = pair["editor"]
            if _is_dirty(editor):
                return "%s has unsaved changes in the shader editor. Save it in the editor, or discard the changes, then read it again and repeat the edit." % res_path
            if _buffer(editor).text != file_lf:
                return STALE_TAB % res_path
    return ""

## {"state": "none"} when no shader tab edits res_path, {"state": "found", "editor": the tab}
## when exactly one does, {"state": "unknown", "reason": ...} otherwise. The editor API has no
## call that names a shader tab's file, but every shader tab connects to the changed signal of
## the shader it edits, and the shader it edits is the one the editor holds for the path.
func _pair_editor(res_path: String) -> Dictionary:
    if not ResourceLoader.has_cached(res_path):
        return {"state": "none"}
    var shader: Resource = load(res_path)
    if shader == null:
        return {"state": "none"}
    var editors: Array[Object] = []
    for connection: Dictionary in shader.get_signal_connection_list("changed"):
        var target: Object = (connection["callable"] as Callable).get_object()
        if target != null and target is Node and target.is_class("ShaderTextEditor") and not editors.has(target):
            editors.append(target)
    if editors.is_empty():
        return {"state": "none"}
    if editors.size() > 1:
        return {"state": "unknown", "reason": "%d shader tabs edit this file" % editors.size()}
    if not (_buffer(editors[0]) is CodeEdit):
        return {"state": "unknown", "reason": "the shader tab has no text editor"}
    return {"state": "found", "editor": editors[0]}

## The text editor of a shader tab. ShaderTextEditor has get_base_editor only from Godot 4.8; the
## older editors keep the CodeEdit as a child of the tab.
func _buffer(editor: Object) -> CodeEdit:
    if editor.has_method("get_base_editor"):
        return editor.call("get_base_editor") as CodeEdit
    var found: Array[Node] = (editor as Node).find_children("*", "CodeEdit", true, false)
    return null if found.is_empty() else found[0] as CodeEdit

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
    if not EXTENSIONS.has(res_path.get_extension()):
        return {"error": "Only .gdshader and .gdshaderinc files are handled by the shader tools: " + res_path}
    if not FileAccess.file_exists(res_path):
        return {"error": "Shader file does not exist: %s. Create it with godot_create_shader." % res_path}
    var file: FileAccess = FileAccess.open(res_path, FileAccess.READ)
    if file == null:
        return {"error": "Could not open %s (%s)." % [res_path, error_string(FileAccess.get_open_error())]}
    if file.get_length() > MAX_BYTES:
        return {"error": "%s is larger than 1 MiB, which the shader tools do not handle. Edit it with your own file tools." % res_path}
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
