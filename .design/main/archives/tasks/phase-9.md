---
phase: 9
name: "Simulation QA Remediation"
status: Done
subsystem: "src, addons/godot_mcp, tests, README"
requires: [6, 8]
provides: []
key_files:
  created: []
  modified: []
patterns_established: []
duration_minutes: ~
---

# Stage 9 Tasks — Simulation QA Remediation

**Phase:** 9
**Status:** Done
**Strategic Goal:** Bring the product in line with the eleven specifications amended from the simulation QA of 2026-10-21 (`scratch/qa/simulation-qa-2026-10-21.md`, git-ignored): edits and creations reach the project the caller named, the running-game guards keep what they promise, the file fallbacks accept what the editor accepts, and the answers, hints and defaults say what happened.

## Atomic Checklist

<!-- A line is `- [ ]` (open) or `- [x]` (done). A Blocked task keeps `- [ ]`: it is blocked by its `Status` in Detailed Tracking, and the `[!]` the workflows write after "Blocked" spells that status — it is not a checkbox state. -->

- [x] [T-9A01] Host: the named project wins over the editor (TL-10)
- [x] [T-9A02] Host: honest stop, status and output answers (TL-11)
- [x] [T-9A03] Host: schema ranges and fields, `res://` for breakpoints, `script_path` alias (TL-1, TL-5)
- [x] [T-9A04] Host: hints by failure class and `res://` spelling (TL-7)
- [x] [T-9A05] Host: the authoring fallbacks accept what the editor accepts (CA-11)
- [x] [T-9A06] Host: an unknown feedback mode gives `draft`; plain diagnostics off a terminal (FB-10)
- [x] [T-9A07] Host: a language server that serves another project is not used (CI-7)
- [x] [T-9A08] Host: a finished run keeps its exit code and stop time (RO-3)
- [x] [T-9B01] Editor: byte order mark, new folders, null properties, scene-only signals, settings keys, port clash toast
- [x] [T-9C01] Runtime: the refusal list of `godot_game_call` and typed arguments (RO-6)
- [x] [T-9C02] Runtime: pointer input in a game without a window (RO-11)
- [x] [T-9C03] Runtime: one scene-stamp note per answer; baseline comparison at the baseline's size
- [x] [T-9D01] User-facing text: README, tool descriptions, agent guide, changelog
- [x] [T-9T01] Validation: pipeline, end-to-end suites and invariant coverage
- [x] [T-9E01] Carry to the release lines (after validation)

## Detailed Tracking

<!-- Optional entry field, created by /magic.run on a task's first dead end — absent means none. A nested list under the entry's field list, one line per abandoned approach, at most five (run.md, Dead-End Record):
  - **Attempts:**
    - {what was tried} → {why it failed}
-->

### [T-9A01] Host: the named project wins over the editor (TL-10)

- **Spec:** l1-tool-catalog.md TL-10, TL-3; l2-tool-handlers.md §4 (TL-10 row)
- **Status:** Done
- **Assignment:** Agent
- **Verify:** New tests in `tests/tools_test.rs` with `MockBridge`, written first and red: the mock editor reports project directory A; a `project_path` of directory B makes `godot_read_script`, `godot_edit_script`, `godot_create_scene`, `godot_create_resource`, `godot_get_project_settings` and `godot_set_project_setting` use the file path in B and send no command to the editor; an equal `project_path` (also spelled with `\`, another case on Windows, or with a trailing separator) and an omitted one still go to the editor; an editor that cannot report its directory refuses the call with the reason; each file-changing answer carries `project_dir`. `cargo clippy --all-targets --all-features -- -D warnings`, `cargo fmt --all -- --check` clean; no `unwrap()` or `expect()` in the new code.
- **Handoff:** T-9A07 reuses the cached editor directory; T-9A05 and T-9A02 touch the same handlers and wait for this.
- **Notes:** The QA evidence is steps M1 to M5 (an edit of `proj2` landed in `proj`). Cache the directory per connection, read it again after a reconnect. Tools with no host path (editor-only tools take no `project_path`) are unaffected. No lock across the round trip (AGENTS.md).
- **Changes:** `ToolManager::editor_choice` and `EditorChoice` (src/tools.rs) decide per call whether the editor or the files are used; `paths::same_directory`; scripts.rs and authoring.rs use them, answers carry `project_dir`. Tests first in `tests/tools_test.rs` (`a_named_project_wins_over_a_connected_editor`, `the_editors_own_project_still_goes_to_the_editor`, `an_editor_that_cannot_name_its_project_is_not_sent_a_file_change`) plus the unit test of `same_directory`; red before the code, and a mutation (comparison forced to equal) makes the first one fail. The three existing mock-editor tests now answer `get_project_info`. `[DR]` the editor's directory is asked each call instead of cached (a restarted editor could otherwise be mistaken for the old one).

### [T-9A02] Host: honest stop, status and output answers (TL-11)

- **Spec:** l1-tool-catalog.md TL-11; l2-tool-handlers.md §4 (TL-11 row)
- **Status:** Done
- **Assignment:** Agent
- **Verify:** Tests in `tests/tools_test.rs` and `tests/stdio_protocol_test.rs`, red first: with the mock editor connected and not playing, two managed runs alive (fake game executable), `godot_stop_project` without `run_id` stops both and lists them; with nothing running it says `nothing was running` and is not an error; with no editor and no run, `godot_get_run_status` and `godot_run_output` answer that nothing runs instead of the bridge-unreachable error; a `run_id` still stops only that run. Clippy and fmt clean.
- **Handoff:** After T-9A01 (same file). README row for `godot_stop_project` is text for T-9D01.
- **Notes:** QA steps G48, W15, O7, O8, O10. Stopping editor Play and managed runs are independent: one failing does not skip the other, and the answer reports each.
- **Changes:** `GameHub::stop_all`; `handle_stop_project` stops managed runs, then asks the editor and stops Play only when it plays, and answers `{message, stopped}`; `handle_run_status` and `godot_run_output` answer that nothing runs instead of the bridge error. Four tests in `tests/tools_test.rs`, red first.

### [T-9A03] Host: schema ranges and fields, `res://` for breakpoints, `script_path` alias (TL-1, TL-5)

- **Spec:** l1-tool-catalog.md TL-1, TL-5; l2-tool-handlers.md §4 (TL-1, TL-5 rows), §5.2
- **Status:** Done
- **Assignment:** Agent
- **Verify:** Tests in `tests/tools_test.rs` and `tests/server_test.rs`, red first: `godot_code_complete` with `line: 0` or `character: -1` fails before any language server is contacted and names the range; `tools/list` shows the fields of `steps`, `until` and `roi`; `godot_debug_breakpoints` refuses `res://../x.gd:1` and a location that is not `res://`; `godot_create_script` accepts `script_path` as well as `file_path` and reports an error that names both when neither is given. Clippy and fmt clean.
- **Handoff:** None; independent of T-9A01 except for the file `src/tools.rs`, so run after it.
- **Notes:** QA steps L7, G46, H4 (the `action` step does not exist: do not add it; describing the existing step types is enough), C4 to C6. Keep `file_path` working (tool parameters are public API).
- **Changes:** `minimum`/`maximum` in `validation.rs` (unit test); `position`, `ir`, `step_schema`, `until_schema`, `roi_schema` in the schemas; `check_breakpoint` for `godot_debug_breakpoints` and for `breakpoints` of `godot_run_project`; `script_path` accepted by `godot_create_script` (nothing required alone). Tests in `tests/server_test.rs` and `tests/tools_test.rs`, red first. The `action` input step is not added (the description lists the real step types). One existing runtime test now expects the schema's message for `frames: 9`.

### [T-9A04] Host: hints by failure class and `res://` spelling (TL-7)

- **Spec:** l1-tool-catalog.md TL-7; l2-tool-handlers.md §4 (TL-7 row)
- **Status:** Done
- **Assignment:** Agent
- **Verify:** Tests in `tests/tools_test.rs` and `tests/editor_e2e_test.rs` (mock replies), red first: each of these gets its own hint and none gets "Correct the arguments according to the message above and retry": a runtime `DENIED`, `UNSUPPORTED`, `NOT_FOUND` and `BAD_PARAMS` reply; an editor answer about unsaved changes and about a file that differs from the editor's text; "Can't open"; a `Process` error for a missing `GODOT_PATH` (hint names `GODOT_PATH`, not syntax); the error of a setting of the wrong type (hint is not the key example). A host error that quotes a project file shows its `res://` path. Clippy and fmt clean.
- **Handoff:** None.
- **Notes:** QA Q12 and Q25 list the observed cases (S6, S10, B31, G36, A2, H11, X2, C14, C8, C11, A7). Map on the structured code of the runtime error (`{"code":...}`) rather than on message text where there is a code.
- **Changes:** `editor_error_hints` reads the `code` of a running-game failure and the editor's refusals (unsaved changes, differs from the file, cannot be matched, already exists, Can't open, expects a); `Process` errors and the fallback class get hints; `validate_script` separates a Godot that cannot run from a syntax error; `paths::res_display` for messages; hints added to the bare errors of the authoring fallbacks and the code tools. Four tests in `tests/tools_test.rs`, red first.

### [T-9A05] Host: the authoring fallbacks accept what the editor accepts (CA-11)

- **Spec:** l1-content-authoring.md CA-3, CA-11; l2-tool-handlers.md §5.5
- **Status:** Done
- **Assignment:** Agent
- **Verify:** Tests in `tests/tools_test.rs`, red first: with no editor `godot_create_scene` refuses `Resource` as a root class and `godot_create_resource` refuses `Node`, and writes no file; a valid class is still created; a setting key with an empty, `.` or `..` segment (`simqa/../evil`) is refused in the fallback as in the editor; the fallback `godot_get_project_settings` returns `{settings, truncated}` with unquoted string values and `source` apart, equal in shape to the editor's. Clippy and fmt clean.
- **Handoff:** Waits for T-9A01 (same handlers).
- **Notes:** The class check is decided in `l2-tool-handlers.md` §5.5: one bounded engine call with a fixed script in the temporary directory and the class after `--`. Start with the probe the section names (does a script outside the project run under `--script`; what does `ClassDB` answer for `Resource` as a scene root and `Node` as a resource) and write the result in Changes; if the probe fails, stop and amend the section through `/magic.spec`. QA steps C9, C12, C13, C17, B31 to B33.
- **Changes:** `CliRunner::class_kind` (throw-away project in the temporary directory, fixed script, class after `--`); the probe on 4.8-beta1 gave 0.4 s and ran a script outside the project. Authoring fallbacks refuse non-Node scene roots and non-Resource classes, write the root name as data (`node_name`), reject `.`/`..` setting-key segments, and answer settings in the editor's shape with `project_dir`. `tests/common` gained `classifying_godot` and `exiting_godot`. Three tests, red first; offline tests that create scenes now use the classifying stand-in. `[DR]` an engine that cannot run refuses the file (nothing is written) with a `GODOT_PATH` hint.

### [T-9A06] Host: an unknown feedback mode gives `draft`; plain diagnostics off a terminal (FB-10)

- **Spec:** l1-agent-feedback.md FB-10; l2-agent-feedback-rust.md §2 (configuration); l2-rust-stdio-server.md (MCP-7 row, diagnostics)
- **Status:** Done
- **Assignment:** Agent
- **Verify:** In `src/config.rs` unit tests, red first: `bogus`, `drafts` and `of` give `draft`; `full`, `Draft` and `OFF` in any case keep their meaning; an empty value gives `full`. In `tests/stdio_protocol_test.rs`: with `GODOT_MCP_FEEDBACK=bogus` the catalog has the note and draft tools and no submit tool. A piped standard error contains no `ESC` byte (a test that runs the binary with a pipe). Clippy and fmt clean.
- **Handoff:** README configuration row is text for T-9D01.
- **Notes:** QA steps MODE bogus and `phase_a.stderr.txt`. `.env.example` gains one sentence after the code does: an unrecognized value means draft (it already says an empty one means full).
- **Changes:** `parse_feedback_mode` returns `Draft` for an unknown value; `main.rs` writes colour codes only when stderr is a terminal. Unit test in `config.rs`, two in `tests/stdio_protocol_test.rs` (the ESC-byte assertion was red first). `.env.example` and the README row say it.

### [T-9A07] Host: a language server that serves another project is not used (CI-7)

- **Spec:** l1-code-intelligence.md CI-7; l2-lsp-client.md §4 (CI-3, CI-7 rows)
- **Status:** Done
- **Assignment:** Agent
- **Verify:** Tests in `tests/bridge_client_test.rs` or `tests/tools_test.rs` with a mock bridge and a fake language server on a port: the bridge reports directory B for a call about project A, so the fake server on the port receives no connection (the client takes the private-editor path, replaced in the test by a fake executable); with matching directories, or with no bridge answer, the server on the port is used as before. The timeout error names the project. Clippy and fmt clean. An ignored `runtime_e2e_test` case is not needed (the real editor path is covered by `language_server_*`).
- **Handoff:** After T-9A01 (it provides the editor's project directory).
- **Notes:** QA steps L10, L11. Without a bridge answer nothing changes by design (the `[DR]` in the concept).
- **Changes:** `LspHub` methods take `external_ok`; `connect_or_start` skips the configured port when it is false; a live client is reused only when the hub started it; `ToolManager::editor_may_serve`. Unit test of the hub; checked end to end by hand with a named project the editor did not have (4.8 s, a private editor was started). No automated test covers the tool-level decision.

### [T-9A08] Host: a finished run keeps its exit code and stop time (RO-3)

- **Spec:** l1-runtime-observability.md RO-3; l2-game-runtime.md §4 (RO-3 row)
- **Status:** Done
- **Assignment:** Agent
- **Verify:** A test in `tests/tools_test.rs` or `tests/detach_test.rs` with a fake game that exits with code 3: `godot_get_run_status` shows `exit_code: 3`, a stop time, and `ended_by: "game"`; a run stopped with `godot_stop_project` shows `ended_by: "server"`; a run that is still alive shows neither. Clippy and fmt clean.
- **Handoff:** None.
- **Notes:** QA steps D18, W17, W20 (`exit_code: null`, `stopped: null` after a stop). The field names are not fixed by the specification; pick them and record the choice in Changes, then put them in the README tool text (T-9D01).
- **Changes:** `Run::record_end` (`ended_by`, `ended_at`, `exit_code`, first writer wins), reported by `summary()`. Test `a_finished_run_keeps_its_exit_code_and_says_who_ended_it`, red first. Names chosen: `ended_by`, `ended_at`.

### [T-9B01] Editor: byte order mark, new folders, null properties, scene-only signals, settings keys, port clash toast

- **Spec:** l2-godot-editor-addon.md §5.2, §5.3; l1-content-authoring.md CA-6, CA-7, CA-11
- **Status:** Done
- **Assignment:** Agent
- **Verify:** New cases in `tests/editor_e2e_test.rs` (extend the existing authoring and script tests), written first, red, then green with the build in `.github/godot-version`: `read_script` and `edit_script` accept a file that starts with a byte order mark; `create_scene` and `create_resource` create `res://newdir/deep/` for a new file and name the folder when they cannot; `get_node_properties` returns `null` for an unset `material`; `list_signals` on a Button lists only connections to nodes of the scene, with the target's path and method; `set_project_setting` refuses `simqa/../evil`; a second editor on a taken port shows a notification (assert the Output line and that the notification call is reached). `godot --headless --check-only` passes for every addon script; any call missing from Godot 4.5.0 goes through `has_method()` + `call()`.
- **Handoff:** None; edits `bridge_server.gd` and `editor/script_ops.gd` only.
- **Notes:** A saved script that lost its byte order mark says so in the answer (`bom_removed`, `l2-godot-editor-addon.md` §5.3); the mark is not restored. Add a case for it. QA steps S6, S7, B6, B18, B31, B33, A2, collision run. The line-ending difference between the editor path and the file path stays as documented (not a task).
- **Changes:** `script_ops.gd` strips and reports a byte order mark (`bom_removed`); `bridge_server.gd`: `_ensure_folder` for new scenes and resources, `null` for unset object properties, `list_signals` with scene-only `{target, method}` connections, `.`/`..` setting-key segments, a clearer port-clash message and an error toast. Two ignored tests in `tests/editor_e2e_test.rs`, red first, green on 4.8-beta1. Measured: `ResourceSaver` writes no mark. Not verified: that the toast is shown in a window.

### [T-9C01] Runtime: the refusal list of `godot_game_call` and typed arguments (RO-6)

- **Spec:** l1-runtime-observability.md RO-6; l2-game-runtime.md §4 (RO-6 row)
- **Status:** Done
- **Assignment:** Agent
- **Verify:** New cases in `tests/runtime_e2e_test.rs`, red first, on a real game: for each name in the refusal list of the specification (`quit`, `set_script`, `call`, `callv`, `call_deferred`, `call_group`, `call_group_flags`, `propagate_call`, `rpc`, `rpc_id`, `set`, `set_deferred`, `set_indexed`, `emit_signal`, `notification`, `propagate_notification`, `free`, `queue_free`, `replace_by`) the call is refused with the permit list naming it, and the node still exists; a test fails if the Rust list and the runtime list (`rt_live.gd`) differ; a permitted call whose argument is a `StringName` (`is_in_group`) or a `NodePath` (`has_node`) works with a JSON string; a wrong type that is not one of these still answers `BAD_PARAMS`. `godot --headless --check-only` passes for `rt_live.gd`; clippy and fmt clean.
- **Handoff:** None; the README sentence is text for T-9D01.
- **Notes:** QA steps G12 to G20, D2 to D17. Adding or removing a child stays allowed (decision in the concept). Do the refusal first, then the conversion: converting `set("script", …)` before the list is extended would open the bypass.
- **Changes:** Both refusal lists extended to the same 19 names, the `queue_free` special case removed, JSON strings converted to `StringName`/`NodePath` in `_coerce`. A unit test keeps the two lists equal; an ignored runtime test refuses every name on a real game and calls `is_in_group`/`has_node`. Red first.

### [T-9C02] Runtime: pointer input in a game without a window (RO-11)

- **Spec:** l1-runtime-observability.md RO-11; l2-game-runtime.md §4 (RO-11 row)
- **Status:** Done
- **Assignment:** Agent
- **Verify:** First a feasibility case in `tests/runtime_e2e_test.rs`: in a headless held game, `click_node` on a Button with a `pressed` handler, then a step, and the handler's flag; the case prints what happens for `click_node`, `mouse_button`, `click`, delivered through `Input.parse_input_event` and through the root viewport's `push_input`. Then the production change, red first: either the Button reacts for the three steps in a headless game (and in a game with a window, as today), or each step answers a failure with the reason. A step is never `ok` after doing nothing.
- **Handoff:** If neither delivery works the task ends with the failure answer and the specification text `l2-game-runtime.md` RO-11 row is amended through `/magic.spec` to say so.
- **Notes:** The cause is not isolated (QA steps H8, H16, H18, H20 against W10). Only API available in Godot 4.5.0.
- **Changes:** Feasibility run first: the headless window is 64 x 64 whatever the project sets, so controls beyond it get the gui events but never `pressed` (cause isolated in a standalone project; `push_input` behaves the same). The step now answers `ok` with a `warning` when its point is outside the viewport (`_done`, `_pointer_note` in `rt_input.gd`). Spec amended: RO-11 warns instead of failing (`l1-runtime-observability.md` 1.2.0). Ignored test red first; the fixture gained a `Tiny` button inside the viewport.

### [T-9C03] Runtime: one scene-stamp note per answer; baseline comparison at the baseline's size

- **Spec:** l2-game-runtime.md §5.4 (answer details)
- **Status:** Done
- **Assignment:** Agent
- **Verify:** Cases in `tests/runtime_e2e_test.rs`, red first: a `godot_run_output` answer with several lines holds the `script_sha256_note` text once; a baseline saved with `max_dimension: 256` is compared by a later `baseline` call without `max_dimension` and the answer reports the comparison instead of the size error. Clippy and fmt clean.
- **Handoff:** None. Lowest priority of the phase; may be postponed without affecting the others.
- **Notes:** QA §7 and W3, W4. Keep every field callers read today; only the repeated text moves.
- **Changes:** `rt.output` answers carry `script_sha256_note` once (the server's fallback lines likewise); a baseline is compared at its own size when `max_dimension` is not given. Two ignored tests, red first. The fixture script prints one line at start so an output has several lines.

### [T-9D01] User-facing text: README, tool descriptions, agent guide, changelog

- **Spec:** l1-content-authoring.md CA-3; l1-runtime-observability.md RO-6; l1-agent-feedback.md FB-10; l1-tool-catalog.md TL-5, TL-10, TL-11
- **Status:** Done
- **Assignment:** Agent
- **Verify:** README Security and tool table, the `godot_edit_script` and `godot_game_call` descriptions in `src/tools/`, the configuration table and `.env.example` say what the specifications now say: an edit can put code in the editor (`@tool`); `game_call` refusal list; an unknown feedback mode gives `draft`; stop and status answers; `project_path` wins over the editor. `CHANGELOG.md` `[Unreleased]` has Changed and Fixed entries in the file's style. `.markdownlint-cli2.jsonc` rules pass (`markdownlint-cli2` if installed, else the rules read by hand and said so); `tests/version_sync_test.rs` still passes. CONTRIBUTING.md and SECURITY.md checked against the change (AGENTS.md checklist). AGENTS.md "Identifier Sanitization" is rewritten to match TL-5: class identifiers keep the strict pattern, node names are data that Godot sanitizes.
- **Handoff:** After the host and runtime tasks it describes.
- **Notes:** The QA report's R29 (pin GitHub's own actions by SHA) is not a task: `l2-ci-release-pipeline.md` records GitHub's own actions by major version as the decision.
- **Changes:** README (security, tools, configuration, `project_path` paragraph), AGENTS.md (identifier sanitization, stop, `godot_game_call`), `.env.example`, `CHANGELOG.md` `[Unreleased]` (Changed, Fixed), the descriptions of `godot_edit_script`, `godot_game_call`, `godot_create_script` and `godot_stop_project`. `markdownlint-cli2` reports 0 issues on README, CHANGELOG and AGENTS. SECURITY.md and CONTRIBUTING.md read against the change: supported lines, reporting channel and branch model are unchanged.

### [T-9T01] Validation: pipeline, end-to-end suites and invariant coverage

- **Spec:** all eleven amended specifications; l1-quality-assurance.md
- **Status:** Done
- **Assignment:** Agent
- **Verify:** On `master` with the build in `.github/godot-version`: clippy, fmt, `cargo test --all-targets`, every GDScript `--check-only` of the AGENTS.md list, then `editor_e2e_test`, `addon_export_test` and `runtime_e2e_test` with `--ignored`. A table in this entry's Notes (run) maps TL-1, TL-5, TL-7, TL-10, TL-11, RO-3, RO-6, RO-11, CA-3, CA-11, CI-7 and FB-10 to the test that proves each; an invariant without a test fails the task. The simulation QA is repeated on the same fixtures for the steps behind Q1 to Q12 (the scripts are in the QA session's scratchpad; if gone, rebuilt from the report) and each step answers as the specification says.
- **Handoff:** T-9E01 after this is Done.
- **Notes:** Run each suite with both `GODOT_PATH` and `GODOT_CONSOLE_PATH` exported (AGENTS.md). An invariant that cannot be tested is stated here, not hidden.
- **Changes:** No product change; the evidence is in the notes below.
- **Notes (run):** Invariant coverage (U = unit test in `src/`, T = `tests/tools_test.rs`, S = `tests/server_test.rs`, P = `tests/stdio_protocol_test.rs`, E = `tests/editor_e2e_test.rs`, R = `tests/runtime_e2e_test.rs`):

| Invariant | Proven by |
| --- | --- |
| TL-1 ranges, object fields | U `validation::a_number_outside_its_documented_range...`; S `schemas_carry_the_documented_ranges_and_describe_object_fields`; T `a_position_below_the_first_is_refused...` |
| TL-5 file arguments, node names | T `a_breakpoint_location_is_a_res_path_inside_the_project`; T `a_root_name_is_data_in_the_file_fallback_as_it_is_in_the_editor` |
| TL-7 hints, `res://` spelling | T `each_failure_class_gets_its_own_hint_and_never_the_generic_one`, `a_godot_executable_that_cannot_run_names_godot_path_not_syntax`, `a_setting_of_the_wrong_type_does_not_get_the_key_example_as_its_hint`, `errors_name_project_files_by_their_res_path` |
| TL-10 named project | T `a_named_project_wins_over_a_connected_editor`, `the_editors_own_project_still_goes_to_the_editor`, `an_editor_that_cannot_name_its_project_is_not_sent_a_file_change`; U `paths::the_same_directory_is_recognized_in_any_spelling` |
| TL-11 stop and status | T `stop_without_a_run_id_stops_every_run_and_says_what_it_stopped`, `..._also_stops_the_editors_play_mode`, `asking_about_runs_with_nothing_running_and_no_editor_is_not_a_failure` |
| RO-3 run record | T `a_finished_run_keeps_its_exit_code_and_says_who_ended_it` |
| RO-6 refusal list, types | U `tools::game::the_server_and_the_runtime_refuse_the_same_methods...`; R `game_call_refuses_what_could_take_the_game_apart_and_converts_argument_types` |
| RO-11 pointer warning | R `pointer_input_in_a_game_without_a_window_warns_when_it_lands_outside_the_viewport` |
| CA-3 setting keys; CA-3 `@tool` statement | T `a_setting_key_with_a_dot_segment_is_refused...`, E `the_editor_commands_follow_the_amended_concepts`; the `@tool` sentence is documentation (README, tool description) and has no test |
| CA-6 folders, CA-7 byte order mark | E `the_editor_commands_follow_the_amended_concepts`; T `edit_script_keeps_the_line_endings_and_the_byte_order_mark_of_the_file` (file path) |
| CA-11 same acceptance | T `the_fallbacks_refuse_a_class_the_engine_cannot_use_and_write_no_file`, `a_root_name_is_data...`, `a_setting_key_with_a_dot_segment...` |
| CI-7 right language server | U `lsp::client::a_server_the_hub_did_not_start_is_not_reused...`; checked by hand end to end (named project, 4.8 s); no automated tool-level test |
| FB-10 fails closed | U `config::an_empty_feedback_mode_is_the_default_and_an_unknown_one_never_widens_the_channel`; P `an_unknown_feedback_mode_offers_drafting_but_never_submission` |
| MCP-1 plain log | P `an_unknown_feedback_mode_falls_back_with_a_warning_on_stderr_only` (no ESC byte) |

Pipeline on master with Godot 4.8-beta1 (the build in `.github/godot-version`): `cargo fmt --check`, `cargo clippy --all-targets --all-features -- -D warnings` and `cargo test --all-targets` pass; every one of the 13 addon scripts passes `--check-only`; `editor_e2e_test` 14/14, `addon_export_test` 2/2, `runtime_e2e_test` 24/24 with `--ignored`. The QA steps behind Q1 to Q12 were repeated on the same fixtures (scratchpad `harness/phase_r.py`): an edit with a named project changes only that project and names it, the refusal list holds, `StringName`/`NodePath` arguments work, a stop without `run_id` stops both managed runs, an ended run shows its exit code. Not covered: the toast in a window, and Godot 4.5, 4.6, 4.7 (T-9E01).

### [T-9E01] Carry to the release lines (after validation)

- **Spec:** l2-release-branches.md; l1-release-lines.md
- **Status:** Done
- **Assignment:** Agent
- **Verify:** On `godot-v4.7`, `godot-v4.6` and `godot-v4.5`, in that order, the product commits applied with their origin named; on each, clippy, fmt, `cargo test --all-targets`, the GDScript checks, `editor_e2e_test`, `addon_export_test` and `runtime_e2e_test` (ignored) pass with that line's own Godot build (`D:/Program Files/Godot/Godot_v4.7.2…`, `4.6.3`, `4.5.2` are installed locally).
- **Handoff:** Pushing the branches needs the maintainer's yes in the conversation.
- **Notes:** Product files only (not the planning files). Run each line with its own engine: the previous carry failed on a metadata key that exists only in 4.8. Calls missing from a line's first release go through `has_method()` + `call()` and into the line's guard list.
- **Changes:** `git cherry-pick -x 514fa28` onto `godot-v4.7` (`a6d43a3`), `godot-v4.6` (`9f10790`) and `godot-v4.5` (`11d7a11`), newest to oldest, one local commit each, no conflicts (the AGENTS.md, README.md and, on 4.6 and 4.5, `bridge_server.gd` merged automatically). On each line, with that line's own build (4.7.2, 4.6.3, 4.5.2): `cargo fmt --check`, `clippy -D warnings` and `cargo test --all-targets` pass, all 13 addon scripts pass `--check-only` with no warning, `editor_e2e_test` 14/14, `addon_export_test` 2/2 and `runtime_e2e_test` 24/24 with `--ignored`. No `has_method()` guard was needed: nothing in the change uses a call missing from 4.5.0. Not carried: the README table commit (`496346c`), the changelog commit (`cecbb4b`) and the planning files. Nothing was pushed; the branches are one commit ahead of their remotes.
