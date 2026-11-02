---
phase: 10
name: "Project Adapter"
status: Done
subsystem: "addons/godot_mcp/runtime, src/tools/game, src/godot, tests, README"
requires: [6, 7]
provides:
  - "godot_game_state and godot_game_command tools"
  - "project adapter registry and loader in the in-game component (godot_mcp/adapter)"
  - "project.state and project.command runtime methods, held and in editor Play"
  - "adapter status in the hello and in godot_get_run_status"
  - "proof on the 4.7, 4.6 and 4.5 lines with their own engines"
key_files:
  created:
    - addons/godot_mcp/runtime/ops/rt_registry.gd
    - addons/godot_mcp/runtime/ops/rt_project.gd
    - src/tools/game/project.rs
    - tests/common/adapter.rs
  modified:
    - addons/godot_mcp/runtime/mcp_runtime.gd
    - addons/godot_mcp/runtime/ops/rt_output.gd
    - src/tools/game/catalog.rs
    - src/tools.rs
    - README.md
    - AGENTS.md
patterns_established:
  - "one development-only adapter script named by a project setting, with godot-dev-bridge's registration contract"
  - "a failure in the project's own code is reported with its class, never hidden"
  - "a fixed feasibility test on the real engine gates a design that rests on an engine fact"
duration_minutes: ~
---

# Stage 10 Tasks — Project Adapter

**Phase:** 10
**Status:** Done
**Strategic Goal:** A project names one development-only adapter script in `godot_mcp/adapter`; the in-game component loads it with godot-dev-bridge's registration contract, and an agent reads the project's states with `godot_game_state` and runs its commands with `godot_game_command`, in a managed run and in editor Play, held or not, with failures in the project's code reported instead of hidden. An adapter written for godot-dev-bridge works unchanged.

## Atomic Checklist

<!-- A line is `- [ ]` (open) or `- [x]` (done). A Blocked task keeps `- [ ]`: it is blocked by its `Status` in Detailed Tracking, and the `[!]` the workflows write after "Blocked" spells that status — it is not a checkbox state. -->

- [x] [T-10A01] Feasibility: the logger sees a handler's script error (engine premise)
- [x] [T-10B01] Runtime: the registry and the output marks (PA-2, PA-5)
- [x] [T-10B02] Runtime: loading the adapter, its status, the hello and the runtime copy (PA-1, PA-6, PA-10)
- [x] [T-10B03] Runtime: `project.state` and `project.command` (PA-3, PA-6 to PA-9)
- [x] [T-10C01] Host: `godot_game_state` and `godot_game_command`, run status and hints (PA-4, TL-1, TL-7)
- [x] [T-10C02] Editor Play: the adapter answers in a played game (PA-10)
- [x] [T-10D01] User-facing text: README, agent guide, check lists, changelog
- [x] [T-10T01] Validation: pipeline, end-to-end suites and invariant coverage
- [x] [T-10E01] Prove the adapter on the release lines (carried with 0.12.0)

## Detailed Tracking

<!-- Optional entry field, created by /magic.run on a task's first dead end — absent means none. A nested list under the entry's field list, one line per abandoned approach, at most five (run.md, Dead-End Record):
  - **Attempts:**
    - {what was tried} → {why it failed}
-->

### [T-10A01] Feasibility: the logger sees a handler's script error (engine premise)

- **Spec:** l2-project-adapter.md §2 (engine premise), §6 step 1; l1-project-adapter.md PA-6
- **Status:** Done
- **Assignment:** Agent
- **Verify:** A new ignored test in `tests/runtime_e2e_test.rs`, run with `GODOT_PATH` set to the build named in `.github/godot-version` (`4.8-beta1`) and `--ignored --test-threads=1`, pins four facts in a managed run: (1) a GDScript handler that reads a property of a null instance, called through `Callable.call()` from the runtime, produces a logger entry before `call()` returns; (2) that entry carries the engine's script error type; (3) `push_error` in another handler produces an entry of a different type; (4) what `Callable.call()` returns for a handler whose first statement is an `await`. The test passes and its assertions name the four facts.
- **Handoff:** If (1) or (2) does not hold, stop the phase before T-10B03 and run `/magic.spec amend l1-project-adapter.md "PA-6 attaches logged errors without failing the call"`; T-10B01 and T-10B02 do not depend on the outcome. Record the four measured facts in this task's Changes.
- **Notes:** The logger is `OutputLogger` in `addons/godot_mcp/runtime/ops/rt_output.gd` (installed in the runtime's `_enter_tree`). The test may reach the facts through a temporary runtime method or through T-10B01's marks if that task lands first; either way the test stays in the suite.
- **Changes:** Test `logger_sees_a_script_error_of_a_called_handler_at_once` and its probe project in `tests/runtime_e2e_test.rs` (4.8-beta1: passes, 1.1 s; clippy and fmt clean). Measured: (1) a script error in a handler called with `Callable.call()` is in the logger when the call returns, on the main thread, with the script error type, its text in `code` and an empty `rationale`, and the handler returns null; (2) `push_error` is logged with the plain error type; (3) an awaiting handler called plainly makes the engine log a script error against the calling function and ends that function only, so the dispatch above it still answers; (4) `await handler.call(args)` from a coroutine runs both kinds. The premise holds, but the second half of the first spec text (an awaiting handler hands back at its first `await`) did not: both adapter specs are amended to 1.1.0 (PA-9: not supported, fails through PA-6), the notes of the two later tasks that depend on it are adjusted. `[DR]` a coroutine layer that supports awaiting handlers was not added (godot-dev-bridge has none either).

### [T-10B01] Runtime: the registry and the output marks (PA-2, PA-5)

- **Spec:** l2-project-adapter.md §4 (PA-2, PA-5, PA-6 rows), §5.1
- **Status:** Done
- **Assignment:** Agent
- **Verify:** Tests written first and red: a fixture adapter (one file kept under `tests/common/`, shared by the runtime and editor end-to-end tests) registers two valid states, two valid commands, a provider that takes an argument, a handler with no argument, an empty name and a name registered twice; a runtime end-to-end test reads the registry through a runtime method and finds the valid ones, the second registration of the repeated name, and the three refused ones listed under `refused` with `{name, kind, reason}`; a command returning `registry.refuse("x")` is told apart from one returning `{"error": "x"}`. `godot --headless --check-only --path . --script addons/godot_mcp/runtime/ops/rt_registry.gd` and the same for `rt_output.gd` report nothing.
- **Handoff:** T-10B02 hands the registry to the adapter; T-10B03 uses the marks.
- **Notes:** Each call into the project's code goes through its own typed-return function (`call_provider`, `call_command`), so a script error that ends the function leaves the dispatch standing. `rt_registry.gd`: `add_state(name, provider) -> bool`, `add_command(name, handler, help = "") -> bool`, `refuse(message) -> Dictionary` returning `{"__bad_args__": str(message)}`, names untyped where godot-dev-bridge has them untyped. No `class_name`. `rt_output.gd` gains `mark()` (the next sequence number) and `lines_since(mark)` (the lines logged after it, with their type). Strict typing; parameters read untyped and validated.
- **Changes:** `addons/godot_mcp/runtime/ops/rt_registry.gd` (`add_state`, `add_command`, `refuse`, `state_names`, `command_help`, `refused`, `has_command`, `call_provider`, `call_command`; arguments read untyped and validated, a repeated name replaces, a refusal pushes a warning and is listed) and `rt_output.gd` (`mark`, `lines_since`, a `script` flag kept off the output tool's lines); the file joins `RUNTIME_FILES`, the three check lists (AGENTS.md, CONTRIBUTING.md, CI) and gets its `.uid`. Fixture adapter in `tests/common/adapter.rs`. Tests `registry_keeps_what_an_adapter_registers_and_lists_what_it_refused` and `output_marks_return_what_was_logged_since_with_the_script_errors_told_apart` written red first, green on 4.8-beta1; a mutation of the handler argument count turns the first red. Fmt, clippy, `cargo test --all-targets`, `--check-only` and `addon_scripts_have_no_analyzer_warnings` pass (the audit caught a local named `since` shadowing the first name of the reader, now `lines_since`). `[DR]` the registry is exercised through a probe game that loads it from the runtime copy, because the loader and the methods belong to the next two tasks; the probe must hold the adapter instance, or its callables go invalid (the loader will).

### [T-10B02] Runtime: loading the adapter, its status, the hello and the runtime copy (PA-1, PA-6, PA-10)

- **Spec:** l2-project-adapter.md §2 (the setting), §4 (PA-1, PA-6, PA-10 rows)
- **Status:** Done
- **Assignment:** Agent
- **Verify:** Tests written first and red, in `tests/runtime_e2e_test.rs` (ignored, 4.8-beta1 build): with no `godot_mcp/adapter` the hello's `adapter` is `{configured: false}`; with the fixture adapter by `res://` path and by `uid://` the status is `loaded: true` with its `states` and `commands` (name → help); a setting of `user://x.gd`, a missing file, a script without `register` and a `register` that raises a script error each give `loaded: false` with an `error` that names the cause (the last one keeps the registrations made before the error); an adapter that extends `Node` is freed when the game exits (no leaked-instance line in the output). In `src/godot/runs.rs` the existing unit tests over `RUNTIME_FILES` cover the two new scripts in the private runtime copy. Clippy and fmt clean.
- **Handoff:** T-10B03 answers from the loaded registry; T-10C01 shows the status in `godot_get_run_status`.
- **Notes:** Load in the runtime's `_ready`, before `_send_hello` is scheduled. Resolve `uid://` with `ResourceUID`; accept only `res://` after resolution. The running-game tools never set the setting (PA-1).

- **Changes:** `addons/godot_mcp/runtime/ops/rt_project.gd` (`load_adapter`, `unload`, `status`; reads `godot_mcp/adapter` once, resolves a uid, loads, instantiates, calls `register` in a function of its own and reads the log for a script error or a parse error), wired into `mcp_runtime.gd` before the hello, which now carries `adapter`; `Run::summary()` shows the announced `adapter`, so `godot_get_run_status` has it; the file joins `RUNTIME_FILES` and the check lists, with its `.uid`. Tests written red first, green on 4.8-beta1: no setting gives `{configured: false}`; the fixture by path and by a uid that an import wrote; five unusable adapters each with its cause (`user://`, missing file, no `register`, parse error, a script error that keeps `early` and drops `late`); a Node adapter outside the tree is freed at exit (the engine's leak warning appears when the free is removed). Fmt, clippy, `cargo test --all-targets`, `--check-only` and the analyzer audit pass. `[DR]` the run summary reads the adapter now, not in the host task: the status cannot be tested without it, and that task keeps the schemas, annotations and hints.

### [T-10B03] Runtime: `project.state` and `project.command` (PA-3, PA-6 to PA-9)

- **Spec:** l2-project-adapter.md §4 (PA-3, PA-6 to PA-9 rows), §5.2
- **Status:** Done
- **Assignment:** Agent
- **Verify:** Tests written first and red, in `tests/runtime_e2e_test.rs` (ignored, 4.8-beta1 build), each named after the rule it pins: an empty `names` reads every provider; an unknown state is an `{error}` entry beside the others' values; a provider that raises a script error fails its own entry only; a command answers `value` and the `errors` logged meanwhile; an unknown command answers `NOT_FOUND` with `data.commands`; a refusal answers `BAD_PARAMS` with its message; a handler's script error answers `GODOT_ERROR`; a handler that awaits answers `GODOT_ERROR` with the hint that handlers must not await, and the runtime still answers the next request; a state whose JSON text exceeds 262 144 bytes answers `BAD_PARAMS` with `data.sizes`; in a game held with `godot_game_physics` `hold`, `project.state` and `project.command` answer, and a command's `call_deferred` change is visible after one held step and not before; with no adapter, `project.command` answers `UNSUPPORTED` with the status. Arguments arrive as parsed JSON (a `3` is a float).
- **Handoff:** T-10C01 maps these codes to tool answers and hints.
- **Notes:** `rt_project.gd` exports the two methods through `methods()` like the other modules; they stay out of `Hold.refusal`. Values through `rt.encode`. If T-10A01 found the logger blind to script errors, this task waits for the PA-6 amendment.

- **Changes:** `rt_project.gd` `state` and `command` (names validated, one provider or handler at a time with the log read around each, `rt.encode` for values, `UNSUPPORTED` when no adapter loaded, `NOT_FOUND` with the names, `BAD_PARAMS` for a refusal, `GODOT_ERROR` for a script error or an awaiting handler, a 262144-byte bound), registered in `mcp_runtime.gd` and served by the hold (`Hold.SERVED`: the hold refuses every method not on it, so the second spec text was wrong and is corrected in 1.1.1). Tests written red first, all green on 4.8-beta1 through the tools: `states_are_read_by_name_with_each_failure_kept_to_its_own_entry` (all names, an unknown one, a script error alone, the bound with `sizes`), `commands_run_by_name_and_every_kind_of_failure_is_told_apart` (float arguments, an `error` entry as a result, refusal, unknown name, script error, awaiting handler and the next call still answered, a value over the bound), `a_game_without_a_usable_adapter_refuses_both_tools_and_says_how_to_name_one`, `a_held_game_answers_both_tools_and_what_a_command_defers_lands_on_the_next_tick` (a mutation of the served list turns it red). The whole `runtime_e2e_test` suite passes (45 tests, 121 s) with the hello now carrying the adapter. `[DR]` a command's value over the bound is `GODOT_ERROR` that says it ran, not `BAD_PARAMS`: asking again would run it again.

### [T-10C01] Host: `godot_game_state` and `godot_game_command`, run status and hints (PA-4, TL-1, TL-7)

- **Spec:** l2-project-adapter.md §4 (PA-4 row), §5.2 (recovery column), §5.3; l1-tool-catalog.md §5.1
- **Status:** Done
- **Assignment:** Agent
- **Verify:** Tests written first and red in `tests/tools_test.rs`: `tools/list` has both tools with their schemas (`names` at most 64 strings of 1 to 256 characters; `name` required, 1 to 256; `args` an object; `run_id`) and annotations `[true, false, true]` and `[false, true, false]`; a schema violation fails before any game is contacted and names the parameter; with no game running both say so and name `godot_run_project`; the recovery for each runtime code of §5.2 is the one the table names (unit test of the mapping). In `tests/runtime_e2e_test.rs` (ignored): `godot_get_run_status` shows the fixture adapter's status for a managed run; `godot_game_command` with `run_id` reaches that run. Clippy and fmt clean; no `unwrap()` or `expect()` in the new code; no lock held across the call into the game.
- **Handoff:** T-10D01 documents the final names and answers.
- **Notes:** The `tools_test.rs` part can start beside track B; the end-to-end part needs T-10B03. Definitions in `src/tools/game/catalog.rs`, handlers in a new `src/tools/game/project.rs`, annotations in `src/tools.rs`. Run selection as `godot_game_get`. The descriptions say that a command does what the project wrote, files included, and point to `adapter` in `godot_get_run_status`.

- **Changes:** `src/tools/game/project.rs` (the two handlers: names and name length checked before the game is asked, the run's id added to the answer, a failure class of the adapter mapped to its own recovery steps), the two definitions in `catalog.rs` with schemas and descriptions, routing in `game.rs`, hints in `tools.rs` (`godot_game_state` read-only, `godot_game_command` destructive and not idempotent). Tests written red first: `the_project_tools_check_their_arguments_before_a_game_is_asked` and `the_project_tools_are_listed_with_their_hints_and_say_what_a_command_may_do` in `tests/tools_test.rs`, three unit tests of the hint mapping in `project.rs`, and the end-to-end tests of the previous two tasks (the run status shows the adapter; a `run_id` reaches its run). Fmt, clippy and the default suite pass; no `unwrap()` or `expect()` in the new code and no lock is held across the call into the game.

### [T-10C02] Editor Play: the adapter answers in a played game (PA-10)

- **Spec:** l1-project-adapter.md PA-10; l2-project-adapter.md §4 (PA-10 row)
- **Status:** Done
- **Assignment:** Agent
- **Verify:** A test written first and red in `tests/editor_e2e_test.rs` (ignored, 4.8-beta1 build, `--test-threads=1`): with the fixture adapter set in the test project, Play through `godot_run_project`, then `godot_game_state` returns the fixture's values and `godot_game_command` runs one of its commands through the relay; `godot_stop_project` ends Play.
- **Handoff:** None.
- **Notes:** After T-10B02, T-10B03 and T-10C01. The relay (`game_relay.gd`) is not changed.

- **Changes:** `editor_play_answers_the_project_adapter` in `tests/editor_e2e_test.rs` (the fixture adapter set in the fixture project; Play, a state read, a command and an unknown command through the relay), green on the 4.8-beta1 editor; the relay is unchanged. It found that an answer from Play had no `run_id`: a call with no managed run now labels its answer `editor`. The status of an editor Play game is in every `project.*` answer, because the server keeps no hello for it (the second spec text said the run status would show it; corrected in 1.1.1).

### [T-10D01] User-facing text: README, agent guide, check lists, changelog

- **Spec:** l2-project-adapter.md §5.4
- **Status:** Done
- **Assignment:** Agent
- **Verify:** `README.md` lists `godot_game_state` and `godot_game_command` and has an adapter section with the setting, the three registry calls, the outcome table, a short example and the note for godot-dev-bridge adapters (set `godot_mcp/adapter` to the same file); the running-game protocol in `AGENTS.md` has the adapter line; `rg -n "rt_registry.gd|rt_project.gd" AGENTS.md CONTRIBUTING.md` shows both scripts in both check lists; `CHANGELOG.md` has an Unreleased entry; the tool descriptions in `src/tools/game/catalog.rs` agree with the README.
- **Handoff:** T-10T01.
- **Notes:** English only. The two check lists change together (AGENTS.md says so). No reference to the planning files in product text.

- **Changes:** README: the two tools in the table and a section on the adapter (the setting, a complete example, the registry calls, the failure classes, the bound, held games and Play, and the note for godot-dev-bridge adapters); the agent guide's running-game protocol has the adapter line; the check lists (guide, contributor guide, CI) already carry both scripts from the tasks that created them; CHANGELOG has an Unreleased entry. The tool descriptions in `catalog.rs` and the README say the same things (the setting, the bound, no `await`, held games).

### [T-10T01] Validation: pipeline, end-to-end suites and invariant coverage

- **Spec:** l1-project-adapter.md PA-1 to PA-10; l2-project-adapter.md §4; l1-quality-assurance.md
- **Status:** Done
- **Assignment:** Agent
- **Verify:** On `master` with the 4.8-beta1 build: `cargo clippy --all-targets --all-features -- -D warnings`, `cargo fmt --all -- --check`, `cargo test --all-targets`; `godot --headless --check-only` for every addon script in the AGENTS.md list, the two new ones included; `editor_e2e_test`, `addon_export_test`, `runtime_e2e_test` (with `addon_scripts_have_no_analyzer_warnings`) and `scenario_e2e_test` with `--ignored --test-threads=1`, run one after another. This task's Changes holds a table from each of PA-1 to PA-10 to the test names that prove it, with no row empty.
- **Handoff:** T-10E01.
- **Notes:** The local editor runs in Russian: `open_scene_returns_what_the_load_errors_dialog_shows` fails on the "Load Errors" text and is environmental; name it in the report. Never start two end-to-end runs at once.

- **Changes:** On `master` with the 4.8-beta1 build, one step after another: clippy and fmt clean; `cargo test --all-targets` passes (187 unit tests, `tools_test` 84, the rest unchanged); all 17 addon scripts pass `--check-only`; `editor_e2e_test` 19 of 20 (the one failure, `open_scene_returns_what_the_load_errors_dialog_shows`, compares the English dialog title with the Russian "Ошибки загрузки" of this machine's editor: environmental); `addon_export_test` 2 of 2; `runtime_e2e_test` 46 of 46, the analyzer audit included; `scenario_e2e_test` 5 of 5. Coverage of the invariants: PA-1 `a_game_without_an_adapter_setting_announces_none`, `an_adapter_named_by_path_or_by_uid_registers_what_it_offers`, `a_game_without_a_usable_adapter_refuses_both_tools_and_says_how_to_name_one`, and the tool schemas in `the_project_tools_are_listed_with_their_hints_and_say_what_a_command_may_do` (no parameter names or changes the adapter); PA-2 `registry_keeps_what_an_adapter_registers_and_lists_what_it_refused`; PA-3 the unknown names in `commands_run_by_name_and_every_kind_of_failure_is_told_apart` and `states_are_read_by_name_with_each_failure_kept_to_its_own_entry`; PA-4 the hints in the listing test and `free_a_node` in the commands test (a command frees a node, which `godot_game_call` refuses); PA-5 the registry test and the commands test (a refusal against an `error` entry); PA-6 `logger_sees_a_script_error_of_a_called_handler_at_once`, `output_marks_return_what_was_logged_since_with_the_script_errors_told_apart`, `an_adapter_that_cannot_be_used_says_why_and_keeps_what_it_registered` and the script-error and awaiting cases of the commands test; PA-7 the state test (`broken` fails alone); PA-8 the state test (the bound with `sizes`, `position` encoded as `game.get` does) and the commands test (a value over the bound); PA-9 `a_held_game_answers_both_tools_and_what_a_command_defers_lands_on_the_next_tick` and the awaiting case; PA-10 the path-or-uid test, `editor_play_answers_the_project_adapter`, `an_adapter_that_is_a_node_outside_the_tree_is_freed_when_the_game_ends`, `a_game_started_without_a_debugger_never_loads_the_adapter` and the unit tests over the private runtime copy. No row is empty.

### [T-10E01] Prove the adapter on the release lines (carried with 0.12.0)

- **Spec:** l2-release-branches.md; l1-release-lines.md; l2-project-adapter.md §5.5
- **Status:** Done
- **Assignment:** Agent
- **Verify:** The project adapter is on `godot-v4.7`, `godot-v4.6` and `godot-v4.5`: `rt_registry.gd`, `rt_project.gd`, `src/tools/game/project.rs` and `tests/common/adapter.rs` exist on each, `godot_game_state` and `godot_game_command` are in `README.md` and both runtime scripts are in the `AGENTS.md` check list (all checked on 2026-11-02 with `git ls-tree`). Then, on each line with that line's own Godot build (4.7.2, 4.6.3, 4.5.2), one after another: `cargo test --all-targets`, and `runtime_e2e_test` (ignored, `--test-threads=1`) including the adapter tests, pass. The Changes field records the line commits that carried the adapter (found with `git log --format=%h -S godot_game_state <line> -- src/tools/game/catalog.rs`) and the results.
- **Handoff:** Pushing the branches needs the maintainer's yes in the conversation.
- **Notes:** The earlier block (the lines lacked 130 commits of master) no longer holds: the lines were brought up to date and released as 0.12.0, and the adapter was carried with them. What is left is to record the carry and prove the adapter on each line's engine. No cherry-pick is needed; if a check fails on a line, fix it there as a line commit (`cherry-pick -x` from master when the fix exists on master). Product files only. Pushing needs the maintainer's yes in the conversation.
- **Attempts:**
  - Applied the uncommitted product changes of master as one patch to detached worktrees of `godot-v4.7`, `godot-v4.6` and `godot-v4.5` → refused: the lines lack 130 commits of master (the unreleased 0.9.0 work, #52 to #62, among them the tool hints and the held-list text this patch builds on), so six files do not apply
  - Cherry-pick with `-x` as the task says → not possible yet: the product changes are not committed on master, and nothing here commits without the maintainer's say-so
- **Changes:** Nothing carried. What was proved instead, against this tree with each line's own engine (4.7.2, 4.6.3, 4.5.2): the 13 end-to-end tests of the adapter, the held game, the unload at exit and the inert component all pass on each of them, so the runtime uses nothing the lines' first releases lack and no `has_method()` guard is needed.

- **Changes (2026-11-02):** The adapter was already on all three lines and needed no carry: `rt_registry.gd`, `rt_project.gd`, `src/tools/game/project.rs` and `tests/common/adapter.rs` exist on `godot-v4.7`, `godot-v4.6` and `godot-v4.5`, which carried it as a240a63, 595f928 and 15e9fa0 and released it with 0.12.0; `README.md` lists both tools and `AGENTS.md` both scripts. Proved on each line's own engine (4.7.2, 4.6.3, 4.5.2), one after another, on the line's release commit (e3c0fc9, c2af857, 6860c90): `cargo test --all-targets` 403 passed, 0 failed; `runtime_e2e_test` (ignored, one thread) 51 of 51, the 11 adapter tests among them. Nothing failed, so no line commit was needed.
