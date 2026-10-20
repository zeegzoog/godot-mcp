# Rust Stdio Server

**Version:** 1.2.0
**Status:** Stable
**Layer:** implementation
**Implements:** l1-mcp-server.md

## Overview

The Tokio-based realization of the MCP server core: process entry point, logging setup, configuration loading, the stdio session loop with concurrent tool tasks and cancellation, JSON-RPC/MCP frame types, and the crate's error taxonomy.

## Related Specifications

- [l1-mcp-server.md](l1-mcp-server.md) - Parent concept.
- [l2-tool-handlers.md](l2-tool-handlers.md) - The tool manager the session dispatches to.
- [l2-bridge-client-rust.md](l2-bridge-client-rust.md) - Constructed at start-up from the configured port.
- [l2-agent-feedback-rust.md](l2-agent-feedback-rust.md) - Feedback settings, client info capture and guidance in `initialize`, unknown tool requests recorded by the session loop. [ADDED]

## 1. Motivation

Records how the concept's guarantees map onto this crate so a change to the session loop, configuration or error types can be checked against them without re-deriving the design.

## 2. Constraints & Assumptions

- Rust edition 2024, minimum toolchain 1.85; async runtime Tokio (multi-threaded).
- Command-line parsing with `clap` (flags with `env` fallbacks); `.env` loading with `dotenvy`; diagnostics with `tracing` written to standard error, filter `godot_mcp=info,warn` unless `RUST_LOG` overrides it.
- Observed conventions: `unwrap()` / `expect()` appear only in tests; mutexes are locked through a helper that recovers from poisoning; no lock is held across an `await` on the editor.

## 4. Invariant Compliance

| L1 Invariant | Implementation |
| --- | --- |
| MCP-1 Pristine protocol channel | `main` installs a `tracing` subscriber writing to stderr before anything else; a single writer task owns stdout and writes one serialized frame plus newline per message; child processes are spawned with null stdio by the engine runner (see [l2-tool-handlers.md](l2-tool-handlers.md)). |
| MCP-2 One answer per request | Requests other than `tools/call` are answered inline; each `tools/call` task sends exactly one response on completion; notifications are never answered. |
| MCP-3 No head-of-line blocking | `tools/call` runs in a `JoinSet` task; the reader loop keeps reading and answering while tasks run. |
| MCP-4 Cancellation | In-flight tasks are registered in a map keyed by the JSON text of the request id (registration happens under the lock before the task can finish); `notifications/cancelled` removes the entry and aborts the task, so no response is sent. |
| MCP-5 Error contract | `parse_message` maps input to parse error (-32700), invalid request (-32600), method not found (-32601) and invalid params (-32602); tool failures are `ToolResult::error(message, hints)` with an "Actionable recovery suggestions" list and `isError: true`. |
| MCP-6 Version negotiation | `SUPPORTED_PROTOCOL_VERSIONS` (newest first); the requested version is echoed when present in the list, else the first entry. |
| MCP-7 Forgiving configuration | Path flags use an infallible parser (empty means unset), the port parser maps an empty value to 9333; `.env` parse errors are logged as a warning with the single-quote hint and otherwise ignored; the removed `--mcp-url` flag is rejected while a stale `GODOT_MCP_URL` variable is ignored. `GODOT_MCP_FEEDBACK` accepts `off`, `draft` and `full` in any case, and an unknown value logs a warning and selects `full`; `GODOT_MCP_GH_PATH` is parsed like the other path settings. [ADDED] |
| MCP-8 No crash paths | All fallible operations return `McpError`; `util::lock` recovers poisoned mutexes; serialization failures of a tool result become an internal-error tool result. |
| MCP-9 Graceful shutdown | On stdin EOF the loop awaits every remaining task, then drops the writer channel and waits for the writer to flush. |

## 5. Detailed Design

### 5.1 Module Map

```plaintext
src/
├── main.rs        entry: logging to stderr, Config::load, McpServer::run_stdio
├── lib.rs         module exports (integration tests link against the library)
├── server.rs      session loop, message classification, task registry, writer, initialize answer (with feedback guidance)
├── protocol.rs    JsonRpcResponse/Error, Tool, ToolResult, ToolContent (text, image)
├── config.rs      flags + env + .env, Godot binary auto-detection per OS, feedback mode and gh path
├── feedback.rs    scrubber, fingerprint, link builder of the agent feedback channel (see l2-agent-feedback-rust.md)
├── journal.rs     agent feedback journal and report builder (see l2-agent-feedback-rust.md)
├── issues.rs      GitHub client runner for feedback submission (see l2-agent-feedback-rust.md)
├── error.rs       McpError: BridgeUnavailable, Bridge, Process, Protocol, ToolExecution, Validation, Io, Json
└── util.rs        poison-tolerant mutex lock
```

### 5.2 Godot Binary Resolution

1. `GODOT_PATH` / `--godot-path` when set and not empty, always used as given: a path to a missing file is logged as a warning at start and fails the tools that run the engine, naming the path, so another installed engine version is never substituted silently. A bare command name is left to the operating system's `PATH` lookup.
2. When unset or empty — platform scan: Windows — `Godot*.exe` (not `*console*`) up to two levels under Program Files, Program Files (x86) and `%LOCALAPPDATA%\Programs`, lexicographically last wins; macOS — `/Applications/Godot.app` and `Godot_mono.app`; Linux — `/usr/bin/godot`, `/usr/local/bin/godot`, the system Flatpak export.
3. `godot` on `PATH`.

### 5.3 Error Taxonomy

| Variant | Raised when | Typical recovery hint |
| --- | --- | --- |
| `BridgeUnavailable` | Connect/handshake to the editor fails | Launch the editor, enable the plugin, check the port |
| `Bridge` | Connection closed or reply timed out | Editor busy or restarting; retry; check Output panel |
| `ToolExecution` | Editor replied with `status: error` | Depends on message (unauthorized, no active scene, not found) |
| `Process` | Engine binary failed, timed out or could not spawn | Check `GODOT_PATH` |
| `Validation` | Script check reported problems | Fix the reported lines |
| `Protocol`, `Io`, `Json` | Framing or I/O failures | — |

## 6. Implementation Notes

- `GODOT_CONSOLE_PATH` is parsed but intentionally unused by the server; it is documented as reserved for log capture (see [l1-runtime-observability.md](l1-runtime-observability.md)).

## 7. Drawbacks & Alternatives

- Migrating to the `rmcp` SDK remains the main alternative (see the parent's section 7).
- An unbounded response channel is used for simplicity; responses are small and produced at the rate of client requests, so back-pressure has not been needed.

## Canonical References

| Alias | Path | Purpose |
| --- | --- | --- |
| `[ENTRY]` | `src/main.rs` | Logging setup and start-up |
| `[LIB]` | `src/lib.rs` | Public module surface for tests |
| `[SESSION]` | `src/server.rs` | Session loop and cancellation |
| `[FRAMES]` | `src/protocol.rs` | Frame and result types |
| `[CONFIG]` | `src/config.rs` | Configuration and binary auto-detection |
| `[ERRORS]` | `src/error.rs` | Error taxonomy |
| `[UTIL]` | `src/util.rs` | Poison-tolerant lock |
| `[ENVEX]` | `.env.example` | Documented configuration template |
| `[MANIFEST]` | `Cargo.toml` | Dependencies and toolchain floor |

## Document History

| Version | Date | Author | Description |
| --- | --- | --- | --- |
| 1.0.0 | 2026-10-16 | Agent (magic.analyze first-time) | Reverse-engineered from the code at 94e4540. |
| 1.1.0 | 2026-10-16 | Agent (magic.spec) | [ADDED] Two modules and two settings of the feedback channel in the module map and the configuration row; link to its implementation. |
| 1.2.0 | 2026-10-20 | Agent (magic.spec) | [MODIFIED] A configured engine path is used as given even when the file is missing (was: silently replaced by the platform scan). Reason: with one release line per engine version, running a different installed version is a silent wrong result. |
