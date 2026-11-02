---
phase: 11
name: "Reality Sync after Ventilation"
status: Done
subsystem: "addons/godot_mcp/bridge_server.gd, .agents/skills/repo-steward, tests, README"
requires: [9]
provides:
  - "the editor path of godot_set_project_setting converts the same text forms as the file fallback (CA-4, CA-11)"
  - "the repo steward's status: planning-owed label and its settle step"
  - "the product commits since 0.12.0 on the 4.7, 4.6 and 4.5 lines, with three portability fixes for older editors"
key_files:
  created: []
  modified:
    - addons/godot_mcp/bridge_server.gd
    - addons/godot_mcp/editor/shader_ops.gd
    - src/tools/authoring.rs
    - tests/editor_e2e_test.rs
    - tests/steward_test.rs
    - .agents/skills/repo-steward/SKILL.md
    - .agents/skills/repo-steward/scripts/labels.sh
patterns_established:
  - "a carry runs the change on the older engines before it counts as carried: an API that exists only on the newest engine (get_base_editor, a .uid from the file system update) is replaced by one every line has"
  - "a text and its conversion rules are kept equal on both paths (editor and file fallback) by one test table"
duration_minutes: ~
---

# Stage 11 Tasks — Reality Sync after Ventilation

**Phase:** 11
**Status:** Done
**Strategic Goal:** The two places where a specification synced on 2026-11-02 asks more than the merged code does are closed: `godot_set_project_setting` accepts and refuses the same values with and without the editor (CA-4, CA-11), and the repo steward labels a merged pull request whose planning update is still owed until that update is committed.

## Atomic Checklist

<!-- A line is `- [ ]` (open) or `- [x]` (done). A Blocked task keeps `- [ ]`: it is blocked by its `Status` in Detailed Tracking, and the `[!]` the workflows write after "Blocked" spells that status — it is not a checkbox state. -->

- [x] [T-11A01] Editor: `set_project_setting` converts the text forms of CA-4
- [x] [T-11B01] Steward: the `status: planning-owed` label, its run step and report line
- [x] [T-11T01] Validation: pipeline, editor suites, settings parity and steward tests
- [x] [T-11E01] Carry the product commits since 0.12.0 to the release lines

## Detailed Tracking

<!-- Optional entry field, created by /magic.run on a task's first dead end — absent means none. A nested list under the entry's field list, one line per abandoned approach, at most five (run.md, Dead-End Record):
  - **Attempts:**
    - {what was tried} → {why it failed}
-->

### [T-11A01] Editor: `set_project_setting` converts the text forms of CA-4

- **Spec:** l1-content-authoring.md CA-4, CA-11; l2-godot-editor-addon.md §6 (open gap); l2-tool-handlers.md §5.5 (setting types)
- **Status:** Done
- **Assignment:** Agent
- **Verify:** A test written first and red in `tests/editor_e2e_test.rs` (ignored, `--test-threads=1`, the build in `.github/godot-version`): through the editor, `application/boot_splash/bg_color` set to `"#102b38"` is stored as a `Color` (read back with `godot_get_project_settings` and in `project.godot` as `Color(...)`); a named color such as `"red"` is accepted for a `Color`; `"Vector2i(1152, 648)"` is accepted for a `Vector2i` setting; a whole number is stored as an `int`; a string that does not convert (`"not a color"` for a `Color`) is refused with the setting named and `project.godot` unchanged; an Object text is refused. The same inputs give the same accept or refuse through the file fallback (the existing `tests/tools_test.rs` cases, extended where one is missing). `README.md` `godot_set_project_setting` row no longer limits the conversions to "without the editor". GDScript `--check-only` of `bridge_server.gd` and the analyzer audit `addon_scripts_have_no_analyzer_warnings` pass.
- **Handoff:** T-11T01; then remove the open-gap note from l2-godot-editor-addon.md §6 and the "does not convert yet" sentence from l2-tool-handlers.md §5.5 with `/magic.spec`.
- **Notes:** `_coerce_setting` in `bridge_server.gd` keeps its exact-type cases and adds: a `Color` from `Color.html_is_valid` / a named color, and for other non-Object types a `str_to_var` parse accepted only when the result has the setting's type (the conversion the server's `SETTING_TYPE_SCRIPT` makes; parse only types whose text holds numbers or strings, so no object is created). Parameters stay read untyped and validated; one reply per path (BR-3). The old value is restored if saving fails, as now.

- **Changes:** `set_project_setting` in `addons/godot_mcp/bridge_server.gd` converts like the file fallback: `_coerce_setting` hands any type without a JSON form to `_setting_from_text` (a Color from an HTML color or a name, a StringName or NodePath from the text, otherwise the `project.godot` text only when it starts with the type's name and parses to that type, never an Object), `_defined_setting_key` gives a feature override such as `viewport_height.mobile` the type of its base setting, and a refusal names the type with the same words as the fallback. The editor test `project_settings_convert_the_same_text_forms_as_the_file_fallback` was written first and failed on the old refusal (`expects a color`); the fallback's real-engine table in `tests/tools_test.rs` gained three refusals (a Vector3 for a Vector2, a number for a Vector2, a fractional int) and passes with the same inputs. The tool description, the README row, the parity table and the changelog no longer limit the conversions to the file path. `--check-only`, the analyzer audit and the existing authoring test pass.

### [T-11B01] Steward: the `status: planning-owed` label, its run step and report line

- **Spec:** l2-repo-steward-procedures.md §5.1 (planning after merge), ST-7 row; l2-repo-steward-skill.md §5.4, §5.5 step 7, §5.7
- **Status:** Done
- **Assignment:** Agent
- **Verify:** Tests written first and red in `tests/steward_test.rs`: `scripts/labels.sh --dry-run` against the fixture repository lists `status: planning-owed` with a colour and a description; `SKILL.md` names the settle step after the pull request step and the `Planning owed` report line; `reference/pull-requests.md` item 8 says to add the label before the merge and remove it after the planning commit, and that a run without the ecosystem keeps it. `cargo test --test steward_test` passes; `bash -n` on every changed script.
- **Handoff:** T-11T01.
- **Notes:** Generic skill files only: nothing names this repository. Messages never mention planning artifacts (SKILL.md rule). The label is set by `act.sh` like the other labels; check that `act.sh` accepts it, and extend its allow-list if it has one.

- **Changes:** `status: planning-owed` (colour `fef2c0`) in `labels.sh`; `SKILL.md` step 7 settles the merged pull requests carrying it, oldest first, when the ecosystem is available, and the run report has a `Planning owed` line; `reference/pull-requests.md` item 8 adds the label before the merge, removes it once the update is committed, and keeps it (listing the pull request in the report) in a run without the ecosystem. `act.sh label` and the unattended workflow's allowed commands already cover it. Tests written first in `tests/steward_test.rs`: `the_skill_creates_the_planning_owed_label` and `the_run_settles_planning_owed_after_the_pull_requests` failed, then passed; `the_planning_owed_text_names_no_planning_artifact` keeps the generic skill free of planning file names. The label does not exist on GitHub until a steward run executes `labels.sh`.

### [T-11T01] Validation: pipeline, editor suites, settings parity and steward tests

- **Spec:** l1-quality-assurance.md; l1-content-authoring.md CA-4, CA-11; l2-repo-steward-procedures.md §5.1
- **Status:** Done
- **Assignment:** Agent
- **Verify:** On `master` with the build in `.github/godot-version`, one run after another: `cargo clippy --all-targets --all-features -- -D warnings`, `cargo fmt --all -- --check`, `cargo test --all-targets`; `godot --headless --check-only` for every addon script in the AGENTS.md list; `editor_e2e_test` and `addon_export_test` with `--ignored --test-threads=1`; `runtime_e2e_test addon_scripts_have_no_analyzer_warnings`. The Changes field lists the CA-4 inputs and the test that proves each with and without the editor.
- **Handoff:** T-11E01.
- **Notes:** The local editor runs in Russian: `open_scene_returns_what_the_load_errors_dialog_shows` fails on the dialog title and is environmental; name it in the report. Never start two end-to-end runs at once.

- **Changes:** On `master` with this machine's Godot 4.8.beta2 (the build in `.github/godot-version` is 4.8-beta1), one step after another: clippy and fmt clean; `cargo test --all-targets` 449 passed, 0 failed; all 20 addon scripts of the AGENTS.md list pass `--check-only`; `editor_e2e_test` 41 of 43 (the 2 failures are the known environmental ones: `open_scene_returns_what_the_load_errors_dialog_shows` compares the English dialog title with the Russian one of this editor, and `close_editor_quits_the_editor_and_its_process_exits` compares a PID with the `_console.exe` wrapper's); `addon_export_test` 2 of 2; `addon_scripts_have_no_analyzer_warnings` passes. CA-4 inputs and their proof, with the editor in `project_settings_convert_the_same_text_forms_as_the_file_fallback` and without it in `project_setting_without_an_editor_converts_with_the_real_engine` (same inputs, real engine): `"#102b38"` and `"white"` for a Color, a whole number or `1280.0` for an int, `720.0` for the int base of `viewport_height.mobile`, `"Vector2(0, 2)"` for a Vector2, `2.5` for a new custom key; refused in both: `"#zzzzzz"` and `3` for a Color, `"wide"` and `1280.5` for an int, `"Object(Node)"`, `"Vector3(0, 2, 1)"` and `2` for a Vector2. The steward tests are in `steward_test` (21 of 21).

### [T-11E01] Carry the product commits since 0.12.0 to the release lines

- **Spec:** l2-release-branches.md; l1-release-lines.md
- **Status:** Done
- **Assignment:** Agent
- **Verify:** The product commits of `master` since the 0.12.0 release commit (`git log --oneline 9b45761..master -- . ':(exclude).design'`), in master's order, are applied to `godot-v4.7`, `godot-v4.6` and `godot-v4.5`, in that order, with `cherry-pick -x`: #94, #91, #98, #100, #101, #99, the editor setting conversion of T-11A01, and the dependency update. Not carried: the steward commits (`.agents/`), `CHANGELOG.md` and planning files, and the identity files keep each line's own values (release skill, procedure B). On each line, with that line's own Godot build (4.7.2, 4.6.3, 4.5.2), one after another: `cargo clippy --all-targets --all-features -- -D warnings`, `cargo fmt --all -- --check`, `cargo test --all-targets`, `godot --headless --check-only` for every addon script, `editor_e2e_test`, `addon_export_test`, `export_e2e_test` and `runtime_e2e_test` (ignored, `--test-threads=1`) pass; the failures already known as environmental (the Russian dialog title, the console wrapper's PID) are named in the report.
- **Handoff:** Pushing the branches needs the maintainer's yes in the conversation.
- **Notes:** The block of the first plan (the lines had to catch up with `master` first) is gone: the lines hold everything up to 0.12.0 and the export tools of #89, so the carry is these eight commits. Risks to check first on 4.5 and 4.6: `addons/godot_mcp/editor/shader_ops.gd` (#91) uses the shader editor, and `import_ops.gd` (#100, #101) uses the editor file system; an API missing from the line's M.N.0 is called through `has_method()` + `call()` (AGENTS.md), and `addon_scripts_have_no_analyzer_warnings` is the first check on each line because an addon that fails to compile hangs the whole editor suite. The Rust 1.87 floor of the dependency update is the same on every line. Pushing the branches, and the release these commits belong to (0.12.1 or 0.13.0, see CHANGELOG), need the maintainer's yes.

- **Changes:** Carried locally to `godot-v4.7`, `godot-v4.6` and `godot-v4.5` with `cherry-pick -x`, in master's order: #94, #91, #98, #100, #101, #99, the dependency update and the editor setting conversion (the CHANGELOG.md part of each dropped, the line's copy kept; on 4.6 and 4.5 the open-scene description conflict kept the line's text and added the new sentence). Running the carried commits on the older engines found three gaps that the 4.8 editor hides, fixed on `master` as ffe4735 and carried: the shader tab has `get_base_editor` only from 4.8 (the text editor is now the tab's CodeEdit child on older editors, and an edit waits up to 30 frames for the tab), the file system update gives a new text shader no `.uid` on older editors (the sidecar is written), and the tab switch after a reload needs to be asked again on each frame on 4.5 and 4.6. Verified on each line with its own engine (4.7.2, 4.6.3, 4.5.2), one after another: clippy and fmt clean; `cargo test --all-targets` 427 passed, 0 failed; all 20 addon scripts pass `--check-only`; the analyzer audit passes; `addon_export_test` 2 of 2; `runtime_e2e_test` 52 of 52; `editor_e2e_test` 41 of 43 and `export_e2e_test` 3 of 4, the failures being only the three known environmental ones, which fail the same way on `master` with the 4.8 engine: the Russian dialog title (`open_scene_returns_what_the_load_errors_dialog_shows`), the console wrapper's PID (`close_editor_quits_the_editor_and_its_process_exits`) and the Russian text of the missing-template error (`an_unusable_release_template_is_an_error_with_the_log` looks for the English words). Line commits (newest last): 4.7 a094417 to ef6b5cd, 4.6 dbf6a51 to c2edca7, 4.5 126db7d to f6d0a8c. Not pushed.
