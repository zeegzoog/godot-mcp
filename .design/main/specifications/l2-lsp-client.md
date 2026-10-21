# GDScript Language Server Client

**Version:** 1.1.0
**Status:** Stable
**Layer:** implementation
**Implements:** l1-code-intelligence.md

## Overview

The Rust client that asks Godot's GDScript language server for diagnostics, symbols, completion, definitions and references: how it finds or starts a server, how frames are written and read, how a document URI is matched with what the server publishes, and how long it waits.

## Related Specifications

- [l1-code-intelligence.md](l1-code-intelligence.md) - Parent concept.
- [l2-tool-handlers.md](l2-tool-handlers.md) - How tool calls reach these functions.

## 1. Motivation

Godot's language server has two quirks that decide the design: it rejects a `Content-Type` header, and on Windows it publishes URIs (`file:///C%3A/My%20Game/a.gd`) that do not look like the ones a client builds from a canonical path. A client that ignores either never gets an answer.

## 2. Constraints & Assumptions

- Godot executable known to the server (`GODOT_PATH`); the language server lives in an editor.
- `GODOT_MCP_LSP_PORT` (default `6005`) names the port of an editor that may already be running with its server.

## 4. Invariant Compliance

| L1 Invariant | Implementation |
| --- | --- |
| CI-1 Godot's answers | `LspHub` forwards the server's replies and published diagnostics; the only transformation is shifting 0-based positions to 1-based (`shift_diagnostics`). |
| CI-2 Silence is not success | `diagnostics` clears the stored list, opens the document, and polls up to 8 s for the published list; no answer is a timeout error, not an empty result. |
| CI-3 Project paths | `tools/code.rs` resolves `script_path` through `resolve_in_project` before the client sees it; line and column must fit in 32 bits (`position_arg`). |
| CI-4 Private child | `connect_or_start` tries the configured port first (400 ms). If nothing answers it starts the command of `private_editor_command`: `godot --headless --editor --lsp-port <port> --path <project>` with null standard streams, `kill_on_drop` and `GODOT_MCP_NO_BRIDGE=1` (the addon, if the project has it enabled, then opens no bridge in that editor), waits up to 60 s for the port, and stops the editor when the hub is dropped. Starting is serialized (`starting`), so concurrent first calls share one editor; a client the server closed (`closed`) or whose writer is gone is replaced by the next call. |
| CI-5 No state left behind | After symbols, completion, definition and references the document is closed again (`close_quiet`); diagnostics already did. `language_server_answers_the_same_script_again` asks three times. |
| CI-6 Bounded frames | `framing.rs` refuses a header over 8 KiB and a body over 4 MiB. |

## 5. Detailed Design

### 5.1 Framing

A frame is `Content-Length: N`, a blank line, and the JSON body, nothing else. Reading accepts frames split across any number of reads.

### 5.2 Document URIs

The URI sent is built from the canonical path without the Windows verbatim prefix (`\\?\`), with every character other than letters, digits and `-._~/:` percent-encoded. Diagnostics are stored and looked up by a normalized key (`uri_key`): percent escapes decoded, a leading slash before a drive letter dropped, backslashes turned into slashes, and the case folded on Windows. Two spellings of one file therefore meet.

### 5.3 Timeouts

| Step | Limit |
| --- | --- |
| Editor start (port opens) | 60 s |
| `initialize` | 20 s |
| Diagnostics after opening | 8 s |
| Symbols, completion, definition, references | 15 s |

## 6. Implementation Notes


## Canonical References

| Alias | Path | Purpose |
| --- | --- | --- |
| `[CLIENT]` | `src/lsp/client.rs` | Hub, requests, URIs |
| `[FRAMING]` | `src/lsp/framing.rs` | Frame reader and writer |
| `[TOOLS]` | `src/tools/code.rs` | Catalog and argument checks |
| `[E2E]` | `tests/runtime_e2e_test.rs` | Real-script tests |

## Document History

| Version | Date | Author | Description |
| --- | --- | --- | --- |
| 1.1.0 | 2026-10-21 | Agent (magic.spec) | [MODIFIED] CI-4: one editor start at a time, dead clients replaced, the private editor opens no bridge; both known limitations removed. |
| 1.0.0 | 2026-10-21 | Agent (magic.spec) | Written after pull request 6 merged, including the Windows URI repair and the document close fixed in review. |
