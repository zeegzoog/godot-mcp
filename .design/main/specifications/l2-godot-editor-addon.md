# Godot Editor Addon

**Version:** 1.11.1
**Status:** Stable
**Layer:** implementation
**Implements:** l1-editor-bridge.md

## Overview

The editor side of the live bridge, written in GDScript and shipped as `addons/godot_mcp`: an `EditorPlugin` that gates on the engine version and starts the bridge node, the bridge node itself (listener, secret, parameter validation, command handlers), and an export plugin that keeps the addon out of exported games.

## Related Specifications

- [l1-editor-bridge.md](l1-editor-bridge.md) - Parent concept.
- [l1-content-authoring.md](l1-content-authoring.md) - CA-7 to CA-10: reading and editing scripts (§5.3); CA-4, CA-14 to CA-17: settings, shaders, import parameters, InputMap actions and file deletion (§5.2, §5.4). [MODIFIED]
- [l2-bridge-client-rust.md](l2-bridge-client-rust.md) - The server-side peer.
- [l1-distribution.md](l1-distribution.md) - DS-5: shipped as a separate archive.
- [l1-release-lines.md](l1-release-lines.md) - RL-4: the addon of each line gates on its line's Godot version.
- [l2-release-branches.md](l2-release-branches.md) - Line identity values in the addon and the per-line guard list.

## 1. Motivation

GDScript fails differently from Rust: assigning a wrongly typed value to a typed variable, or calling a method the running engine lacks, raises a script error that aborts the handler without a reply, and the caller only sees a timeout. The conventions below exist to prevent exactly that.

## 2. Constraints & Assumptions

- [MODIFIED] Target: the Godot minor version of the branch's release line (4.5, 4.6, 4.7; 4.8 on the development line). `MIN_MAJOR`/`MIN_MINOR` in `plugin.gd` and `config/features` in the development `project.godot` are line identity values; `tests/version_sync_test.rs` keeps them equal to the line in the version string.
- Observed conventions:
  - Strict static typing for locals, members and return types.
  - No `class_name` in any addon script: a global class would be listed in every exported game's class list while its script is left out. Scripts are loaded by path.
  - Command parameters are read untyped and validated against `PARAM_TYPES` before a handler runs.
  - [MODIFIED] APIs missing from the line's M.N.0 are called through `has_method()` + `call()`; APIs present there are called directly. Example: `EditorFileSystem.is_importing` exists from 4.7, so lines 4.5 and 4.6 guard it and line 4.7 and the development line call it directly.
  - No third-party dependencies; standard Godot API only.
  - Log lines are prefixed `[Godot MCP]`; connect/disconnect logs only when the project setting `godot_mcp/verbose` is true.
- The repository root is a minimal Godot project (`project.godot`) so the addon can be enabled and syntax-checked in isolation.

## 4. Invariant Compliance

| L1 Invariant | Implementation |
| --- | --- |
| BR-1 Loopback only | `TCPServer.listen(port, "127.0.0.1")`. |
| BR-2 Shared secret | `prepare_token`: `GODOT_MCP_TOKEN` if set, else 24 random bytes hex-encoded, written to `OS.get_data_dir()/godot-mcp/bridge-{port}.token` with owner-only permissions (and to `bridge-9333.token` as well while the legacy listener of BR-13 runs [MODIFIED]) and deleted in `stop_server`. `handle_message` rejects any message whose `token` is not a string equal to the secret, before reading anything else. |
| BR-3 Exactly one reply | Malformed JSON and non-object payloads get an error reply; `type`/`params` are type-checked by hand; `validate_params` checks each declared parameter (JSON null treated as absent, int accepted for float); unknown types get "Unknown command type"; `send_json` falls back to a short error reply when the full reply cannot be sent. WebSocket buffers are 4 MiB inbound and 32 MiB outbound. |
| BR-4 Correlated commands | Every reply echoes `commandId` (non-string ids are stringified). |
| BR-5 Undoable mutations | `add_node`, `remove_node` and `set_node_properties` each create one `EditorUndoRedoManager` action in the scene's history; remove records the child index and every owned descendant so undo restores order and ownership; the new or removed node is kept alive by an undo/do reference. |
| BR-6 Scene confinement | `find_scene_node` accepts a result only if it is the edited root or a descendant of it; `.`/empty means the root; removing the root is refused; reported paths are `root.get_path_to(node)`. |
| BR-7 All-or-nothing updates | `set_node_properties` converts every value first (`convert_property_value`: numbers, vectors/rects/colors from arrays or objects, colors from strings, `res://` resources, packed arrays, Godot literals via `str_to_var`), rejects the whole call on any unknown property or conversion error, then applies all values in one action and replies with values read back. [MODIFIED] A `metadata/<name>` key the node does not have yet is the one unknown key that is created instead of rejected, as the inspector's Add Metadata does: `set_meta` inside the same action (undo calls `remove_meta`, and the property list is refreshed on both sides). The name must be a valid identifier and the value must not be `null`; a whole number becomes an `int`; any other value keeps its JSON type; a violation rejects the whole call. An existing entry keeps its type. [MODIFIED] A name with `:` steps through the node's own resources with the running game's resolver (`game.set`), so a path means the same in the editor and in the game; the last segment is a property of the last resource, converted like any other, and the change joins the same action. A resource saved outside the edited scene (a `.tres` file or another scene's sub-resource) is refused with the file named. `get_node_properties` takes an optional `properties` list with the same paths and reports a name it cannot read in `property_errors`. [MODIFIED] Every value is read back after it is set: `set_node_properties` tries the values, puts them back, and refuses the whole call when a setter kept another value, naming the property and both values (floats and float-based types compare approximately; `name` is not compared); an unknown audio bus lists the editor's buses and points to the bus layout file and an editor restart. `add_node` checks its `properties` the same way and lists them in `applied`. |
| BR-8 Fast, classified failures | Errors are replied as `status: error` with a human-readable message; the server classifies them. |
| BR-9 Version gate | [MODIFIED] `plugin.gd` compares `Engine.get_version_info()` with the line's `MIN_MAJOR`.`MIN_MINOR`: an older version prints an error and the bridge does not start; a newer version prints a warning naming the release line to install ("This godot-mcp build is for Godot 4.6; install the release for Godot 4.7") and the bridge starts (RL-4). |
| BR-10 Editor-only | `export_plugin.gd` (registered before the bridge so it works even if the bridge fails) skips every file under its own directory; its name sorts before the built-in "GDScript" export plugin, which would otherwise remap `.gd` files first and end the chain. |
| BR-11 Project-confined paths | `to_res_path` normalizes separators, localizes absolute paths, and rejects anything outside `res://` or containing a `..` segment; used by `open_scene`, `get_scene_tree`, `run_project`, `resave_resources`, `read_script` and `edit_script`, and [MODIFIED] by `script_tab`, the shader commands, `import_params` (the file and `copy_from`) and `delete_files` (each path). |
| BR-12 Saved edits only | [ADDED] `set_node_properties`, `add_node` (the parent), `attach_script` and `connect_signal` (the source) walk the node's owner chain up to the edited root (`locked_instances`); an instance on it whose children are not editable (`is_editable_instance`) means Godot would save nothing about the node, so the command is refused with the instance and its scene named, unless `make_editable` is true: then `set_editable_instance(…, true)` joins the same undo action and the answer lists the instances in `made_editable`. `remove_node` refuses a node owned by an instance, which Godot cannot delete. `get_scene_tree` reports `instance` and `editable_children` on instance nodes. |
| BR-13 One port per project | [ADDED] `read_configuration`: `godot_mcp/bridge_port`, else `GODOT_DEBUG_PORT` when it is an integer, else `core/bridge_port.gd` (`project_path()` resolves symlinks by opening the directory and reading back its current path; `port_for` is the server's formula, with 64-bit integer arithmetic masked to 32 bits). `port_origin` names the source in the start-up line and in a failure. With a derived port other than 9333, `start_server` also listens on 9333 through a second loopback `TCPServer` when it is free, and writes the token for it; a busy legacy port is skipped silently. |

## 5. Detailed Design

### 5.1 Files

```plaintext
addons/godot_mcp/
├── plugin.cfg          name, version (must equal the server's), entry script
├── plugin.gd           EditorPlugin: version gate, export plugin, bridge node lifecycle
├── bridge_server.gd    bridge node: listener, secret, validation, handlers, replies
├── export_plugin.gd    EditorExportPlugin: skip the addon folder on export
├── editor/script_ops.gd       read and edit a script through the script editor (see §5.3)
├── editor/shader_ops.gd       create, read and edit a text shader through the shader editor (see §5.4) [ADDED]
├── editor/import_ops.gd       a file's import parameters by ConfigFile; also run by the server's headless path (see §5.4) [ADDED]
├── editor/file_ops.gd         delete files the way the FileSystem dock does (see §5.4) [ADDED]
├── core/bridge_port.gd        the port derived from the project path (BR-13) [ADDED]
├── core/runtime_override.gd   override.cfg for editor Play, restored afterwards
├── editor/game_relay.gd       EditorDebuggerPlugin relaying mcp:* messages of a played game
├── runtime/            in-game component loaded only while a debugger is attached (see l2-game-runtime.md)
└── *.gd.uid            Godot UID sidecars (committed)
```

### 5.2 Handler Notes

- `capture_viewport`: refuses headless editors; switches the main screen to 2D or 3D (`auto` picks 3D for a `Node3D` root), waits two frames, forces a draw (an idle editor in low-processor mode would otherwise never redraw), rejects images under 16 px, downscales to `max_dimension` (default 1568) with Lanczos, returns base64 PNG with width, height and view.
- `resave_resources`: refuses while an import is running (`is_import_running`: guarded call before 4.7, direct call from 4.7); with no paths starts a background `scan()`; otherwise re-imports importable files synchronously and calls `update_file` for others; reports reimported, refreshed, missing and invalid paths. [MODIFIED] A resource the editor has loaded is reloaded in place, and a loaded script without a tab gets its new source, listed in `reloaded_resources`, so a material sees a rewritten shader's new uniforms.
- `open_scene`: after reloading an open background tab from disk it switches to that tab and names it in `active_scene`; when the editor did not switch, the command fails with the fix. [ADDED]
- `add_node`: native classes must derive from `Node` and be instantiable; global script classes are looked up in the project's class list; non-Node script instances are freed.
- `get_node_properties`: only properties with editor usage; values in Godot text form, truncated at 2000 characters. A property that holds no object (an unset `material`) is read as `null`, not as a freed object. [MODIFIED]
- `list_signals`: for each signal, `connections` is a list of `{target, method}` for the connections whose target is a node of the edited scene (`target` is the scene-relative path); connections the editor made to its own panels are left out. [MODIFIED]
- `create_scene`, `create_resource`: create the missing folders of the path first (`make_dir_recursive_absolute`); a failure to create one is reported with the folder; refuse an existing file; the scene is packed and opened, the resource saved with `ResourceSaver` and registered with the file system. `attach_script`, `connect_signal`: one undo action each; the script must exist in the project, the target must have the method, a connection that exists is refused. `set_project_setting`: the key is a plain `section/path` without an empty, `.` or `..` segment, the value a string, number or boolean; the setting's type is the stored one, else the engine's (`_defined_setting_key`; a feature override such as `name.mobile` has the type of `name`), and a value of that type is kept: a whole number becomes an `int`, an HTML color or color name a `Color`, a name a `StringName` or `NodePath`, and for any other type the `project.godot` text, accepted only when it starts with the type's name and parses to it (`_setting_from_text`, never an Object), the same rules as the server's file fallback (CA-4, CA-11); the old value is restored if saving fails. [MODIFIED]
- `script_tab` (`script_path`, `reload`): reports whether the script has an open tab and whether it holds unsaved changes or cannot be matched to the file; with `reload`, an unchanged tab that shows an older text gets the file's text. The server asks it before it overwrites a script, and refuses the overwrite when typing could be lost. [ADDED]
- `get_input_map`, `edit_input_action`: events are built from `type` `key` (key name, `physical` by default, modifiers), `mouse_button`, `joypad_button` or `joypad_motion`; the change of one action is one `EditorUndoRedoManager` action and `ProjectSettings.save()` writes it (CA-16). [ADDED]
- `game_send`, `game_status`: the relay of `editor/game_relay.gd` to a game started by Play; `game_send` refuses when the relay is not active. [ADDED]
- `bridge_server.gd` start-up: when the port is taken (`Already in use`, typically a second editor), the failure message says that probably another editor uses the port and that a server connected to it talks to that editor, and names the two settings that change the port. It is printed to the Output and, when the editor has a toaster (`has_method` guard, not measured in a window), shown as an error toast. [MODIFIED]
- `plugin.gd`: with the environment variable `GODOT_MCP_NO_BRIDGE=1` the plugin still registers the export exclusion but opens no bridge and says so in the log; the server sets it for the private editor of the language server.
- `game_status`, relay: the plugin lists the played games and their `hello`; the host waits for it before it sends anything to a game started by Play.
- `close_editor` (`discard`): lists unsaved scenes (`EditorInterface.get_unsaved_scenes` and the bridge's own undo-version check) and script tabs with unsaved typing; with any and no `discard` it answers `closing: false` with the lists and stays open. Otherwise it answers `closing: true` with `OS.get_process_id()` and what it discarded, stops Play, and calls `get_tree().quit()` three frames later, after the reply has left. [ADDED]

### 5.3 Script Commands

`read_script` and `edit_script` realize CA-7 to CA-10 of [l1-content-authoring.md](l1-content-authoring.md). Their logic lives in `editor/script_ops.gd` (loaded by path, no `class_name`); the two handlers in `bridge_server.gd` only validate the parameters, turn the path into a `res://` path with `to_res_path` and send the reply. The version label and the line-ending and size rules are the ones of [l2-tool-handlers.md](l2-tool-handlers.md) §5.4, with the same test vectors.

Facts the design rests on, measured on Godot 4.8-beta1 in the headless editor (`script_editor_facts_for_reading_and_editing`):

- A script opened with `EditorInterface.edit_script` has a code editor even without a window: `ScriptEditor.get_open_script_editors()` lists it and `get_base_editor()` returns its `CodeEdit`.
- A script has unsaved changes when the `CodeEdit`'s `get_version()` differs from `get_saved_version()`. The unsaved text is the `CodeEdit`'s `text`; the loaded script resource keeps the saved text (`source_code` does not follow the buffer).
- A replacement made between `begin_complex_operation()` and `end_complex_operation()` is reversed by one `undo()`, which also returns the version to the saved one.
- `script.source_code = text`, `ResourceSaver.save`, `tag_saved_version()`, `EditorInterface.get_resource_filesystem().update_file` and `script.reload(true)` write the file, clear the unsaved state and give a running `@tool` node the new code.
- `ScriptEditorBase` has no `get_edited_resource()` and `get_current_editor()` is null in the headless editor, so an editor is matched to a path through the two lists: `get_open_scripts()` and `get_open_script_editors()` have the same order.

Pairing rule: an editor is paired with a path only when both lists have the same length and, where the editor names its tab (the `_tab_name` metadata, present in Godot 4.8-beta1 and absent in 4.5.2, as the carry to the older lines showed), that name starts with the file name. Where the metadata is absent the equal lengths are the only check. Other tabs (a help page, a text file) can make the lists differ in a real window; that case was not measured headless, so it is handled by refusing: the answer says the editor state cannot be matched and names the fix (close the extra tabs, or save the script, then read again).

`read_script` (`script_path`): the path must end in `.gd`, name an existing file and be at most 1 MiB; a leading UTF-8 byte order mark is removed before the text is checked, so the file is not refused as invalid UTF-8. The answer is `{path, text, source, dirty, version}`: `source` is `editor` and `dirty` is true when the paired editor has unsaved changes (the text is the `CodeEdit`'s, line endings as `\n`); otherwise `source` is `file`, `dirty` is false and the text is the file's. `version` is the label of the returned text. An unpaired or unmatched editor is read as the file; the answer does not claim `dirty: false` for a script whose editor state is unknown, it carries `editor_state: "unknown"` and the reason.

`edit_script` (`script_path`, `old_text`, `new_text`, optional `version`):

1. Validate as for `read_script`; `old_text` is not empty and differs from `new_text`.
2. Pair the editor. Unknown editor state → refuse (pairing rule). A paired editor with unsaved changes → refuse with the path and the fix (save in the editor or discard, then read again) and change nothing (CA-9). A paired saved editor whose text differs from the file (the file changed on disk and the editor has not reloaded it) → refuse with the same kind of fix.
3. The text to edit is the paired editor's, else the file's, with `\r\n` read as `\n`. If `version` is given it must equal the label of that text, otherwise refuse (CA-8). Count the occurrences of `old_text`: zero or more than one → refuse with the count.
4. Apply: open the script in the editor when it is not open (`edit_script`; this adds a tab, which is visible to the person and acceptable), replace the fragment on its `CodeEdit` as one complex operation (one undo step), then save as in the facts above so the editor, the file and the loaded script agree (CA-10). A failure after the change started (the save) undoes the operation before replying.
5. The answer is `{path, replaced: 1, version, source: "editor"}` with the label of the new text.

A file that started with a byte order mark is saved the way Godot saves a script, and the answer says whether the mark is gone (`bom_removed`), by looking at the first bytes of the file after the save; the mark is not written back. Measured on Godot 4.8-beta1: `ResourceSaver` writes no mark, so `bom_removed` is true. Godot writes none itself, so restoring it would make the editor and the file disagree on the next save.

The editor writes the file with its own line-ending setting, so a script edited through the editor ends up with the editor's endings; the file fallback keeps the file's.

Every reply path sends exactly one answer (BR-3); the parameters are read untyped and validated (`PARAM_TYPES`: `script_path`, `old_text`, `new_text`, `version` are strings).

### 5.4 Shader, Import and File Commands [ADDED]

Each lives in its own file under `editor/`, loaded by path with no `class_name`; the handler in `bridge_server.gd` validates the parameters, confines the path with `to_res_path` and sends the one reply. Each operation returns a Dictionary that is either `{"error": …}` or the answer.

- **Shaders** (`shader_ops.gd`, CA-14): `create_shader` (`shader_path`, `code`, `overwrite`), `read_shader` (`shader_path`), `edit_shader` (`shader_path`, `old_text`, `new_text`, `version`). They follow the script commands of §5.3 through the shader editor: the version label is the same 64-bit FNV-1a label, an edit is one undo step in the shader's tab, a tab with unsaved changes refuses an overwrite or an edit, and the shader resource the editor holds gets the new code, so every material using it follows. A new file gets a `.uid` file; an existing one keeps its uid.
- **Import parameters** (`import_ops.gd`, CA-15): `import_params` (`path`, `action` `get` or `set`, `params`, `copy_from`, `reimport`, `importer`). Only `ConfigFile` work and no editor class, so the server's headless path copies the same file into its throw-away project. A string value is parsed with `str_to_var` only into types whose text holds numbers or strings, so parsing never creates an object. A refusal writes nothing. The editor handler reimports through the editor's file system and reports the importer the reimport used.
- **Deletion** (`file_ops.gd`, CA-17): `delete_files` (`paths`, `force`, `allow_empty_dir`, `dry_run`, `trash`). Text files are searched for a path or uid of each file being deleted and binary resources are asked for their dependencies; the reference count per file is always complete. Protected paths and aliases are refused first; then tabs are closed (a tab with unsaved changes refuses the call), the files leave the resource cache and the editor file system, and they are moved to the system trash unless `trash` is false. Nothing is deleted while any path is refused.

## 6. Implementation Notes

- Known limitation: the owner-only permission on the token file is set through `FileAccess.set_unix_permissions` and has not been verified on a multi-user Linux host.
- Known limitation: the bridge processes commands on the editor's main thread; a synchronous re-import blocks other commands until it finishes.
- When `undo_redo` is not injected (only possible outside the plugin) mutations apply directly; the plugin always injects it.

## 7. Drawbacks & Alternatives

- **A GDExtension in Rust** for the editor side: shared types with the server, but a compiled artifact per platform and engine version; GDScript keeps the addon portable and dependency-free.

## Canonical References

| Alias | Path | Purpose |
| --- | --- | --- |
| `[PLUGIN]` | `addons/godot_mcp/plugin.gd` | Version gate and lifecycle |
| `[BRIDGE]` | `addons/godot_mcp/bridge_server.gd` | Listener and command handlers |
| `[EXPORT]` | `addons/godot_mcp/export_plugin.gd` | Export exclusion |
| `[CFG]` | `addons/godot_mcp/plugin.cfg` | Addon metadata and version |
| `[SCRIPTS]` | `addons/godot_mcp/editor/script_ops.gd` | Reading and editing a script through the script editor |
| `[DEVPROJ]` | `project.godot` | Development project enabling the addon |
| `[OVERRIDE]` | `addons/godot_mcp/core/runtime_override.gd` | override.cfg for editor Play, restored afterwards |
| `[PORT]` | `addons/godot_mcp/core/bridge_port.gd` | Port derived from the project path |
| `[SHADERS]` | `addons/godot_mcp/editor/shader_ops.gd` | Shader commands |
| `[IMPORT]` | `addons/godot_mcp/editor/import_ops.gd` | Import parameter edits, shared with the headless path |
| `[FILES]` | `addons/godot_mcp/editor/file_ops.gd` | File deletion |

## Document History

| Version | Date | Author | Description |
| --- | --- | --- | --- |
| 1.0.0 | 2026-10-16 | Agent (magic.analyze first-time) | Reverse-engineered from the addon at 94e4540; known limitations carried over from the 2026-10-14 QA report's open items. |
| 1.1.0 | 2026-10-20 | Agent (magic.spec) | [MODIFIED] Target, guard convention and BR-9 per release line; newer-minor warning. Stable → RFC → Stable after Post-Update Review. |
| 1.2.0 | 2026-10-21 | Agent (magic.spec) | [ADDED] The runtime, relay and override files, and the handler notes of the authoring commands. |
| 1.3.0 | 2026-10-21 | Agent (magic.spec) | [ADDED] The `GODOT_MCP_NO_BRIDGE` switch of the plugin. |
| 1.3.1 | 2026-10-21 | Agent (magic.spec) | Added the Play override script to Canonical References. No requirement changed. |
| 1.4.0 | 2026-10-21 | Agent (magic.run) | [ADDED] §5.3: the `read_script` and `edit_script` commands, the measured script-editor facts they rest on, the pairing rule between an editor and a path, the refusal while unsaved changes exist, and the new file `editor/script_ops.gd`. Stable → RFC → Stable after Post-Update Review. The canonical reference for the new file is added with the file. |
| 1.4.1 | 2026-10-21 | Agent (magic.run) | Added the script operations file to Canonical References now that it exists. No requirement changed. |
| 1.4.2 | 2026-10-21 | Agent (magic.run) | The tab-name check of the pairing rule applies only where the editor names its tab: the carry to the 4.5, 4.6 and 4.7 lines failed because those editors have no `_tab_name` metadata. No other requirement changed. |
| 1.5.0 | 2026-10-21 | Agent (magic.spec) | [MODIFIED] Handler notes: unset object properties, scene-only signal connections, missing folders created, settings key segments, a port clash shown as an editor notification, byte order mark on read, and the open measurement of the byte order mark on save. Source: simulation QA of 2026-10-21 (Q7, Q8, Q13, Q15, Q17, Q28). The line-ending difference of §5.3 stays as written. Stable → RFC → Stable after Post-Update Review. |
| 1.5.1 | 2026-10-21 | Agent (magic.spec) | §5.3: the byte order mark is reported when a save drops it and is not restored; the answer measures it instead of the specification guessing. No other requirement changed. |
| 1.6.0 | 2026-10-22 | Agent (magic.run) | [MODIFIED] `list_signals` answers objects with `target` and `method`; the byte order mark note records the measurement; the port clash message and toast as built. Stable → RFC → Stable after Post-Update Review. |
| 1.7.0 | 2026-10-29 | Agent (magic.spec) | [MODIFIED] BR-7: a missing `metadata/<name>` entry is created by `set_node_properties` inside the same undo action, with the identifier and non-null checks; the all-or-nothing rule is unchanged. Source: pull request 63. Stable → RFC → Stable after Post-Update Review. |
| 1.8.0 | 2026-10-29 | Agent (magic.spec) | [MODIFIED] BR-7: `:` resource steps in `set_node_properties` and `get_node_properties`, shared resources refused, the optional `properties` list of the read. Source: pull request 68. Stable → RFC → Stable after Post-Update Review. |
| 1.9.0 | 2026-10-29 | Agent (magic.spec) | [ADDED] the `close_editor` command: the unsaved check, the `discard` option, the process id in the answer and the quit after the reply. |
| 1.10.0 | 2026-10-31 | Agent (magic.spec) | [ADDED] BR-12: edits Godot would not save (inside an instance without editable children) are refused, or `make_editable` turns Editable Children on in the same action; removing a node an instance brings is refused. |
| 1.11.0 | 2026-11-02 | Agent (magic.spec) | Reality sync after ventilation. [ADDED] BR-13 (derived port and legacy listener, pull request 75), §5.4 (shader, import and file commands, pull requests 91, 100, 101, 60), the handler notes of `script_tab`, the InputMap commands (58), the relay and `open_scene` (94), four files and their references, the open gap of the settings command against CA-4. [MODIFIED] BR-2 (token for the legacy port), BR-7 (read-back refusal, pull request 88), BR-11 (more commands confine paths), `resave_resources` reloads loaded resources (87). Stable → RFC → Stable after Post-Update Review. |
| 1.11.1 | 2026-11-02 | Agent (magic.run) | [MODIFIED] The `set_project_setting` note describes the conversion as built and the open gap against CA-4 is closed: the editor path converts the text forms the file fallback converts. Source: phase 11. No other requirement changed. |
