# Feature parity

This matrix describes the Rust server after the companion pull requests in this
series are merged (scene reload, debugger client, running game, breakpoints,
GDScript language server, and editor authoring). On `master` alone, only the
editor bridge from before that series is present.

Comparison of this Rust server with the three Godot MCP servers it was measured against.
A cell is **implemented** only when the behavior exists in this tree and a test or a
real Godot 4.8.beta1 run checked it. **Partial** means the code is in the tree but the
check was narrower than the feature. **Documented-only** means a reference describes
it and this server does not do it. **Unsupported** means it was left out on purpose.

## The four implementations

| | Rust (this repo) | shin2344234/godot-mcp | Coding-Solo/godot-mcp | Vollkorn-Games/godot-mcp |
| --- | --- | --- | --- | --- |
| Path / remote | `github.com/zeegzoog/godot-mcp`, fork `github.com/theromis/godot-mcp` | `github.com/shin2344234/godot-mcp` `@shin2344234/godot-mcp` 0.1.2, commit `7116e47` | commit `1209744` | commit `6325306` |
| Runtime | Native binary. No Node.js. | Node.js 22, TypeScript | Node.js, TypeScript | Node.js, TypeScript (fork of Coding-Solo plus a large interactive layer) |
| Godot | This branch's plugin requires 4.8 (`MIN_MINOR = 8`). Editor, export, and runtime checks below used `4.8.beta1.official.4d10419ca`. | 4.7+ (docs say 4.7.2) | Godot 4 CLI | Godot 4 CLI, same operations script family as Coding-Solo |
| Editor transport | WebSocket `127.0.0.1:9333`, shared token | WebSocket editor bridge | None. Each tool spawns `godot --headless --script` | TCP client to a long-lived Godot process, plus one-shot CLI |
| Game transport | Godot remote debugger (`--remote-debug tcp://127.0.0.1:<port>`), `mcp:` messages. Editor Play is relayed by `EditorDebuggerPlugin` | Same debugger protocol. This is the architecture this server follows | stdout of the CLI process | Custom input receiver script inside the played scene, not `EngineDebugger` |
| Edits | `EditorUndoRedoManager` while the editor is connected. Headless fallback writes `.tscn` / `.tres` / `project.godot` text for a few tools | Undoable editor ops, headless Godot for the rest | CLI script mutates files, no undo | CLI script, undo only where Godot's editor is actually open |

`godot-mcp-js` is shin2344234/godot-mcp. `package.json` name `@shin2344234/godot-mcp`,
`mcpName` `io.github.shin2344234/godot-mcp`, remote `git@github.com:shin2344234/godot-mcp.git`.
It is not a checkout of Coding-Solo or Vollkorn.

## What was already in the Rust server

Before this parity work the server was an editor bridge plus a CLI fallback:

`godot_get_version`, `godot_get_project_info`, `godot_launch_editor`, `godot_run_project`,
`godot_stop_project`, `godot_get_run_status`, `godot_get_open_scenes`, `godot_open_scene`,
`godot_save_scene`, `godot_get_scene_tree`, `godot_add_node`, `godot_remove_node`,
`godot_get_node_properties`, `godot_set_node_properties`, `godot_capture_viewport`,
`godot_create_script`, `godot_validate_script`, `godot_resave_resources`, and the feedback
tools `godot_note_feedback`, `godot_draft_feedback`, `godot_submit_feedback`.

Scene edits already went through undo/redo. `godot_run_project` without an editor spawned
Godot but could not attach a debugger, inject input, or stop a managed run reliably.
`godot_open_scene` on an already-open scene called `EditorInterface.open_scene_from_path`,
which does not reload a tab, so a file changed on disk stayed stale (zeegzoog/godot-mcp#1).

## Capability matrix

Columns: Rust, JS (shin2344234), Solo (Coding-Solo), Vollkorn.
Tool names are the MCP names. Solo and Vollkorn do not use a `godot_` prefix.

| Capability | Rust | JS | Solo | Vollkorn | Godot API / transport | Rust decision |
| --- | --- | --- | --- | --- | --- | --- |
| Editor version and project info | implemented `godot_get_version`, `godot_get_project_info` | implemented `godot_status`, `godot_project_info` | implemented `get_godot_version`, `get_project_info`, `list_projects` | same names as Solo | `Engine.get_version_info`, `ProjectSettings` | Keep the existing names |
| Launch editor | implemented `godot_launch_editor` | implemented `godot_editor_launch` (waits for the plugin) | implemented `launch_editor` | implemented `launch_editor` | `godot --editor` | Existing. Does not wait for the bridge |
| Run / stop / status | implemented. `mode: "process"` attaches the debugger. `godot_stop_project` stops editor Play or a process run | implemented `godot_run_start`, `godot_run_stop` | partial `run_project`, `stop_project` (process kill, no live protocol) | partial, same plus interactive session | `--remote-debug`, `EditorInterface.play_*` | Reuse `CliRunner` only for validate/launch. Games go through `GameHub` |
| Run logs, errors, stacks | partial `godot_run_output`. Debugger `error` frames and stdout/stderr. Verified the probe's `push_error` text. Stack triples are parsed, not separately asserted | implemented `godot_run_output` | partial `get_debug_output` (stdout) | partial `get_debug_output`, `get_runtime_errors` | `DebuggerMarshalls::OutputError` | JS layout. Errors do not pause the game unless `break_on_errors` |
| Live scene tree | implemented `godot_game_tree`. Verified on a headless 4.8.beta1 process run | implemented | unsupported | partial `find_nodes`, `game_state` via the input-receiver script | `EngineDebugger` `mcp:request` | JS runtime, not Vollkorn's receiver |
| Live properties read | implemented `godot_game_get`. Verified script vars after input | implemented | unsupported | partial `get_node_properties` is the editor/CLI scene, `game_state` is runtime | `Node.get` | Same |
| Live property write | partial `godot_game_set`. Refuses `script` and `_` names. Not in the e2e | implemented `godot_game_edit` op `set` | unsupported | partial `set_property` | `Node.set`. Not saved | No eval. No arbitrary property names that escape the node |
| Live method call | implemented `godot_game_call`. Caller must pass `permit`. Denylist: quit, set_script, call, callv, call_deferred, rpc, rpc_id, free. Verified `poke` returned 4 | implemented inside `godot_game_edit`. `godot_game_eval` is opt-in | unsupported | partial `call_method`, `evaluate_expression` (unrestricted) | `Node.callv` | Permit list. No `godot_game_eval` |
| Signals | partial. Editor: `godot_list_signals`, `godot_connect_signal` (CONNECT_PERSIST, undo). Runtime: `godot_game_signals`, `godot_game_observe` (connects, waits frames, disconnects). Observe returned without error; a signal fired during the watch window was not asserted | implemented `godot_signal_*` and runtime observe | unsupported | partial `connect_signal`, `subscribe_signals`, `wait_for_signal` | `Node.connect` | Editor connections are undoable. Runtime observation is temporary |
| Snapshot | partial `godot_game_snapshot` (scene, frame, paused, children). Not in the e2e | implemented as part of tree/time | unsupported | partial `game_state` | `SceneTree` | Small on purpose |
| Keyboard, mouse, touch, gamepad, wait | implemented `godot_game_input`. Verified on headless 4.8.beta1: key `A`, left click, touch, joy button A, left stick X, one-frame wait. Events hit the game's `_input`, not the editor | implemented, richer (actions, drag, click-on-node) | unsupported | implemented `send_key`, `send_mouse_*`, `send_joypad_*`, `send_input` through `input_receiver.gd` | `Input.parse_input_event` + `Input.flush_buffered_events` inside the game | JS path. Vollkorn's receiver never sees the editor's input map the same way and is easier to leave stuck |
| Release stuck input | implemented `godot_game_input_release`. Verified held counts return to 0. The runtime also releases on `_exit_tree` | implemented (release steps; recording layer is separate) | unsupported | partial; no single release-all in the tool list | same `Input` events with `pressed = false`, axes to 0 | Required. No tool leaves a button down without a matching release path |
| Frame wait | implemented `godot_game_wait` and a `wait` input step | implemented `godot_game_time` (also pause, speed, physics ticks, wait for node/signal) | unsupported | partial `wait_for_node`, `wait_for_signal` | `_process` on the autoload (`PROCESS_MODE_ALWAYS`) | Frames only. Time-scale and suspend are a later gap |
| Game screenshot | partial `godot_game_screenshot`. PNG magic, IHDR size, and non-empty buffer required before an MCP image. Headless returns an error and no image (verified). A windowed capture was not run | implemented, including editor viewports and image diff | unsupported | implemented `game_screenshot`, `capture_screenshot` (writes a file) | `Viewport.get_texture().get_image().save_png_to_buffer` | Image content only for real PNG bytes |
| Editor screenshot | partial `godot_capture_viewport` (pre-existing). Needs a window | `godot_game_screenshot` can target the editor view | unsupported | `capture_screenshot` | editor viewport | Left as the existing tool |
| Breakpoints, pause, step, stack, variables | partial. Process runs only. Verified pause (`reason: pause`, no script stack, which matches `core:break`) and a line breakpoint whose stack contained `_process`. Step into/over/out and variable decode are implemented (`src/godot/debug_ctl.rs`) and not driven by the e2e | implemented `godot_debug_*`, plus evaluate | unsupported | unsupported as a real debugger. `pause_game` sets `SceneTree.paused` | `core:breakpoint`, `core:break`, `step`/`next`/`out`/`continue`, `get_stack_dump`, `get_stack_frame_vars` | Client of Godot's debugger. Editor Play stays on the editor's debugger; the relay does not receive `debug_enter` |
| GDScript LSP | partial. `godot_code_diagnostics` verified against a broken script (Godot 4.8.beta1 headless `--lsp-port`). Complete, symbols, definition, references are implemented (`src/lsp/`) and not in the e2e | implemented `godot_code_diagnostics`, `godot_code_symbols`, `godot_code_lookup`, `godot_code_rename` | unsupported | unsupported | Godot's GDScript language server, TCP, `Content-Length` only | Client. No language server of our own. Rename is not ported |
| Create / open / save scene | implemented. `godot_create_scene` (editor pack, or text if the editor is closed — text path unit-tested). `godot_open_scene` reloads an already-open scene when the file bytes differ (editor e2e: node `Before` replaced by `FromDisk`). `godot_save_scene` pre-existing. Absolute paths inside the project are accepted; on macOS `/var` and `/private/var` are treated as the same directory | implemented, plus inherit, save-as, close, batch | implemented `create_scene`, `save_scene` via CLI | implemented, plus more node ops | `PackedScene.pack`, `EditorInterface.reload_scene_from_path` | Reload compares a content hash, not mtime, so a same-second edit counts. Unsaved bridge edits need `discard: true` |
| Nodes add / remove / properties | implemented (pre-existing), undoable | implemented, plus rename, reparent, duplicate, instance, groups | partial `add_node` | implemented, plus those extras | `EditorUndoRedoManager` | Kept. Rename/reparent/duplicate not added |
| Script attach | partial `godot_attach_script` (undoable, editor only). Not in the e2e | implemented `godot_script_attach` | unsupported | implemented `attach_script` | `Node.script` via undo | Editor only. Creating the file stays `godot_create_script` |
| Resources | partial `godot_create_resource` (editor `ResourceSaver`, or a text `.tres` if the editor is closed). `godot_resave_resources` reimports. No property editor for an existing `.tres` | implemented read/create/set/deps | unsupported | implemented `create_resource` | `ClassDB.instantiate` + `ResourceSaver` | Class must extend `Resource` |
| Import | partial `godot_resave_resources` (pre-existing reimport). No import-option editor | implemented `godot_import_get`, `godot_import_set` | unsupported | partial asset search/install | `EditorFileSystem.reimport_files` | Changing `.import` options is documented-only here |
| Project settings | partial `godot_get_project_settings`, `godot_set_project_setting`. Editor uses `ProjectSettings.save`. Offline path rewrites `project.godot` (unit-tested for a string and an int). Input map and autoloads are not tools | implemented, plus input map, autoloads, plugins | unsupported | implemented `edit_project_settings`, `manage_autoloads` | `ProjectSettings` | One key at a time |
| UID rewrite, sprite, mesh library | unsupported | files tools cover moves; uid is implicit | implemented `get_uid`, `update_project_uids`, `load_sprite`, `export_mesh_library` | same plus tile helpers | CLI | Not the reliable path for an AI loop. Left unsupported |
| Animation, tilemap, navigation, audio, visual shader, export presets, class reference, GUT/GdUnit4 | unsupported | implemented as their own groups | unsupported | partial: animation, tiles, `run_tests`, asset install | various editor APIs | Documented-only. Porting them as one-shot CLI scripts would skip undo and the debugger. Add them on the editor bridge when a workflow needs them |
| Unrestricted eval | unsupported | `godot_game_eval` only with `--allow-eval` | unsupported | `evaluate_expression`, `execute_script` | `Expression` / `GDScript` | Not implemented |

## Source map (Rust)

| Area | Files |
| --- | --- |
| Tool catalog and handlers | `src/tools.rs`, `src/tools/game.rs`, `src/tools/code.rs`, `src/tools/authoring.rs` |
| Remote debugger session, variant codec | `src/godot/debug_session.rs`, `src/godot/variant.rs` |
| Breakpoints, pause, step, stack | `src/godot/debug_ctl.rs` |
| Process runs, `override.cfg` autoload, logs | `src/godot/runs.rs` |
| GDScript LSP client | `src/lsp/client.rs`, `src/lsp/framing.rs` |
| Editor bridge commands, stale-scene reload | `addons/godot_mcp/bridge_server.gd` |
| In-game runtime | `addons/godot_mcp/runtime/mcp_runtime.gd`, `addons/godot_mcp/runtime/ops/rt_*.gd` |
| Editor Play relay | `addons/godot_mcp/editor/game_relay.gd`, `addons/godot_mcp/core/runtime_override.gd` |
| Behavior tests | `src/godot/variant.rs`, `src/godot/debug_session.rs`, `src/lsp/framing.rs`, `src/tools/game.rs`, `src/tools/authoring.rs`, `tests/tools_test.rs`, `tests/runtime_e2e_test.rs`, `tests/editor_e2e_test.rs`, `tests/addon_export_test.rs` |

## Errors and limits

- A disconnected editor is `BridgeUnavailable`. Scene create, resource create, and project
  settings then use the text fallback. Attach, signals, and live editor tools return the
  bridge error and say how to launch the editor.
- A crashed or exited game fails the next runtime call (`the game disconnected` / `the game
  exited before the runtime said hello`). `godot_stop_project` and dropping the server kill
  the child and restore `override.cfg`. The autoload is injected only through `override.cfg`,
  never `project.godot`. A stale marker from a crash is removed on the next run.
- Script errors are logged and, by default, do not freeze the game
  (`core:set_ignore_error_breaks`). `break_on_errors: true` on `godot_run_project` keeps
  Godot's default stop.
- `scene:runtime_node_select_set_type 0` is sent on connect so SceneDebugger's default
  2D node-select mode does not swallow injected input after a pause.
- Screenshots: headless display server, empty viewport, or a buffer that is not a PNG with
  a non-zero IHDR is an error. No placeholder image is returned.
- Input: unknown key, button, axis, or phase is `BAD_PARAMS` before anything is held.
  `godot_game_input_release` releases keys, mouse buttons, joy buttons, axes, and touches
  this server still holds.
- Debugger stepping works on process runs this server launched. Editor Play uses Godot's
  own debugger; the plugin only relays `mcp:*`.
- LSP frames are `Content-Length` only. Godot rejects a `Content-Type` header.
- This branch's plugin requires Godot 4.8 (`MIN_MINOR = 8` in `plugin.gd`). The checks
  below used `4.8.beta1.official.4d10419ca` on macOS. Release lines 4.5, 4.6, and 4.7
  were not re-run. Windows process-group flags exist; they were not run.
- `get_method_argument_type` is not a Node method in 4.7. Argument types come from
  `get_method_list()`.

## Tests and quality

Unit and integration tests (no Godot): variant round-trip, framed packets split across
reads, oversized packet rejected, debugger session sends `core:set_ignore_error_breaks`
and `scene:runtime_node_select_set_type`, LSP framing has no `Content-Type`, tool schema
validation, `godot_game_call` refuses a method that is not permitted and refuses `quit`,
PNG dimension check, offline `project.godot` setting write, offline `.tscn` create.

Godot 4.8.beta1 (`4.8.beta1.official.4d10419ca`), the build named in `.github/godot-version`:

- `cargo test --test runtime_e2e_test -- --ignored` — process run, live tree, keyboard,
  mouse, touch, joy button, joy axis, wait, property readback, permitted call, observe,
  headless screenshot rejection, release-all, error log, pause, breakpoint stack, resume,
  stop, `override.cfg` restored. LSP diagnostics on a broken script.
- `cargo test --test editor_e2e_test -- --ignored` — see the verification section below.
  The scene test rewrites an already-open `.tscn` and expects `reloaded: true` and the
  new node.

Not run: windowed screenshot, editor Play relay, step into/over/out, LSP completion and
references, GUT/GdUnit4. Vollkorn's vitest suite is the model for one-shot CLI tests;
it does not cover the debugger protocol, so the runtime e2e follows the JS server instead.
GUT/GdUnit4 were not added: they need those addons installed in a fixture project, and
`godot --headless --check-only` plus the e2e above already compile the scripts.

## Verification

These checks were run with every pull request in this series combined on current `master`.
Godot was `4.8.beta1.official.4d10419ca`, the build named in `.github/godot-version`.
An earlier pre-split tree had been checked on 4.7.2; that run is not this branch.

`cargo fmt --all -- --check` and `cargo clippy --all-targets --all-features -- -D warnings`
finished with no warnings.

`cargo test --all-targets` (ignored Godot tests excluded): 206 passed, 0 failed.
The ignored tests were then run with `GODOT_PATH` set to that 4.8.beta1 binary:

- `runtime_e2e_test`: `language_server_reports_a_real_script_error` and
  `running_game_accepts_input_and_can_be_inspected` passed (keyboard, mouse, touch, joy
  button, joy axis, property readback, permitted call, observe, headless screenshot
  rejection, release-all, error text, pause, breakpoint in `_process`, resume, stop,
  `override.cfg` restored).
- `editor_e2e_test`: `bridge_is_loopback_only_and_requires_the_token`,
  `scene_and_property_tools_behave` (including the stale-open-scene reload), and
  `undo_and_resource_refresh_behave` passed.
- `addon_export_test`: `should_skip_matches_the_addon_folder_and_nothing_else` and
  `exported_pack_contains_the_game_but_not_the_addon` passed.

`godot --headless --check-only --path . --script` reported no script error for
`plugin.gd`, `bridge_server.gd`, `export_plugin.gd`, `editor/game_relay.gd`,
`core/runtime_override.gd`, `runtime/mcp_runtime.gd`, and each `runtime/ops/rt_*.gd`.

Not run: a windowed game screenshot, editor Play (`game_relay.gd` against a played
scene), step into/over/out, LSP completion/symbols/definition/references, GUT/GdUnit4.
Release lines `godot-v4.7`, `godot-v4.6`, and `godot-v4.5` were not checked.

## Remaining gaps

- Windowed `godot_game_screenshot` and editor-Play input were not executed. The code paths
  exist (viewport PNG, `game_relay.gd`).
- `godot_game_time` features beyond frame waits: suspend, speed, physics ticks, wait for a
  node or signal.
- LSP rename, hover, and the offline class reference (`godot_docs_*`).
- Input-map, autoload, plugin, and import-option editors.
- Animation, tilemaps, navigation, audio buses, visual shaders, export.
- Scene inherit, batch undo, node rename/reparent/duplicate.
- GUT / GdUnit4 runner.
- `godot_game_eval` and any arbitrary-method trampoline. Left unsupported.
