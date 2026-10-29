# Godot MCP Server

A fast, cross-platform [Model Context Protocol](https://modelcontextprotocol.io) (MCP) server, written in Rust, that lets AI assistants inspect and edit a live Godot 4 editor.

- **One native binary** for Windows, Linux, and macOS (Intel and Apple Silicon). No Node.js or Python.
- **Live editor bridge.** A small GDScript plugin exposes the open scenes over a local WebSocket (loopback only, token-protected). Node edits go through `EditorUndoRedoManager`, so anything the AI changes can be undone with `Ctrl+Z`.
- **Headless fallback.** Without a running editor, the server calls the Godot binary directly to report the engine version, validate scripts, launch the editor, run the project as a managed game and ask Godot's language server about scripts. It writes scripts, scenes, resources and project settings as files.

## Installation

Requirements: **Godot 4.5 or newer**, the godot-mcp files made for your Godot version, and an MCP client such as Claude Code, Claude Desktop, Cursor, VS Code, or Antigravity. No Rust toolchain needed.

### Pick the files for your Godot version

Each godot-mcp release holds the files for every supported Godot minor version. Download the files whose names end in `-godotM.N`, where `M.N` is your Godot version (for example `-godot4.6`).

| Your Godot | Branch | Files to download |
| --- | --- | --- |
| 4.8 (pre-release) | `master` | none; build from source |
| 4.7.x | `godot-v4.7` | the files ending in `-godot4.7` |
| 4.6.x | `godot-v4.6` | the files ending in `-godot4.6` |
| 4.5.x | `godot-v4.5` | the files ending in `-godot4.5` |

Release 0.4.0 and older are one build for Godot 4.5 and newer, with file names that have no `-godotM.N` suffix.

### Option A: let your AI assistant install it

Open your Godot project in the assistant and paste:

```text
Install the Godot MCP server (https://github.com/zeegzoog/godot-mcp) for this Godot project. Use the GitHub release, not a local clone of that repository.

1. Detect the OS and CPU architecture (Windows, Linux, or macOS; x86_64 or arm64).
2. Ask me for the full path to my Godot executable. Never guess it: Windows builds are portable, version-named files (e.g. Godot_v4.5-stable_win64.exe) stored wherever I put them. Run `<path> --version` to confirm it works, and note its major.minor version (4.6.3.stable.official → 4.6).
3. Take the files for that Godot version from the latest release: `gh release view --repo zeegzoog/godot-mcp --json tagName,assets` lists them; use the assets whose names end in `-godot<major.minor>` (with a tag like `v0.5.1`). If the latest release has no such files, tell me and do not take the files of another Godot version. Download the server archive for this platform and the addon archive, e.g.:
   gh release download <tag> --repo zeegzoog/godot-mcp --pattern "godot-mcp-<target-triple>-godot<major.minor>*"
   gh release download <tag> --repo zeegzoog/godot-mcp --pattern "godot-mcp-addon-godot<major.minor>*"
   Extract the server to a permanent folder outside the project: %LOCALAPPDATA%\godot-mcp\ (Windows), ~/.local/share/godot-mcp/ (Linux), ~/Library/Application Support/godot-mcp/ (macOS).
4. Copy addons/godot_mcp from the addon archive into <project>/addons/godot_mcp and enable it in project.godot:
   [editor_plugins]
   enabled=PackedStringArray("res://addons/godot_mcp/plugin.cfg")
5. Register the server as "godot" in my MCP client config, with the binary from step 3 as the command and GODOT_PATH from step 2 in env. If the config is a project-scoped .mcp.json in a shared repository, do not commit real paths: write a .mcp.json.example with placeholders and add .mcp.json to .gitignore.
6. Ask me to restart the MCP client and (re)open the project in the Godot editor, then call godot_get_version and report the result.
```

### Option B: install manually

**1. Download** the server for your platform and the addon from the [latest release](https://github.com/zeegzoog/godot-mcp/releases/latest), the files for your Godot version (see [the table above](#pick-the-files-for-your-godot-version)). `M.N` is your Godot version, e.g. `4.6`.

| Platform | Asset |
| --- | --- |
| Windows x86_64 | `godot-mcp-x86_64-pc-windows-msvc-godotM.N.zip` |
| Linux x86_64 | `godot-mcp-x86_64-unknown-linux-gnu-godotM.N.tar.gz` |
| macOS Apple Silicon | `godot-mcp-aarch64-apple-darwin-godotM.N.tar.gz` |
| macOS Intel | `godot-mcp-x86_64-apple-darwin-godotM.N.tar.gz` |
| Godot addon (required, any platform) | `godot-mcp-addon-godotM.N.zip` |

The addon warns in the editor's Output panel when your Godot is newer than the release was made for, and refuses to start in an older Godot.

**2. Enable the plugin.** Copy `addons/godot_mcp` from the addon archive into your project's `addons/` folder, open the project, and enable **Godot MCP Bridge** in **Project → Project Settings → Plugins**. The bridge listens on port `9333`. The addon is editor-only and leaves itself out of exported games while the plugin is enabled; if you export with it disabled, add `addons/godot_mcp/*` to the export preset's exclude filter.

**3. Register the server** in your MCP client, pointing `command` at the extracted binary and `GODOT_PATH` at your Godot executable:

```json
{
  "mcpServers": {
    "godot": {
      "command": "/path/to/godot-mcp",
      "args": [],
      "env": {
        "GODOT_PATH": "/path/to/Godot_v4.x-stable",
        "GODOT_DEBUG_PORT": "9333"
      }
    }
  }
}
```

| Client | Config location |
| --- | --- |
| Claude Code | `.mcp.json` in the project root, or `claude mcp add godot -e GODOT_PATH=/path/to/godot -- /path/to/godot-mcp` |
| Claude Desktop | `claude_desktop_config.json` |
| Cursor | `.cursor/mcp.json` |
| VS Code | `.vscode/mcp.json` (uses a top-level `servers` key instead of `mcpServers`) |
| Antigravity / others | the client's MCP settings file |

On Windows, escape backslashes in JSON: `"C:\\Program Files\\godot-mcp\\godot-mcp.exe"`.

Both paths are machine-specific. Don't commit a `.mcp.json` with real paths: commit a `.mcp.json.example` with placeholders, add `.mcp.json` to `.gitignore`, and let each developer copy it locally.

## Configuration

The server reads its settings from CLI flags, the environment (e.g. the MCP client's `env` block), or a `.env` file in its working directory or a parent directory, in that order of precedence.

| Variable | Flag | Default | Purpose |
| --- | --- | --- | --- |
| `GODOT_PATH` | `--godot-path` | auto-detect | Godot executable. If unset, the server tries a few common install locations, then `godot` on `PATH`; set it explicitly. A value that points to a missing file is not replaced: tools that run Godot fail and name the path. |
| `GODOT_DEBUG_PORT` | `--debug-port` | `9333` | Editor bridge port. |
| `GODOT_MCP_LSP_PORT` | | `6005` | Port of a running editor's GDScript language server. The `godot_code_*` tools use it if an editor listens there, and otherwise start a private headless editor. |
| `GODOT_MCP_TOKEN` | | generated | Shared secret for the bridge. Normally unnecessary, see [Security](#security). |
| `GODOT_CONSOLE_PATH` | `--godot-console-path` | | Windows console wrapper. Unused by the server; the editor end-to-end tests use it for editor logs. |
| `GODOT_MCP_FEEDBACK` | `--feedback` | `full` | Feedback channel for the maintainers: `off`, `draft` (the AI can write a report but never send one) or `full`. A value the server does not recognize gives `draft`. See [Feedback](#feedback-to-the-maintainers). |
| `GODOT_MCP_GH_PATH` | `--gh-path` | `gh` on `PATH` | The [GitHub CLI](https://cli.github.com) used when you let the AI submit a report. Set it when your MCP client does not see `gh` on its `PATH`. |

An empty value counts as "not set", so placeholders in a copied `.env.example` or an MCP client's `env` block are harmless.

In a `.env` file, put **Windows paths in single quotes** (`GODOT_PATH='C:\Program Files\Godot\Godot.exe'`). In double quotes a backslash starts an escape sequence and the whole file fails to load; the server logs a warning when that happens. See [.env.example](.env.example).

The plugin picks its port from the project setting `godot_mcp/bridge_port` if you add one, otherwise from the editor's own `GODOT_DEBUG_PORT` environment variable, otherwise `9333`. Keep it equal to the server's port. Set the environment variable `GODOT_MCP_NO_BRIDGE=1` for an editor that must not open the bridge at all; the server does this for the private editor it starts for the language server. Add the project setting `godot_mcp/verbose` = `true` to log every client connection in the editor's Output panel. Editor Play loads the running-game runtime unless `godot_mcp/runtime_on_play` is `false`.

## Security

- The bridge listens on `127.0.0.1` only, so other machines cannot reach it.
- Every command must carry a random token. The plugin writes it when the editor starts to `<user data dir>/godot-mcp/bridge-<port>.token` (readable by your user only), and the server reads it from there. This also keeps web pages in your browser from driving the editor. Both processes must therefore run as the same OS user. If they can't (containers, Flatpak, different users), set the same `GODOT_MCP_TOKEN` for both instead.
- `godot_create_script` only writes `.gd` files inside the project, never overwrites without `overwrite: true`, and accepts only plain identifiers for `class_name` and `extends_class`. Text you pass in `content` is written as given, never run by the tool; as with `godot_edit_script`, a `@tool` script runs in the editor once the editor loads it.
- `godot_read_script` and `godot_edit_script` work on existing `.gd` files inside the project only, change nothing but the one fragment you name, and never overwrite text you have typed in the editor and not saved. Your fragment is treated as plain text by the tools, never run by them. But code you put into a script is code: Godot runs a `@tool` script inside the editor as soon as it is saved, so editing one runs it with your authority.
- Node paths are resolved inside the edited scene only; the scene root can't be removed.
- `godot_game_call` runs only the methods you list in `permit`, and never one that can quit, evaluate code, call another method by name (`call`, `call_group`, `propagate_call`, `emit_signal`, `notification`, …), set a property by name (`set`, `set_deferred`, `set_indexed`) or free or replace a node (`free`, `queue_free`, `replace_by`). `godot_game_set` refuses `script` and private names, including on a resource reached with `:`. Nothing `godot_game_set` changes is saved to disk.
- `godot_run_project` `user_args` are placed after `--` and only for `mode: "process"`. A value that starts with `-` is a game argument, not a Godot engine option.
- `godot_game_record` writes PNG files (and, with ffmpeg, one movie) only below the system temporary directory, never over an existing recording, and returns paths, not images.
- The authoring tools never replace an existing scene or resource. `godot_set_project_setting` accepts only a plain `section/path` key, escapes the value, and keeps the type of a setting that exists, so it cannot add lines or sections to `project.godot`.

To report a vulnerability, follow [SECURITY.md](SECURITY.md). Never open a public issue for it.

## Tools

"Editor" means the tool needs a running Godot editor with the plugin enabled. "Running game" means a game started with `godot_run_project`, in the editor's Play mode or as a managed process. "Process run" means a managed process run, the one this server can debug.

`project_path` can be omitted when the server runs inside the project: tools then use the project open in the editor, else the first folder with a `project.godot` from the server's working directory upwards, else the only subfolder that has one (such as `app/`). A `project_path` you give always wins: when the open editor has another project, the tools that can work on files (reading and editing a script, creating a scene or a resource, project settings) use the files of the project you named and leave the editor alone, and an answer that changed something names the project directory.

Every tool takes only the parameters its schema lists (`additionalProperties: false`). A call with any other key does nothing and is refused with an error that names the unknown key and the accepted ones, so a misspelled or invented parameter is never dropped while the call reports success. A key set to `null` counts as absent.

| Tool | Needs | What it does |
| --- | --- | --- |
| `godot_get_version` | — | Engine version, from the editor or the Godot binary. |
| `godot_get_project_info` | Editor | Name, main scene, features and directory of the open project. |
| `godot_create_project` | — | Creates a minimal Godot 4 project at an absolute `project_path` with a display `name`, without needing an engine or editor. Preserves existing files and refuses to replace `project.godot`. Uses the Compatibility renderer; no main scene or bridge is installed. |
| `godot_launch_editor` | — | Launches the editor for `project_path`, or for the project the server runs in. |
| `godot_run_project` | — | Plays the main scene or a given `res://` scene. With the editor, uses its Play mode. Without it (or with `mode: "process"`), starts a managed game with the remote debugger attached; `held: true` (process runs only) starts it held, for exact steps. `user_args` (process only) are strings the game reads with `OS.get_cmdline_user_args()`; they are placed after `--` and refused for editor Play and `mode: "auto"`. A source newer than its `.godot/imported` cache is reimported first when the editor has this project open, and otherwise the result names the paths and `godot_resave_resources`. `godot_stop_project` stops either one. |
| `godot_stop_project` | — | Stops what runs: the editor's Play mode when it plays and every managed process run, and lists what it stopped (or says that nothing was running). Pass `run_id` to stop one process run. |
| `godot_get_run_status` | — | Whether a scene is playing, and the managed process runs, each with its `user_args`, exit code, `ended_by` (`game` or `server`) and `ended_at` once it has ended. Says that nothing is running when that is so. |
| `godot_get_open_scenes` | Editor | Open scene tabs and the active one. |
| `godot_open_scene` | Editor | Opens a scene by `res://` path. If that tab is already open and the file on disk has changed, reloads it and drops its undo history. A reload over unsaved editor changes, typed by hand or made by a tool, is refused unless you pass `discard: true`; `reload: true` reloads an open scene even when the file is unchanged. |
| `godot_save_scene` | Editor | Saves the active scene. Names in `scripts_saved` any open script tab with unsaved typing that the editor saved with it; clean scripts are not reformatted. |
| `godot_get_scene_tree` | Editor | Node hierarchy of the active scene, or of another open tab via `scene_path`. Paths are relative to the scene root. |
| `godot_add_node` | Editor | Adds a node of any instantiable `Node` class or global script class, or an instance of a saved scene (`scene`), with optional initial `properties` (undoable). |
| `godot_remove_node` | Editor | Removes a node, restoring order and ownership on undo. |
| `godot_get_node_properties` | Editor | Reads a node's editor-visible properties as `{type, value}` in Godot's text format. `properties` reads only the names given, and `:` steps through a resource (`material_override:shader_parameter/glow_amount`). |
| `godot_set_node_properties` | Editor | Sets properties (undoable), converting values to the property's type: `[10, 20]` or `{"x": 10, "y": 20}` for vectors, `"#ff0000"` for colors, `res://` paths for resources. `:` steps through a resource built into the scene (`material_override:shader_parameter/glow_amount`) in the same undo action; a `.tres` file or another scene's resource is shared, so it is refused. The whole call is rejected if any property is unknown or unconvertible. |
| `godot_capture_viewport` | Editor with a window | Screenshot of the 2D or 3D editor viewport as PNG (`view`, `max_dimension` options). Switches the editor to that main screen; doesn't work with `--headless`. |
| `godot_create_scene` | — | Creates a `.tscn` with one root node (`root_type`, `root_name`) and opens it when the editor is connected. Without the editor it writes the file. Refuses to replace an existing scene. |
| `godot_attach_script` | Editor | Attaches a script to a node (undoable); an empty `script_path` detaches. The script must exist in the project. |
| `godot_connect_signal` | Editor | Connects a node's signal to a method inside the edited scene (undoable). The target must already have the method; a connection that exists is refused. |
| `godot_list_signals` | Editor | Signals of a node in the edited scene and what each is connected to. |
| `godot_create_resource` | — | Creates a `.tres` of a built-in `Resource` class. With the editor, Godot saves it; without it, an empty resource is written as text. Never replaces an existing file. |
| `godot_get_project_settings` | — | Reads project settings: live from the editor, else the entries stored in `project.godot`. Filter with `prefix`. |
| `godot_set_project_setting` | — | Sets one setting (`section/path`) to a string, number or boolean and saves it. A setting that exists keeps its type: a value of another type is refused. |
| `godot_get_input_map` | Editor | Lists the InputMap actions with their deadzone and events (`type` `key`, `mouse_button`, `joypad_button` or `joypad_motion`). Godot's built-in `ui_` actions are left out unless `include_builtin` is true. |
| `godot_edit_input_action` | Editor | Adds or removes events of one action (`add_events`, `remove_events`), sets its `deadzone`, creates it (`create: true`) or deletes it (`delete: true`), and saves `project.godot`. One undo step in the editor. All or nothing: an event to remove that the action lacks, or an unknown key name, changes nothing; an event already there is listed under `already_present`. Keys use the physical key by default (`physical: false` for the layout key). Built-in `ui_` actions can be edited but not deleted. |
| `godot_create_script` | — | Writes a `.gd` file into the project: the text you pass in `content`, then checked as `godot_validate_script` does (a script that does not validate is an error answer, and the file stays for `godot_edit_script`), or else an `extends` / `class_name` / `_ready()` template (`res://` path; the project is `project_path`, the editor's, or the one the server runs in). Refuses to overwrite unless `overwrite: true`. When the script is open in the editor, the overwrite is refused while its tab has unsaved changes and otherwise reloads the tab. |
| `godot_read_script` | — | Reads an existing `.gd` script as the editor would save it: the editor's unsaved text when the script is open with unsaved changes (`dirty: true`), otherwise the file. Returns the text and a `version` label for it; `stale_tab: true` means an open tab still shows older text than the file, and `godot_resave_resources` with the path reloads it. Without the editor it reads the file. Scripts over 1 MiB are not handled. |
| `godot_edit_script` | — | Replaces one fragment of an existing `.gd` script: `old_text` must occur exactly once, and `new_text` takes its place. With the editor the change is one undoable action and the script is saved; it is refused while the script has unsaved changes in the editor (save or discard them, then read again). Pass the `version` from `godot_read_script` to refuse the edit if the script changed since. Without the editor the file is replaced as a whole and keeps its line endings. Never creates a file. |
| `godot_validate_script` | — | Checks a `.gd` file. When a language server is already running, lists its errors and warnings (`file:line:col`, code, message); a warning does not fail the check. Without one, `godot --headless --check-only` sees parse errors only and the answer says analyzer warnings were not checked. |
| `godot_run_script` | Godot binary | Runs an `extends SceneTree` script headless (`--path` project, `-s` script, `user_args` after `--`). Returns the exit code, stdout, stderr, and each `SCRIPT ERROR` with its file and line. `timeout_seconds` defaults to 120 (max 600); on timeout the process tree is killed and the last output lines are included. A `SCRIPT ERROR` that leaves the main loop idle (the process stays up, as an error in `_init` does) stops the run then, and the answer says so, instead of waiting out the timeout. A script that does not extend `SceneTree` or `MainLoop` is refused before Godot starts; a script Godot could not load (parse error, wrong base, missing file) is an error with `load_error`, even though Godot exits with code 0. Paths outside the project are refused. A `.tscn`/`.tres` the script saves loses its `uid://` references and gets `null` for every shader uniform it never set (headless Godot has no editor file system and no renderer); `resource_damage` names what each rewritten file lost, so edit scenes through the editor tools instead. |
| `godot_delete_file` | — | Deletes project files (`paths`, up to 64) with their `.uid`, `.import` and `.godot/imported` data. Refuses a file another project file still names by `res://` path or `uid://` unless `force: true`, and lists those `references`; refuses `project.godot`, `.godot/`, the Godot MCP addon, a `.uid`/`.import` whose file exists, and directories (an empty one with `allow_empty_dir: true`). All or nothing: while any path is `refused`, nothing is deleted. With the editor, open scene and script tabs of the files are closed (a tab with unsaved changes is refused), the FileSystem dock is updated and the files go to the system trash unless `trash: false`. Without the editor the files are deleted permanently and `rescan_needed` is true. `dry_run: true` only reports. |
| `godot_resave_resources` | — | Refreshes the given `res://` files after external edits. With the editor, assets it already knows are re-imported synchronously and other files are re-scanned; an open script tab without unsaved changes is reloaded from its file (`reloaded_tabs`), and one with unsaved changes is kept (`unsaved_tabs`). A path it has never scanned is scanned first and listed under `reimported` only once a `.import` file exists; otherwise it is `not_scanned`. Without paths, rescans the whole project in the background. `wait: true` returns only after scanning and importing finish (up to 60s) and reports how many files were imported. Without the editor, runs `godot --headless --path <project> --import` and reports the sources whose `.import` sidecar appeared or changed. |
| `godot_check_voxel_model` | Froxel | Exports one `.vox`, or up to 16 `models`, through `froxel mcp`, writes each `.glb` into the project, imports it and checks the `.import` sidecar. `preview: true` adds a Froxel path-traced PNG. See [Checking voxel model exports](#checking-voxel-model-exports). |
| `godot_run_output` | Running game | Logs, warnings and errors, with stack traces, from a managed process run or the editor's running game (`since`, `level`). |
| `godot_game_tree` | Running game | Live nodes of the running game: path, class, position, visible text. Filter by `name`, `class`, `group` or `text`. |
| `godot_game_get` | Running game | A live node's properties, groups and, optionally, signals and methods. `/` reads a Dictionary key or an Object property (get only; private names are refused). A missing segment is that property's entry in `property_errors` and names the segment; other properties are still returned. `null` is a value that really is null. `:` steps through a Resource (`material:shader_parameter/caustics`), including a name that starts with `_`. A requested `script` is omitted. A non-resource `:` step names the `/` form. |
| `godot_game_set` | Running game | Sets properties on a live node. `/` writes an Object field or a Dictionary key; an unknown property is an error that names the segment. `:` steps through a Resource. A `res://` path assigns a saved resource to an Object property. Refuses `script`, private names, and a Node in the middle of a `/` path. Nothing is saved to disk. |
| `godot_game_call` | Running game | Calls one method on a live node. The method must be listed in `permit`; methods that can quit, evaluate code, call other methods by name, set properties by name or free or replace the node are refused. A JSON string is passed as a `StringName` or a `NodePath` where the method asks for one. When the method takes the node out of the tree (it reloads or changes the scene), the answer has `left_tree: true` and the path the node had before the call. |
| `godot_game_signals` | Running game | A live node's signals and their connections. |
| `godot_game_observe` | Running game | Watches signals for a few frames, returns the ones that fired, then disconnects. |
| `godot_game_snapshot` | Running game | Scene path, monotonic `scene_generation`, `scene_instance_id`, frame, pause flag, children, and the loaded root script path plus its SHA-256. |
| `godot_game_state` | Running game | Reads the states the project's own adapter script registered for its game (the run, the level, the balls...), by name; with no names it reads all of them and lists the adapter's commands with their help. See [The project adapter](#the-project-adapter). |
| `godot_game_command` | Running game | Runs one command the project's adapter registered, by name, with a dictionary of arguments. The command does what the project wrote, files included. See [The project adapter](#the-project-adapter). |
| `godot_game_input` | Running game | Injects key, text, mouse, touch and gamepad input, and waits, as a sequence of steps. Pointer coordinates are in `coordinate_space`: `viewport` (default) is pixels of the root viewport's visible rectangle (what `_input` sees after content scale), `window` is pixels of the game window; neither is a screen or world pixel. The answer names the space and the mapping between the two, and `click_node` returns the point it clicked. A touch step also emits the engine's emulated mouse event first (`device` `DEVICE_ID_EMULATION`, same position) when `emulate_mouse_from_touch` is on; `emulate_mouse: false` turns that off for the call and restores it afterwards. Gestures: `click`, `swipe`, `path` (press, drag through points, release; at most 600 frames), `circle` (that drag around a center) and `click_node` (the center of a Control, Node2D or Node3D). More than 20 steps return `ok_count` and `failed` unless `verbose` is true. `time_scale` sets `Engine.time_scale`. A step that fails releases everything the server still holds. Mouse steps are Godot events and leave the OS cursor in place unless `warp_mouse` is true. |
| `godot_game_input_release` | Running game | Releases every key, button, axis and touch this server still holds down. |
| `godot_game_wait` | Running game | Waits for process frames, for a node to exist (`node`), for a signal (`node` and `signal`), for a public property (`property`, `op`, `value`), or for a newer `scene_generation`. A missing `/` segment is an error that names it, not a timeout; `null` means the value is null. `timeout` is in frames. `time_scale` (0 to 8) sets `Engine.time_scale`; the runtime restores the previous value when the game exits. |
| `godot_game_physics` | Running game | Controls game time. `hold` stops the game at the start of a physics tick; while held, `step` runs an exact number of ticks (or until a condition holds), input is applied by tick, and `resume` lets the game run. Without a hold, `step` advances N engine physics iterations and leaves the tree paused. See [Holding a game](#holding-a-game). |
| `godot_game_scenario` | Running game | Plays one input sequence in a held managed game: Input Map actions and pointers, a known number of physics ticks per step, then `CharacterBody` and property snapshots, warnings, errors and an optional screenshot. Releases all input when it ends. See [Playing a scenario](#playing-a-scenario). |
| `godot_game_screenshot` | Running game with a window | PNG of the game window, a viewport or a camera; `frames` (up to 8) stitches shots taken every `every` frames into one sheet. The text names the `godot_game_input` coordinate space (root viewport width and height) and the scale from image pixels to that space, including a sheet (per frame) and a camera. Compares with the previous capture, and can save or compare a named baseline (`save_baseline`, `baseline`, `tolerance`). `roi` is the comparison region and requires `baseline`; it does not crop the image. The text names the drawn-frame count and says STALE when the window drew no new frame before the capture (a covered or minimized window on macOS), so an old picture is not taken for the current one. An error, never an empty image; a headless game has nothing to capture. |
| `godot_game_record` | Running game with a window | Saves up to 240 PNG frames of the game window to a directory below the system temporary directory (`frames`, `every`, `max_dimension`, `directory`). The reply lists paths, sizes and timings and does not embed the images. `format` `gif` or `mp4` also builds a movie when `ffmpeg` is on `PATH`; without it the PNG files are the result. |
| `godot_debug_breakpoints` | Process run | Lists, adds, removes or clears breakpoints (`res://script.gd:LINE`). Editor Play uses the editor's own debugger. |
| `godot_debug_control` | Process run | Pauses, resumes or steps (into, over, out) and waits for the next stop. |
| `godot_debug_state` | Process run | Stack frames and variables of a stopped run. |
| `godot_code_diagnostics` | Godot binary | Errors and warnings from Godot's language server, including analyzer warnings that `godot_validate_script` only sees when this server is already running. Uses an editor already listening, or starts a private headless one. Never reports a clean file when the server does not answer. Godot reports no warnings for scripts under `res://addons/` unless the project sets `debug/gdscript/warnings/exclude_addons=false`; the answer says so (`warnings_hidden`). |
| `godot_code_complete` | Godot binary | Code completion at a line and column. |
| `godot_code_symbols` | Godot binary | Symbols of a GDScript file. |
| `godot_code_definition` | Godot binary | Go to definition at a position. |
| `godot_code_references` | Godot binary | Find references at a position. |
| `godot_note_feedback` | — | The AI records something missing, broken or misleading in godot-mcp itself. Kept in memory only. |
| `godot_draft_feedback` | — | Turns the notes into one report for the maintainers and shows it to you. Sends nothing. |
| `godot_submit_feedback` | — | Publishes a draft as a GitHub issue, only after you said yes. Not available in `draft` mode. |

## Feedback to the maintainers

While it works, the AI can note what is missing, broken or misleading in godot-mcp itself: a tool that fails for no clear reason, a capability no tool offers, a description that is wrong. When your request is done, it builds one report and shows it to you. You decide what happens next:

- **Copy it** and open an issue yourself, or open the prefilled new-issue link the AI gives you.
- **Let the AI submit it.** It publishes the issue through your own signed-in [GitHub CLI](https://cli.github.com) (`gh auth login`), under your GitHub account. This needs your explicit yes in the conversation.
- **Discard it.**

What is recorded: the AI's own notes, and from the server only the versions (server, addon, Godot), platform, whether the editor was reachable, the first line of recent failures of the tools a note names, and the names of tools the AI asked for that do not exist. Never gathered: project name, scene, script or resource contents, screenshots, environment values, the bridge token. Everything stays in memory until you approve a submission, and absolute paths, tokens and `@mentions` are removed from the text. The report will be public, so read it before you agree; the redaction is a safety net, not a guarantee.

To tighten it, set `GODOT_MCP_FEEDBACK=draft` (the AI can write reports but has no way to send one) or `off` (the feature does not exist). If your MCP client lets you pre-approve tools, do not pre-approve `godot_submit_feedback`: a per-call prompt is the only check outside the AI's own judgement.

## Checking a running game

These calls work on a managed process and on editor Play. A property wait reads with `get` only. It does not evaluate an expression or call a method. A name that starts with `_`, the name `script`, and a path that goes through an Array are refused. `eq` and `ne` need the same type, except `null`: it compares with any value, so `eq null` waits for a property to be cleared. JSON has one number type, so a whole number such as `1` may match an int property; `1.5` against an int is a type error. Strings are not turned into numbers. Float equality is exact.

```json
{"node": ".", "property": "stats/hp", "op": "gte", "value": 1, "timeout": 60}
```

`scene_generation` on a snapshot increases when the current scene instance changes, including a reload of the same path. Wait until the running generation is newer, optionally also until a node exists:

```json
{"scene_generation": 3, "node": "Player", "timeout": 180}
```

`script_sha256` is the SHA-256 of the root script source Godot had loaded when that scene instance became current. It is not a later read of the file on disk. Reloading the same scene keeps that hash when Godot reuses the script it already loaded (`Resource.reload` on a script that is in use returns "Already in use"). The hash changes when a new scene instance loads different source, for example a scene whose script was not loaded yet. Log lines copy that stamp when they are written, so an older error keeps the hash from its own scene. `script_sha256_basis` is `loaded_source_at_scene_enter` when the source was available, and `unavailable` when it was not. A line recovered only from the debugger buffer after the runtime logger was gone is also `unavailable`; that is not the file currently on disk. `StringName` values, such as a node's `name`, are compared as text. A bool is not a number.

A named baseline is the returned image, so `roi` uses that image's pixels (`roi_space` is `returned_image`), including a sheet. `tolerance` is the per-channel difference that still counts as unchanged, from 0 to 255. The text reports `changed`, `sampled`, `ratio`, `bounds`, `tolerance`, and whether the count is `exact` or `sampled`. At most 8 baselines are kept, each at most 512×512 pixels. A missing name, a bad name, or an `roi` outside the image is an error and does not replace a saved image. The previous-capture comparison is unchanged: a sheet compares its last frame, and only with a capture of the same size.

```json
{"save_baseline": "menu", "max_dimension": 256}
{"baseline": "menu", "roi": {"x": 0, "y": 0, "w": 80, "h": 24}, "tolerance": 8}
```

`godot_game_set` writes a property of the live node. A value is converted to the property's current type: numbers, `[x, y]` / `[x, y, z]` arrays for vectors, `[r, g, b, a]` for colors, and a Godot literal string (`"Transform3D(1, 0, 0, 0, 1, 0, 0, 0, 1, 2, 2, 4)"`, `"Basis(...)"`) for any typed property, as `godot_set_node_properties` parses them in the editor. A Basis, Transform3D or Transform2D also accepts vector axes as GDScript constructs them: `"Basis(Vector3(x axis), Vector3(y axis), Vector3(z axis))"`, with the origin last for a transform. An Object property (`mesh`, `material_override`, a script's `Resource` variable) accepts the `res://` path of a saved resource, which the game loads and assigns when its class is one the property takes, or `null` to clear it; a missing file, a resource of another class or text that is not a `res://` path is an error that names the path and the class expected. A value of the wrong type is an error that names the property's type. Every value is checked before any is written, and a refused value or a write that did not take leaves every property of the call unchanged. `/` writes a field of an Object or a key of a Dictionary, the same way `godot_game_get` reads one. A missing property or key is an error that names the segment, and a new Dictionary key is not created. The value is read back; a write that did not take is an error, not success with `null`. Floats, vectors, colors and transforms are compared approximately, because many engine floats are 32-bit. `:` steps through a Resource. A `/` that belongs to one Godot property name stays that name, which is how a shader uniform is spelled. A `/` step stays inside the object it starts from, so a Node along the way is an error and that node is named with `path`.

```json
{"path": ".", "properties": {"round_state/letter_index": 22}}
{"path": "WaterPaper", "properties": {"material_override:shader_parameter/caustics": true}}
```

`script`, a segment that starts with `_`, and a `:` step through something that is not a resource are refused. The change lasts until the game exits. It is not saved. A shader uniform that nothing has written yet reads as null; Godot does not return the default declared in the shader until a value is stored.

`godot_game_record` writes that motion to disk when eight screenshot frames are not enough. `frames` is 1 to 240, `every` is the process frames between shots, and `frames * every` is at most 480. Each frame only grabs an image. The PNG files are written after the last frame, so the timings are the capture times, and `frames × width × height × 4` must stay within 512 MB after `max_dimension`. `directory` must be an absolute path below the system temporary directory (nothing is written elsewhere); omit it for a new one. The reply has no image bytes. `format` `gif` or `mp4` runs `ffmpeg` when it is installed; otherwise the PNG files are still there.

```json
{"frames": 180, "every": 1, "max_dimension": 480, "format": "png"}
```

`user_args` on `godot_run_project` is only for `mode: "process"`. The server places them after `--`, so `--caustics` is a game flag and not a Godot option. `godot_get_run_status` lists them on the process run.

`godot_game_physics` with `{"action": "step", "ticks": 4}` advances four real engine physics iterations and then pauses the tree. `{"action": "resume"}` continues. Each tick has the normal physics delta, so four ticks are four times `1 / physics_ticks_per_second` seconds of game time, multiplied by `Engine.time_scale`. The step allows at most one physics iteration per process frame while it runs, then restores `max_physics_steps_per_frame`; the tick rate and the time scale are not changed. It does not call `_physics_process` on your nodes. It does not step idle `_process`, nodes with `process_mode` `ALWAYS` (those keep running while the tree is paused), custom threads, or GPU compute, and it does not make the game deterministic. The engine physics-frame counter also keeps moving while the tree is paused, so it is not a count of pausable game physics. If the step fails, or the runtime is removed while a step is still in flight, `max_physics_steps_per_frame` is restored and the tree is left paused.

## Holding a game

`godot_game_physics` with `{"action": "hold"}` stops the running game at the start of a physics tick. Nothing advances while it is held: no tick, frame, timer or awaited signal, including nodes whose `process_mode` ignores pause, and the game's own pause cannot release it. The window stays responsive. Reads are still answered (`godot_game_tree`, `godot_game_get`, `godot_game_set`, `godot_game_call` with `permit`, `godot_game_signals`, `godot_game_snapshot`, `godot_run_output`, and a single `godot_game_screenshot` of the last frame the game drew).

```json
{"action": "step", "ticks": 5}
{"action": "step", "until": {"node": ".", "property": "stats/hp", "op": "lte", "value": 0}, "max": 600}
{"action": "resume"}
```

A step runs exactly that many physics ticks (1 to 600) and holds again; the answer says how many ran. With `until` the condition is read before every tick, the first included, so one that already holds runs no tick, and the step stops at `max` (default 600, at most 3600) with `until_met` saying which happened. `until` reads a public property with `get`, like a `godot_game_wait` property wait: it never evaluates an expression. A condition that cannot be read yet does not hold; one that does not parse is refused before any tick runs.

Input is applied by tick while held. `godot_game_input` takes key, mouse button, touch and joypad button taps, clicks, mouse moves, axis values and `time_scale` at once; a tap stays down for `hold_ticks` ticks (default 2, 1 to 600) and is let go at the start of the tick it falls due on, so splitting a step in two does not change what the game sees. A press made while held is down from the next tick and is reported as just pressed on the second one. Leaving the hold, with `resume` or by closing the game, releases everything still held.

A held game refuses what needs time to pass, and says to step instead: `godot_game_wait`, `godot_game_observe`, a screenshot sheet or camera switch, `godot_game_record`, and input `wait`, `text`, `swipe`, `path` and `circle` steps. `godot_debug_control` `pause` is refused too, since the game is already still; a breakpoint hit during a step stops the game as usual.

Steps are **exact** only for a game started with `godot_run_project` `{"mode": "process", "held": true}`. That start fixes the engine's frame rate to the project's tick rate (`physics/common/physics_ticks_per_second`, 60 when unset), so every tick is paired with one frame, and the answers say `"deterministic": true`. A hold entered later, or on the editor's Play, still stands still but is reported as `"deterministic": false` with a `deterministic_reason`: `not_started_held`, `fixed_rate_differs` (the game changed its tick rate) or `frames_did_not_follow_ticks` (a step saw frames fail to follow its ticks, which ends the claim for the rest of the run). A step runs its ticks as fast as the game can, so `max` bounds ticks, not seconds. Work on other threads and file loading takes real time, so after starting a game repeat short `until` steps instead of guessing a tick count. A held step that takes longer than 60 seconds is not cancelled: the call returns an error saying the game finishes it and holds again, and `status` shows where it is.

Closing the game's window ends a hold. The server stopping the run does too. A server that disappears without stopping the run leaves its game held, with a window that can still be closed.

## Checking voxel model exports

`godot_check_voxel_model` exports one `.vox`, or a batch of up to 16 models,
through Froxel MCP and imports the resulting GLBs into the selected Godot project.
Set `FROXEL_BIN` to a Froxel executable supporting `export_models` (or put it on
PATH). Both are trusted programs, like `GODOT_PATH`: the server starts `<bin> mcp`
without the bridge token in its environment. Pass `vox` and `dest`, or
`models: [{vox, dest}]`; source paths are absolute paths on this machine (a network
share is refused, and the source is not confined to the project), and destinations are
project-local `res://` GLB paths outside `.godot/`. Optional `preview: true`
path-traces `<model>.preview.png` beside each successfully exported model, so a
texture with the model's name is never overwritten. This is a Froxel preview, not a
Godot viewport capture.

All output paths are validated before writing. Output names must be unique,
including previews, links and case-only aliases. Each Froxel process has a
60-second deadline covering initialization, tool replies and process exit.
A Froxel reply that leaves a model out, or that reports a model as exported without
leaving its `.glb`, makes the check fail (`missing_exports`). The report preserves
per-model export failures even when no model can be imported; a mixed batch imports
and previews only successful exports. The import
goes through the connected editor when it has the project open, and is headless
otherwise. The Godot check verifies import sidecars and reports a changed `.glb`
whose cache is older than the file under `stale_imports` (an export that left the
bytes unchanged is not re-imported, so a cache that is older for another reason is
not reported); it does not yet verify the instantiated model's meshes or animation
behavior.

```json
{"vox": "/path/lily_pad.vox", "dest": "res://art/lily_pad.glb", "project_path": "/path/frog-world"}
```

The end-to-end test needs a Froxel build and the Godot build named in `.github/godot-version`:

```bash
FROXEL_BIN=/path/to/froxel GODOT_PATH=/path/to/godot \
  cargo test --test model_chain_e2e_test -- --ignored --test-threads=1
```

## The project adapter

A game has its own vocabulary: the run, the level, the balls still in play. `godot_game_state` and `godot_game_command` let an agent read and act in those terms instead of walking the node tree. The project teaches them through one script it owns, the adapter, named in the project setting `godot_mcp/adapter` (a `res://` path or the `uid://` the file chooser stores). The setting is read once, when the game starts; set it with `godot_set_project_setting` or in Project Settings, then start the game again. The adapter is development code: nothing in the addon names it, and the caller can only ask for what it registered.

```gdscript
extends RefCounted

var _registry

func register(registry) -> void:
    _registry = registry
    registry.add_state("level", _level)
    registry.add_command("grant", _grant, "Grants an effect: {name}.")

func _level() -> Dictionary:
    return {"index": 3}

func _grant(args: Dictionary) -> Dictionary:
    if not args.has("name"):
        return _registry.refuse("name is required")
    return {"granted": args["name"]}
```

- A state provider takes no arguments and returns any value. A command takes one dictionary (the call's arguments, numbers as floats) and returns any value. A command refuses its arguments with `refuse(message)` on the registry it was given in `register`; the caller gets the message as bad arguments. A returned dictionary with an `error` entry is just a result.
- Reading is `godot_game_state` with `names` (all states when omitted); each state that fails is its own `{error}` entry, and the errors and warnings the engine logged meanwhile come back in `errors`. An answer larger than 262144 bytes is refused with the size of each state.
- A command is `godot_game_command` with `name` and `args`. It is not limited by the refusals of `godot_game_call`: it may free nodes, change the scene or write files, whatever the project wrote. A script error inside it fails the call and may leave the game changed. A command and a state must not `await`; what a command defers (`call_deferred`, a timer) takes effect when the game next advances, so step a held game before reading the result.
- Both tools work in a managed run, in editor Play and in a held game. `godot_get_run_status` shows each run's adapter: whether it loaded, what it registered and why a registration or the whole adapter was refused.
- An adapter written for godot-dev-bridge works unchanged: set `godot_mcp/adapter` to the same script. Values with engine types (vectors, colors) come back as `godot_game_get` shows them.

## Playing a scenario

`godot_game_scenario` is one call that presses Input Map actions and pointers, advances physics ticks, and reads the result. It does not run GDScript from the caller. Start the game with `godot_run_project` `{"mode": "process", "held": true}` when the tick count must be exact. The call leaves the game held; `godot_game_physics` `{"action": "resume"}` lets it run, and `godot_stop_project` stops it. The arguments, what is and is not deterministic, and a worked example are in [docs/game-checks.md](docs/game-checks.md).

```json
{"steps": [
  {"frames": 20, "actions": [{"action": "move_right", "phase": "press", "strength": 1}, {"action": "jump", "phase": "tap"}]},
  {"frames": 40}
], "bodies": ["Player"], "nodes": [{"path": "Player", "properties": ["jump_count"]}]}
```

## How it works

```
AI client <-stdio JSON-RPC-> godot-mcp <-WebSocket :9333-> addons/godot_mcp (inside the Godot editor)
                                 |
                                 +-> Godot binary (headless fallback)
```

Game tools reach a game through Godot's remote debugger: a managed process connects to this server on loopback, and the plugin relays editor Play. The comparison with the other Godot MCP servers is in [docs/feature-parity.md](docs/feature-parity.md).

## Roadmap

Not implemented yet:

- HTTP/SSE transport

## Development

To contribute — set up, choose the branch for your Godot version, run the checks and open a pull request — read [CONTRIBUTING.md](CONTRIBUTING.md). AI agents start with [AGENTS.md](AGENTS.md). Releases are described in [CONTRIBUTING.md](CONTRIBUTING.md#releases).

Running-game input, inspection, and the remote debugger need the Godot build named in [`.github/godot-version`](.github/godot-version):

```bash
GODOT_PATH=/path/to/godot cargo test --test runtime_e2e_test -- --ignored --test-threads=1
```

## License

MIT. See [LICENSE](LICENSE).
