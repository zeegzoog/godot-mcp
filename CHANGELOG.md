# Changelog

All notable changes to godot-mcp. The server and the Godot addon always ship with the same
version number. The section for a version is used as the text of its GitHub Release, so every
release needs one (the release workflow fails without it).

From 0.5.0 on, each Godot version has its own release line and branch (`godot-v4.5`, `godot-v4.6`, `godot-v4.7`, …), and each line lists its releases in the `CHANGELOG.md` of its branch. All releases are also on the [Releases page](https://github.com/zeegzoog/godot-mcp/releases). This file keeps the history up to 0.4.0.

## [Unreleased]

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

[Unreleased]: https://github.com/zeegzoog/godot-mcp/compare/v0.4.0...HEAD
[0.4.0]: https://github.com/zeegzoog/godot-mcp/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/zeegzoog/godot-mcp/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/zeegzoog/godot-mcp/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/zeegzoog/godot-mcp/releases/tag/v0.1.0
