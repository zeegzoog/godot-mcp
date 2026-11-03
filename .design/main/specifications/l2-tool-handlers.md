# Tool Handlers (Rust)

**Version:** 1.11.0
**Status:** Stable
**Layer:** implementation
**Implements:** l1-tool-catalog.md

## Overview

The Rust realization of the tool catalog: the static catalog with input schemas, argument validation, dispatch to the bridge or to host-side handlers, project discovery and path confinement, and the engine runner that validates scripts and launches detached editor and game processes.

## Related Specifications

- [l1-tool-catalog.md](l1-tool-catalog.md) - Parent concept.
- [l2-bridge-client-rust.md](l2-bridge-client-rust.md) - Transport for editor-backed tools.
- [l2-rust-stdio-server.md](l2-rust-stdio-server.md) - Session that calls `ToolManager::call_tool`.
- [l2-agent-feedback-rust.md](l2-agent-feedback-rust.md) - The feedback tools routed here and the outcome recording done after every call. [ADDED]
- [l2-game-runtime.md](l2-game-runtime.md) - The running-game and debugger tools routed to `tools/game.rs` and `tools/game/`. [ADDED]
- [l2-lsp-client.md](l2-lsp-client.md) - The language-server tools routed to `tools/code.rs`. [ADDED]
- [l2-held-stepping.md](l2-held-stepping.md) - The scenario tool routed to `tools/scenario.rs` (§5.8 there). [ADDED]
- [l1-content-authoring.md](l1-content-authoring.md) - CA-7 to CA-10: the script read and edit tools (§5.4); CA-11: the authoring fallbacks (§5.5); CA-12: project creation (§5.6); CA-14 to CA-17: shaders, import parameters, InputMap actions and deletion (§5.8 to §5.10). [MODIFIED]
- [l1-project-export.md](l1-project-export.md) - EX-1 to EX-7: the export tools (§5.12). [ADDED]

## 1. Motivation

Maps each safety rule of the catalog to the function that enforces it, so a new tool can reuse the same helpers instead of re-implementing path or identifier checks.

## 2. Constraints & Assumptions

- The catalog is built once (`OnceLock`) and shared by `tools/list` and dispatch.
- Engine time limits: `--version` 10 s, `--check-only` 30 s, the class check and the shader check 30 s; a one-off script 120 s by default (at most 600); an import or an export 600 s by default (1 to 3600) each. All use `kill_on_drop`; the long runs kill their process tree at the limit. [MODIFIED]
- [ADDED] A fixed engine script never receives caller text in its source or on its command line. It is written to a new directory under the operating system's temporary directory, removed afterwards. Either that directory is a throw-away project (an empty `project.godot`) and the script reads its request from `res://request.json` there (setting types, import parameters, export presets), or the script gets one value as a user argument after `--` (the class check, in its throw-away project; the shader check, run on the caller's project with the script kept outside it).
- Windows-only code uses `windows-sys` (`Win32_Foundation`, `Win32_System_Console`).
- Observed convention: every `unsafe` block carries a `SAFETY:` comment explaining why the call is sound.

## 4. Invariant Compliance

| L1 Invariant | Implementation |
| --- | --- |
| TL-1 Schema-first validation | `call_tool` runs `validate_arguments(tool.input_schema, args)` before dispatch: object-or-null arguments, required keys present and non-null, `string`/`boolean`/`object`/`array`/`number`/`integer` (whole floats accepted), array item types, enums, and `minimum`/`maximum` where the schema gives them (lines, columns, tick counts). [MODIFIED] Every schema built by `tool()` sets `additionalProperties: false`, and a non-null top-level key the schema does not declare is refused before any other check, with a message that names the unknown keys and lists the accepted ones (a tool without parameters says it takes none); an older parameter name stays accepted only by being declared in the schema (`file_path` of `godot_create_script`). Object-typed parameters (`steps`, `until`, `roi`) declare their fields in `properties` so a caller can read them from `tools/list`; the nested fields are checked by the side that uses them. [MODIFIED] |
| TL-2 Live-first fallback | `handle_get_version` falls back unless the editor returned `ToolExecution`; `handle_run_project` and `resolve_project` fall back only on `BridgeUnavailable`. |
| TL-3 Project resolution | `resolve_project` (`tools/project.rs`): `project_path` → editor `get_project_info.project_dir` → `find_project(cwd)`; `host_project` (launch, CLI run) skips the editor step. `find_project` walks ancestors, then accepts exactly one child with `project.godot`, else lists candidates. `ensure_project_dir` requires `project.godot`. The working directory is captured at start-up and overridable for tests. |
| TL-10 Named project wins | `ToolManager::editor_choice` asks the editor for its `project_dir` (`get_project_info`) before a live-first call that changes a file or a setting, and before any call that names a `project_path`; it is asked each time (one round trip), so an editor restarted with another project is never mistaken for the old one. The two directories are compared with `paths::same_directory` (canonical path, `\` read as `/`, no verbatim prefix or trailing separator, case folded on Windows). Equal or no `project_path`: the editor command is sent and its answer gets `project_dir`. Different: the tool takes its file path (`host_project`) and its answer names that directory. No editor (`BridgeUnavailable`): the call goes on and falls back as before. An editor that answers without a directory, or with an error, is not sent the call: the tool returns that reason. [MODIFIED] |
| TL-4 Confined, non-destructive writes | `resolve_in_project` strips `res://`, normalizes `\` to `/`, rejects `..` components (names containing `..` are fine), requires the nearest existing ancestor to canonicalize inside the project (blocks symlink escapes). `handle_create_script` opens with `create_new` unless `overwrite: true`, and flushes the file before reporting success. `godot_read_script` and `godot_edit_script` use the same path check and write only through a temporary file and a rename (§5.4). [MODIFIED] The shader tools write the same way (§5.8). An export replaces its output file by EX-4, after `output_file` refused a folder, a path in `.godot/` and a project file extension (§5.12). |
| TL-5 No injection | `is_valid_identifier` gates `extends_class` and `class_name`; script content comes from a fixed template; `godot_run_project` requires `scene` to start with `res://` and contain no `..`; a breakpoint location (`res://file.gd:LINE`) passes the same check before it is stored or sent; [MODIFIED] the bridge receives values only as JSON parameters. |
| TL-6 Detached launches | `spawn_detached`: null stdin/stdout/stderr; Windows `DETACHED_PROCESS \| CREATE_NEW_PROCESS_GROUP` and the server's own std handles marked non-inheritable before spawning; Unix `process_group(0)`. The child is not awaited. A managed process run (`GameHub::start`) takes the same process-group flags and a closed stdin, pipes its output into a bounded log instead of inheriting anything, and is killed when the hub is dropped. [MODIFIED] |
| TL-7 Hints by failure class | `bridge_error_result` maps `BridgeUnavailable`, `Bridge` and `ToolExecution`; `editor_error_hints` specializes on "unauthorized", "no active scene", and not-found messages; host-side errors carry their own hints (`PROJECT_PATH_HINTS`, overwrite, writable directory, `GODOT_PATH`). Runtime codes (`DENIED`, `UNSUPPORTED`, `NOT_FOUND`, `BAD_PARAMS`) and the editor's refusals about unsaved changes or a file that differs from the editor's text each map to their own hint; `Process` errors (a Godot executable that is missing or does not start) name `GODOT_PATH` instead of syntax advice; paths in messages are shown as `res://` paths. The catch-all "correct the arguments" hint is used for nothing that has a better one. [MODIFIED] |
| TL-11 Honest stop and status | `handle_stop_project` without `run_id` stops the managed runs first (`GameHub::stop_all`), then asks the editor `get_run_status` and sends `stop_project` only when it plays (an absent editor is not an error). The answer is `{message, stopped: {editor_play, runs}}`; `Nothing was running.` when it was so. A failure of the editor after runs were stopped is reported with their count. `handle_run_status` answers from the managed runs alone when no editor is connected (`running: false`, `editor: not connected (...)`, and `message: Nothing is running.` when no run is alive); `godot_run_output` with no run, no `run_id` and no editor answers `Nothing is running...` as text. [MODIFIED] |
| TL-8 Bounded engine calls | `get_version` and `validate_script` wrap the process in `timeout`; `strip_banner` drops `Godot Engine v…` lines; validation fails if the exit code is non-zero or stderr is non-empty. |
| TL-9 Editor stays in sync | After `godot_create_script` writes into the editor's own project (taken from the editor or named by a `project_path` the bridge reports as its `project_dir`), it sends `resave_resources` for the new `res://` path (best effort). |
| TL-12 Closing an editor | [ADDED] `tools/editor.rs`: an editor connected to the named project gets `close_editor`; its answer carries the editor's PID, which the tool waits on (`timeout_seconds`, 1 to 120, default 30). Without a bridge, `LaunchedEditors` keeps the editors `godot_launch_editor` started with their directories; only such an editor of the named project is stopped, with `discard`: SIGTERM on Unix so the engine can shut down, a kill when forced or on Windows; a process already reaped is never signalled, since its id may belong to another process. An exited launched editor is reaped so it never lingers as a zombie; an editor that reports another project is never asked to quit. |
| TL-13 Headless scripts keep references | [ADDED] §5.11. |
| TL-14 Older addon named, addon update | [ADDED] §5.13. |
| CA-14 to CA-17 | [ADDED] §5.8 to §5.10. |
| EX-1 to EX-7 | [ADDED] §5.12. |
| CA-12 Project creation | §5.6: validation before any write, `create_new` for `project.godot`, an escaped name. |

## 5. Detailed Design

### 5.1 Files

```plaintext
src/
├── tools.rs        ToolManager, catalog, dispatch, fallback, hints, the helpers every tool group shares
├── tools/          authoring.rs (scene, script, signal, resource and setting tools, project creation, and their file fallbacks), scripts.rs (reading and editing existing scripts), shaders.rs (§5.8), import_params.rs (§5.9), input_map.rs and files.rs (§5.10), export.rs (§5.12), editor.rs (closing an editor, TL-12), main_loop.rs (whether a script can be the main loop, §5.11), game.rs and game/ (routing, the log and method-call tools; the child modules hold the tool definitions, input steps, screenshots, recordings, the debugger tools and the way a call reaches the game), scenario.rs (the scenario tool), code.rs (language-server tools), feedback.rs [MODIFIED]
│                   and, as handlers of the ToolManager: run.rs (Play mode and managed runs: run, stop, status, the stale-import notice), headless.rs (script validation, one-off scripts and their repairs, resaving and the headless import, which run the engine binary), project.rs (which project a call acts on, and where a live-first call goes)
├── godot/          managed runs, debugger session, breakpoint control, Variant codec; and, for the host-side tools, imports.rs (sources whose import cache is older than the file), resource_audit.rs and binary_resource.rs (what a headless save dropped, §5.11) [MODIFIED]
├── lsp/            language-server client and framing
├── validation.rs   JSON-schema subset checks
├── paths.rs        is_valid_identifier, ensure_project_dir, find_project, resolve_in_project, same_directory
├── bridge_port.rs  the port derived from a project path (see l2-bridge-client-rust.md)
└── cli_runner.rs   CliRunner: get_version, validate_script, launch_editor, run_project, spawn_detached, run_script, the class, setting-type and shader checks, import_project, import_params, export_presets, export_project [MODIFIED]
```

### 5.2 Adding a Tool

1. Add the catalog entry with a complete schema (required keys, types, enums) and a description that states whether it needs the editor.
2. Write the failing test first in `tests/tools_test.rs` (host-side) or `tests/stdio_protocol_test.rs` / `tests/editor_e2e_test.rs` (bridge-backed).
3. Route the name in `call_tool`; for an editor command, add the handler and its `PARAM_TYPES` entry in the addon.
4. Reuse `resolve_in_project`, `is_valid_identifier` and `bridge_error_result`; never build engine arguments or script text from raw caller strings. A parameter that names a script is called `script_path`; `godot_create_script` takes `file_path` for compatibility and also accepts `script_path`. [MODIFIED]
5. Update the README tool table.

### 5.3 Feedback Hooks [ADDED]

- `call_tool` hands every outcome to the feedback journal after the handler has returned: a failed argument check as `invalid_arguments`, an error result as `error`, and a per-tool call and failure count. Calls to the three feedback tools are not recorded. Recording never alters the result.
- The three feedback tools are routed like host tools and appear in the catalog according to the configured feedback mode.
- Details, limits and the order of work are in [l2-agent-feedback-rust.md](l2-agent-feedback-rust.md) (5.2).

### 5.4 Script Tools

`godot_read_script` and `godot_edit_script` realize CA-7 to CA-10 of [l1-content-authoring.md](l1-content-authoring.md). Both are live-first: the editor commands `read_script` and `edit_script` (described in [l2-godot-editor-addon.md](l2-godot-editor-addon.md) §5.3) are tried first, and only `BridgeUnavailable` falls back to the file in the resolved project (`host_project`), as `create_scene` does; any other bridge error is returned as it is. They live in `tools/scripts.rs`.

| Tool | Parameters | Answer |
| --- | --- | --- |
| `godot_read_script` | `script_path` (required), `project_path` | `{path, text, source, dirty, version}`; in the file fallback `source` is `file`, `dirty` is false, and `editor_consulted` is false |
| `godot_edit_script` | `script_path`, `old_text`, `new_text` (required), `version`, `project_path` | `{path, replaced: 1, version, source}`; in the file fallback `source` is `file` and `editor_consulted` is false |

Shared rules, in the editor and in the file fallback:

- The path goes through `resolve_in_project` (TL-4), ends in `.gd`, names an existing file and the file is at most 1 MiB; a file that is not valid UTF-8 is refused, never converted. The edit tool never creates a file: a missing file is an error that points to `godot_create_script`.
- Version label: the 64-bit FNV-1a hash of the UTF-8 bytes of the text, with `\r\n` read as `\n`, written as 16 lowercase hex digits. No dependency is added on either side. Test vectors, one per side: the empty text gives `cbf29ce484222325`; `extends Node` followed by a newline gives `688cd8553db5c904`; `a` gives `af63dc4c8601ec8c`.
- Matching: `old_text` and the file text are compared with `\r\n` read as `\n`; `old_text` is not empty and differs from `new_text`; it must occur exactly once, otherwise the edit is refused with the count. A supplied `version` must equal the label of the current text, otherwise the edit is refused and the caller reads again. A refused edit leaves the file and the editor untouched.
- The caller's strings are data: they are matched and inserted as text, never executed, never interpolated into a command or into generated script text (TL-5).

File fallback only:

- The host cannot know that an editor holds unsaved typing when no bridge is connected, so CA-9 is about the editor only; the answer says `editor_consulted: false`.
- A file that uses `\r\n` keeps it: the result is written with `\r\n` for every line, and a leading UTF-8 byte order mark is kept.
- The new text is written to a temporary file in the same directory and renamed over the script, so no half-written script is ever seen (CA-10). The temporary file has an unpredictable name and is created exclusively (`create_new`), so a file or link planted at that name is never followed and the write cannot leave the project (TL-4). A script that is itself a symbolic link is not replaced: the edit is refused with the fix. When the write or the rename fails, the temporary file is removed and the script is unchanged.
- No lock is held across the bridge round-trip (TL-2); the fallback reads and writes through `tokio::fs`.

Refusals are `ToolResult` errors with at least one fix (the TL-7 rule): the count of matches, the stale label, the unsaved changes, the size limit, the extension.

### 5.5 Authoring Fallbacks [ADDED]

`godot_create_scene`, `godot_create_resource`, `godot_get_project_settings` and `godot_set_project_setting` write or read the file when no editor is connected, or when `project_path` names a project the editor does not have open (TL-10), and accept and refuse what the editor path does (CA-11).

- **Class check.** `CliRunner::class_kind` asks the engine whether a class is a `Node` class (a scene root) or a `Resource` class it can create. The class name first passes `is_valid_identifier`. The server writes an empty `project.godot` and the fixed script `class_check.gd` into a new directory under the operating system's temporary directory (never into the project, so no project file is read and no project code runs), runs `godot --headless --path <that directory> --script class_check.gd -- <Class>` with the 30 s limit of a syntax check, reads the line `GODOT_MCP_CLASS=scene|resource|no` and removes the directory. The class travels as a user argument and is never part of the script text (TL-5). A scene whose root is not a `Node` class and a resource that is not a `Resource` class are refused before a file is written, with the editor's words (`... is not a Node type`, `Unknown resource class: ...`). When the engine cannot be run, nothing is written: the answer says the class could not be checked and names `GODOT_PATH`. Measured on Godot 4.8-beta1: 0.4 s; a script outside the project runs under `--script`; `Node2D` and `Control` answer `scene`, `Curve`, `Gradient` and `Resource` answer `resource`, `Object`, `RefCounted` and an unknown name answer `no`.
- A scene's root name is data: the characters Godot forbids in a node name (`. : @ / " %` and control characters) become `_`, an empty name becomes `Root`, anything else is written as the quoted name (TL-5).
- A missing folder of a new file is created before the write (`create_dir_all`) and a failure names the file.
- A settings key is checked by `is_setting_key`, the rule of CA-3: letters, digits and `_ / . -`, a section first, no empty, `.` or `..` segment.
- The settings fallback answers `{source, project_dir, settings, truncated}`, with `settings` a map from key to the value's text without Godot's quoting (`hello`, not `"hello"`), as the editor path does.
- [ADDED] Setting types (CA-4). A setting stored in `project.godot` as a plain string, number or boolean keeps that type. For one not stored yet, or stored as another type, `CliRunner::setting_type` asks the engine in a throw-away project for the setting's type and text; the value is then converted (an HTML color or color name for a `Color`, the `project.godot` text such as `Vector2i(1152, 648)` for other types, never an Object, a whole number for an `int`) or refused with nothing written; an engine that cannot run refuses the call with a way out. A new entry goes after the last entry of its section, keeping the blank line before the next section header. The editor path applies the same rules ([l2-godot-editor-addon.md](l2-godot-editor-addon.md) §5.2).

### 5.6 Project Creation [ADDED]

`godot_create_project` realizes CA-12 in `tools/authoring.rs`. It takes `project_path` (absolute) and `name` (not blank, no NUL), validates both before it touches the disk, creates the directory, resolves it with `resolve_in_project` (TL-4) and creates `project.godot` with `create_new`, so an existing project is refused whatever raced it. The file holds `config_version=5`, the name as an escaped string value (the same escaping as a setting, TL-5) and the Compatibility renderer for desktop and mobile. The answer is `{project_path, created, name}`, where `created` is `res://project.godot`. It needs neither the engine nor the editor; the bridge addon and a main scene are added by the caller with the other authoring tools.

### 5.7 Removed

The number of the removed voxel model check is kept so that the references to the later sections stay valid.

### 5.8 Shader Tools [ADDED]

`tools/shaders.rs` realizes CA-14 like the script tools of §5.4: live-first through `create_shader`, `read_shader` and `edit_shader` ([l2-godot-editor-addon.md](l2-godot-editor-addon.md) §5.4), the file only on `BridgeUnavailable` or when `project_path` names another project (TL-10). The path ends in `.gdshader` or `.gdshaderinc` and goes through `resolve_in_project`; the version label, the matching and the 1 MiB limit are those of §5.4. The file fallback writes through a temporary file and a rename, refuses an existing file without `overwrite`, keeps a `\r\n` file's endings on edit, and gives a new file a `.uid` file (`ensure_uid_file`); a `.uid` it cannot write is an error that names the file. `godot_validate_shader` refuses a missing file and runs `CliRunner::check_shader`: the fixed `shader_check.gd` loads the shader (preprocessor, includes) and asks for its RID, so the headless renderer compiles it; a `Logger` subclass collects shader errors as `{file, line, message}`, printed after the marker `GODOT_MCP_SHADER=`. A `.gdshaderinc` is refused with the fix: Godot compiles an include only inside a shader, so the caller validates a `.gdshader` that includes it.

### 5.9 Import Parameters [ADDED]

`tools/import_params.rs` realizes CA-15. With an editor that has the project open (`editor_has_project`) the bridge's `import_params` command does the work and reimports through the editor's file system. Otherwise `CliRunner::import_params` copies the addon's `editor/import_ops.gd` into a throw-away project with a fixed driver script and the request in `res://request.json`, and `import_project_within` reimports headless. An `importer` is first checked against the built-in importers of the file type, because Godot replaces a name it lacks with the default; the importer the reimport used is checked after. The answer reports the importer before and after, the imported type, `new_options`, `ignored_options` and `reimported`; with `reimport: false` only the sidecar changes.

### 5.10 InputMap and Deletion [ADDED]

- `tools/input_map.rs` (CA-16) sends `get_input_map` and `edit_input_action` and has no fallback; without an editor the call fails with the editor-unreachable hints (TL-7). The event schema is published in full (`type` and its fields), so the caller does not guess key names.
- `tools/files.rs` (CA-17) sends `delete_files` when the editor has the project open. The fallback applies the same refusals (protected paths, companions whose file exists, directories, references by `res://` path or `uid://` unless `force`), deletes the files and their `.uid`, `.import` and `.godot/imported` data permanently, and answers `rescan_needed: true`. Both answer `deleted`, `refused` and `references`; with `dry_run`, `would_delete` and nothing deleted.

### 5.11 One-Off Scripts [ADDED]

`godot_run_script` (`headless.rs`) realizes TL-13.

1. **Check.** The script path goes through `resolve_in_project`; `main_loop_problem` follows the script's `extends` chain (at most 16 base scripts, global classes from `.godot/global_script_class_cache.cfg`) and refuses a script that does not extend `SceneTree` or `MainLoop`, because Godot 4.8 prints an error and exits with code 0 for one.
2. **Snapshot.** `TextResourceSnapshot::take` records every text scene and resource (uids, `[ext_resource]` uids) and the header uid of every binary one (`binary_resource.rs` reads `RSRC` and the zstd-compressed `RSCC` form). Files over 16 MiB are fingerprinted only.
3. **Run.** `CliRunner::run_script`: `--headless --path <project> -s <script>`, `user_args` after `--`, a timeout of 120 s by default (at most 600) after which the process tree is killed and the last lines are quoted. After a `SCRIPT ERROR`, a stretch of silence means the main loop is idle and the run is stopped (`stopped_idle`). `script_load_failure` turns a script Godot could not load into `load_error`.
4. **Repair.** `resource_audit.rs` compares the files with the snapshot: a rewritten text file gets its `uid=` and the `[ext_resource]` uids written back and its `shader_parameter/<name> = null` lines removed; a binary file that lost its header uid gets it back through `CliRunner::set_binary_uids` (a fixed script outside the project calling `ResourceSaver.set_uid`, the pairs in a JSON file), and the headers are read again to see which took. `resource_repairs` lists what was repaired, `resource_damage` what was not, with a fix.
5. **Refresh.** When an editor has the project open, the written files go to `resave_resources`, which also reloads what the editor holds; its answer is `editor_refresh`, a failure `editor_refresh_error`.
6. **Keep it short.** [ADDED] `RepairReport` bounds the answer: `resource_repairs` holds the first 20 repairs with the count of restored `[ext_resource]` uids instead of the uids, `resource_repairs_total` counts them all, every list of `editor_refresh` stops at 20 with `<list>_total` beside a cut one, and the summary and the `resource_damage` warning name at most three files and count the rest. `exit_code`, the output, `script_errors`, `load_error` and `resource_damage` itself are never cut.

`imports.rs` serves `godot_run_project` instead: the stale-import notice lists sources whose import cache is older than the file, since a process run does not reimport.

### 5.12 Export Tools [ADDED]

`tools/export.rs` realizes EX-1 to EX-7.

- **Presets.** `godot_list_export_presets` and `godot_set_export_preset` call `CliRunner::export_presets`: a fixed script edits `export_presets.cfg` with `ConfigFile` in a throw-away project, the request in `res://request.json`. Limits: names 128 characters, fields 4096, at most 128 options; an option value is a string, number or boolean and keeps its stored type. A new preset needs `platform` (the built-in names, or one an export plugin adds); an existing one must match its stored platform. `set` refuses while `editor_has_project` (EX-2). The answer lists `new_options`.
- **Export.** `godot_export_project`: `output_file` resolves the output (the preset's `export_path` by default) inside the project and refuses a folder, a path under `.godot/`, an extension of `SOURCE_EXTENSIONS` (`gd`, `tscn`, `scn`, `tres`, `res`, `godot`, `cfg`, `import`, `uid`, `gdshader`, `cs`) and, in pack mode, anything but `.pck` or `.zip`; the folder is created and Godot gets the absolute path, which never starts with `-`. `import_project_within` runs first unless `import` is false or the editor has the project open; then `CliRunner::export_project` runs `--export-pack`, `--export-release` or `--export-debug` with the preset name. The answer quotes up to 40 `ERROR`/`WARNING` lines and the last 12 KiB of each stream; a missing template, no file written or a timeout is an error with that log.

### 5.13 Addon Update [ADDED]

`build.rs` lists every file below `addons/godot_mcp` (names starting with `.` are skipped), embeds each with `include_bytes!` in a generated table and watches the folder, so a build carries the addon of its own sources. `addon.rs` holds that table (`FILES`) and `SERVER_VERSION`, the manifest version, which the addon's `plugin.cfg` carries too. `tools/addon_update.rs` realizes TL-14.

1. **Resolve.** `godot_update_addon` resolves the project (TL-3) and refuses the server's own source tree (`same_directory` with the manifest directory of the build).
2. **Compare.** `addon::plan` compares the files on disk with the table by content, because a `-dev` version number stays the same while the addon changes; a symbolic link in place of the folder is refused. The version the connected editor runs comes from `get_project_info` (`addon_version`); an editor of another project is ignored.
3. **Editor.** With an editor of that project connected: files and loaded version both equal to this build's make the call a no-op; otherwise it is refused, with the two ways out, unless `restart_editor` is true. Then the `close_editor` handler runs (`discard` passed through; its refusal ends the call with the editor still open and the addon untouched), the install follows, and the editor is launched again; a failed launch is reported as such after a finished update.
4. **Install.** `addon::install` writes the files into the hidden `addons/.godot_mcp.new` (Godot does not scan it), renames the old folder to `.godot_mcp.old` and the new one into place; a failed second rename puts the old folder back. A file the new addon lacks goes with the old folder.
5. **Enable.** `addon::enable_in` adds the manifest to `[editor_plugins] enabled` and keeps other plugins; a `project.godot` whose `enabled=` line has another shape is left unchanged.
6. **Explain.** `ToolManager::call_tool` passes every result through `explain_unknown_command`: an error holding `Unknown command type: X` becomes one that names the editor's addon version, the server's and the update call with the editor's project path. `godot_read_shader` and `godot_edit_shader` (§5.8) answer such an error from the shader file instead, with the same note and the fields `addon_version` and `server_version`.

## 7. Drawbacks & Alternatives

- **A full JSON-Schema validator crate**: more complete, but the catalog uses a small subset; the hand-written checker gives clearer messages and no extra dependency.

## Canonical References

| Alias | Path | Purpose |
| --- | --- | --- |
| `[TOOLS]` | `src/tools.rs` | Catalog and dispatch |
| `[SCHEMA]` | `src/validation.rs` | Argument validation |
| `[PATHS]` | `src/paths.rs` | Project discovery and confinement |
| `[ENGINE]` | `src/cli_runner.rs` | Engine runner and detached spawning |
| `[SCRIPTS]` | `src/tools/scripts.rs` | Script read and edit tools and their file fallback |
| `[RUN]` | `src/tools/run.rs` | Play mode and managed runs: run, stop, status, the stale-import notice |
| `[HEADLESS]` | `src/tools/headless.rs` | Engine-binary tools: validation, one-off scripts, resaving, headless import |
| `[PROJECT]` | `src/tools/project.rs` | Project resolution and live-first routing (TL-3, TL-10) |
| `[SCENARIO]` | `src/tools/scenario.rs` | The scenario tool (see [l2-held-stepping.md](l2-held-stepping.md) §5.8) |
| `[SHADERS]` | `src/tools/shaders.rs` | Shader tools (§5.8) |
| `[IMPORT]` | `src/tools/import_params.rs` | Import parameter tools (§5.9) |
| `[INPUT]` | `src/tools/input_map.rs` | InputMap tools (§5.10) |
| `[FILES]` | `src/tools/files.rs` | File deletion (§5.10) |
| `[EDITOR]` | `src/tools/editor.rs` | Closing an editor (TL-12) |
| `[ADDON]` | `src/addon.rs`, `build.rs` | The embedded addon, its comparison and installation (§5.13) |
| `[ADDON-UPDATE]` | `src/tools/addon_update.rs` | The update tool and the older-addon explanations (TL-14) |
| `[MAIN-LOOP]` | `src/tools/main_loop.rs` | Main-loop check of a one-off script (§5.11) |
| `[AUDIT]` | `src/godot/resource_audit.rs` | What a headless save dropped, and its repair (§5.11) |
| `[BINARY]` | `src/godot/binary_resource.rs` | The header uid of a binary scene or resource |
| `[IMPORTS]` | `src/godot/imports.rs` | Stale import caches |
| `[EXPORT]` | `src/tools/export.rs` | Export tools (§5.12) |
| `[ENGINE]` | `src/cli_runner.rs` | Engine runs and the fixed scripts |
| `[DOCS]` | `README.md` | User-facing tool table |
| `[PARITY]` | `docs/feature-parity.md` | Comparison of the tool set with other Godot MCP servers |

## Document History

| Version | Date | Author | Description |
| --- | --- | --- | --- |
| 1.0.0 | 2026-10-16 | Agent (magic.analyze first-time) | Reverse-engineered from the code at 94e4540. |
| 1.1.0 | 2026-10-16 | Agent (magic.spec) | [ADDED] Section 5.3 on the feedback hooks in dispatch and the catalog; link to the feedback implementation. |
| 1.2.0 | 2026-10-21 | Agent (magic.spec) | [ADDED] The tool modules and the engine-protocol modules in the file map, links to their specifications; [MODIFIED] TL-6 for managed process runs. |
| 1.2.1 | 2026-10-21 | Agent (magic.spec) | Added the feature parity document to Canonical References. No requirement changed. |
| 1.3.0 | 2026-10-21 | Agent (magic.run) | [ADDED] §5.4: the `godot_read_script` and `godot_edit_script` tools, their parameters and answers, the version label and its test vectors, the matching and line-ending rules, the file fallback and its atomic write. Stable → RFC → Stable after Post-Update Review. |
| 1.3.1 | 2026-10-21 | Agent (magic.run) | The script tools live in `tools/scripts.rs`, not `authoring.rs`, which would have passed 700 lines; the file is a canonical reference. No requirement changed. |
| 1.3.2 | 2026-10-21 | Agent (magic.run) | After the automated security review: the temporary file of the edit fallback is created exclusively under an unpredictable name, and a script that is a symbolic link is refused. No other requirement changed. |
| 1.4.0 | 2026-10-21 | Agent (magic.spec) | [ADDED] §5.5 (the authoring fallbacks, CA-11), TL-10 and TL-11 rows (the editor's project compared with `project_path`; stop and status answers); [MODIFIED] the TL-1, TL-5 and TL-7 rows and the naming step of §5.2. Source: simulation QA of 2026-10-21 (Q1, Q4, Q12, Q20, Q21, Q22, Q24, Q25, Q26). Stable → RFC → Stable after Post-Update Review. |
| 1.4.1 | 2026-10-21 | Agent (magic.spec) | §5.5: decided how the fallback checks a class (one bounded engine call with a fixed temporary script and the class as a user argument) and left the first step as a probe. No requirement changed. |
| 1.5.0 | 2026-10-22 | Agent (magic.run) | [MODIFIED] The TL-10 and TL-11 rows and §5.5 as built: the editor is asked each time instead of caching its directory; the class check runs in a throw-away project of its own and the probe's result is recorded; a scene root name is data; the settings fallback shape. Stable → RFC → Stable after Post-Update Review. |
| 1.6.0 | 2026-10-26 | Agent (magic.spec) | [MODIFIED] The file map and Canonical References: the handlers of the tool manager live in `run.rs`, `headless.rs` and `project.rs`; [ADDED] §5.6 (project creation) and §5.7 (the voxel model check) as merged. Stable → RFC → Stable after Post-Update Review. The running-game tools are in `tools/game.rs` and `tools/game/`. |
| 1.7.0 | 2026-10-29 | Agent (magic.spec) | [MODIFIED] TL-1: unknown argument keys are refused with the accepted names instead of ignored, and every schema says `additionalProperties: false`. Source: a `content` key passed to `godot_create_script` was dropped and the default template reported as created. Stable → RFC → Stable after Post-Update Review. |
| 1.8.0 | 2026-11-02 | Agent (magic.spec) | Reality sync after ventilation: modules merged without a specification. [ADDED] §5.8 shader tools (pull request 91), §5.9 import parameters (100, 101), §5.10 InputMap and deletion (58, 60), §5.11 one-off scripts and their repairs (31, 87, 99), §5.12 export tools (89), the TL-12, TL-13, CA-14 to CA-17 and EX rows, the fixed-script rule, the setting-type step of §5.5 (98), the file map and references. [MODIFIED] Engine time limits; TL-4 names the export output. Stable → RFC → Stable after Post-Update Review. |
| 1.8.1 | 2026-11-02 | Agent (magic.run) | The setting-type step no longer lists the editor path as an open gap: it converts the same forms. Source: phase 11. No requirement changed. |
| 1.9.0 | 2026-11-02 | Agent (magic.spec) | [ADDED] §5.13 (the addon embedded by `build.rs`, the update tool and the explanation of an unknown editor command) and its Canonical References; TL-14 in the compliance table. Source: pull request 102. Stable → RFC → Stable after Post-Update Review. |
| 1.10.0 | 2026-11-02 | Agent (magic.spec) | [ADDED] §5.11 step 6: the bounds of the repair report (twenty listed repairs and `resource_repairs_total`, cut `editor_refresh` lists with totals, at most three named files in a summary). Source: pull request 103. Stable → RFC → Stable after Post-Update Review. |
| 1.11.0 | 2026-11-02 | Agent (magic.spec) | [REMOVED] §5.7 (the voxel model check) with its compliance rows, file map entry and Canonical Reference; the section number stays as a stub. Stable → RFC → Stable after Post-Update Review. |
