# Code Intelligence

**Version:** 1.1.0
**Status:** Stable
**Layer:** concept

## Overview

GDScript diagnostics, symbols, completion, definitions and references for an agent, answered by Godot's own language server, so what the agent hears about a script is what the editor would say.

## Related Specifications

- [l1-tool-catalog.md](l1-tool-catalog.md) - TL-4 and TL-5 apply to the script paths these tools take.
- [l1-mcp-server.md](l1-mcp-server.md) - MCP-1: nothing the language server prints may reach the protocol channel.
- [l2-lsp-client.md](l2-lsp-client.md) - Where this concept is realized.

## 1. Motivation

`godot_validate_script` runs a syntax check in a separate engine process: it knows nothing about the rest of the project, cannot see type errors, and cannot answer where a symbol is defined. The language server inside the editor can. Without it, an agent edits a script, runs the game, and only then learns of an error the editor had flagged the moment the file was saved.

## 2. Constraints & Assumptions

- Godot's server speaks the Language Server Protocol over TCP and is part of the editor, so a Godot executable is required; the server does not implement a language server of its own.
- Godot reads the length header strictly: a frame carries only `Content-Length`.
- Lines and columns are 1-based at the tool boundary, as in the editor.
- [DR] CI-7 compares projects through the editor bridge (its reported project directory), because the language server does not say which project it serves. Without a bridge answer nothing shows a mismatch and the server on the port is used as before. Rejected: refusing every server that cannot be proven right (a private editor start for every user whose plugin is off), and asking the language server for its root (not offered).

## 3. Core Invariants

- **CI-1** Answers come from Godot's language server. The tools never guess, cache a stale answer as current, or fall back to a pattern match.
- **CI-2** Silence is not success. A diagnostics request the server does not answer in time is an error; it never reports a clean file.
- **CI-3** Script paths are `res://` paths inside the project, checked like every other path the tools take.
- **CI-4** A language server started for the tools is the server's own child: it gets no handle to the server's standard streams, does not outlive the server, and is private to it.
- **CI-5** A query leaves no state behind in the language server: a document opened for a question is closed again, so the same script can be asked about repeatedly.
- **CI-6** Messages are bounded in size; an oversized frame is refused.
- **CI-7** The answer is about the project that was asked about. A language server that an editor already runs is not used when the bridge shows that editor has another project open; the tools start their own for the right project, and a timeout names the project. [ADDED]

## 7. Drawbacks & Alternatives

- **Parsing GDScript in Rust**: would work without an engine but would drift from the engine's own rules on every release.
- **Rename and hover** are not offered yet; they need workspace edits the tools have no safe way to apply.

## Canonical References

| Alias | Path | Purpose |
| --- | --- | --- |
| `[CODE]` | `src/tools/code.rs` | Tool catalog and handlers |
| `[LSP]` | `src/lsp/` | Client and framing |
| `[E2E]` | `tests/runtime_e2e_test.rs` | `language_server_*` tests on a real script |

## Document History

| Version | Date | Author | Description |
| --- | --- | --- | --- |
| 1.0.0 | 2026-10-21 | Agent (magic.spec) | Specified after pull request 6 merged (diagnostics, symbols, completion, definition, references). |
| 1.1.0 | 2026-10-21 | Agent (magic.spec) | [ADDED] CI-7 (a language server that the bridge shows to serve another project is not used). Source: simulation QA of 2026-10-21 (Q9); impossible positions are covered by TL-1 (Q22). Stable → RFC → Stable after Post-Update Review. |
