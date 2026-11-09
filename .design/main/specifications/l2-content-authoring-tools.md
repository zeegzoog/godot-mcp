# Content Authoring Tools (Realization)

**Version:** 1.1.0
**Status:** Stable
**Layer:** implementation
**Implements:** l1-content-authoring.md

## Overview

Where each invariant of the content authoring concept is kept: which file in the Rust crate and which command in the editor addon, and which test fails when the guard is removed. The mechanisms themselves are described once, in the sections of the two realization specifications this one points to.

## Related Specifications

- [l1-content-authoring.md](l1-content-authoring.md) - Parent concept: CA-1 to CA-18.
- [l2-tool-handlers.md](l2-tool-handlers.md) - The host side: script tools (§5.4), authoring fallbacks (§5.5), project creation (§5.6), shaders (§5.8), import parameters (§5.9), InputMap and deletion (§5.10).
- [l2-godot-editor-addon.md](l2-godot-editor-addon.md) - The editor side: handler notes (§5.2), script commands (§5.3), shader, import and file commands (§5.4).
- [l1-tool-catalog.md](l1-tool-catalog.md) - TL-4 (confined, non-destructive writes) and TL-5 (no injection), which several rows rely on.

## 1. Motivation

The authoring tools are realized by two specifications written for other concepts (the Rust tool layer and the editor addon), and the concept has seventeen invariants spread over both. A change to an authoring tool needs the list of what it must keep and where each guard lives; this table is that list, so no invariant depends on someone remembering which section holds it.

## 2. Constraints & Assumptions

- This specification adds no behaviour and repeats none: a row names a place and a test, never the rule's text.
- A test name is a function in `tests/tools_test.rs` (host side, no engine) or `tests/editor_e2e_test.rs` (a real editor, ignored by default) unless the row says otherwise.

## 4. Invariant Compliance

| L1 Invariant | Where it is kept | Test |
| --- | --- | --- |
| CA-1 No unrequested replacement | `create_new_file` (`create_new(true)`) in `src/tools/authoring.rs` for scenes, resources and `project.godot`; the editor's `create_scene` and `create_resource` refuse an existing file; shaders replace only with `overwrite` (§5.8 of the tool handlers) | `create_resource_without_an_editor_never_replaces_a_file`; `authoring_tools_create_what_they_should_and_refuse_the_rest` |
| CA-2 One undoable action | `attach_script` and `connect_signal` in `bridge_server.gd` record one `EditorUndoRedoManager` action each | `scene_and_property_tools_behave`; `undo_and_resource_refresh_behave` |
| CA-3 Caller text is data | `is_setting_key` and the escaped single-line value in `src/tools/authoring.rs`; fixed script templates; the fragments of an edit are matched and inserted as text (§5.4, §5.5) | `project_setting_without_an_editor_cannot_add_lines_to_project_godot`; `a_setting_key_with_a_dot_segment_is_refused_and_the_settings_answer_matches_the_editors` |
| CA-4 A setting keeps its type | `CliRunner::setting_type` and the conversion in `src/tools/authoring.rs` (§5.5); `_coerce_setting` in `bridge_server.gd` | `project_setting_without_an_editor_stores_the_engine_type`; `project_setting_without_an_editor_refuses_a_value_of_another_type`; `project_settings_convert_the_same_text_forms_as_the_file_fallback` |
| CA-5 A link is checked first | The checks of `connect_signal` in `bridge_server.gd`, before the connection is made | `authoring_tools_create_what_they_should_and_refuse_the_rest` |
| CA-6 Paths stay inside the project | `resolve_in_project` and the extension checks of each tool; `create_dir_all` for the folders of a new file (§5.5); `to_res_path` in the editor | `authoring_tools_create_what_they_should_and_refuse_the_rest` |
| CA-7 Reading returns the editor's text | §5.4 of the tool handlers; `read_script` in `editor/script_ops.gd` (§5.3 of the addon); the shared version label | `read_script_without_an_editor_reads_the_file_and_labels_it`; `read_script_answers_from_the_file_or_from_unsaved_editor_text`; `the_script_label_matches_its_published_vectors` |
| CA-8 One match, the right version | The shared matching rules of §5.4; `edit_script` in `editor/script_ops.gd` | `edit_script_refuses_what_it_should_and_leaves_the_file_alone`; `edit_script_accepts_the_current_label_and_treats_text_as_data` |
| CA-9 Unsaved typing is never overwritten | The pairing rule of `edit_script` (§5.3 of the addon); the answer's `editor_consulted` in the file fallback | `edit_script_replaces_one_fragment_and_never_overwrites_typing`; `edit_script_asks_the_editor_first_and_does_not_hide_its_refusals` |
| CA-10 One undo step, never half a file | One action in the editor; in the file fallback a temporary file created exclusively and renamed over the script (§5.4) | `edit_script_leaves_the_script_unchanged_when_it_cannot_be_replaced`; `edit_script_never_follows_a_planted_link` |
| CA-11 Same input, same answer | `CliRunner::class_kind` and the settings conversion in the fallbacks (§5.5), equal to the editor's checks | `a_setting_key_with_a_dot_segment_is_refused_and_the_settings_answer_matches_the_editors`; `project_settings_convert_the_same_text_forms_as_the_file_fallback` |
| CA-12 Project creation | `create_project` in `src/tools/authoring.rs` (§5.6) | `creates_project_without_engine_and_preserves_existing_work`; `project_creation_validates_before_creating_directories` |
| CA-14 Shaders | `src/tools/shaders.rs` (§5.8); `editor/shader_ops.gd` (§5.4 of the addon) | `shader_tools_refuse_what_they_should_with_a_fix`; `edit_shader_without_an_editor_replaces_one_fragment`; `shader_commands_keep_the_shader_editor_in_step` |
| CA-15 Import parameters | `src/tools/import_params.rs` (§5.9); `editor/import_ops.gd`, shared with the headless path | `import_params_go_through_the_editor_that_has_the_project_and_name_dropped_options`; `import_params_are_read_set_copied_and_reimported_headless` (`tests/export_e2e_test.rs`) |
| CA-16 InputMap actions | `src/tools/input_map.rs` (§5.10); `get_input_map` and `edit_input_action` in `bridge_server.gd` | `input_action_edits_are_checked_first_and_never_write_project_godot`; `input_actions_are_edited_saved_and_undone_through_the_editor` |
| CA-17 Deleting | `src/tools/files.rs`, `src/tools/reference_kind.rs` (§5.10); `editor/file_ops.gd` and its classifier | `files_are_deleted_on_disk_with_their_companions_unless_still_referenced`; `a_file_only_written_by_a_generator_script_is_deleted_without_force`; `files_are_deleted_through_the_editor_with_companions_and_tabs`; `the_addon_classifies_script_references_like_the_server` |
| CA-18 Created classes are known to runs | `src/godot/class_cache.rs` (`register`, `unregistered`), called from `godot_create_script` in `src/tools.rs`, from the edit fallback in `src/tools/scripts.rs` and from the syntax check in `src/tools/headless.rs` (§5.4) | `create_and_edit_without_an_editor_keep_the_class_cache_up_to_date`; `validate_script_fails_a_script_that_uses_an_unregistered_class`; `a_class_created_without_the_editor_is_known_to_the_next_run`; the unit tests of `class_cache.rs` |

## 7. Drawbacks & Alternatives

- **Moving the authoring sections out of the tool handlers into this specification**: it would put each rule beside its row, but the section numbers (§5.4 to §5.10) are cited by other specifications, and the tool handlers also hold the rules every tool shares. Rejected; the rows point into the sections instead.

## Canonical References

| Alias | Path | Purpose |
| --- | --- | --- |
| `[AUTHORING]` | `src/tools/authoring.rs` | Scene, resource, setting and project tools and their fallbacks |
| `[SCRIPTS]` | `src/tools/scripts.rs` | Script read and edit tools |
| `[SHADERS]` | `src/tools/shaders.rs` | Shader tools |
| `[IMPORT]` | `src/tools/import_params.rs` | Import parameter tools |
| `[INPUT]` | `src/tools/input_map.rs` | InputMap tools |
| `[FILES]` | `src/tools/files.rs` | File deletion |
| `[HOST-TESTS]` | `tests/tools_test.rs` | Host-side tests of the rows above |
| `[E2E]` | `tests/editor_e2e_test.rs` | Real-editor tests of the rows above |

## Document History

| Version | Date | Author | Description |
| --- | --- | --- | --- |
| 1.0.0 | 2026-11-05 | Agent (magic.spec) | Created: `l1-content-authoring.md` had no implementation specification of its own, so a change to it reached nothing downstream. The table names, for each of CA-1 to CA-17, the file and the test that keep it; the mechanisms stay in the tool handlers and the addon. |
| 1.1.0 | 2026-11-09 | Agent (magic.spec) | [ADDED] The row of CA-18. Source: pull requests 130 and 131. Stable → RFC → Stable after Post-Update Review. |
