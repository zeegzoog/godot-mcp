# Game Runtime Channel

**Version:** 1.4.0
**Status:** Stable
**Layer:** implementation
**Implements:** l1-runtime-observability.md

## Overview

How the Rust server and the GDScript addon reach a running game: managed process runs and their debugger session, the temporary in-game component, the editor Play relay, the tools built on them, and the limits that keep them safe.

## Related Specifications

- [l1-runtime-observability.md](l1-runtime-observability.md) - Parent concept.
- [l2-tool-handlers.md](l2-tool-handlers.md) - How the tools are routed.
- [l2-godot-editor-addon.md](l2-godot-editor-addon.md) - The addon this channel adds files to.
- [l1-tool-catalog.md](l1-tool-catalog.md) - TL-6: a managed process run is the one launch the server owns.
- [l2-held-stepping.md](l2-held-stepping.md) - The held mode of the physics step, built on this channel.

## 1. Motivation

Godot already has a debugger protocol that every editor-launched game speaks. Speaking it from the server gives logs, stack traces, breakpoints and stepping without a debugger of our own; a small component inside the game adds what the protocol does not carry: the live tree, input and pictures.

## 2. Constraints & Assumptions

- Godot connects out to the address in `--remote-debug`, so the server is the listener.
- Messages between server and game use the debugger's own framing: a little-endian length and one Variant. The runtime's own requests ride in `mcp:request` / `mcp:reply` messages as JSON text.
- The managed game's output is read by the server (piped) and kept in a bounded log; it never reaches the server's own standard streams.

## 4. Invariant Compliance

| L1 Invariant | Implementation |
| --- | --- |
| RO-1 Observation does not change the game | Read operations (`game.tree`, `game.get`, `game.signals`, `game.snapshot`, `game.screenshot`, log reads) mutate nothing; `game.observe` connects to signals for a few frames and disconnects again. `game.set`, `game.call`, input and the time scale are separate calls. |
| RO-2 Bounded output | `Run` keeps at most `MAX_LOGS` (2000) entries, oldest first dropped; errors are kept with their stack triples; reads page by sequence number (`since`) and `level`. |
| RO-3 Hints and reasons | `ready_editor_session` and `select` fail with how to start a game; a closed session reports `the game disconnected` or `the game exited before the runtime said hello`. `Run::record_end` stores `ended_by` (`game` or `server`), `ended_at` (seconds since the Unix epoch) and `exit_code` once, the first writer winning: the watcher for a game that quit by itself, `kill` for a stop (a process ended by a signal has -1). `summary()` reports the three, `GameHub::stop_all` stops every live run and returns their summaries. [MODIFIED] |
| RO-4 The component never ships | `ensure_runtime` uses the project's own addon when it has every runtime file; otherwise it writes a private copy under `.godot/godot_mcp/run-<id>/` (not scanned by the editor, ignored by version control) and never touches the project's `addons` folder; copies older than a day (a crashed run's) are swept. `inject_override` writes `res://override.cfg` with a marker line and `restore_override` puts the user's file back and removes the copy and the folders it emptied, on exit, on drop and, after a crash, on the next run. The runtime does nothing unless `EngineDebugger.is_active()`. `export_plugin.gd` excludes the whole addon folder from exports. |
| RO-5 Local access | `DebugSession::listen` binds `127.0.0.1` on a free port, accepts one connection and stops listening. |
| RO-6 No caller code | `game_call` requires `permit` to name the method and rejects, by name, `quit`, `set_script`, `call`, `callv`, `call_deferred`, `call_group`, `call_group_flags`, `propagate_call`, `rpc`, `rpc_id`, `set`, `set_deferred`, `set_indexed`, `emit_signal`, `notification`, `propagate_notification`, `free`, `queue_free` and `replace_by`; changing the tree by adding, removing or reparenting a node stays allowed, because it changes only the running game (RO-1); the runtime repeats that list (`rt_live.gd`), and one test per name keeps the two lists equal and the list effective. Before the call the runtime reads the method's declared argument types and converts a JSON string to `StringName` or `NodePath` where the method asks for it; any other mismatch stays `BAD_PARAMS`; [MODIFIED] `game.set` refuses `script`, names starting with `_` and property paths (`:`); the runtime has no evaluator. |
| RO-7 Held input released | `rt_input.gd` remembers every press; `input.release_all`, a failing step and `_exit_tree` release them all. |
| RO-8 Real pictures | `screenshot` checks the PNG magic and the IHDR size before returning an image; a headless game returns an error; the runtime compares a capture with the previous one only when both have the same size. |
| RO-9 Bounded waits | A runtime call has 20 s on the server; a physics step waits in time (2 s plus four times the ticks' own duration, at most 15 s); debugger waits are capped at `MAX_WAIT_SECONDS` (300 s); frame waits are capped at 600 frames, a swipe at 60, a sheet at 8 frames; a wait for a node or signal has a frame timeout and reports when it ran out. |
| RO-10 Debugging only where the server is the debugger | `debug_ctl.rs` talks only to a `Run`'s own session; editor Play is reached through `game_relay.gd`, which forwards `mcp:*` messages and leaves every other prefix to the editor's debugger. |
| RO-11 Delivered or warned | Pointer events go through `Input.parse_input_event` as for every step. `rt_input.gd` `_done` adds `warning` to the result of a `mouse_button`, `mouse_move`, `touch` or `swipe` step (a `click` and a `click_node` are mouse buttons by then) whose point, or either end of a swipe, lies outside `root.get_visible_rect()`; the text names the point and the viewport size and, for a game without a window, the 64 × 64 fact. A step that cannot be delivered answers `UNSUPPORTED`. [MODIFIED] |

## 5. Detailed Design

### 5.1 Files

```plaintext
src/
├── godot/
│   ├── runs.rs           GameHub, Run: start, stop, status, logs, override and runtime files
│   ├── debug_session.rs  loopback listener, framing, message queue
│   ├── debug_ctl.rs      breakpoints, pause, continue, step, stack, frame variables
│   └── variant.rs        encode and decode of Godot's binary Variant (bounded)
└── tools/game.rs         catalog and handlers of the godot_run_output, godot_game_* and godot_debug_* tools
addons/godot_mcp/
├── core/runtime_override.gd   override.cfg for editor Play, restored afterwards
├── editor/game_relay.gd       EditorDebuggerPlugin relaying mcp:* messages
└── runtime/
    ├── mcp_runtime.gd         autoload: registers the "mcp" capture, sends hello, dispatches requests
    └── ops/rt_*.gd            result helpers, live tree and properties, input and gestures, pictures, output
```

### 5.2 Managed Process Run

1. `start` picks the runtime source (the addon, or a private copy under `.godot/`), writes the override, binds the debugger listener, and spawns Godot with `--path`, `--remote-debug tcp://127.0.0.1:<port>`, optional `--scene` and `--breakpoints`, in its own process group, with the server's stdin closed and output piped.
2. The game connects; the runtime sends `mcp:hello` when its main scene is ready; calls wait for it.
3. A watcher task records the exit code; `stop`, a drop of the hub or the end of the process restores the override and removes the private copy.
4. At most eight runs are kept; finished ones are dropped first.

### 5.3 Editor Play

`plugin.gd` writes the override through `runtime_override.gd` before Play unless the project setting `godot_mcp/runtime_on_play` is `false`, and `game_relay.gd` carries `mcp:*` messages between the played game and the bridge. A game tool called right after Play waits for the newest session to say hello before sending anything (`ready_editor_session`), so input is never sent to a game that has not registered yet.

### 5.4 Runtime Operations

| Method | Tools |
| --- | --- |
| `game.tree`, `game.get`, `game.set`, `game.signals`, `game.observe`, `game.snapshot` | `godot_game_tree`, `godot_game_get`, `godot_game_set`, `godot_game_signals`, `godot_game_observe`, `godot_game_snapshot` |
| `input.run`, `input.release_all`, `input.wait` | `godot_game_input`, `godot_game_input_release`, `godot_game_wait` |
| `game.physics` | `godot_game_physics` |
| `game.screenshot` | `godot_game_screenshot` |
| output and errors | `godot_run_output` |
| (debugger commands, not runtime methods) | `godot_debug_breakpoints`, `godot_debug_control`, `godot_debug_state` |

Gestures (`click`, `click_node`, `swipe`) go through `Input.parse_input_event` like every other step; mouse steps leave the operating system's cursor alone unless `warp_mouse` is set. A wait can also read a public property (`property`, `op`, `value`) or wait for a newer `scene_generation`: the read is a plain `get` on public names, never an expression or a method call, with strict types. The runtime stamps every scene instance with a generation, an instance id and the SHA-256 of the root script source Godot had loaded at that moment, and copies the stamp onto each log line when it is written. A screenshot can save the returned image as one of eight named baselines (each at most 512 x 512 pixels) and compare a later one with it, over a region and with a tolerance. `godot_game_physics` lets exactly N physics iterations run with the game's normal physics delta: it allows one iteration per frame while it runs, counts them and pauses the tree after the Nth; the tick rate and the time scale are never changed.

A scene stamp carries the long explanation of `script_sha256` once per answer, not once per stamped log line, and a baseline comparison without `max_dimension` is captured at the baseline's own size, so a baseline saved at 256 pixels can be compared without repeating the option. [MODIFIED]

A click on a node uses the node's transform with its canvas, so a scrolling camera or a CanvasLayer does not misplace it. The time scale set by a step or a wait is restored when the game exits.

## 6. Implementation Notes

- Accepted risk: the debugger port accepts the first local connection without authentication. The port is random, bound to loopback and closed to further connections once the game connects; a web page cannot use it (a browser's request is not a Variant frame and is refused as oversized). Another process of the same user could connect first, but such a process can already read the bridge token and the project. Another user on a shared machine could, in theory, connect first and feed the agent false state; checking the peer's process would need a different mechanism on each operating system, and the game's own handshake carries nothing to check, so this is left as it is.
- The runtime uses only the engine API of the line's first release, and the editor tests run it on every line's Godot build.

## 7. Drawbacks & Alternatives

- **Screenshots through the editor's viewport**: cannot show a running game; kept as `godot_capture_viewport` for the editor.
- **Reading the game's stdout only**: no stack traces, no breakpoints, no state.

## Canonical References

| Alias | Path | Purpose |
| --- | --- | --- |
| `[RUNS]` | `src/godot/runs.rs` | Managed runs |
| `[SESSION]` | `src/godot/debug_session.rs` | Debugger session |
| `[CTL]` | `src/godot/debug_ctl.rs` | Breakpoints and stepping |
| `[VARIANT]` | `src/godot/variant.rs` | Variant codec |
| `[GAME]` | `src/tools/game.rs` | Tools |
| `[RUNTIME]` | `addons/godot_mcp/runtime/` | In-game component |
| `[RELAY]` | `addons/godot_mcp/editor/game_relay.gd` | Editor Play relay |
| `[E2E]` | `tests/runtime_e2e_test.rs` | Process run tests |
| `[PLAY]` | `tests/editor_e2e_test.rs` | Editor Play tests |

## Document History

| Version | Date | Author | Description |
| --- | --- | --- | --- |
| 1.0.0 | 2026-10-21 | Agent (magic.spec) | Written after pull requests 3 to 5, 9 and 13 merged, with the review changes (bounded variant decoding, capped waits, Play wait for the runtime, canvas-aware clicks, same-size picture comparison). |
| 1.1.0 | 2026-10-21 | Agent (magic.spec) | [MODIFIED] The runtime copy moved out of the addons folder (RO-4, 5.2); the debugger port limitation recorded as an accepted risk with its reasoning; a capture retries for an undrawn window. |
| 1.2.0 | 2026-10-21 | Agent (magic.spec) | [ADDED] Property and scene-generation waits, scene stamps, baselines and the physics step (pull request 18); the physics step keeps the normal physics delta. |
| 1.2.1 | 2026-10-21 | Agent (magic.spec) | Linked the held-stepping specification built on this channel. No requirement changed. |
| 1.2.2 | 2026-10-21 | Agent (magic.spec) | Document History rows put in version order. No requirement changed. |
| 1.3.0 | 2026-10-21 | Agent (magic.spec) | [MODIFIED] RO-3, RO-6 (the extended refusal list with one test per name, argument type conversion) and the answer details; [ADDED] RO-11 (delivered or failed, with the headless pointer cause left open). Cancellation is documented, not required: a cancelled call aborts the server task only, and a held step already sent to the game finishes (at most 3600 ticks) and holds again. Source: simulation QA of 2026-10-21 (Q2, Q5, Q6, Q19, Q27, R28). Stable → RFC → Stable after Post-Update Review. |
| 1.4.0 | 2026-10-22 | Agent (magic.run) | [MODIFIED] RO-3 (the run record as built: `ended_by`, `ended_at`, `exit_code`, `stop_all`) and RO-11 (a viewport warning; the open cause is isolated and its TBD removed). Output answers carry the scene-hash note once, as 1.3.0 said. Stable → RFC → Stable after Post-Update Review. |
