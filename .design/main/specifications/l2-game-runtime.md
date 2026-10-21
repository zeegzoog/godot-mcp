# Game Runtime Channel

**Version:** 1.1.0
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
| RO-3 Hints and reasons | `ready_editor_session` and `select` fail with how to start a game; a closed session reports `the game disconnected` or `the game exited before the runtime said hello`. |
| RO-4 The component never ships | `ensure_runtime` uses the project's own addon when it has every runtime file; otherwise it writes a private copy under `.godot/godot_mcp/run-<id>/` (not scanned by the editor, ignored by version control) and never touches the project's `addons` folder; copies older than a day (a crashed run's) are swept. `inject_override` writes `res://override.cfg` with a marker line and `restore_override` puts the user's file back and removes the copy and the folders it emptied, on exit, on drop and, after a crash, on the next run. The runtime does nothing unless `EngineDebugger.is_active()`. `export_plugin.gd` excludes the whole addon folder from exports. |
| RO-5 Local access | `DebugSession::listen` binds `127.0.0.1` on a free port, accepts one connection and stops listening. |
| RO-6 No caller code | `game_call` requires `permit` to name the method and rejects `quit`, `set_script`, `call`, `callv`, `call_deferred`, `rpc`, `rpc_id` and `free`, and the runtime repeats that list (`rt_live.gd`); `game.set` refuses `script`, names starting with `_` and property paths (`:`); the runtime has no evaluator. |
| RO-7 Held input released | `rt_input.gd` remembers every press; `input.release_all`, a failing step and `_exit_tree` release them all. |
| RO-8 Real pictures | `screenshot` checks the PNG magic and the IHDR size before returning an image; a headless game returns an error; the runtime compares a capture with the previous one only when both have the same size. |
| RO-9 Bounded waits | A runtime call has 20 s on the server; debugger waits are capped at `MAX_WAIT_SECONDS` (300 s); frame waits are capped at 600 frames, a swipe at 60, a sheet at 8 frames; a wait for a node or signal has a frame timeout and reports when it ran out. |
| RO-10 Debugging only where the server is the debugger | `debug_ctl.rs` talks only to a `Run`'s own session; editor Play is reached through `game_relay.gd`, which forwards `mcp:*` messages and leaves every other prefix to the editor's debugger. |

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
| `game.screenshot` | `godot_game_screenshot` |
| output and errors | `godot_run_output` |
| (debugger commands, not runtime methods) | `godot_debug_breakpoints`, `godot_debug_control`, `godot_debug_state` |

Gestures (`click`, `click_node`, `swipe`) go through `Input.parse_input_event` like every other step; mouse steps leave the operating system's cursor alone unless `warp_mouse` is set. A click on a node uses the node's transform with its canvas, so a scrolling camera or a CanvasLayer does not misplace it. The time scale set by a step or a wait is restored when the game exits.

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
| 1.1.0 | 2026-10-21 | Agent (magic.spec) | [MODIFIED] The runtime copy moved out of the addons folder (RO-4, 5.2); the debugger port limitation recorded as an accepted risk with its reasoning; a capture retries for an undrawn window. |
| 1.0.0 | 2026-10-21 | Agent (magic.spec) | Written after pull requests 3 to 5, 9 and 13 merged, with the review changes (bounded variant decoding, capped waits, Play wait for the runtime, canvas-aware clicks, same-size picture comparison). |
