# Feature parity

A comparison of this Rust server with the three Godot MCP servers it was measured against.
The Rust column describes `master` and the release lines `godot-v4.5` to `godot-v4.7`,
which carry the same tools. The other columns describe the commits named below, so they
can be out of date.

A cell is **implemented** only when the behavior exists in this tree and a test or a
run on a real Godot checked it. **Partial** means the code is in the tree but the
check was narrower than the feature. **Documented-only** means a reference describes
it and this server does not do it. **Unsupported** means it was left out on purpose.

## The four implementations

| | Rust (this repo) | shin2344234/godot-mcp | Coding-Solo/godot-mcp | Vollkorn-Games/godot-mcp |
| --- | --- | --- | --- | --- |
| Path / remote | `github.com/zeegzoog/godot-mcp` | `github.com/shin2344234/godot-mcp` `@shin2344234/godot-mcp` 0.1.2, commit `7116e47` | commit `1209744` | commit `6325306` |
| Runtime | Native binary. No Node.js. | Node.js 22, TypeScript | Node.js, TypeScript | Node.js, TypeScript (fork of Coding-Solo plus a large interactive layer) |
| Godot | One release line per Godot minor: 4.5, 4.6, 4.7, and 4.8 on `master`. The plugin refuses an older editor (`MIN_MINOR` in `plugin.gd`). | 4.7+ (docs say 4.7.2) | Godot 4 CLI | Godot 4 CLI, same operations script family as Coding-Solo |
| Editor transport | WebSocket `127.0.0.1:9333`, shared token | WebSocket editor bridge | None. Each tool spawns `godot --headless --script` | TCP client to a long-lived Godot process, plus one-shot CLI |
| Game transport | Godot remote debugger (`--remote-debug tcp://127.0.0.1:<port>`), `mcp:` messages. Editor Play is relayed by `EditorDebuggerPlugin` | Same debugger protocol. This is the architecture this server follows | stdout of the CLI process | Custom input receiver script inside the played scene, not `EngineDebugger` |
| Edits | `EditorUndoRedoManager` while the editor is connected. Headless fallback writes `.tscn` / `.tres` / `project.godot` text for a few tools | Undoable editor ops, headless Godot for the rest | CLI script mutates files, no undo | CLI script, undo only where Godot's editor is actually open |

`godot-mcp-js` is shin2344234/godot-mcp. `package.json` name `@shin2344234/godot-mcp`,
`mcpName` `io.github.shin2344234/godot-mcp`, remote `git@github.com:shin2344234/godot-mcp.git`.
It is not a checkout of Coding-Solo or Vollkorn.

## Capability matrix

Columns: Rust, JS (shin2344234), Solo (Coding-Solo), Vollkorn.
Tool names are the MCP names. Solo and Vollkorn do not use a `godot_` prefix.

| Capability | Rust | JS | Solo | Vollkorn | Godot API / transport | Rust decision |
| --- | --- | --- | --- | --- | --- | --- |
| Editor version and project info | implemented `godot_get_version`, `godot_get_project_info` | implemented `godot_status`, `godot_project_info` | implemented `get_godot_version`, `get_project_info`, `list_projects` | same names as Solo | `Engine.get_version_info`, `ProjectSettings` | Keep the existing names |
| Launch editor | implemented `godot_launch_editor` | implemented `godot_editor_launch` (waits for the plugin) | implemented `launch_editor` | implemented `launch_editor` | `godot --editor` | Existing. Does not wait for the bridge |
| Run / stop / status | implemented. `mode: "process"` attaches the debugger. `godot_stop_project` stops editor Play or a process run | implemented `godot_run_start`, `godot_run_stop` | partial `run_project`, `stop_project` (process kill, no live protocol) | partial, same plus interactive session | `--remote-debug`, `EditorInterface.play_*` | Reuse `CliRunner` only for validate/launch. Games go through `GameHub` |
| Run logs, errors, stacks | partial `godot_run_output`. Debugger `error` frames and stdout/stderr. Verified the probe's `push_error` text. Stack triples are parsed, not separately asserted | implemented `godot_run_output` | partial `get_debug_output` (stdout) | partial `get_debug_output`, `get_runtime_errors` | `DebuggerMarshalls::OutputError` | JS layout. Errors do not pause the game unless `break_on_errors` |
| Live scene tree | implemented `godot_game_tree`. Verified on a headless process run | implemented | unsupported | partial `find_nodes`, `game_state` via the input-receiver script | `EngineDebugger` `mcp:request` | JS runtime, not Vollkorn's receiver |
| Live properties read | implemented `godot_game_get`. Verified script vars after input. A requested private name is returned, including inside a resource path. A requested `script` is omitted. A path that is not a resource is refused | implemented | unsupported | partial `get_node_properties` is the editor/CLI scene, `game_state` is runtime | `Node.get` | Same |
| Live property write | partial `godot_game_set`. Refuses `script` and `_` names, including a segment of a resource path. A `:` steps through a Resource (`material:shader_parameter/caustics`, verified headless). Not saved | implemented `godot_game_edit` op `set` | unsupported | partial `set_property` | `Node.set` and `Resource.set`. Not saved | No eval. No method calls. A path cannot walk from one node to another |
| Live method call | implemented `godot_game_call`. Caller must pass `permit`. Denylist: quit, set_script, call, callv, call_deferred, rpc, rpc_id, free. Verified `poke` returned 4 | implemented inside `godot_game_edit`. `godot_game_eval` is opt-in | unsupported | partial `call_method`, `evaluate_expression` (unrestricted) | `Node.callv` | Permit list. No `godot_game_eval` |
| Signals | partial. Editor: `godot_list_signals`, `godot_connect_signal` (CONNECT_PERSIST, undo; the editor e2e test connects, lists, and refuses an unknown signal, a missing method and a second connection). Runtime: `godot_game_signals`, `godot_game_observe` (connects, waits frames, disconnects). Observe returned without error; a signal fired during the watch window was not asserted | implemented `godot_signal_*` and runtime observe | unsupported | partial `connect_signal`, `subscribe_signals`, `wait_for_signal` | `Node.connect` | Editor connections are undoable. Runtime observation is temporary |
| Snapshot | implemented `godot_game_snapshot`: scene, frame, paused, children, monotonic `scene_generation`, `scene_instance_id`, and the SHA-256 of the root script source loaded when that instance became current. A reload of the same path changes the generation and the instance id. The hash is not a later disk read | implemented as part of tree/time | unsupported | partial `game_state` | `SceneTree` plus `HashingContext` of `script.source_code` | The basis is `loaded_source_at_scene_enter` or `unavailable` |
| Keyboard, mouse, touch, gamepad, wait | implemented `godot_game_input`. Verified on a headless process run: key `A`, left click, touch, joy button A, left stick X, one-frame wait, `click`, mouse and touch `swipe`, `click_node` on a Button and a Node2D. A failing step releases what is held. The OS cursor stays put unless `warp_mouse` is set. Events hit the game's `_input`, not the editor | implemented, richer (actions, drag, click-on-node) | unsupported | implemented `send_key`, `send_mouse_*`, `send_joypad_*`, `send_input` through `input_receiver.gd` | `Input.parse_input_event` + `Input.flush_buffered_events` inside the game | JS path. Vollkorn's receiver never sees the editor's input map the same way and is easier to leave stuck |
| Release stuck input | implemented `godot_game_input_release`. Verified held counts return to 0. The runtime also releases on `_exit_tree` | implemented (release steps; recording layer is separate) | unsupported | partial; no single release-all in the tool list | same `Input` events with `pressed = false`, axes to 0 | Required. No tool leaves a button down without a matching release path |
| Physics step | implemented `godot_game_physics`: advance N engine physics iterations at the normal physics delta (`max_physics_steps_per_frame = 1` while it runs), then pause. `resume` continues. The setting is restored, including when the step fails or the runtime exits mid-step. Does not call node `_physics_process`. Idle `_process`, `PROCESS_MODE_ALWAYS`, custom threads and GPU compute are outside the step, and the result says so. Not a determinism guarantee | implemented inside `godot_game_time` | unsupported | partial `pause_game` sets `SceneTree.paused` | `Engine.get_physics_frames` after the main loop's own physics iteration | `SceneTree._physics_process` is not callable from GDScript. The engine frame counter still moves while paused |
| Frame wait | implemented `godot_game_wait` and a `wait` input step: frames, a node to exist, or a signal, with a timeout; `time_scale` 0 to 8, restored when the game exits. Also a public property (`eq`, `ne`, `lt`, `lte`, `gt`, `gte`, `exists`, nested Dictionary or Object, no method calls) and a newer `scene_generation` | implemented `godot_game_time` (also pause, speed, physics ticks, wait for node/signal) | unsupported | partial `wait_for_node`, `wait_for_signal` | `_process` on the autoload (`PROCESS_MODE_ALWAYS`) | No eval. Private names, `script`, and Array indexing are refused. A signal wait is not combined with a property |
| Game screenshot | implemented `godot_game_screenshot`. PNG magic, IHDR size, and non-empty buffer required before an MCP image. Headless returns an error and no image (verified). A windowed capture returns a PNG, a sheet of up to 8 frames, and a count and rectangle of changed pixels against the previous capture of the same size (`windowed_screenshot_returns_a_sheet_and_a_diff`). Named baselines (`save_baseline` / `baseline`) compare the returned image, with an `roi` in that image's pixels and a per-channel `tolerance`. At most 8 images, each at most 512×512. `exact` is true only when every pixel in the region was visited. Longer motion is `godot_game_record`: up to 240 PNG files on disk (not base64), optional gif/mp4 when ffmpeg is on PATH | implemented, including editor viewports and image diff | unsupported | implemented `game_screenshot`, `capture_screenshot` (writes a file) | `Viewport.get_texture().get_image().save_png` | Image content only for real PNG bytes. A baseline is not a file. A recording is files, and the reply does not embed them |
| Editor screenshot | partial `godot_capture_viewport` (pre-existing). Needs a window | `godot_game_screenshot` can target the editor view | unsupported | `capture_screenshot` | editor viewport | Left as the existing tool |
| Breakpoints, pause, step, stack, variables | partial. Process runs only. Verified pause (`reason: pause`, no script stack, which matches `core:break`) and a line breakpoint whose stack contained `_process`. Step into/over/out and variable decode are implemented (`src/godot/debug_ctl.rs`) and not driven by the e2e | implemented `godot_debug_*`, plus evaluate | unsupported | unsupported as a real debugger. `pause_game` sets `SceneTree.paused` | `core:breakpoint`, `core:break`, `step`/`next`/`out`/`continue`, `get_stack_dump`, `get_stack_frame_vars` | Client of Godot's debugger. Editor Play stays on the editor's debugger; the relay does not receive `debug_enter` |
| GDScript LSP | partial. `godot_code_diagnostics` and `godot_code_symbols` verified against a real script (headless `--lsp-port`; the same script is queried repeatedly). Complete, definition, references are implemented (`src/lsp/`) and not in the e2e | implemented `godot_code_diagnostics`, `godot_code_symbols`, `godot_code_lookup`, `godot_code_rename` | unsupported | unsupported | Godot's GDScript language server, TCP, `Content-Length` only | Client. No language server of our own. Rename is not ported |
| Create / open / save scene | implemented. `godot_create_scene` (editor pack, or text if the editor is closed; both refuse to replace a scene, and both paths are tested). `godot_open_scene` reloads an already-open scene when the file bytes differ (editor e2e: node `Before` replaced by `FromDisk`). `godot_save_scene` pre-existing. Absolute paths inside the project are accepted; on macOS `/var` and `/private/var` are treated as the same directory | implemented, plus inherit, save-as, close, batch | implemented `create_scene`, `save_scene` via CLI | implemented, plus more node ops | `PackedScene.pack`, `EditorInterface.reload_scene_from_path` | Reload compares a content hash, not mtime, so a same-second edit counts. Unsaved bridge edits need `discard: true` |
| Nodes add / remove / properties | implemented (pre-existing), undoable | implemented, plus rename, reparent, duplicate, instance, groups | partial `add_node` | implemented, plus those extras | `EditorUndoRedoManager` | Kept. Rename/reparent/duplicate not added |
| Script attach | implemented `godot_attach_script` (undoable, editor only; the editor e2e test attaches, detaches, and refuses a missing script) | implemented `godot_script_attach` | unsupported | implemented `attach_script` | `Node.script` via undo | Editor only. Creating the file stays `godot_create_script` |
| Script read / edit | implemented `godot_read_script` and `godot_edit_script`. Verified in the editor e2e test on a real editor: the unsaved editor text is read, one fragment is replaced as one undo step and saved, a stale label, zero and several matches and unsaved typing are refused, a running tool script gets the new code. Without an editor the file is changed through a temporary file (host tests: line endings, byte order mark, refusals) | not compared | not compared | not compared | `ScriptEditor` code editor, `ResourceSaver` | Fragment replacement only, refused over unsaved editor changes. No full-file overwrite |
| Resources | partial `godot_create_resource` (editor `ResourceSaver`, or a text `.tres` if the editor is closed). Never replaces an existing file. `godot_resave_resources` reimports. No property editor for an existing `.tres` | implemented read/create/set/deps | unsupported | implemented `create_resource` | `ClassDB.instantiate` + `ResourceSaver` | Class must extend `Resource` |
| Import | partial `godot_resave_resources` (pre-existing reimport). No import-option editor | implemented `godot_import_get`, `godot_import_set` | unsupported | partial asset search/install | `EditorFileSystem.reimport_files` | Changing `.import` options is documented-only here |
| Project settings | partial `godot_get_project_settings`, `godot_set_project_setting`. Editor uses `ProjectSettings.save`. Offline path rewrites `project.godot` (tested: escaping, the key check, and that a setting keeps its type). A value of another type than the existing setting is refused. Input map and autoloads are not tools | implemented, plus input map, autoloads, plugins | unsupported | implemented `edit_project_settings`, `manage_autoloads` | `ProjectSettings` | One key at a time |
| UID rewrite, sprite, mesh library | unsupported | files tools cover moves; uid is implicit | implemented `get_uid`, `update_project_uids`, `load_sprite`, `export_mesh_library` | same plus tile helpers | CLI | Not the reliable path for an AI loop. Left unsupported |
| Animation, tilemap, navigation, audio, visual shader, export presets, class reference, GUT/GdUnit4 | unsupported | implemented as their own groups | unsupported | partial: animation, tiles, `run_tests`, asset install | various editor APIs | Documented-only. Porting them as one-shot CLI scripts would skip undo and the debugger. Add them on the editor bridge when a workflow needs them |
| Unrestricted eval | unsupported | `godot_game_eval` only with `--allow-eval` | unsupported | `evaluate_expression`, `execute_script` | `Expression` / `GDScript` | Not implemented |

## Source map (Rust)

| Area | Files |
| --- | --- |
| Tool catalog and handlers | `src/tools.rs`, `src/tools/game.rs`, `src/tools/code.rs`, `src/tools/authoring.rs`, `src/tools/scripts.rs` |
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
- Each release line's plugin requires its own Godot minor (`MIN_MINOR` in `plugin.gd`) and
  is tested with that build (`.github/godot-version` on the line).
- `get_method_argument_type` is not a Node method in 4.7. Argument types come from
  `get_method_list()`.

## Tests

Unit and integration tests need no Godot: variant round trip, framed packets split across
reads, an oversized packet rejected, the debugger session's start-up messages, LSP framing
with no `Content-Type`, tool schema validation, `godot_game_call` refusing a method that is
not permitted and refusing `quit`, the PNG dimension check, and the text fallbacks of the
authoring tools (a scene, a resource, and project settings, including what they refuse).

The tests that need a real Godot are ignored by default. Run them with `GODOT_PATH` set to the
build named in `.github/godot-version` (see the README):

- `runtime_e2e_test`: a process run with a live tree, keyboard, mouse, touch, joypad button
  and axis, a wait step, property readback, a permitted call, signal observation, headless
  screenshot rejection, release-all, gestures (`click`, `swipe`, `click_node`), the time
  scale, waits for a signal and for a missing node, a failed click releasing a held key, the
  error log, pause, a line breakpoint with its stack, resume, and stop with `override.cfg`
  restored; a windowed screenshot with a frame sheet and a pixel diff (skipped on a Linux
  machine without a display); and the language server on a broken script and on a script
  queried again.
- `editor_e2e_test`: the loopback-only, token-checked bridge; the scene and property tools,
  including the reload of an open scene whose file changed; undo and resource refresh; the
  authoring tools; and a game tool called right after editor Play (skipped on a Linux
  machine without a display).
- `addon_export_test`: the addon stays out of exported games.

No end-to-end test drives: step into/over/out, language-server completion, definition and
references, `godot_game_set`, `godot_game_signals`, or `godot_game_snapshot`. A GUT / GdUnit4
runner was not added: it needs those addons in a fixture project, and `godot --headless --check-only` plus
the end-to-end tests already compile the scripts.

## Remaining gaps

- `godot_game_time` features beyond waits and the time scale: pausing the tree, physics ticks.
- LSP rename, hover, and the offline class reference (`godot_docs_*`).
- Input-map, autoload, plugin, and import-option editors.
- Animation, tilemaps, navigation, audio buses, visual shaders, export.
- Scene inherit, batch undo, node rename/reparent/duplicate.
- GUT / GdUnit4 runner.
- `godot_game_eval` and any arbitrary-method trampoline. Left unsupported.
