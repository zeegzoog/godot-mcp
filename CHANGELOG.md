# Changelog

All notable changes to godot-mcp. The server and the Godot addon always ship with the same
version number. The section for a version is used as the text of its GitHub Release, so every
release needs one (the release workflow fails without it).

Each Godot version has its own release line and branch (`godot-v4.5`, `godot-v4.6`, `godot-v4.7`, …). From 0.5.1 on, one release holds the files of every supported Godot version, and its notes are the section of this file; they mark changes that concern some Godot versions only.

## [Unreleased]

### Added

- `godot_game_input` and `godot_game_scenario` take `coordinate_space`: `viewport` (the default, unchanged) or `window`. `viewport` is the root viewport's visible rectangle after content scale, the space `_input` sees and Control global positions use; `window` is pixels of the game window, mapped into viewport space when the call is parsed. Every input answer names the space (`coordinate_space`) and the mapping between the two (`space`: both sizes, `window_per_viewport_pixel`, `window_offset`), and a `click_node` step returns `at`, the point it clicked, in that space. Before 0.8.0 pointer positions were window pixels, while `click_node` aimed at viewport coordinates; a test written for one space can now say which it means instead of being converted by hand.
- `godot_code_diagnostics` adds `warnings_hidden`, and `godot_validate_script` a line, for a script under `res://addons/` in a project that keeps Godot's default `debug/gdscript/warnings/exclude_addons=true`: the analyzer reports no warnings for such a script, so a clean answer did not mean the script was clean.

### Fixed

- The addon's own scripts have no GDScript analyzer warnings (unused parameters, shadowed names, integer division, an int used as an enum, an incompatible ternary). They were never reported because Godot leaves `addons/` out of warnings by default and `--check-only` prints none. The addon development project now sets `debug/gdscript/warnings/exclude_addons=false`, and `addon_scripts_have_no_analyzer_warnings` in `tests/runtime_e2e_test.rs` asks the language server about every addon script.

### Tests

- `click_node_completes_a_click_while_the_tree_is_paused` clicks a button that works while the game has paused its tree, with `click_node`, a touch `click_node`, `click` and a mouse tap, in a headless and a stretched windowed game and in a held one, and in both coordinate spaces. A paused tree was not the cause of the missed clicks reported from a game with a 1920 x 1080 viewport in a 1280 x 720 window: before 0.8.0, `click_node` aimed at viewport coordinates and the event treated them as window pixels. The test fails if the content-scale mapping is removed.

## [0.8.0] - 2026-10-25

The AI can now run a GDScript headless and get its errors back, write to a deep property path of a running game and be told when a write did not take, see analyzer warnings in `godot_validate_script`, and refresh imported resources even when the editor is closed. The release holds the files for Godot 4.5, 4.6 and 4.7 as before: download the files whose names end in `-godotM.N` for your Godot version, and update the server and the addon together. Everything below works on all three versions.

### Added

- `godot_resave_resources` works without the editor: it runs `godot --headless --path <project> --import` and lists the sources whose `.import` file appeared or changed (`imported`, `not_scanned`). Pass `project_path` to name the project; without the editor and without a project the error names that command. Thanks to @theromis.
- `godot_resave_resources` takes `wait: true` for a full rescan: the answer comes after scanning and importing finish (at most 60 seconds) and reports how many files were imported and whether the wait timed out.
- `godot_run_script` runs a GDScript that extends `SceneTree` headless with the configured Godot binary (`--path`, `-s`, `user_args` after `--`). It returns the exit code, stdout, stderr and each `SCRIPT ERROR` with its file and line. `timeout_seconds` defaults to 120 (maximum 600); on timeout the process tree is killed and the last output lines are included. Paths outside the project are refused.
- `godot_game_input` takes `emulate_mouse: false` to turn off the engine's emulated mouse events for a touch step during one call; the previous value is restored afterwards.

### Changed

- `godot_resave_resources` no longer reports a file the editor has never scanned as reimported: such a path is scanned first and is listed under `reimported` only once its `.import` file exists, otherwise under `not_scanned` (`load()` would find no loader). Thanks to @theromis.
- `godot_run_script` stops when a `SCRIPT ERROR` has been printed and the process then sits idle for 1.5 seconds, as it does after an error in `_init`, instead of waiting out the timeout; the answer sets `stopped_idle`. Thanks to @theromis.
- `roi` of `godot_game_screenshot` is documented as the comparison region of a baseline: it requires `baseline` and never crops the returned image. Thanks to @theromis.
- `godot_game_set` writes a `/` path through an Object field or a Dictionary key, the same walk `godot_game_get` uses. A missing property or key is an error that names it (a new Dictionary key is not created), a `/` step into another Node is refused, and the written value is read back: a write that did not take is an error instead of success with `null`. Floats, vectors, colors and transforms are compared approximately, because many engine floats are 32-bit.
- `godot_game_screenshot` names the `godot_game_input` coordinate space (the root viewport's visible size, after content scale) and the scale from returned image pixels to that space, per frame for a sheet and also for an `roi` and a camera capture.
- Pointer positions of `godot_game_input` (mouse, touch, swipe, path, circle) are pixels of the root viewport's visible rectangle: the content scale is applied before the event is parsed, so a stretched window delivers the position that was asked for. Without stretching nothing changes.
- `godot_game_get` and `godot_game_wait` treat a missing segment of a `/` property path as an error that names the segment (`property_errors` for `godot_game_get`); `null` is only a value that really is null.
- `godot_validate_script` reports the language server's errors and warnings when that server is already running (the open editor, or one `godot_code_diagnostics` started). A warning, such as a local `seed` shadowing the global, is listed as `file:line:col` with its code and message, and does not fail the check; an error does. Without a language server the check stays the headless parse check and says that analyzer warnings were not checked. `godot_code_diagnostics` is what starts a language server when none is running.

## [0.7.0] - 2026-10-22

The AI can now hold a running game and step it by exact ticks, wait for a value, tell a reloaded scene from the old one, compare screenshots with a saved baseline, drag along a path, record a game as PNG files, change a material while the game runs, start a game with its own flags, and read and edit existing scripts. The release holds the files for Godot 4.5, 4.6 and 4.7 as before: download the files whose names end in `-godotM.N` for your Godot version, and update the server and the addon together. Everything below works on all three versions unless it says otherwise.

### Added

- **Hold a game and step it by exact ticks.** `godot_game_physics` with `hold` stops a running game at the start of a physics tick: nothing advances, not even nodes that ignore pause, while reads are still answered. While held, `step` runs an exact number of ticks, or until a condition on a public property holds (`until`, `max`), and input is applied by tick (`hold_ticks`). `godot_run_project` with `held: true` starts a game held at a fixed frame rate equal to its tick rate, so the steps are exact; the answers say `deterministic` and why not when they are not. A held game refuses what needs time to pass (waits, signal watches, screenshot sheets, recordings, timed input) and says to step instead. Without a hold, `step` advances real engine physics iterations at the game's normal tick and leaves the tree paused.
- **Wait on a value, tell scene reloads apart, compare with a baseline.** `godot_game_wait` can wait for a public property (`property`, `op`, `value`, no method calls) or for a newer `scene_generation`. Snapshots, logs and screenshots carry the generation, the instance id of the scene and the hash of its root script, so a reloaded scene cannot be mistaken for the old one. `godot_game_screenshot` saves a named baseline and compares a later capture with it over a region and with a tolerance.
- **Start a game with its own flags, record its motion and change a material while it runs.** `godot_run_project` takes `user_args` (managed runs only): strings placed after `--`, so the game reads them with `OS.get_cmdline_user_args()` and Godot never takes them for engine options. `godot_game_record` saves up to 240 frames of the game window as PNG files below the system temporary directory without slowing the game down, and builds a gif or mp4 when `ffmpeg` is installed. `godot_game_get` and `godot_game_set` follow a resource with `:` (`material_override:shader_parameter/caustics`); writing still refuses `script` and private names. Thanks to @theromis.
- **Drag along a path.** `godot_game_input` has a `path` step (press at the first point, move through the others, release at the last) and a `circle` step (that drag around a centre), with a mouse or a touch pointer; one gesture takes at most 600 frames. A held game refuses them like `swipe`. An answer to more than 20 steps lists only `ok_count` and the failed steps unless you pass `verbose`. `godot_game_get` reads a field of an Object or a Dictionary with `/` (`firefly_law/births`), the way `godot_game_wait` does, and reads a property whose own name has a `/` (`metadata/note`) whole. Thanks to @theromis.
- **Read and edit existing scripts.** `godot_read_script` returns a script as the editor would save it, including text you have typed and not saved, with a version label. `godot_edit_script` replaces one fragment of a script: with the editor it is a single undoable action and the script is saved; without it the file is replaced safely. An edit is refused, with the fix, while the editor holds unsaved changes to the script, when the fragment is not found exactly once, or when the script changed since it was read.

### Changed

- `godot_run_project` compares every imported file with its `.godot/imported` cache first. A source saved after its cache is reimported when the editor has the project open; otherwise the answer lists the files and names `godot_resave_resources`. A game used to keep running on the old import without a word.
- An editor that cannot be reached is reported with the port the server dials and where it came from (`--debug-port`, `GODOT_DEBUG_PORT` or the default), and the hint says the addon must listen on that port.
- **A `project_path` you name always wins over the open editor.** Reading and editing a script, creating a scene or a resource, and project settings used to go to the open editor even when you named another project, and edited the wrong one while saying it succeeded. They now use the files of the project you named, and an answer that changed something names the project directory.
- `godot_stop_project` without `run_id` stops the editor's Play mode and every managed run, and says what it stopped. It used to report success while managed games kept running. `godot_get_run_status` and `godot_run_output` say that nothing is running instead of failing when there is no editor, and an ended run shows its exit code, who ended it and when.
- `godot_game_call` also refuses the methods that call another method by name (`call_group`, `propagate_call`, `emit_signal`, `notification`, …), set a property by name (`set`, `set_deferred`, `set_indexed`) and free or replace a node (`queue_free`, `replace_by`), as the documentation always said. A JSON string can now be passed where a method asks for a `StringName` or a `NodePath`.
- A value of `GODOT_MCP_FEEDBACK` that is none of `off`, `draft` and `full` gives `draft` instead of `full`, so a typo can never switch report submission on.
- Without an editor, `godot_create_scene` and `godot_create_resource` ask the engine whether the class can be created and refuse one that cannot, as the editor does (a scene with a `Resource` root used to be written and could not be opened). Settings keys with `.` or `..` segments are refused everywhere, and the settings answer has the same shape with and without the editor.
- Parameters that have a range say so in the tool schema (`line`, `character`, ticks, `tolerance`, …), and the step, `until` and `roi` objects describe their fields. `godot_create_script` takes `script_path` like the other script tools (`file_path` still works).
- Error hints match what failed (a refusal of the running game, unsaved changes in the editor, a Godot executable that does not start), files in messages are shown as `res://` paths, and the log on standard error has no colour codes when it is not a terminal.
- An output answer from a running game explains `script_sha256` once instead of on every line, and a screenshot baseline is compared at its own size unless you give `max_dimension`.

### Fixed

- Script tabs are found in editors that do not name them, so reading and editing an open script works there, and the temporary file of an edit without an editor is created exclusively.
- A script that starts with a byte order mark can be read and edited through the editor; the answer says when saving removed the mark.
- `godot_create_scene` and `godot_create_resource` create the folders of a new file through the editor, and name the folder when they cannot.
- `godot_get_node_properties` reports an unset object property as `null` instead of a freed object, `godot_list_signals` lists only connections to nodes of the scene, with their paths, and a second editor on a taken bridge port says that a server connected to that port talks to the other editor.
- A pointer step outside the game's viewport says that controls there do not react. A game without a window has a 64 x 64 viewport whatever the project sets, so most clicks on its controls landed outside it without a word.
- Breakpoint locations must be `res://` paths without parent segments, and the language server of an editor that has another project open is not used for a project it does not serve.

## [0.6.0] - 2026-10-21

The AI can now run a game, look at it and drive it, set breakpoints and step through the code, ask Godot's own language server about its scripts, and create scenes, resources and settings. The release holds the files for Godot 4.5, 4.6 and 4.7 as before: download the files whose names end in `-godotM.N` for your Godot version, and update the server and the addon together. Everything below works on all three versions unless it says otherwise.

### Added

- **Look at a running game.** `godot_run_output` reads a game's logs, warnings and errors with stack traces. `godot_game_tree`, `godot_game_get`, `godot_game_signals` and `godot_game_snapshot` show the live nodes, their properties, their signals and a summary; `godot_game_set` changes a property and `godot_game_call` calls a method, only one you name in `permit` (never one that can quit, run code or call other methods). `godot_game_observe` watches signals for a few frames. Nothing a game tool changes is saved to disk.
- **Drive a running game.** `godot_game_input` presses keys, types text, moves and clicks the mouse, touches and swipes, and moves gamepad buttons and sticks, as a sequence of steps; `click_node` clicks the centre of a node by its path. The real mouse pointer stays where it is unless you pass `warp_mouse`. A step that fails lets go of everything the server still holds, and `godot_game_input_release` does the same on request. `godot_game_wait` waits for frames, for a node to exist or for a signal, and can slow down or speed up the game (`time_scale`, restored when the game ends).
- **Screenshots of the game.** `godot_game_screenshot` returns the game window, a viewport or a camera as a PNG, never an empty image. It can stitch up to eight frames into one sheet and says how many pixels changed since the previous picture and where. A game started without a window has nothing to capture and says so.
- **Breakpoints and stepping** for a game the server started: `godot_debug_breakpoints`, `godot_debug_control` (pause, resume, step into, over and out) and `godot_debug_state` (the stack and the variables of each frame). Editor Play keeps the editor's own debugger.
- **Ask Godot about scripts.** `godot_code_diagnostics`, `godot_code_symbols`, `godot_code_complete`, `godot_code_definition` and `godot_code_references` are answered by Godot's own GDScript language server, so they report what the editor would. The server uses an editor that already runs it (`GODOT_MCP_LSP_PORT`, default 6005) or starts a private headless one; that editor opens no bridge (`GODOT_MCP_NO_BRIDGE=1`, which you can set for any editor that must not open one). A request the server does not answer is an error, never a clean file.
- **Create content.** `godot_create_scene`, `godot_attach_script`, `godot_connect_signal`, `godot_list_signals`, `godot_create_resource`, `godot_get_project_settings` and `godot_set_project_setting`. Changes to an open scene are undoable in the editor. Nothing replaces an existing scene or resource, a signal is connected only to a method that exists, a setting keeps its type, and a value can never add lines or sections to `project.godot`. Without the editor, scenes, resources and settings are written as files.
- The README lists every tool, and `docs/feature-parity.md` compares this server with three other Godot MCP servers.

### Changed

- `godot_run_project` without a running editor (or with `mode: "process"`) now starts a game that the server watches: the AI can read its output, inspect it, drive it and debug it, `godot_get_run_status` lists it and `godot_stop_project` stops it. It ends when the server ends; before, it was a separate process that nobody could stop. The game's output is kept in a bounded log. The server adds its runtime script through a temporary `override.cfg` and a private folder under `.godot/`, and removes both when the game ends; your `override.cfg` and your `addons` folder are left as they are.

### Fixed

- `godot_open_scene` on a scene that is already open reloads it when the file on disk changed, so edits made outside the editor are no longer shown stale and later edits through the tools do not overwrite them. Unsaved edits made through the tools are not discarded unless you pass `discard: true`.
- A project path spelled `/var/...` on macOS is accepted, as it is the same folder as `/private/var/...`.
- A game tool called right after the editor starts Play waits until the game's runtime is ready instead of failing or sending input too early.
- A screenshot of a game that is still starting waits for the first frame instead of failing.

## [0.5.1] - 2026-10-20

First release made for each Godot version separately. It holds the files for Godot 4.5, 4.6 and 4.7 in one release; download the files whose names end in `-godotM.N` for your Godot version. There is no release 0.5.0: a first attempt that published one release per Godot version was withdrawn in favour of this one. The server and the addon changed together: update both.

### Changed (breaking)

- **One build per Godot version.** Files end in `-godotM.N` (for example `godot-mcp-x86_64-pc-windows-msvc-godot4.6.zip` and `godot-mcp-addon-godot4.6.zip`). Pick the files made for your Godot version; see the table in the README.
- The addon refuses to start in a Godot older than the version it was made for and prints which files to install. In a newer Godot it starts and prints a warning naming the files for that version.
- A `GODOT_PATH` that points to a missing file is no longer replaced by another Godot the server finds on its own, which could run a different engine version than the one you chose. The server logs a warning at start, and tools that run Godot fail with a message naming the path. Leaving `GODOT_PATH` unset (or empty) still looks for an installed Godot.

### Added

- `SECURITY.md` explains which versions get security fixes and how to report a vulnerability privately, and `CONTRIBUTING.md` explains how to contribute, including which branch a change belongs to.

## [0.4.0] - 2026-10-16

### Added

- A feedback channel from the AI to the maintainers. `godot_note_feedback` records something missing,
  broken or misleading while the AI works, `godot_draft_feedback` turns the notes into one report
  (versions, platform, related failures; paths, tokens and mentions removed) and shows it to you,
  and `godot_submit_feedback` publishes it as a GitHub issue through your own `gh` after you agree.
  You can also just copy the text or open the prefilled link. Nothing leaves your machine without
  your approval. `GODOT_MCP_FEEDBACK` (`off`, `draft`, `full`) and `GODOT_MCP_GH_PATH` control it.
- The addon leaves itself out of exported games: a new `export_plugin.gd`, registered by the
  plugin, skips every file in the addon folder. It only acts while the plugin is enabled; with it
  disabled, exclude `addons/godot_mcp/*` in the export preset.
- `project_path` is optional when the server runs inside the project. Without it, tools use the
  project open in the editor, else the first folder with a `project.godot` from the server's
  working directory upwards, else the only subfolder that has one. This applies to
  `godot_launch_editor` (where `project_path` is no longer required), the CLI fallback of
  `godot_run_project`, `godot_create_script` and `res://` paths in `godot_validate_script`.

### Changed

- `bridge_server.gd` no longer declares `class_name GodotMcpBridgeServer` (nothing used it; the
  plugin loads the script by path). A global class would stay in every exported game's class list
  even though the addon's files are left out of the export.

### Fixed

- A launched editor or game no longer keeps the server's stdin, stdout and stderr pipes open on
  Windows. An MCP client that waited for the server's stderr to close after stopping it used to
  wait until the editor was closed too.
- Ctrl+C in the MCP client's terminal no longer closes an editor or game started by
  `godot_launch_editor` or `godot_run_project`: they now start detached from the client's console
  (Windows) and in a process group of their own (all platforms).

### Removed

- The unused `GODOT_MCP_URL` setting (`--mcp-url`). A leftover `GODOT_MCP_URL` in the environment
  or a `.env` file is ignored, but passing `--mcp-url` now stops the server with an error.

## [0.3.0] - 2026-10-14

Hardening release. The server and the addon changed together: update both (the addon archive is
part of every release), and note the behaviour changes below.

### Changed (breaking)

- **Godot 4.5 or newer is required.** The plugin refuses to start on older editors and says why
  (`EditorInterface.get_open_scene_roots()` does not exist before 4.5).
- The editor bridge listens on `127.0.0.1` only and every command needs a token. The plugin writes
  it to `<user data dir>/godot-mcp/bridge-<port>.token` and the server reads it from there, so
  both must run as the same OS user; otherwise set the same `GODOT_MCP_TOKEN` for both.
- `godot_create_script` writes only `.gd` files inside a project, refuses to overwrite an existing
  file unless `overwrite: true`, and accepts only plain identifiers for `class_name` and
  `extends_class`. `res://` paths work, with `project_path` or a connected editor.
- `godot_get_node_properties` returns each property as `{type, value}` with the value in Godot's
  text format (for example `Vector2(5, 7)`).
- `godot_set_node_properties` converts values to the property's type (arrays or objects for
  vectors, `#rrggbb` for colors, `res://` paths for resources). It rejects the whole call when a
  property is unknown or a value cannot be converted, and reports the applied values. Before, it
  answered `updated: true` while changing nothing.
- Node paths in responses are relative to the scene root (`Player/Body`, `.` for the root).
- Node paths are resolved inside the edited scene only; the scene root cannot be removed.
- `godot_run_project` accepts only `res://` scene paths.
- `godot_get_project_info` no longer takes the `project_path` argument (it was ignored) and also
  reports `project_dir`.
- Unknown tools are JSON-RPC errors (`-32602`); malformed messages get `-32600` with their id.

### Added

- `godot_capture_viewport` renders the 2D or 3D editor viewport (`view`, `max_dimension`) and
  reports the image size.
- `godot_create_script`: `project_path` and `overwrite` arguments.
- Tool calls run concurrently, so `ping` and other calls are never held up by a slow editor
  command; `notifications/cancelled` aborts a running call.
- MCP protocol version negotiation (`2025-06-18`, `2025-03-26`, `2024-11-05`).
- One persistent, multiplexed connection to the editor with automatic reconnect; an unreachable
  editor is reported in about 0.3 s.
- Project setting `godot_mcp/verbose` to log client connections in the editor, and
  `GODOT_MCP_TOKEN` to share the bridge token explicitly.
- A `.env` file that cannot be parsed is reported with a warning instead of being ignored
  silently; empty settings (`GODOT_PATH=`) now count as unset instead of stopping the server.
- Continuous integration (Rust on Linux, Windows and macOS, plus a headless Godot 4.5 job),
  53 automated tests and 3 end-to-end tests against a real editor. Releases are gated on
  formatting, lints and tests.

### Fixed

- `godot_run_project` without an editor wrote the game's output into the server's stdout and
  corrupted the MCP stream.
- Scene trees, property dumps and screenshots larger than 64 KiB never arrived (the caller only
  saw a timeout).
- Wrongly typed arguments hung the call for 15 s; they are now rejected immediately.
- `godot_set_node_properties` silently ignored vectors, colors and unknown properties.
- `godot_remove_node` undo restores the node's position and ownership of its subtree.
- `godot_add_node` rejects non-`Node` and abstract classes cleanly and supports global script
  classes.
- `godot_capture_viewport` returned a 2x2 image as a success.
- `godot_resave_resources` pushed scripts and scenes through the importer; it now re-imports
  assets and only refreshes other files. It also no longer hangs on Godot 4.5.
- `godot_run_project` hid editor errors behind a second, detached game process.
- `godot_validate_script` reports a missing file as such, strips the engine banner and times out.
- `godot_create_script` could leave an empty file on Linux and macOS.
- Recovery hints now match the error (connectivity hints are no longer attached to bad paths).

### Security

- The bridge was reachable from the local network and from web pages in a browser, with no
  authentication.
- `godot_create_script` could write to any path, overwrite files without warning, and inject code
  through `class_name` and `extends_class`.
- A scene argument starting with `-` reached the Godot command line as an engine option.
- Node paths such as `..` could read and change the editor's own nodes.

### Notes

- Most of this release is in commit `24fe18a`, whose message lists only the tests.

## [0.2.0] - 2026-09-16

### Added

- Run, stop and query the project through the live editor's Play mode.
- Inspect any open scene tab, and list the open scenes.
- Re-import resources or rescan the project from the editor.

## [0.1.0] - 2026-09-16

### Added

- First release: Rust MCP server over stdio, the Godot Editor bridge plugin (scene tree, node and
  property operations, viewport capture), headless CLI fallback for version, script validation
  and launching the editor.

[Unreleased]: https://github.com/zeegzoog/godot-mcp/compare/v0.8.0...master
[0.4.0]: https://github.com/zeegzoog/godot-mcp/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/zeegzoog/godot-mcp/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/zeegzoog/godot-mcp/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/zeegzoog/godot-mcp/releases/tag/v0.1.0
