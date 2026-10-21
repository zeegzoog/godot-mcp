---
phase: 6
name: "Game Runtime, Authoring & Code Intelligence"
status: Done
subsystem: "src (godot, lsp, tools), addons/godot_mcp (runtime, editor, core), tests, README, docs"
requires: [4]
provides:
  - Running-game tools: logs, live tree and properties, input and gestures, waits, screenshots with a frame sheet and a pixel diff
  - Managed process runs with Godot's remote debugger; breakpoints, pause, stepping, stack and variables
  - GDScript language-server client: diagnostics, symbols, completion, definition, references
  - Authoring tools: scene, script attachment, signals, resources, project settings
  - Reload of an open scene whose file changed on disk
key_files:
  created:
    - src/godot/
    - src/lsp/
    - src/tools/authoring.rs
    - src/tools/game.rs
    - src/tools/code.rs
    - addons/godot_mcp/runtime/
    - addons/godot_mcp/editor/game_relay.gd
    - addons/godot_mcp/core/runtime_override.gd
    - tests/runtime_e2e_test.rs
    - docs/feature-parity.md
  modified:
    - addons/godot_mcp/bridge_server.gd
    - tests/editor_e2e_test.rs
    - tests/tools_test.rs
    - README.md
    - AGENTS.md
patterns_established:
  - A client of an engine protocol instead of a reimplementation of it (remote debugger, language server)
  - A runtime component that exists only while a debugger is attached and is removed when the run ends
  - Contributor pull requests reviewed on live editors of every release line, fixed on the contributor's branch, squash-merged and carried to the lines
duration_minutes: ~
---

# Stage 6 Tasks — Game Runtime, Authoring & Code Intelligence

**Phase:** 6
**Status:** Done
**Strategic Goal:** Let an agent run a game, look at it, act on it and debug it, ask the language server about its scripts, and create scenes, resources and settings, all within the catalog's safety rules.

## Atomic Checklist

- [x] [T-6A01] Scene reload for a stale open scene (pull request 2)
- [x] [T-6B01] Remote debugger client and Variant codec (pull request 3)
- [x] [T-6B02] Running-game inspection, input and screenshots (pull request 4)
- [x] [T-6B03] Breakpoints and stepping for a managed game (pull request 5)
- [x] [T-6C01] Language-server client (pull request 6)
- [x] [T-6D01] Authoring tools (pull request 7)
- [x] [T-6B04] Play-time race, gestures, waits and picture comparison (pull requests 9 and 13)
- [x] [T-6S01] Specifications for the concepts and their realizations

## Detailed Tracking

### [T-6A01] to [T-6D01] The pull request series

- **Spec:** l1-runtime-observability.md; l1-code-intelligence.md; l1-content-authoring.md; l2-game-runtime.md; l2-lsp-client.md
- **Status:** Done
- **Assignment:** Contributor (theromis), review and fixes: Agent
- **Verify:** Each pull request: default tests, clippy and fmt; `editor_e2e_test`, `addon_export_test` and `runtime_e2e_test` on Godot 4.8-beta1 and, with the change applied to each release branch, on 4.7.2, 4.6.3 and 4.5.2; CI green on four jobs; squash-merged, carried to `godot-v4.7`, `godot-v4.6`, `godot-v4.5` and checked there again.
- **Notes:** Defects found in review and fixed on the contributor's branches: an unbounded allocation in the Variant decoder (a 12-byte packet reserved 64 GB), a panic on an oversized wait, diagnostics that never matched on Windows (verbatim path prefix and percent-encoded drive letter in the published URI), documents left open in the language server, hand edits to an open scene lost on reload, a game tool sent before the runtime had registered after editor Play, authoring tools that could replace a resource, connect a signal to a missing method, or put lines into `project.godot`, clicks that ignored the canvas transform, and a pixel diff between captures of different sizes.
- **Changes:** Merged as #2, #3 (chore), #4, #5, #6, #9, #11, #7, #8 (documentation and the README tool table) and #13; no release has been cut. Follow-ups from the review were then done (pull requests 14 to 17): the runtime copy moved to a private folder under `.godot/` and the project's addons folder is never written; a screenshot waits for an undrawn window; language-server starts are serialized and a dead client is replaced; the language server's private editor opens no bridge. The debugger port that accepts the first local connection stays as an accepted risk, with its reasoning in the game runtime specification.

### [T-6S01] Specifications

- **Spec:** the five above, plus amendments to l1-tool-catalog.md 1.2.0, l2-tool-handlers.md 1.2.0, l2-godot-editor-addon.md 1.2.0, l2-test-suite.md 1.4.0
- **Status:** Done
- **Assignment:** Agent
- **Verify:** Every tool in the catalog table exists in `tools/list`; every file named in a Canonical References table exists.
- **Notes:** Written after the code, from the merged tree and the review findings; the concepts that were Draft (runtime observability, content authoring) are Stable, with the one open question left as a marker.
