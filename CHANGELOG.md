# Changelog

All notable changes to godot-mcp. The server and the Godot addon always ship with the same
version number. The section for a version is used as the text of its GitHub Release, so every
release needs one (the release workflow fails without it).

Each Godot version has its own release line and branch (`godot-v4.5`, `godot-v4.6`, `godot-v4.7`, …). From 0.5.1 on, one release holds the files of every supported Godot version, and its notes are the section of this file; they mark changes that concern some Godot versions only.

## [Unreleased]

### Added

- `godot_run_script` takes `fixed_fps`, which starts Godot with `--fixed-fps`: frames stop waiting for the clock, so a test script that waits on `physics_frame` runs as fast as the CPU allows and counts the same frames. 600 physics frames take about 10 s in real time and well under a second with `fixed_fps: 60`. Game time stays exact (physics keeps the project's rate and delta); what reads the wall clock, threads and background loads do not follow it, and the tool description says so.
- `godot_review_assets` renders a set of the project's scenes together in one PNG, without writing a scene into the project. The scenes stand in a row `gap` apart, or at the positions, rotations and scales you give; the light comes from a level scene's `WorldEnvironment` and `DirectionalLight3D` nodes, an `Environment` resource plus a sun, or a neutral default; the camera is orthogonal or perspective (`yaw`, `pitch`, `size` or `fov`, fitted to the assets, or copied from a scene's `Camera3D`); `planes` adds ground or water. The answer gives each asset's bounds, triangles, surfaces and mesh instances, and Godot's rendered primitives and draw calls with that asset alone, plus the totals of the picture. It runs a fixed script in a temporary Godot process (a small window: a headless Godot does not render) that reads the checked request from a JSON file, so no argument reaches the command line or generated source.
- `godot_game_run_script` runs one function of a `.gd` file inside the running game, against the live scene, and returns its value, what was printed, the errors with their stacks and the ticks that passed, so a test of a heavy scene does not load it again on every run. The file is compiled again on every call. The function may await physics frames, process frames, timers or signals: a game held at a fixed step (`godot_run_project` `held` and `headless`) runs ticks as fast as it can until the function returns, then holds again. A script error, a value that is too large or a function still waiting after `timeout_seconds` is an error that carries the same answer, and the scene stays loaded.
- `godot_world_map` draws a top-down map of a world split into many scene files (streamed zones) without running the game: the scenes from `dir` and `glob`, or a list of `scenes`, stand at the transforms saved in their files and are rendered with an orthographic camera in tiles, loading only the scenes that touch a tile, so memory stays bounded. `labels` draws each scene's bounds and file name (in red where collision or geometry is missing), and `view: "collision"` draws the collision shapes, which shows holes in the ground. The answer lists `totals` and the `problems` (scenes that lack collision or geometry, or fail to load) first, then the map's world rect and scale, and a JPEG preview; `detail` adds one entry per scene, and `output_path` writes the full-size PNG and the full JSON inside the project.

### Fixed

- A managed game that ends during a call no longer leaves that call waiting for its whole timeout, and the calls after it no longer report an unreachable editor bridge. Game tools now answer with the run's id, its exit code or signal, who ended it (`ended_by`) and the last stderr lines. `godot_get_run_status` reports the same, with new fields `signal`, `how_ended` and `last_stderr`, and `godot_stop_project` lists `recently_ended`. A timeout says what it ended: `godot_game_run_script` marks `timeout_ended` and `game_running` when the game ended the function, and a timeout of the server's wait says the run is still alive and that nothing was stopped. `godot_run_output` without a `run_id` reads the run that ended last.
- A game line on stdout or stderr that is not UTF-8 no longer kills a managed game. The server stopped reading that pipe, so the game died of SIGPIPE at its next print.
- `godot_stop_project` no longer claims it ended a game that had already exited by itself.
- `godot_run_script` says why a timed-out run has no output, where it answered `(no output)`. Godot writes each `print()` line as it is made and the server reads it at once, so everything printed before the kill was already returned; a script that prints only at the end has nothing to return, and the answer now says to print each result as it is made. The exception is a project with `application/run/flush_stdout_on_print.debug=false`, where a kill loses what Godot still buffers and no flag overrides it: `stdout_buffered` says so and names `printerr()`. A stream that outgrows the 64 KiB of the answer is kept whole, up to 256 MiB, in `.godot/godot-mcp` (`stdout_log`, `stderr_log`, with `stdout_bytes` and `stderr_bytes`). A script still printing after that much output and an earlier `SCRIPT ERROR` is no longer taken for idle and stopped.
- `godot_run_script` allows a `timeout_seconds` of up to 3600: 600 had no technical reason, a run does not block other tool calls, and a long test could not finish within it.
- `godot_run_script` sends `notifications/progress` when the client passes `_meta.progressToken`: the newest output line while the script runs, and a heartbeat every 5 seconds without output.
- A scene opened right after the editor started no longer loses its tab to the project's main scene. The addon's bridge listens while the editor is still scanning the project's files, and the editor opens the main scene (or the scenes of the last session) only when that scan ends, so `godot_open_scene` could answer `active_scene` with the scene and the next `godot_get_node_properties` or `godot_set_node_properties` acted on the main scene. Until the editor has started, the bridge answers `ping` with `starting: true`, `get_version` and `get_project_info`, and refuses every other command with a message to call again in a few seconds. Thanks to @theromis.
- `godot_run_script` records the uids it writes back into headless-saved scenes and resources in the project's uid cache (`.godot/uid_cache.bin`) when no connected editor holds the project. Only the editor writes that cache, so a script that built new scenes left their new uids out of it, and the next run that loaded them printed an "ext_resource, invalid UID … using text path instead" warning for every reference until `godot_resave_resources`. The answer counts the recorded entries in `uid_cache`, and `uid_cache_stale` names `godot_resave_resources` when the cache could not be updated (the project has no cache yet, it does not parse, or the editor did not refresh the files). Thanks to @theromis.
- `godot_delete_file` no longer refuses a file that a GDScript file only writes, such as a preview scene a tool script saves with `const OUT := "res://…"` and `ResourceSaver.save(scene, OUT)`: the script can create the file again. Every reference now has a `kind`, `written` when the path only reaches `ResourceSaver.save`, `FileAccess.open` for writing, `DirAccess` removal or `Image.save_png` and the like (directly or through a constant or variable), `used` for anything else and for whatever the analysis cannot follow. Only `used` references refuse the call without `force`; a file named only by writers is deleted with a warning naming them, and `references` counts both kinds. Thanks to @theromis.
- `godot_game_screenshot` captures a game whose window is covered or minimized. On macOS the engine stops drawing such a window, so every capture returned the last drawn frame marked STALE, with or without `camera`, and an agent could not check visuals while the person worked in other windows. When the window draws no new frame within 4 process frames, the capture now draws one frame of the rendering server itself (`RenderingServer.force_draw`, not presented in the window) and reads that, so the image shows the current game state. The text and `render.path` say which path produced the image: `window`, `forced_draw`, or `mixed` for a sheet; `render.forced_frames` counts the frames drawn that way. A held game whose window did not draw the held tick (covered, or a step that ran several ticks in one frame) is drawn the same way; the game does not advance. A visible window is captured as before. Thanks to @theromis.

## [0.13.0] - 2026-11-04

The server can update a project's addon (`godot_update_addon`) and tells an editor whose addon is older than the server how to; it can export a project without the editor, change a file's import parameters and importer, and create, read and edit text shaders. The answer of a long script stays readable, a script no longer starts on stale imports, and the voxel model check is removed.

### Added

- `godot_update_addon` installs or updates `addons/godot_mcp` in a project with the addon built into this server, so the plugin and the server have the same version, keeps the plugin enabled in `project.godot`, and with `restart_editor: true` closes and reopens a connected editor around the update. A command the editor's addon does not have (an addon older than the server) is no longer a bare "Unknown command type": the error names the addon's version, the server's and the update call, and `godot_read_shader` and `godot_edit_shader` read and edit the shader file instead, saying so. The addon reports the version it loaded in `get_project_info`. Thanks to @theromis.
- `godot_list_export_presets`, `godot_set_export_preset` and `godot_export_project` export a project without the editor. The presets in `export_presets.cfg` are listed, created and changed through Godot's own `ConfigFile` (an option keeps its type, `new_options` names the options that were not stored before, and the call is refused while the editor has the project open because it writes its own copy back). An export runs `godot --headless --import`, then `--export-pack` (a `.pck` or `.zip`, no template needed), `--export-release` or `--export-debug`, and answers with `ok`, the output path and size, the exit code, the `ERROR`/`WARNING` messages with their reasons and the end of the log. A missing preset, a missing export template, a failed import, an export that wrote no file and a timeout are errors with a way out; the output must stay inside the project. Thanks to @theromis.
- `godot_get_import_params` and `godot_set_import_params` read and change a file's import parameters, the `[params]` of its `.import` sidecar, the way the Import dock does, and reimport it, so a texture can get VRAM compression (`compress/mode: 2`) or copy every parameter of a sibling (`copy_from`) without a hand-written script. With the editor the edit goes through its file system and the dock and loaded resource follow; without it Godot's `ConfigFile` edits the file and `godot --headless --import` reimports. An option keeps its type, `new_options` and `ignored_options` name options that were not stored before and the ones the importer dropped, and a file with no `.import` yet is refused with `godot_resave_resources` as the way out. Thanks to @theromis.
- `godot_set_import_params` takes `importer` to switch the importer a file uses, as the Import dock's "Import As" does, so a PNG strip of 33 slices becomes a `Texture3D` (`importer: "3d_texture"`, `params: {"slices/horizontal": 33, "slices/vertical": 1}`) for `Environment.adjustment_color_correction`. The new importer starts from its own defaults; the name is checked against Godot's built-in importers for the file type, and an importer the reimport did not use (Godot falls back to the file type's default one) is an error. The answer gives `importer`, `previous_importer` and the imported `type`; with the editor, a loaded copy of the old type is replaced in its resource cache. Thanks to @theromis.
- `godot_create_shader`, `godot_read_shader` and `godot_edit_shader` create, read and edit text shaders (`.gdshader`, `.gdshaderinc`) the way the script tools do for `.gd` files: create from given code without replacing a file unless `overwrite: true`, read the text with a `version` label, and replace one exact fragment. With the editor connected they go through the shader editor and the shader it holds, so an open tab, the editor's copy and the materials using it stay in step, an edit is one undo step, and unsaved typing is never overwritten; without it they work on the file. A new shader gets a `.uid` file, and an existing one keeps its uid. `godot_validate_shader` compiles a saved shader headless with Godot's shader compiler and lists the errors with file and line, including errors inside includes. Thanks to @theromis.
- `godot_game_screenshot` `aim` takes `size` (0.001 to 10000 metres) or `fov` (1 to 179 degrees) for the temporary camera copy, so one aimed capture can be a wide orthographic overview or a narrow perspective look while the game's own cameras keep their projection, size and fov. The aim result reports the lens it used (`projection`, `size` or `fov`, `aspect_axis`, `lens_from`, and `projection_replaced` when the override switched the copy's projection); an out-of-range value, or both at once, is refused. Thanks to @theromis.

### Changed

- The version the server reports, in the `initialize` answer and in feedback reports, now names the commit it was built from as build metadata (`0.13.0-dev+godot.4.8.g6295b51ab`). The version in `Cargo.toml` and `plugin.cfg` still changes only in a release commit, so two builds between releases used to report the same number. A build without git (a source archive) reports the manifest version alone. The addon's version has no commit.

### Fixed

- `godot_run_script` no longer buries a script's errors under the leak reports Godot prints while it shuts down (`Leaked instance dependency`, `N RID allocations of type ... were leaked at exit`, `ObjectDB instances were leaked at exit`, `resources still in use at exit`, `Pages in use exist at exit in PagedAllocator`). A script that loaded a hundred scenes ended with about 150 identical lines, some on stdout, so a run that succeeded looked like a failure. A report printed many times is now shown once, where it first appeared, with `×N` after it (with its `at:` line); `exit_leaks` counts the reports by kind and `hint` says what causes them: nodes made with `instantiate()` or `new()` that are not in the tree, or were removed from it, must be freed before `quit()`, and `--verbose` lists them. Every other line stays as it was. Thanks to @theromis.
- `godot_open_scene` makes the scene the editor's active tab when it reloads a background tab from disk. The reload used to keep the previous tab active while answering `opened: true`, so the next `godot_set_node_properties`, `godot_add_node` or `godot_save_scene` without `scene_path` acted on another scene. The answer names the active scene in `active_scene`, and the call fails with a way out when the editor did not switch to it. Thanks to @theromis.
- `godot_run_script` now also repairs binary scenes and resources (`.scn`, `.res`, compressed or not): headless Godot re-saved them without their uid, so scenes referring to one by `uid://` logged "invalid UID". The server reads the uid from each binary header before the run and, for a file rewritten without it, has Godot write it back with `ResourceSaver.set_uid` (a new uid for a file that had none), listed in `resource_repairs`; a failure is in `binary_uid_error` and `resource_damage`. Thanks to @theromis.
- `godot_run_script` keeps its answer short when a script saves many scenes and resources. A run that rewrote 113 kit files answered with about 54 KB, which agents could not read: the summary named every repaired path and `resource_repairs` and `editor_refresh` listed each one with all its restored references. The summary now counts the repairs (uids written back and how many are new, ext_resource uids restored, null shader parameters removed) and names at most three files; `resource_repairs` lists the first 20 with `ext_resource_uids_restored` as a count and `resource_repairs_total` beside it, and each `editor_refresh` list stops at 20 with `<list>_total`. `exit_code`, `stdout`, `stderr` and `script_errors` are unchanged. Thanks to @theromis.
- `godot_set_project_setting` without an editor stores a setting with the type Godot gives it. A setting not yet in `project.godot` was written with the JSON type of the value, so `application/boot_splash/bg_color = "#102b38"` became the string `"#102b38"` instead of a `Color`, and Godot ignored it. The server now asks the Godot executable (headless, in a throw-away project) for the setting's type and converts the value: an HTML color or a color name for a Color, the `project.godot` text such as `"Vector2i(1152, 648)"` for other types, a whole number for an int. A value that does not convert is refused and nothing is written; when the engine cannot be run, a new setting is refused with a way out. A new entry now goes after the last entry of its section, keeping the blank line before the next section. Thanks to @theromis.
- `godot_set_project_setting` with the editor accepts the same values as without it. The editor path refused a value for any setting that is not a string, number or boolean, so `"#102b38"` for `application/boot_splash/bg_color` worked only when no editor was connected. It now converts an HTML color or a color name for a Color and the `project.godot` text such as `"Vector2(0, 2)"` for other types, never creates an Object, refuses a string that does not convert with the setting named and nothing written, and gives a feature override such as `display/window/size/viewport_height.mobile` the type of its base setting, as the file path does.
- `godot_resave_resources` without the editor lists every file the headless import reimported, with its kind. It used to compare the text of the `.import` sidecars before and after, which does not change when a model is reimported with the same options, so a re-exported `.glb` was left out while the new `.png` beside it was listed. It now also compares the `.md5` record and the cache Godot rewrites on each import, and `imported` gives each path with `kind` (the importer) and `type`. A requested path whose imported data still does not match the source's bytes is listed under `still_stale` with a reason, and the call is an error: Godot's scan trusts its cache when a source's modification time in whole seconds is the one it recorded, so a source rewritten within that second is never reimported, however often the import runs, and a script loaded the old mesh. `force: true` drops the `.md5` record of the given paths and moves their sidecar's time, so Godot tests and reimports them (`forced`). With the editor, `imported` and `still_stale` are given for the reimported paths too. Thanks to @theromis.
- `godot_run_project` no longer warns about an imported file that was rewritten with the same bytes. Godot skips that reimport (it compares the `source_md5` it recorded for the cache), so `godot_resave_resources` could not clear the warning and the next run repeated it. The check now compares that checksum before it calls a cache stale; a missing cache is still stale. Thanks to @theromis.
- `godot_run_script` checks the imports before the script starts. Headless Godot does not import, so a script that `load()`s a file regenerated outside Godot got the old imported data and built its result from it. With the editor open on the project the stale files are reimported first; otherwise the script still runs and the answer carries `import_warning` and `stale_imports` (the first 20 paths, their total and whether they were reimported), naming `godot_resave_resources`. Thanks to @theromis.

### Removed

- `godot_check_voxel_model` and the executable setting it needed. The tool ran an external exporter program that is not part of the technology this project is built on, and the project no longer integrates products it does not build on (see "Technology Scope" in CONTRIBUTING.md). Model files are imported by the engine: put the `.glb` into the project and call `godot_resave_resources`; `godot_get_import_params` and `godot_set_import_params` read and set their import options. A call to the removed tool now fails as an unknown tool.

## [0.12.0] - 2026-11-01

An agent can close the editors it opened, the bridge port follows the project path, and several edits that used to be reported as done although the scene would not keep them are now refused with the reason.

### Added

- `godot_close_editor` closes a project's editor and answers once its process has exited, so an agent can close the editors it opened. The connected editor is asked to quit without saving anything; unsaved scenes and scripts keep it open and are listed, and `discard: true` closes it anyway. An editor that has another project open is never closed. When no editor is connected, an editor this server launched for the project with `godot_launch_editor` is stopped by its PID, only with `discard: true`. Thanks to @theromis.

### Changed

- The editor bridge port is derived from the project path when no port is set, so editors of several projects, such as git worktrees of one repository, can run at once, each reached by a server started in its own project directory. The plugin uses the project setting `godot_mcp/bridge_port`, then the editor's `GODOT_DEBUG_PORT`, then FNV-1a 32 of the canonical project path mapped to 10000–29999; the server uses `--debug-port`, then `GODOT_DEBUG_PORT`, then the same formula for the project found from its working directory, and `9333` outside any project. A plugin with no configured port also listens on `9333` while that is free, so configs that set `GODOT_DEBUG_PORT=9333` keep reaching the first editor. Editors started by `godot_launch_editor` get the server's port. The busy-port and unreachable-editor errors name where the port came from. Thanks to @theromis.

### Fixed

- `godot_set_node_properties` and `godot_add_node` read every value back after setting it and refuse the whole call when Godot kept another value, instead of reporting a change the scene would not save. The case that hid a real bug: an `AudioStreamPlayer` `bus` that the editor's AudioServer does not know (a bus the game creates at run time) reads back as `Master`, so the scene saved `Master` while the tool said it was done. The message names the property and both values and, for a bus, the bus layout to add it to. The check is general: a setter that clamps a value (`Range.value` above `max_value`) or refuses it (`pitch_scale = 0`) is refused the same way, and nothing is changed. `godot_add_node` now returns the values it set in `applied`. Thanks to @theromis.

- `godot_read_script`, `godot_edit_script` and `godot_resave_resources` find an open script tab whose title carries its parent folder, which the script editor shows when two open scripts share a file name (`tests/continuous_shore.gd`). Such a script used to fail with "The script editor did not open …" or report an unknown editor state. Thanks to @theromis.

- `godot_set_node_properties`, `godot_add_node`, `godot_attach_script` and `godot_connect_signal` no longer report a change to a node inside an instanced scene whose children are not editable as done. Godot saves nothing about such a node (no property, child, script or outgoing connection), so the change was lost on the next save. They now refuse it, naming the instance and its scene; `make_editable: true` turns on Editable Children for every instance on the way, in the same undo action, and the answer lists them in `made_editable`. `godot_remove_node` refuses a node an instance brings, which Godot cannot delete even with editable children. `godot_get_scene_tree` marks an instance node with `instance` and `editable_children`. Thanks to @theromis.

- After `godot_run_script`, the connected editor follows every shader, shader include and script the script rewrote, not only the `.tscn`/`.tres` files the server repaired. A script that rewrote a `.gdshader` with `FileAccess` left the editor's loaded Shader on its old code, so saving a scene that uses it wrote the old uniform defaults into the scene and pinned them (`shader_parameter/foam_pulse = 0.35` after the default became 0.5). The server now sends every scene, resource, script and shader the script wrote to the editor (`editor_refresh`), and `godot_resave_resources` reloads a resource the editor has loaded in place, so a material sees the new uniforms and defaults, and gives a loaded script without a tab its new source (`reloaded_resources`). Thanks to @theromis.

- The stale-import check of `godot_run_project`, the import and resave checks of the headless tools, the reference search of `godot_delete_file` and the script watch of the code tools now skip what Godot's file system does not scan: hidden files and folders (a name starting with `.`), folders holding a `.gdignore`, and nested projects (a folder with its own `project.godot`). A project holding git worktrees in `.claude/worktrees/` used to have every copy's imports listed as stale and reimported before each run. Thanks to @theromis.

- `godot_validate_script` no longer reports a sound script as failing because the editor's language server has not registered a `class_name` yet. A script written to disk while the editor is open (not through the editor tools) is missing from the editor's class list until the editor rescans, so a script using that class failed with `Could not find type "StreamLane" in the current scope`. When an unknown type or identifier is a `class_name` another script of the project declares, the connected editor now registers that script and the check runs again; if the class is still unknown, the answer says the class list is stale, names the declaring script and says how to refresh it. Thanks to @theromis.

- A scene or resource saved by a `godot_run_script` script keeps its `uid://` references. Headless Godot's saver asks for uids through a callback only the editor's file system installs, so it writes none; the server now writes back what the editor would have written into every `.tscn`/`.tres` the script rewrote: the file's own uid (the one it had, or a new one for a new file), the uid of each `[ext_resource]` whose target has one, and no `shader_parameter/<name> = null` lines for uniforms the script never set. `resource_repairs` lists what was written back. When the editor has the same project open, it refreshes the repaired files (`editor_refresh`), so its file system and uid cache know the new uids. `resource_damage` and the warning remain for a file the server could not write. Thanks to @theromis.

## [0.11.0] - 2026-10-30

A level can be assembled from saved scenes, typed array and dictionary properties are set element by element, and the code tools stop answering from another project's editor. Several calls that used to fail quietly now say what happened.

### Added

- `godot_add_node` takes `scene`, the `res://` path of a saved scene, instead of `node_type`. The scene is instanced the way the editor's Instantiate Child Scene does, so it is saved as an instance that keeps the scene's uid, and the whole call is one undo step. A missing file, a resource that is not a scene, the edited scene itself and a scene that contains it are refused. `properties` sets initial values (position, transform, ...) with the conversions of `godot_set_node_properties`; a bad value adds nothing. Thanks to @theromis.

### Fixed

- `godot_set_node_properties` and `godot_add_node` convert a JSON array or object for a typed array or dictionary property (`Array[NodePath]`, `Dictionary[String, int]`) element by element. A typed array used to keep only elements of its own type, so `["../Bed"]` for an `Array[NodePath]` left the property empty while the answer said `updated: true`. An element that does not convert now refuses the whole call and names the element; a Godot literal is parsed and its elements are checked the same way. `godot_get_node_properties` reports the element type. Thanks to @theromis.

- `godot_game_set` assigns a saved resource to an Object property from its `res://` path (`{"mesh": "res://quad.tres"}`) as `godot_set_node_properties` does, instead of failing with "Property 'mesh' is <Object#null> after writing". A missing file, a path outside the project and a resource of another class are refused before anything is written, naming the path and the class expected; writing `null` to an Object property no longer counts as a write that did not take. Thanks to @theromis.

- `godot_validate_script` and the `godot_code_*` tools no longer report diagnostics from another project's language server. With several editors open, the one holding port 6005 answered for every project, so a script just created in your project was a missing preload and its `class_name` an unknown type. A language server that says it has another project open is now refused, and a private headless editor of the right project is used instead; that editor is started again when another script of the project changed since it started, so a new `class_name` is known. `godot_create_script` with a `project_path` naming the editor's project now tells the editor about a new script too. Thanks to @theromis.

- `godot_run_script` no longer reports success for a script Godot did not run. Godot 4.8 exits with code 0 when the script does not extend `SceneTree` or `MainLoop`, does not parse or cannot be opened; the answer is now an error with Godot's message in `load_error` and in `script_errors`, and the summary says the script did not run. A script whose `extends` (followed through base scripts and the project's `class_name`s) is not `SceneTree` or `MainLoop` is refused before Godot starts. Thanks to @theromis.

## [0.10.0] - 2026-10-29

The AI can read and act in a game through the project's own adapter, delete files and create scripts and node metadata through the editor, and step into a node's resources with `:`. Tools now refuse arguments they do not declare, and several calls that used to look like success say what happened.

### Added

- `godot_game_state` and `godot_game_command` read and act in a game's own terms. The project names one adapter script in the project setting `godot_mcp/adapter`; its `register(registry)` adds named states and commands. `godot_game_state` reads states by name (or all of them) and lists the commands; `godot_game_command` runs one with a dictionary of arguments. A state that fails is its own entry, a command that refuses its arguments, an unknown name, a script error inside the project's code and an answer over 262144 bytes are told apart, and `godot_get_run_status` shows each run's adapter, what it registered and why anything was refused. Both work in a managed run, in editor Play and in a held game. A command does what the project wrote and is not limited by the refusals of `godot_game_call`; a command must not `await`.
- `godot_delete_file` deletes up to 64 `res://` paths together with their `.uid`, `.import` and imported data. It searches the project for each file's path and `uid://` first and refuses a file that is still referenced unless `force` is set; `project.godot`, `.godot/`, the addon and directories are refused, nothing is deleted while any path is refused, and `dry_run` only reports. Thanks to @theromis.
- Every tool declares `readOnlyHint`, `destructiveHint`, `idempotentHint` and `openWorldHint` in `tools/list`, so a host can tell a scene-tree read from a deletion. Only `godot_submit_feedback` is open-world.
- `godot_create_script` takes `content`, the whole script written as given (up to 1 MiB). The script is checked after it is written: one that does not validate is an error that quotes the problem, and the file stays so it can be fixed with `godot_edit_script`. `content` cannot be combined with `extends_class` or `class_name`. Thanks to @theromis.
- `godot_set_node_properties` creates a missing `metadata/<name>` entry the way the inspector's Add Metadata does, in the same undo action. The name must be an identifier and the value not `null`; a whole number becomes an `int`. Thanks to @theromis.
- `godot_get_node_properties` takes `properties`, a list of names to read, and `godot_set_node_properties` steps through a node's own resources with `:` (`material_override:shader_parameter/glow_amount`), the way `godot_game_set` does. The change joins the same undo action. A resource saved outside the edited scene (a `.tres` file or another scene's sub-resource) is refused with the file named. Thanks to @theromis.
- `godot_game_set` accepts `Basis(Vector3, Vector3, Vector3)`, `Transform3D(Vector3 x 4)` and `Transform2D(Vector2 x 3)`, the forms GDScript constructs. Thanks to @theromis.

### Changed

- Tools refuse a key their schema does not declare, naming the accepted ones, instead of dropping it and reporting success; every schema in `tools/list` says `additionalProperties: false`. A key set to `null` still counts as absent. Thanks to @theromis.
- `godot_run_script` reports `resource_damage` when the script rewrote a `.tscn` or `.tres`: headless Godot has no editor file system, so its saver drops every `uid://` (the file's own and each `[ext_resource]`'s), and with no renderer it writes `shader_parameter/<name> = null` for every uniform the script never set. The answer names each file, the uids its references should carry and the null parameters, and the tool description warns about it. The cause is in the engine; the server only finds it. Thanks to @theromis.

### Fixed

- `godot_game_set` checks every value before it writes any: a refused value no longer leaves the node half changed (a text written to a `Basis` used to leave the identity basis behind a refusal), and a write the engine does not keep puts back the properties the call already changed. Thanks to @theromis.

## [0.9.0] - 2026-10-28

The AI can now create a project, play a repeatable input scenario in a held game, list and edit InputMap actions, and be told what an overwritten script tab, a saved scene and a loaded scene did to the editor. Several running-game calls stop failing quietly.

### Added

- `godot_create_project` creates a Godot project through MCP. Thanks to @theromis.
- `godot_game_scenario` plays one input sequence in a held managed game: Input Map actions and pointers, a known number of physics ticks per step, and snapshots, optional screenshots and the call's logs in one answer. `godot_game_input` takes an `action` step for an Input Map action. Thanks to @theromis.
- `godot_check_voxel_model` exported `.vox` models through an external exporter program, imported the `.glb` files into the project and checked the `.import` files (removed in a later release). Thanks to @theromis.
- `godot_get_input_map` and `godot_edit_input_action` (editor only) list the InputMap actions, add or remove key, mouse button and joypad events, set a deadzone, and create or delete an action. A change is one undo step and is saved through the editor; it is all or nothing. `godot_set_project_setting` refuses `input/` keys and points to these tools. Built-in `ui_` actions can be edited but not deleted. Thanks to @theromis.
- `godot_game_input` and `godot_game_scenario` take `coordinate_space`: `viewport` (the default, unchanged) or `window`. Every input answer names the space and the mapping between the two, and a `click_node` step returns the point it clicked.
- `godot_code_diagnostics` adds `warnings_hidden`, and `godot_validate_script` a line, for a script under `res://addons/` in a project that keeps Godot's default `debug/gdscript/warnings/exclude_addons=true`: the analyzer reports no warnings for such a script, so a clean answer did not mean the script was clean.
- `godot_open_scene` lists in `load_errors` what Godot reported while loading, for example a node renamed because its name clashed with a child of an instanced scene, and names the editor dialog that shows it. From Godot 4.7 on, `unsaved` says whether the load left the scene with unsaved changes. Thanks to @theromis.
- `godot_read_script` reports `stale_tab: true` when an open script tab without unsaved changes shows older text than the file. `godot_resave_resources` reloads such a tab (`reloaded_tabs`) and keeps one with unsaved changes (`unsaved_tabs`). Thanks to @theromis.
- `godot_save_scene` names the scripts the editor saved with the scene (`scripts_saved`) and returns `scene_path`. Thanks to @theromis.
- `godot_game_call` answers `left_tree: true`, with the path the node had before the call, when the method took the node out of the tree, for example by reloading the scene. Thanks to @theromis.

### Changed

- `godot_create_script` with `overwrite: true` refuses to replace a script whose editor tab has unsaved changes, and reloads a clean tab with the new text. Thanks to @theromis.
- `godot_game_wait` and the `until` condition of `godot_game_physics` accept `null` as a value, so a wait can see a property being cleared: `eq null` holds only while the value is null. Thanks to @theromis.
- `godot_game_set` reads a Godot literal string such as `Transform3D(...)` for any typed property except an Object, and refuses a value of the wrong type with a message that names the property's type. Thanks to @theromis.
- `godot_game_screenshot` waits up to 30 process frames for a new drawn frame and marks the image `STALE` when the window drew nothing new, for example a minimized or covered window.
- Tokio and uuid are updated to 1.54.1 and 1.29.0.

### Fixed

- `godot_save_scene` no longer reformats the open scripts: the editor's convert-indent and trim-on-save settings are held off for the length of the save. Thanks to @theromis.
- `godot_game_set` no longer logs a script error in the running game when the stored and the written values have different types. Thanks to @theromis.
- The addon's own scripts have no GDScript analyzer warnings. The addon development project now sets `debug/gdscript/warnings/exclude_addons=false`, and `addon_scripts_have_no_analyzer_warnings` in `tests/runtime_e2e_test.rs` asks the language server about every addon script.

### Tests

- `click_node_completes_a_click_while_the_tree_is_paused` clicks a button that works while the game has paused its tree, in a headless and a stretched windowed game and in a held one, and in both coordinate spaces.

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

[Unreleased]: https://github.com/zeegzoog/godot-mcp/compare/v0.13.0...master
[0.4.0]: https://github.com/zeegzoog/godot-mcp/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/zeegzoog/godot-mcp/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/zeegzoog/godot-mcp/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/zeegzoog/godot-mcp/releases/tag/v0.1.0
