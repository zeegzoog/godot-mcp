# Content Authoring

**Version:** 1.4.0
**Status:** Stable
**Layer:** concept

## Overview

Tools that create and change project content beyond node edits in an open scene: new scene files, attaching a script to a node, connecting and listing signals, resources, and project settings. They also read an existing script and change it by replacing a fragment of its text, without ever overwriting what a person has typed in the editor and not yet saved. Two more tools create a minimal project in a directory that has none, and check a voxel model by exporting it through an external program and importing the result into a project.

## Related Specifications

- [l1-tool-catalog.md](l1-tool-catalog.md) - TL-4 (confined, non-destructive writes), TL-5 (no injection) and TL-9 (editor sync) apply to every tool here.
- [l1-editor-bridge.md](l1-editor-bridge.md) - BR-5: scene changes made through the editor must stay undoable.
- [l2-tool-handlers.md](l2-tool-handlers.md) - Where the host side of these tools is realized.

## 1. Motivation

Without these tools an agent can add nodes to an open scene and write a script from a fixed template, but cannot create a scene, attach the script, wire a signal, create a resource or change a setting. Agents work around the gap by writing files directly, which bypasses the editor (no undo, no UID tracking, conflicts with resources the editor holds in memory) and, for `project.godot`, can leave a file that no longer loads.

## 2. Constraints & Assumptions

- The core set is `godot_create_scene`, `godot_attach_script`, `godot_connect_signal`, `godot_list_signals`, `godot_create_resource`, `godot_get_project_settings` and `godot_set_project_setting`; script reading and editing, project creation and the voxel model check follow below.
- With the editor connected, every tool goes through it: changes to a scene that is open are undoable editor actions, new files are registered with the editor file system so they get a UID at once.
- Without an editor, creating a scene, creating a resource, and reading or setting a project setting fall back to the file itself. Attaching a script, connecting a signal and listing signals need the editor, because they act on an open scene.
- A project setting value is a string, a number or a boolean. Structured values (vectors, arrays, resources) are changed in the editor's Project Settings.
- Script reading and editing are two further tools, `godot_read_script` and `godot_edit_script`. `godot_create_script` stays the only way to create a script, from the caller's `content` or from a template; the edit tool never creates a file. [MODIFIED]
- `godot_create_project` is host-side, because there is no project yet for an editor to open. It writes `project.godot` and nothing else: the editor bridge, a main scene and any other file are the caller's to add. [ADDED]
- `godot_check_voxel_model` exports `.vox` models with Froxel, an external program named by `FROXEL_BIN` or found as `froxel` on `PATH`, writes the `.glb` files into the project and has the project import them. Froxel is trusted the way the Godot executable is; it is not a dependency of the server and is only started by this tool. [ADDED]
- [DR] A preview picture is named `<model>.preview.png`. Rejected: `<model>.png`, which replaces a texture that has the model's name. [ADDED]
- [DR] An edit is a replacement of one fragment, given as the old text and the new text. Rejected: sending the whole file (large and stale if the content changed since it was read), and line-range patches (line numbers shift as soon as anything else changes the file).
- [DR] A script open in the editor with unsaved changes is never edited: the edit is refused with the file and the fix. Rejected: applying the edit to the editor's unsaved text (ties the tool to the script editor's internals), and writing the file anyway (loses the person's typing).
- **Assumption (forecast):** with the editor connected, an edit goes through the editor as one undoable action and the file is saved, so what the editor shows, what is on disk and what the syntax check and a running game see agree. Runner-up: leave the changed script unsaved in the editor for the person to save. Override: `/magic.spec amend l1-content-authoring "leave edited scripts unsaved"`.

## 3. Core Invariants

- **CA-1** No authoring tool replaces an existing file. Creating a scene or a resource that already exists is refused, also when the file appears between the check and the write.
- **CA-2** Attaching or detaching a script and connecting a signal in an open scene are each a single undoable editor action.
- **CA-3** Caller text is only ever data to the tools. They do not execute it and do not interpolate it into generated code; a project setting value is written as one escaped value on one line, and a setting key is a plain path of letters, digits and `_ / . -` that starts with a section, with no empty, `.` or `..` segment, so no call can add an entry or a section to `project.godot`. What a script does once it is in the project is not the tools' to control: Godot runs a `@tool` script inside the editor as soon as it loads, so writing or editing one is running code in the editor with the person's authority. The tool descriptions and the user documentation say so. [MODIFIED]
- **CA-4** A project setting that exists keeps its type: a value of another type is refused, in the editor and in the file fallback.
- **CA-5** A link is checked before it is made: the script exists in the project, the signal exists on the node, the target has the method, and the connection is not already there. Each failure names the fix.
- **CA-6** Paths stay inside the project: scenes end in `.tscn`, resources in `.tres` (or `.res` through the editor), scripts in `.gd`, and `res://` paths cannot leave the project. Folders that a new scene or resource needs are created with it. [MODIFIED]
- **CA-7** Reading a script returns the text the editor would save: the editor's unsaved text when the script is open with unsaved changes, otherwise the file. The answer says which one it is and carries a version label of the returned text, so a caller can tell that what it read is not what is on disk. A script that starts with a UTF-8 byte order mark is read and edited like any other, with the editor and without it. [MODIFIED]
- **CA-8** An edit changes a script only when the old text occurs exactly once. No match or more than one match is refused and the answer says how many were found. A version label supplied with the edit must match the current text, otherwise the edit is refused and the caller reads again. A refused edit leaves the script untouched.
- **CA-9** An edit never overwrites unsaved typing: a script open in the editor with unsaved changes is refused with its path and the fix (save it in the editor or discard the changes, then read again). The old and new text are data under CA-3: never executed and never interpolated into generated code.
- **CA-10** With the editor connected an edit is a single undoable editor action (CA-2) and the editor and the file agree afterwards; without it the file is replaced as a whole so no half-written script is ever seen.
- **CA-11** With and without the editor the same input is accepted or refused. What the file fallback accepts equals what the editor accepts: a scene root is a `Node` class the engine can create and a resource a built-in `Resource` class, and anything else is refused before a file is written; a setting key is held to CA-3 in both paths; and a settings answer has the same shape from the editor and from the file. [ADDED]
- **CA-12** Creating a project never replaces `project.godot`: the file is created exclusively, an existing project is refused with the reason, and files already in the directory (documentation, assets) are kept. The directory is an absolute path without parent-directory segments and the name is not blank and has no NUL. The name is written as one escaped value, so it cannot start a new entry or section, and the renderer is the Compatibility one. Nothing is validated after the directory is created that could have been validated before it. [ADDED]
- **CA-13** A voxel model check passes only when every requested model was exported and imported. The exporter's reply must list each requested model with a boolean result, a model it reports as exported must have left its `.glb`, and the project must hold an `.import` sidecar for it whose cache is not older than a changed `.glb`; otherwise the check fails and names the model and the step. Every output is validated before anything is written: inside the project, outside `.godot`, unique within the batch including case-only and linked aliases, and a preview has a name of its own. A failed preview or a failed import also fails the check. The `.vox` is read only and may lie anywhere on the machine; where the system has network and device paths, they are refused. The exporter runs without the bridge token, its output is bounded, and the whole exchange, including its exit, has a deadline; a failed export is not imported. [ADDED]

## 7. Drawbacks & Alternatives

- **Free-form script editing** stays limited to one fragment replacement under CA-8 and CA-9; whole-file rewrites and line-range patches are rejected in §2.
- **An edit applied over the editor's unsaved text** would let an agent and a person change one script at once and would depend on how the script editor keeps its text; the refusal in CA-9 costs the person one save.
- **Refusing a script that is or becomes `@tool`**, or asking the caller to opt in, would stop silent code execution in the editor, but a script written without `@tool` that an autoload or a scene loads reaches the same effect, so the refusal would add friction to editor-tool development and no safety. Decided against; CA-3 states the consequence instead and the documents say it. Override: `/magic.spec amend l1-content-authoring.md "refuse @tool edits"`.
- **A structured-value setting editor** would need a value syntax a caller can get wrong in ways that break project loading; scalars cover the common cases safely.

## Canonical References

| Alias | Path | Purpose |
| --- | --- | --- |
| `[HOST]` | `src/tools/authoring.rs` | Catalog entries and the file fallbacks |
| `[EDITOR]` | `addons/godot_mcp/bridge_server.gd` | Editor-side handlers |
| `[E2E]` | `tests/editor_e2e_test.rs` | `authoring_tools_create_what_they_should_and_refuse_the_rest` |
| `[TOOLS]` | `tests/tools_test.rs` | Fallback tests: no overwrite, escaping, key and type checks |
| `[PROJECT]` | `src/tools/authoring.rs` | `godot_create_project` |
| `[MODEL]` | `src/tools/model_chain.rs` | `godot_check_voxel_model`: validation, the exporter exchange, the import and the report |
| `[MODEL-E2E]` | `tests/model_chain_e2e_test.rs` | The chain with a real exporter and a real engine; needs `FROXEL_BIN` and `GODOT_PATH` |

## Document History

| Version | Date | Author | Description |
| --- | --- | --- | --- |
| 0.1.0 | 2026-10-16 | Agent (magic.analyze first-time) | Draft collected from the README roadmap. |
| 1.0.0 | 2026-10-21 | Agent (magic.spec) | Seven tools implemented (pull request 7, reviewed and hardened). Constraints resolved: scene creation works with or without the editor. Invariants rewritten around what the tools guarantee; script reading and updating stays an open question. Draft → Stable. |
| 1.1.0 | 2026-10-21 | Agent (magic.spec) | [ADDED] Reading an existing script and editing it by one fragment replacement (CA-7 to CA-10), resolving the open question: replacement of a fragment, not whole-file or line-range edits; an edit is refused while the editor holds unsaved changes to the script; a separate read tool that reports the editor's unsaved text and a version label. Tools planned, not yet implemented. Stable → RFC → Stable after Post-Update Review. |
| 1.2.0 | 2026-10-21 | Agent (magic.spec) | [MODIFIED] CA-3 (the tools never run caller text, but a `@tool` script runs in the editor once written; settings keys reject `.`/`..`), CA-6 (new folders are created), CA-7 (byte order mark), and the stale note that the script tools were not implemented; [ADDED] CA-11 (same acceptance with and without the editor) and an open question on `@tool` edits. A refusal to edit the addon's own files was considered and dropped: any new `@tool` script reaches the same effect, so the refusal would only cost contributors who edit this repository with the tools. The line-ending difference between the editor path and the file path stays as documented in the addon specification. Source: simulation QA of 2026-10-21 (Q3, Q7, Q8, Q10, Q14, Q15). Stable → RFC → Stable after Post-Update Review. |
| 1.2.1 | 2026-10-21 | Agent (magic.spec) | Closed the open question on `@tool` edits: no refusal, the consequence is stated in CA-3 and in the user documentation. No requirement changed. |
| 1.3.0 | 2026-10-26 | Agent (magic.spec) | [ADDED] `godot_create_project` (CA-12) and `godot_check_voxel_model` (CA-13) with their constraints and the preview-name decision, as merged. Stable → RFC → Stable after Post-Update Review. |
| 1.4.0 | 2026-10-29 | Agent (magic.spec) | [MODIFIED] `godot_create_script` creates a script from the caller's `content` as well as from the template (see the tool catalog). Stable → RFC → Stable after Post-Update Review. |
