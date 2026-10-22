# Tool Handlers (Rust)

**Version:** 1.5.0
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
- [l2-game-runtime.md](l2-game-runtime.md) - The running-game and debugger tools routed to `tools/game.rs`. [ADDED]
- [l2-lsp-client.md](l2-lsp-client.md) - The language-server tools routed to `tools/code.rs`. [ADDED]
- [l1-content-authoring.md](l1-content-authoring.md) - CA-7 to CA-10: the script read and edit tools (§5.4); CA-11: the authoring fallbacks (§5.5).

## 1. Motivation

Maps each safety rule of the catalog to the function that enforces it, so a new tool can reuse the same helpers instead of re-implementing path or identifier checks.

## 2. Constraints & Assumptions

- The catalog is built once (`OnceLock`) and shared by `tools/list` and dispatch.
- Engine time limits: `--version` 10 s, `--check-only` 30 s; both use `kill_on_drop`.
- Windows-only code uses `windows-sys` (`Win32_Foundation`, `Win32_System_Console`).
- Observed convention: every `unsafe` block carries a `SAFETY:` comment explaining why the call is sound.

## 4. Invariant Compliance

| L1 Invariant | Implementation |
| --- | --- |
| TL-1 Schema-first validation | `call_tool` runs `validate_arguments(tool.input_schema, args)` before dispatch: object-or-null arguments, required keys present and non-null, `string`/`boolean`/`object`/`array`/`number`/`integer` (whole floats accepted), array item types, enums, and `minimum`/`maximum` where the schema gives them (lines, columns, tick counts); unknown keys are ignored. Object-typed parameters (`steps`, `until`, `roi`) declare their fields in `properties` so a caller can read them from `tools/list`; the nested fields are checked by the side that uses them. [MODIFIED] |
| TL-2 Live-first fallback | `handle_get_version` falls back unless the editor returned `ToolExecution`; `handle_run_project` and `resolve_project` fall back only on `BridgeUnavailable`. |
| TL-3 Project resolution | `resolve_project`: `project_path` → editor `get_project_info.project_dir` → `find_project(cwd)`; `host_project` (launch, CLI run) skips the editor step. `find_project` walks ancestors, then accepts exactly one child with `project.godot`, else lists candidates. `ensure_project_dir` requires `project.godot`. The working directory is captured at start-up and overridable for tests. |
| TL-10 Named project wins | `ToolManager::editor_choice` asks the editor for its `project_dir` (`get_project_info`) before a live-first call that changes a file or a setting, and before any call that names a `project_path`; it is asked each time (one round trip), so an editor restarted with another project is never mistaken for the old one. The two directories are compared with `paths::same_directory` (canonical path, `\` read as `/`, no verbatim prefix or trailing separator, case folded on Windows). Equal or no `project_path`: the editor command is sent and its answer gets `project_dir`. Different: the tool takes its file path (`host_project`) and its answer names that directory. No editor (`BridgeUnavailable`): the call goes on and falls back as before. An editor that answers without a directory, or with an error, is not sent the call: the tool returns that reason. [MODIFIED] |
| TL-4 Confined, non-destructive writes | `resolve_in_project` strips `res://`, normalizes `\` to `/`, rejects `..` components (names containing `..` are fine), requires the nearest existing ancestor to canonicalize inside the project (blocks symlink escapes). `handle_create_script` opens with `create_new` unless `overwrite: true`, and flushes the file before reporting success. `godot_read_script` and `godot_edit_script` use the same path check and write only through a temporary file and a rename (§5.4). |
| TL-5 No injection | `is_valid_identifier` gates `extends_class` and `class_name`; script content comes from a fixed template; `godot_run_project` requires `scene` to start with `res://` and contain no `..`; a breakpoint location (`res://file.gd:LINE`) passes the same check before it is stored or sent; [MODIFIED] the bridge receives values only as JSON parameters. |
| TL-6 Detached launches | `spawn_detached`: null stdin/stdout/stderr; Windows `DETACHED_PROCESS \| CREATE_NEW_PROCESS_GROUP` and the server's own std handles marked non-inheritable before spawning; Unix `process_group(0)`. The child is not awaited. A managed process run (`GameHub::start`) takes the same process-group flags and a closed stdin, pipes its output into a bounded log instead of inheriting anything, and is killed when the hub is dropped. [MODIFIED] |
| TL-7 Hints by failure class | `bridge_error_result` maps `BridgeUnavailable`, `Bridge` and `ToolExecution`; `editor_error_hints` specializes on "unauthorized", "no active scene", and not-found messages; host-side errors carry their own hints (`PROJECT_PATH_HINTS`, overwrite, writable directory, `GODOT_PATH`). Runtime codes (`DENIED`, `UNSUPPORTED`, `NOT_FOUND`, `BAD_PARAMS`) and the editor's refusals about unsaved changes or a file that differs from the editor's text each map to their own hint; `Process` errors (a Godot executable that is missing or does not start) name `GODOT_PATH` instead of syntax advice; paths in messages are shown as `res://` paths. The catch-all "correct the arguments" hint is used for nothing that has a better one. [MODIFIED] |
| TL-11 Honest stop and status | `handle_stop_project` without `run_id` stops the managed runs first (`GameHub::stop_all`), then asks the editor `get_run_status` and sends `stop_project` only when it plays (an absent editor is not an error). The answer is `{message, stopped: {editor_play, runs}}`; `Nothing was running.` when it was so. A failure of the editor after runs were stopped is reported with their count. `handle_run_status` answers from the managed runs alone when no editor is connected (`running: false`, `editor: not connected (...)`, and `message: Nothing is running.` when no run is alive); `godot_run_output` with no run, no `run_id` and no editor answers `Nothing is running...` as text. [MODIFIED] |
| TL-8 Bounded engine calls | `get_version` and `validate_script` wrap the process in `timeout`; `strip_banner` drops `Godot Engine v…` lines; validation fails if the exit code is non-zero or stderr is non-empty. |
| TL-9 Editor stays in sync | After `godot_create_script` writes into the editor's own project, it sends `resave_resources` for the new `res://` path (best effort). |

## 5. Detailed Design

### 5.1 Files

```plaintext
src/
├── tools.rs        ToolManager, catalog, dispatch, fallback, hints
├── tools/          authoring.rs (scene, script, signal, resource and setting tools and their file fallbacks), scripts.rs (reading and editing existing scripts), game.rs (running game and debugger tools), code.rs (language-server tools), feedback.rs
├── godot/          managed runs, debugger session, breakpoint control, Variant codec
├── lsp/            language-server client and framing
├── validation.rs   JSON-schema subset checks
├── paths.rs        is_valid_identifier, ensure_project_dir, find_project, resolve_in_project
└── cli_runner.rs   CliRunner: get_version, validate_script, launch_editor, run_project, spawn_detached
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
