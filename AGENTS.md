# Agent Guidelines & Engineering Standards

This document establishes the architectural principles, development workflow, and operational rules for AI agents and human contributors developing or interacting with the Godot MCP Server.

The contribution workflow — branches per Godot version, carrying changes between them, commit and pull request rules — is in [CONTRIBUTING.md](CONTRIBUTING.md). The verification commands below are the same as its Verification section; change both together.

## Mission & Scope

The primary objective of this project is to build an uncompromising, ultra-fast, cross-platform Model Context Protocol (MCP) server providing AI agents with full, reliable control over the Godot Game Engine (4.x+) across Windows, Linux, and macOS.

Agents must treat the Godot Editor as a live, stateful environment while ensuring complete safety, non-destructive mutations, and immediate visual/diagnostic feedback.

## Architecture Guidelines

### 1. Dual-Mode Operation

- **Live Mode (Primary)**: Connect to the Godot Editor bridge via WebSocket on `GODOT_DEBUG_PORT` (default `9333`). All scene, node, and property mutations must execute in real time. The bridge listens on loopback only and every command carries the shared token (`<user data dir>/godot-mcp/bridge-<port>.token` or `GODOT_MCP_TOKEN`); never add a code path that skips either.
- **Headless Mode (Fallback)**: When the editor GUI is not running, fall back to headless CLI invocation (`godot --headless --script ...`) for batch operations, scene scaffolding, and automated testing. Never fail silently if the editor socket is temporarily unreachable; provide clear diagnostic context and suggest launching the editor or falling back to CLI.

### 2. Safety & Undo/Redo Integration

- Every mutation executed within the Godot Editor (adding nodes, altering properties, removing branches) MUST be wrapped in `EditorUndoRedoManager`.
- This ensures human developers retain full visibility and the ability to undo any action performed by an AI agent using standard shortcuts (`Ctrl+Z`).
- Never overwrite scene files (`.tscn`) or resource files (`.tres`) directly on disk when the editor is actively holding those resources in memory without notifying the editor or triggering a resource re-import.

### 3. Cross-Platform Path Handling & Security

- **Path Traversal**: Strictly validate all file paths against directory traversal (`../`). Paths must resolve within the target project's `res://` directory or designated export folders. Host-side file access goes through `src/paths.rs` (`resolve_in_project`); editor-side paths go through `to_res_path` and `find_scene_node` in `bridge_server.gd`.
- **Argument Injection**: Never pass user-controlled strings to the Godot CLI as bare arguments (a value starting with `-` becomes an engine option) or interpolate them into generated GDScript. Validate identifiers with `is_valid_identifier` and require `res://` for scene arguments.
- **Multi-OS Normalization**: Always handle differences in path separators across Windows (`\`), Linux (`/`), and macOS (`/`). Normalize paths before passing them to the engine or returning them via the protocol.
- **Identifier Sanitization**: Validate node names and class identifiers using strict alphanumeric patterns (`^[A-Za-z_][A-Za-z0-9_]*$`).
- **Resource Integrity**: Maintain Godot UID tracking (`.uid` files and `uid://` references) whenever modifying scenes or assets to prevent broken dependencies in Godot 4.x.

## Implementation Standards

### Core Server Standards (Rust)

- **Zero-Overhead Async I/O**: Use Tokio event loops for stdio JSON-RPC transport and WebSocket bridge communication.
- **Error Handling**: Never use unhandled panics (`unwrap()` / `expect()`) in production code paths. Use explicit `Result` propagation with informative error types.
- **Clean Stdio Separation**: All internal logging and diagnostics MUST be emitted to `stderr` using `tracing`. The `stdout` stream is strictly reserved for MCP JSON-RPC frames. Every child process spawned by the server (editor, game) gets null stdio.
- **Concurrency**: Tool calls run as independent tasks so a slow editor command never blocks `ping` or other calls; `notifications/cancelled` aborts a running call. Never hold a lock across an editor round-trip.
- **Standardized MCP Responses**: All errors must return an informative message and at least one actionable recovery solution.

### GDScript Standards (Editor Plugin)

- Target the Godot minor version of the branch's release line (`godot-vM.N`; `master` targets the next version) with modern GDScript and strict typing (`var count: int = 0`, `func get_node_by_id(id: String) -> Node:`). `plugin.gd` refuses to start on an older editor and warns on a newer one. The line's identity (`MIN_MAJOR`/`MIN_MINOR`, the version string, `config/features` in the dev `project.godot`, `.github/godot-version`) is checked by `tests/version_sync_test.rs`; the list is in CONTRIBUTING.md.
- Anything missing from the line's first Godot release (M.N.0) must be called through `has_method()` + `call()` (see `is_import_running` on lines before 4.7): an unguarded call to a method the running engine lacks is a script error that sends no reply, and the analyzer only warns about it. Run the e2e tests with the Godot build named in `.github/godot-version`; CI uses the same build.
- Read command parameters untyped and validate them (see `PARAM_TYPES`): assigning a wrongly typed value to a typed variable is a script error that sends no reply and leaves the caller waiting for its timeout.
- Utilize standard signals for asynchronous event handling.
- Keep the plugin lightweight with zero external third-party dependencies outside the standard Godot API.

## Quality Assurance & Verification Protocols

Before submitting or committing changes, agents must execute and pass the complete verification pipeline:

1. **Static Analysis & Linting**:

   ```bash
   cargo clippy --all-targets --all-features -- -D warnings
   ```

2. **Code Formatting**:

   ```bash
   cargo fmt --all -- --check
   ```

3. **Automated Testing**:

   ```bash
   cargo test --all-targets
   ```

   Includes `tests/version_sync_test.rs`: the version in `addons/godot_mcp/plugin.cfg` must always equal `Cargo.toml` — bump both together — and every file naming the line's Godot version must agree with the version's `+godot.M.N`. New behaviour needs a test first: protocol and tool behaviour in `tests/stdio_protocol_test.rs` / `tests/tools_test.rs` (helpers in `tests/common/`), bridge client in `tests/bridge_client_test.rs`, keeping the addon out of exports in `tests/addon_export_test.rs`, launched processes staying independent of the server in `tests/detach_test.rs`.

4. **GDScript Syntax Check**:

   The repo root has its own minimal `project.godot` (it also makes the addon loadable as a plugin for manual testing). The addon declares no `class_name` on purpose: a global class would be listed in every exported game's class list while its script is left out of the export, so keep addon scripts free of `class_name` and load them by path.

   ```bash
   godot --headless --path . --import   # first run only, or after script changes
   godot --headless --check-only --path . --script addons/godot_mcp/plugin.gd
   godot --headless --check-only --path . --script addons/godot_mcp/bridge_server.gd
   godot --headless --check-only --path . --script addons/godot_mcp/export_plugin.gd
   ```

5. **Editor End-to-End Tests** (whenever `addons/godot_mcp/` changed; needs the Godot build named in `.github/godot-version`, ignored by default):

   ```bash
   GODOT_PATH=/path/to/godot cargo test --test editor_e2e_test -- --ignored
   GODOT_PATH=/path/to/godot cargo test --test addon_export_test -- --ignored
   ```

   Set `GODOT_CONSOLE_PATH` as well on Windows to get editor logs in failure output (the editor is started from the console binary when it is set, and `.env` values fill in variables you did not export, so set both when testing a specific Godot version). A `.env` file with double-quoted Windows paths does not load (see `.env.example`) — export the variables instead.

## Tool Invocation Protocols for AI Agents

When interacting with a Godot project, agents must adhere to the following workflow:

1. **Inspection Before Mutation**:
   - Always query `godot_get_scene_tree` or `godot_get_node_properties` before attempting to modify, add, or delete nodes.
   - Confirm parent node paths exist before instantiating child nodes. Node paths are relative to the scene root.
   - `godot_set_node_properties` is all-or-nothing and returns the applied values; read them back instead of assuming.

2. **Validation After Modification**:
   - Run `godot_validate_script` immediately after creating or altering any `.gd` file. Without `project_path`, `godot_create_script` uses the editor's project, else the project the server was started in; it refuses to overwrite without `overwrite: true`.
   - Catch and resolve syntax errors before attempting to run scenes.

3. **Visual Verification**:
   - Use `godot_capture_viewport` to visually inspect scene changes, layouts, and rendering quality when performing level design or UI assembly.

4. **Runtime Monitoring**:
   - After `godot_run_project`, check `godot_get_run_status` and stop the run with `godot_stop_project` when done.
   - Runtime log capture (`godot_get_debug_log`) and in-game camera capture (`godot_capture_camera`) are on the roadmap, not implemented — do not call them.

5. **Feedback to the Maintainers**:
   - When godot-mcp itself blocks or misleads you (a tool fails for a reason its message does not explain, a needed capability has no tool, a description or schema is wrong), record it with `godot_note_feedback` in English: what you tried, what came back, what you expected, one problem per note. Do not note your own mistakes or problems in the user's project.
   - When the request is completed or abandoned, and notes exist, call `godot_draft_feedback`, show the user the complete report, and offer: copy it, open the link, submit it, or discard it.
   - Never call `godot_submit_feedback` without the user's explicit yes in the conversation.

## Completion Protocol (Mandatory Checklist)

Before concluding any development task, every agent MUST verify the following checklist:

- [ ] **Technical Language**: All code, identifiers, comments, and technical docs are in English.
- [ ] **Verification Pipeline**: `cargo clippy`, `cargo fmt --check`, and `cargo test --all-targets` pass; the GDScript syntax check passes for every addon script.
- [ ] **Editor End-to-End Tests**: If `addons/godot_mcp/` changed, `editor_e2e_test` and `addon_export_test` pass with the Godot build named in `.github/godot-version`, on every branch the change lands on.
- [ ] **Tests First**: New behaviour is covered by a test in the matching `tests/` file.
- [ ] **Version Sync**: The version in `addons/godot_mcp/plugin.cfg` equals the one in `Cargo.toml`.
- [ ] **Undo/Redo**: Every editor mutation is wrapped in `EditorUndoRedoManager`, and no `.tscn` / `.tres` held by the editor is overwritten on disk behind its back.
- [ ] **Path and Input Safety**: File paths go through `resolve_in_project` / `to_res_path`, identifiers are validated, and no user-controlled string reaches the Godot CLI as a bare argument or is interpolated into generated GDScript.
- [ ] **Bridge Security**: The bridge still listens on loopback only, and no code path skips the token check.
- [ ] **Rust Hygiene**: No `unwrap()` / `expect()` in production paths, diagnostics go to `stderr` only, and no lock is held across an editor round-trip.
- [ ] **Error Responses**: Every error carries an informative message and at least one actionable recovery step.
- [ ] **GDScript Hygiene**: Strict typing is used, parameters are read untyped and validated, methods missing from the line's M.N.0 are called through `has_method()` + `call()`, and the addon declares no `class_name`.
- [ ] **Policy Documents**: `SECURITY.md` and `CONTRIBUTING.md` are still true after this change (supported lines, reporting channel, checks, branch model).
- [ ] **Agent Workflow**: Scenes were inspected before mutation, `.gd` files were validated after changes, and any run started with `godot_run_project` was stopped.
