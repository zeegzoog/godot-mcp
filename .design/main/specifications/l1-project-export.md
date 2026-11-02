# Project Export

**Version:** 1.0.0
**Status:** Stable
**Layer:** concept

## Overview

Tools that list and change a Godot project's export presets and export the project headless with one of them: a data pack that needs no export template, or a release or debug build that does. They store presets the way the editor's Export dialog does and never write an export over a project file.

## Related Specifications

- [l1-tool-catalog.md](l1-tool-catalog.md) - The rules every tool follows; TL-4 points here for the one file an export may replace.
- [l1-content-authoring.md](l1-content-authoring.md) - The import parameters an export packs (CA-15).
- [l2-tool-handlers.md](l2-tool-handlers.md) - Where the export tools are realized (§5.12).
- [l1-quality-assurance.md](l1-quality-assurance.md) - The opt-in tests that run the tools against a real engine.

## 1. Motivation

Before these tools an agent could build and run a game but not ship it: presets were edited by hand in `export_presets.cfg`, where a misspelled option is silently ignored by Godot, and an export needed the editor's Export dialog. An agent preparing a build for a device (a Linux pack for a 32-bit ARM board whose GPU has ETC2 but no S3TC) needs to set the preset's options, export, and learn from the answer whether the export worked.

## 2. Constraints & Assumptions

- The tools are `godot_list_export_presets`, `godot_set_export_preset` and `godot_export_project`. They run the Godot executable named by `GODOT_PATH`, which must be an editor build: export templates and `--export-*` exist only there.
- `export_presets.cfg` is Godot's file. It is read and written by Godot's own configuration reader and writer, never by text editing, so a preset made here equals one made in Project > Export.
- A running editor keeps the presets in memory and writes them back when its Export dialog changes one; a preset changed on disk while the editor has the project open would be lost at that moment.
- A pack (`.pck` or `.zip`) holds the project data only and needs no export template; it runs with an engine binary of the same version. A release or debug build needs the export templates of that exact Godot version, or the preset's own template options.
- [DR] The output stays inside the project. Rejected: any path the caller names (the server would write wherever it can), and an extra allow-list of export folders (one more setting for a case the caller solves by copying the file afterwards). Override: `/magic.spec amend l1-project-export.md "export folders"`.
- [DR] An existing output file is replaced without an overwrite flag. An export is a build product that is rebuilt on purpose, and an agent re-exporting after a fix would otherwise have to delete it first; EX-4 keeps every project file out of reach. Rejected: TL-4's overwrite flag (a flag nobody would leave off). Override: `/magic.spec amend l1-project-export.md "overwrite flag"`.

## 3. Core Invariants

- **EX-1 Presets through Godot.** Presets are read and written by Godot's configuration reader and writer, in the layout the Export dialog uses. Only the fields and options the caller gives change; an option keeps the type it already has; a new preset exports all resources and leaves every option it does not store to the platform's default.
- **EX-2 No lost preset edit.** A preset is not changed while an editor has the project open: the call is refused with the reason and the fix (close the editor first).
- **EX-3 Ignored options are visible.** Godot ignores an option its platform does not have, so a change reports the option names that were not stored before; a listing shows every stored option and says that an absent one takes the platform's default.
- **EX-4 Confined output.** The output file resolves inside the project, outside Godot's own cache directory, is a file and not a folder, and never has the extension of a project file (scripts, scenes, resources, shaders, project and import configuration, uid files); a pack ends in `.pck` or `.zip`. Its folder is created. An existing build file there is replaced, as the decision in §2 says.
- **EX-5 Imported before exported.** The project is imported headless before the export, so the pack holds current imports; the caller can skip it, and it is skipped while an editor has the project open, since that editor keeps the imports current.
- **EX-6 Honest result.** The answer says whether the export worked, with the output path, its size, the exit code, the duration, Godot's error and warning lines and the end of its log. A missing preset, a missing export template, a failed import, an export that wrote no file, or a run that hit its time limit is an error that carries that log.
- **EX-7 Bounded and injection-free.** The import and the export each have a time limit the caller can set, and a run that exceeds it is killed with its process tree. No caller string reaches the engine command line as an option or the source of a generated script: a preset request reaches the engine only as data, and an output path only as an absolute path.

## 7. Drawbacks & Alternatives

- **Editing `export_presets.cfg` as text**: no engine needed, but the file's sections and option types are Godot's, and a hand-written value Godot cannot read is dropped without a word; EX-1 lets Godot write it.
- **Exporting through the running editor**: no extra process, but the editor has no export command it could run for a caller, and a long export would block every other bridge command (the editor runs commands on its main thread).
- **An export without the import**: faster, but a pack from a project whose imports are stale contains the old data; EX-5 makes it the caller's explicit choice.

## Canonical References

| Alias | Path | Purpose |
| --- | --- | --- |
| `[TOOLS]` | `src/tools/export.rs` | The three tools: schemas, preset requests, output checks, answers |
| `[ENGINE]` | `src/cli_runner.rs` | The headless import and export runs and the preset script |
| `[E2E]` | `tests/export_e2e_test.rs` | A preset created, changed and used for a pack export on a real engine; an unusable template |
| `[DOCS]` | `README.md` | The Exporting section |

## Document History

| Version | Date | Author | Description |
| --- | --- | --- | --- |
| 1.0.0 | 2026-11-02 | Agent (magic.spec) | Reality sync after ventilation: written from the export tools as merged in pull request 89, which no specification covered. The two decisions record what the code does (confined output, an existing build file replaced). Promoted to Stable after Post-Update Review. |
