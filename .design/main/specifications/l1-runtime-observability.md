# Runtime Observability

**Version:** 1.2.0
**Status:** Stable
**Layer:** concept

## Overview

How an agent observes and drives a running game, not only the editor: the game's log output and error stack traces, its live node tree and properties, input injection, screenshots, and, for a game the server started itself, breakpoints and stepping. The tools reach the game through Godot's own remote debugger and a small runtime component the game loads only while a debugger is attached.

## Related Specifications

- [l1-tool-catalog.md](l1-tool-catalog.md) - The runtime tools join the catalog under its invariants.
- [l1-mcp-server.md](l1-mcp-server.md) - MCP-1: captured game output must never reach the protocol channel.
- [l1-editor-bridge.md](l1-editor-bridge.md) - Loopback-only access applies to the game channel too.
- [l2-game-runtime.md](l2-game-runtime.md) - Where this concept is realized.
- [l1-controlled-time.md](l1-controlled-time.md) - Holding a running game still and stepping it by exact ticks; extends this concept.

## 1. Motivation

After `godot_run_project` an agent could tell whether a scene plays, but not why it misbehaves: script errors, warnings and `print()` output were invisible, the editor viewport shows the scene as authored rather than as it runs, and nothing could press a key or click. Observation and control of the running game close that loop: run, look, act, look again.

## 2. Constraints & Assumptions

- Two kinds of run exist. A **managed process run** is a game this server starts with the remote debugger pointed at itself; it can be observed, driven and debugged. **Editor Play** is started by the editor; it can be observed and driven through a relay in the plugin, but breakpoints and stepping stay with the editor's own debugger.
- The runtime component is a script loaded as an autoload through a temporary `override.cfg`, never through `project.godot`. It does nothing unless a debugger is attached, so a stale override cannot affect a normal or exported run.
- Game output captured by the server goes to a bounded buffer, never to the server's standard output.
- A game without a window (headless) has nothing to capture as an image.
- [DR] A pointer step outside the viewport warns and does not fail: `_input` and `_unhandled_input` still receive the event, and a game may rely on that at any coordinate. Measured on Godot 4.8-beta1: the headless window is 64 × 64 and ignores the project's size; a button inside it is pressed, one outside it receives the gui events and never emits `pressed`. Rejected: failing such a step (breaks games that read the event position), and enlarging the headless window (not a setting the engine offers).
- [DR] RO-6 is a refusal of classes of effect, not an allowlist of safe methods. Rejected: allowing only methods the node's own script defines (the safest guard, but it forbids engine methods such as playing an animation or asking for a group, which the permit list exists to allow). Revisit if gaps in the refusal keep turning up. Override: `/magic.spec amend l1-runtime-observability.md "allowlist"`.
- The console wrapper setting (`GODOT_CONSOLE_PATH`) is not used by the server; log capture comes from the debugger channel.

## 3. Core Invariants

- **RO-1** Observation does not change the game. Reading the tree, properties, logs and pictures has no side effect; everything that does change a running game (setting a property, calling a method, injecting input, setting the time scale) is an explicit tool call, and nothing it changes is saved to disk.
- **RO-2** Captured output is bounded in memory and readable on demand; the oldest entries are dropped first.
- **RO-3** Runtime tools follow the catalog's live-first and error-hint rules: with no game running they say so and how to start one, and a game that disconnected or exited fails the next call with that reason. A run that has ended keeps its exit code and the time it stopped, and says whether the server or the game ended it. [MODIFIED]
- **RO-4** The runtime component never ships. It is loaded only while a debugger is attached, through a temporary override that is removed when the run ends (also after a crash, on the next run), and the addon is excluded from exports.
- **RO-5** Access to a game is local. The debugger listener binds to loopback, accepts one connection and then stops listening.
- **RO-6** No code the caller wrote runs in the game, and a call cannot take the game apart. A method call needs the method named in an explicit permit list, and is refused for any method that can quit, evaluate code, call another method by name (directly, on a group, or through a signal or a notification), set a property or the script through a name argument, or free or replace a node. Property writes refuse `script` and private names; there is no expression evaluator. The permit list says what the caller wants to call; the refusal does not depend on it. A permitted call is usable: its arguments are converted to the types the method declares (a JSON string becomes a `StringName` or a `NodePath` where the method asks for one). [MODIFIED]
- **RO-7** Input the server holds is released. A failed step, an explicit release, and the end of the game each clear every key, button, axis and touch that was pressed.
- **RO-8** A picture is a real picture: a capture returns a validated PNG or an error, never a placeholder or an empty image. A comparison between captures of different sizes says it was not made.
- **RO-9** Waits are bounded. Every wait for frames, a node, a signal or a stop has a limit and reports when it ran out.
- **RO-10** Breakpoints and stepping work only on a game the server started, where the server is the debugger; the plugin relay carries only the server's own messages and leaves every other debugger message to the editor.
- **RO-11** An input step is delivered or says what it could not do, never silently skipped. A step answers `ok` when the game received the event. A pointer step that lands outside the game's viewport carries a warning that controls there do not react to the pointer: a game without a window has a viewport of 64 × 64 pixels whatever the project sets, so most of a screen lies outside it. A step that cannot be delivered at all fails with the reason. [MODIFIED]

## 7. Drawbacks & Alternatives

- Parsing the editor's Output panel: fragile and editor-version dependent.
- Implementing a debugger of our own: the server is a client of Godot's protocol instead, so a new engine version changes little.
- A runtime component that stays in the project: richer access at any time, but a second artifact to ship and keep out of exports; the temporary autoload avoids that.

## Canonical References

| Alias | Path | Purpose |
| --- | --- | --- |
| `[GAME]` | `src/tools/game.rs` | Tool catalog and handlers |
| `[RUNS]` | `src/godot/runs.rs` | Managed process runs |
| `[RUNTIME]` | `addons/godot_mcp/runtime/mcp_runtime.gd` | In-game component |
| `[E2E]` | `tests/runtime_e2e_test.rs` | Real-game tests |

## Document History

| Version | Date | Author | Description |
| --- | --- | --- | --- |
| 0.1.0 | 2026-10-16 | Agent (magic.analyze first-time) | Draft collected from the README roadmap, the agent guide's "not implemented" note and the reserved console-wrapper setting. Ownership left open. |
| 1.0.0 | 2026-10-21 | Agent (magic.spec) | Implemented by pull requests 3 to 5, 9 and 13: ownership resolved (the server as debugger client plus a temporary in-game component, a plugin relay for editor Play). Scope widened from logs and a camera image to observation and control. Invariants rewritten. Draft → Stable. |
| 1.0.1 | 2026-10-21 | Agent (magic.spec) | Linked the controlled-time concept that extends this one. No requirement changed. |
| 1.1.0 | 2026-10-21 | Agent (magic.spec) | [MODIFIED] RO-3 (an ended run keeps its exit code and stop time) and RO-6 (the refusal covers freeing and replacing nodes, name-based property writes and indirect calls through groups, signals and notifications, which a live test showed were permitted; permitted calls convert argument types); [ADDED] RO-11 (an input step is delivered or failed). A rule that a cancelled call stops its work in the game was considered and dropped: a held step is bounded (at most 3600 ticks, under a second) and nothing worse than a few extra ticks was seen; the cancellation behaviour is documented in the implementation instead. Source: simulation QA of 2026-10-21 (Q2, Q5, Q6, Q19). Stable → RFC → Stable after Post-Update Review. |
| 1.2.0 | 2026-10-22 | Agent (magic.run) | [MODIFIED] RO-11: a pointer step outside the viewport answers ok with a warning instead of failing; the cause that the plan left open was isolated (the headless window is 64 × 64). Stable → RFC → Stable after Post-Update Review. |
