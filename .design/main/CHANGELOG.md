# Workspace Phase Journal

Internal journal of finished phases. User-facing release notes are in the root `CHANGELOG.md`.

## Phase 3 — 2026-10-16

- Scrubber, fingerprint, percent-encoding and link builder (`src/feedback.rs`), with a mutation check on every scrub step.
- In-memory journal of notes, evidence, counters and drafts with a Pending / Submitting / Submitted state machine and a fenced report builder (`src/journal.rs`).
- GitHub CLI runner with duplicate look-up, fixed argument vectors and outcome mapping (`src/issues.rs`); stand-in `gh` example target and test helper.
- Settings `GODOT_MCP_FEEDBACK` and `GODOT_MCP_GH_PATH`; catalog and guidance by mode; outcome and unknown-tool recording that never alters a result.
- Tools `godot_note_feedback`, `godot_draft_feedback` and `godot_submit_feedback` (consent flag, concurrency and cancellation safe).
- README, configuration template, changelog entry, agent protocol item and a manual-path issue template.
- Found by the tests: a project under the home directory would have shown its folder name; the home directory is now shortened only once the project is known.

## Phase 7 — 2026-10-21

- Hold a running game at the start of a physics tick (`rt_hold.gd`): nothing advances, even in nodes that ignore pause, while reads, `set` and `call` with `permit` are still answered; the debugger channel is driven from the hold with `EngineDebugger.line_poll()`.
- Held steps: an exact number of ticks, or until a public property satisfies a condition (no expression, no method call), bounded by `max`; the answer says whether stepping is exact and why not.
- Input for a held game by tick: taps last `hold_ticks` ticks (default 2, measured: just pressed on the second tick), releases fall due at a tick start, ending a hold lets go of everything.
- Operations that need time to pass are refused while held with the step that replaces them; a single capture shows the last drawn frame.
- `godot_run_project` `held`: a game started held at a fixed frame rate equal to the project's tick rate, so its steps are exact; the server keeps a held step's 60 s wait and refuses a debugger `pause` on a held game.
- Found by the tests: a step that did not count the tick it was started on ran one tick too many; the engine keeps `EngineDebugger.is_active()` true after the server's socket closes, so a vanished server cannot be detected from the game (a recorded limitation, with a keepalive as the deferred alternative); a screenshot test that moved one pixel passed only when a counter wrapped.
- Carried to `godot-v4.7`, `godot-v4.6` and `godot-v4.5` as one commit each, verified with each line's pinned build.

## Phase 5 — 2026-10-21

- The portable repo-steward skill: scripts for context, profile, labels, readiness, board and the one way to write to GitHub, instructions and references for triage, replies, pull requests, changes, board, readiness and setup, and templates.
- Installed here with its profile, priorities, workflows (readiness, stale reminders, an agent run that stays off until switched on), issue forms and pull request template; contributor and agent guides describe it; `tests/steward_test.rs` guards the contracts.
- Labels and a public board with fields, views and policies; a release-readiness notice kept current by a workflow.
- Attended acting runs reviewed the contributor series and merged it with the review fixes, carried each change to the release lines, and made the 0.6.0 release at the maintainer's request; a later run posted the released message on the fixed issue and proved that a second pass posts nothing.
- A ruleset now protects `master`; head branches are deleted after merge; first-time contributors need approval for workflow runs.
- Found along the way: a native Windows `gh` cannot open a POSIX temporary path, so the action script translates it; GitHub reserves the field name Type, so the board field is Kind; the token needs the project scope for board steps, and a run without it skips them and says so.

## Phase 8 — 2026-10-21

- Feasibility on a real engine first (Godot 4.8-beta1): the script editor exists without a window, unsaved typing is the code editor's version differing from its saved version, a fragment replacement is one undo step, and saving gives a running tool script the new code. Found false: no call names an editor's resource, so editors are paired with paths through the two lists the script editor offers.
- `godot_read_script`: the text the editor would save (unsaved typing included), where it came from, whether it is unsaved, and a version label (64-bit FNV-1a, the same in the addon and the host). `godot_edit_script`: one fragment that must occur exactly once, an optional version check, one undoable editor action that saves, refused while the editor holds unsaved changes to the script. File fallback through an exclusive temporary file; line endings and byte order mark kept.
- Specifications: Content Authoring 1.1.0 (CA-7 to CA-10), Godot Editor Addon 1.4.2, Tool Handlers 1.3.2, Tool Catalog 1.4.0.
- Found by carrying: Godot 4.5.2, 4.6.3 and 4.7.2 give script tabs no `_tab_name` metadata; the pairing check now applies only where it exists. Found by the security review: the edit's temporary file is now created exclusively under an unpredictable name and a script that is a symbolic link is refused.
- Carried to `godot-v4.7`, `godot-v4.6` and `godot-v4.5` as three commits each, verified with each line's pinned build.

## Phase 10 — 2026-11-02

- A project names one development-only adapter script in `godot_mcp/adapter`; the in-game component loads it with godot-dev-bridge's registration contract (so its adapters work unchanged), keeps what it registers and what it refused, and shows its status in the hello and in `godot_get_run_status`.
- `godot_game_state` reads the states the adapter registered and `godot_game_command` runs one of its commands by name, in a managed run and in editor Play, held or not (a held game defers what a command schedules to the next tick); answers are bounded and each failure class has its own recovery steps.
- Failures in the project's own code are reported instead of hidden: the output logger marks script errors of a called handler, and a state that breaks fails alone.
- Feasibility on a real engine first (Godot 4.8-beta1): the logger sees a handler's script error at once, so no specification amendment was needed.
- Specifications: Project Adapter and Project Adapter Runtime (new), Runtime Observability 1.3.0, Tool Catalog 1.8.0, Game Runtime Channel 1.6.0.
- Proved on every release line with its own engine (4.7.2, 4.6.3, 4.5.2): the adapter was carried with 0.12.0 and its end-to-end tests pass on each line (51 of 51 runtime tests).

## Phase 11 — 2026-11-02

- `godot_set_project_setting` with the editor accepts the same values as without it: an HTML color or a color name for a Color, the `project.godot` text for other types (never an Object), a feature override takes the type of its base setting; one test table now holds both paths.
- The repo steward labels a merged pull request `status: planning-owed` until the planning update for it is committed, and a later run with the planning ecosystem settles the labelled ones first.
- The specifications were first brought in line with the code merged since Phase 10 (the derived bridge port, the export, shader, import, InputMap and deletion tools, one-off scripts and their repairs, branch cleanup); a new concept covers export.
- Carried the product commits since 0.12.0 to the 4.7, 4.6 and 4.5 lines. Found by carrying: the shader tab has `get_base_editor` only from Godot 4.8, a new text shader gets no `.uid` from the file system update on older editors, and a reloaded scene's tab is switched to a frame later on 4.5 and 4.6; all three are fixed in one commit on `master` and carried with the rest.
