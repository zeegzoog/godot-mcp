# Agent Guidelines & Engineering Standards

This document establishes the architectural principles, development workflow, and operational rules for AI agents and human contributors developing or interacting with the Godot MCP Server.

The contribution workflow — branches per Godot version, carrying changes between them, commit and pull request rules — is in [CONTRIBUTING.md](CONTRIBUTING.md). The verification commands below are the same as its Verification section; change both together.

Work on this repository's issues and pull requests — triage, replies, reviews, merges, the board and release readiness — follows the repo-steward skill, [.agents/skills/repo-steward/SKILL.md](.agents/skills/repo-steward/SKILL.md); its autonomy settings bound what an agent posts, closes or merges without asking.

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
- **Identifier Sanitization**: Validate class identifiers (`extends_class`, `class_name`, `root_type`, resource classes) using the strict pattern `^[A-Za-z_][A-Za-z0-9_]*$`. A node name is data, not an identifier: it reaches the engine only as a value (never inside generated source or a command line), and Godot replaces the characters it forbids and reports the final name.
- **Resource Integrity**: Maintain Godot UID tracking (`.uid` files and `uid://` references) whenever modifying scenes or assets to prevent broken dependencies in Godot 4.x.

## Implementation Standards

### File Names

- Files that GitHub looks up by name in the repository root or in `.github/` are capitalized: `README.md`, `CONTRIBUTING.md`, `SECURITY.md`, `PULL_REQUEST_TEMPLATE.md`. The `ISSUE_TEMPLATE/` folder is capitalized as GitHub writes it.
- Every other file name is lowercase, and several words are joined with hyphens: `agent-feedback.md`, `steward-priorities.md`, `release-notes.sh`. The issue forms keep the underscores GitHub generates (`ISSUE_TEMPLATE/bug_report.yml`). A name a tool requires keeps its spelling (`dependabot.yml`, `config.yml`, `workflows/`).

### Core Server Standards (Rust)

- **Zero-Overhead Async I/O**: Use Tokio event loops for stdio JSON-RPC transport and WebSocket bridge communication.
- **Error Handling**: Never use unhandled panics (`unwrap()` / `expect()`) in production code paths. Use explicit `Result` propagation with informative error types.
- **Clean Stdio Separation**: All internal logging and diagnostics MUST be emitted to `stderr` using `tracing`. The `stdout` stream is strictly reserved for MCP JSON-RPC frames. No child process spawned by the server inherits its standard streams: an editor launch gets null stdio, and a managed game run has its output piped into a bounded log.
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

   Includes `tests/version_sync_test.rs`: the version in `addons/godot_mcp/plugin.cfg` must always equal `Cargo.toml` — bump both together — and every file naming the line's Godot version must agree with the version's `+godot.M.N`. New behaviour needs a test first: protocol and tool behaviour in `tests/stdio_protocol_test.rs` / `tests/tools_test.rs` (helpers in `tests/common/`), the agent feedback channel in `tests/feedback_test.rs`, bridge client in `tests/bridge_client_test.rs`, editor commands in `tests/editor_e2e_test.rs`, the repo steward in `tests/steward_test.rs`, keeping the addon out of exports in `tests/addon_export_test.rs`, launched processes staying independent of the server in `tests/detach_test.rs`, and a running game in `tests/runtime_e2e_test.rs`.

4. **GDScript Syntax Check**:

   The repo root has its own minimal `project.godot` (it also makes the addon loadable as a plugin for manual testing). The addon declares no `class_name` on purpose: a global class would be listed in every exported game's class list while its script is left out of the export, so keep addon scripts free of `class_name` and load them by path.

   ```bash
   godot --headless --path . --import   # first run only, or after script changes
   godot --headless --check-only --path . --script addons/godot_mcp/plugin.gd
   godot --headless --check-only --path . --script addons/godot_mcp/bridge_server.gd
   godot --headless --check-only --path . --script addons/godot_mcp/export_plugin.gd
   godot --headless --check-only --path . --script addons/godot_mcp/editor/game_relay.gd
   godot --headless --check-only --path . --script addons/godot_mcp/editor/script_ops.gd
   godot --headless --check-only --path . --script addons/godot_mcp/core/runtime_override.gd
   godot --headless --check-only --path . --script addons/godot_mcp/runtime/mcp_runtime.gd
   godot --headless --check-only --path . --script addons/godot_mcp/runtime/ops/rt_hold.gd
   godot --headless --check-only --path . --script addons/godot_mcp/runtime/ops/rt_input.gd
   godot --headless --check-only --path . --script addons/godot_mcp/runtime/ops/rt_live.gd
   godot --headless --check-only --path . --script addons/godot_mcp/runtime/ops/rt_output.gd
   godot --headless --check-only --path . --script addons/godot_mcp/runtime/ops/rt_result.gd
   godot --headless --check-only --path . --script addons/godot_mcp/runtime/ops/rt_view.gd
   godot --headless --check-only --path . --script addons/godot_mcp/runtime/ops/rt_record.gd
   ```

5. **Editor End-to-End Tests** (whenever `addons/godot_mcp/` changed; needs the Godot build named in `.github/godot-version`, ignored by default). Run the ignored end-to-end suites with `--test-threads=1`: windowed tests and process-heavy tests interfere when they run in parallel.

   ```bash
   GODOT_PATH=/path/to/godot cargo test --test editor_e2e_test -- --ignored --test-threads=1
   GODOT_PATH=/path/to/godot cargo test --test addon_export_test -- --ignored --test-threads=1
   ```

   Set `GODOT_CONSOLE_PATH` as well on Windows to get editor logs in failure output (the editor is started from the console binary when it is set, and `.env` values fill in variables you did not export, so set both when testing a specific Godot version). A `.env` file with double-quoted Windows paths does not load (see `.env.example`) — export the variables instead.

6. **Running game** (whenever the running-game tools, the debugger client, or the GDScript language-server client changed; same Godot build, ignored by default):

   ```bash
   GODOT_PATH=/path/to/godot cargo test --test runtime_e2e_test -- --ignored --test-threads=1
   ```

## Tool Invocation Protocols for AI Agents

When interacting with a Godot project, agents must adhere to the following workflow:

1. **Inspection Before Mutation**:
   - Always query `godot_get_scene_tree` or `godot_get_node_properties` before attempting to modify, add, or delete nodes.
   - Confirm parent node paths exist before instantiating child nodes. Node paths are relative to the scene root.
   - `godot_set_node_properties` is all-or-nothing and returns the applied values; read them back instead of assuming.

2. **Validation After Modification**:
   - Run `godot_validate_script` immediately after creating or altering any `.gd` file. Without `project_path`, `godot_create_script` uses the editor's project, else the project the server was started in; it refuses to overwrite without `overwrite: true`.
   - To change an existing script, read it with `godot_read_script` and replace one fragment with `godot_edit_script`, passing the `version` you read. Do not rewrite the file behind the editor's back. An edit is refused while the editor holds unsaved changes to the script: ask the person to save or discard them, then read again.
   - Catch and resolve syntax errors before attempting to run scenes.

3. **Visual Verification**:
   - Use `godot_capture_viewport` to visually inspect scene changes, layouts, and rendering quality when performing level design or UI assembly.

4. **Runtime Monitoring**:
   - After `godot_run_project`, check `godot_get_run_status` and stop the run with `godot_stop_project` when done: without `run_id` it stops the editor's Play mode and every managed run, and lists what it stopped.
   - Read logs and errors with `godot_run_output`. Inspect the live tree with `godot_game_tree` before `godot_game_set` or `godot_game_call`.
   - Capture the game with `godot_game_screenshot`. A headless game returns an error and no image; do not treat that error as a picture. For a series of frames on disk, use `godot_game_record`.
   - Pass game flags with `godot_run_project` `user_args` and `mode: "process"`. They go after `--`. Do not put Godot engine options there.
   - `godot_game_call` runs only a method named in `permit`, and never one that quits, evaluates, calls another method by name, sets a property by name or frees or replaces a node.
   - To act, advance a known amount and look, hold the game with `godot_game_physics` (`hold`, then `step`, `resume`), starting it with `godot_run_project` `held: true` when the steps must be exact. Check `deterministic` in the answers. A held game refuses waits, signal watches, screenshot sheets, recordings and timed input; step instead. Resume or stop the run when done: a held game does not advance on its own.
   - For a repeatable play-through, use `godot_game_scenario` instead of writing a GDScript test. It presses Input Map actions and pointers, advances a known number of physics ticks, and returns that call's snapshots, warnings and errors. The schema, the determinism limits and an example are in [docs/game-checks.md](docs/game-checks.md).

5. **Feedback to the Maintainers**:
   - When godot-mcp itself blocks or misleads you (a tool fails for a reason its message does not explain, a needed capability has no tool, a description or schema is wrong), record it with `godot_note_feedback` in English: what you tried, what came back, what you expected, one problem per note. Do not note your own mistakes or problems in the user's project.
   - When the request is completed or abandoned, and notes exist, call `godot_draft_feedback`, show the user the complete report, and offer: copy it, open the link, submit it, or discard it.
   - Never call `godot_submit_feedback` without the user's explicit yes in the conversation.

## Completion Protocol (Mandatory Checklist)

Before concluding any development task, every agent MUST verify the following checklist:

- [ ] **Technical Language**: All code, identifiers, comments, and technical docs are in English.
- [ ] **Verification Pipeline**: `cargo clippy`, `cargo fmt --check`, and `cargo test --all-targets` pass; the GDScript syntax check passes for every addon script.
- [ ] **Editor End-to-End Tests**: If `addons/godot_mcp/` changed, `editor_e2e_test` and `addon_export_test` pass with the Godot build named in `.github/godot-version`, on every branch the change lands on.
- [ ] **Runtime End-to-End Tests**: If the running game, the debugger client, or the GDScript language-server client changed, `runtime_e2e_test` passes with that same Godot build.
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
