# Project Export Tools (Realization)

**Version:** 1.1.0
**Status:** Stable
**Layer:** implementation
**Implements:** l1-project-export.md

## Overview

Where each invariant of the project export concept is kept: the module of the Rust crate and the test that fails when the guard is removed. The mechanisms are described once, in §5.12 and §5.15 of the tool handlers.

## Related Specifications

- [l1-project-export.md](l1-project-export.md) - Parent concept: EX-1 to EX-8.
- [l2-tool-handlers.md](l2-tool-handlers.md) - §5.12 describes the presets, the export and their limits.
- [l1-tool-catalog.md](l1-tool-catalog.md) - TL-4 and TL-5, which EX-4 and EX-7 rely on.
- [l2-test-suite.md](l2-test-suite.md) - The opt-in end-to-end file that runs the tools against a real engine.

## 1. Motivation

The export tools have no editor-side half: everything runs in `src/tools/export.rs`, `src/tools/pack_run.rs` and the engine runner. The invariants sit in two sections of a specification written for the whole tool layer, so this table names where each one is kept and which test proves it, without repeating the section.

## 2. Constraints & Assumptions

- This specification adds no behaviour and repeats none: a row names a place and a test.
- A test name is a function in `tests/tools_test.rs` (the engine replaced by a stand-in executable) unless the row says otherwise.

## 4. Invariant Compliance

| L1 Invariant | Where it is kept | Test |
| --- | --- | --- |
| EX-1 Presets through Godot | `CliRunner::export_presets`: a fixed script reads and writes `export_presets.cfg` with `ConfigFile` in a throw-away project; only the given fields and options change and an option keeps its type; `export_filter` and `export_files` are checked on the host first | `a_preset_is_created_changed_and_used_for_a_pack_export` (`tests/export_e2e_test.rs`); `an_export_filter_and_export_files_are_checked_before_godot_runs`; `a_resources_filter_exports_only_the_listed_files_and_their_dependencies` (`tests/export_e2e_test.rs`) |
| EX-2 No lost preset edit | `src/tools/export.rs` refuses a preset change while an editor has the project open, with the fix | `a_preset_is_not_changed_while_the_editor_has_the_project_open` |
| EX-3 Ignored options are visible | The change answer lists `new_options`; a listing shows every stored option | `a_preset_is_created_changed_and_used_for_a_pack_export` (`tests/export_e2e_test.rs`) |
| EX-4 Confined output | `output_file` in `src/tools/export.rs`: inside the project, outside `.godot/`, a file, never the extension of a project file (`SOURCE_EXTENSIONS`) | `export_outputs_and_preset_names_are_checked_before_godot_runs` |
| EX-5 Imported before exported | The export runs `import_project_within` first; it is skipped with `import: false` or while an editor has the project open | `an_export_imports_first_and_reports_the_written_pack` |
| EX-6 Honest result | The answer carries `ok`, the path, the size, the exit code, the duration and the log; a missing preset, a missing template, a failed import, no file written and a timeout are errors | `an_unknown_preset_is_refused_before_anything_is_imported`; `a_missing_export_template_is_named_with_a_way_out`; `an_export_that_writes_no_file_is_an_error` |
| EX-7 Bounded, no injection | `timeout_seconds` (1 to 3600) for the import and for the export, each; a run over the limit is killed with its process tree; no caller string reaches the command line as a bare argument | `export_outputs_and_preset_names_are_checked_before_godot_runs`; `creating_a_preset_needs_a_platform` |
| EX-8 Pack check | `src/tools/pack_run.rs` (§5.15 of the tool handlers) | `a_pack_run_names_a_file_the_game_loads_by_a_built_path` (`tests/export_e2e_test.rs`) and the stand-in tests of the command line, the missing files and the refused arguments |

## 7. Drawbacks & Alternatives

- **Keeping the rows in the tool handlers**: they were there, which left the concept without an implementation specification of its own and a change to it without a downstream. Rejected; the section stays where it is and this table points to it.

## Canonical References

| Alias | Path | Purpose |
| --- | --- | --- |
| `[TOOLS]` | `src/tools/export.rs` | The three tools: schemas, preset requests, output checks, answers |
| `[PACK-RUN]` | `src/tools/pack_run.rs` | The pack smoke run |
| `[ENGINE]` | `src/cli_runner.rs` | The headless import and export runs and the preset script |
| `[HOST-TESTS]` | `tests/tools_test.rs` | Host-side tests of the rows above |
| `[E2E]` | `tests/export_e2e_test.rs` | A preset created, changed and used for a pack export on a real engine |

## Document History

| Version | Date | Author | Description |
| --- | --- | --- | --- |
| 1.0.0 | 2026-11-05 | Agent (magic.spec) | Created: `l1-project-export.md` had no implementation specification of its own. The table names, for each of EX-1 to EX-7, the module and the test that keep it; the mechanisms stay in §5.12 of the tool handlers. |
| 1.1.0 | 2026-11-27 | Agent (magic.spec) | [ADDED] The row of EX-8. [MODIFIED] The row of EX-1. Source: pull requests 150 and 151. Stable → RFC → Stable after Post-Update Review. |
