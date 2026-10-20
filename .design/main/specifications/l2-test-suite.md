# Test Suite

**Version:** 1.3.0
**Status:** Stable
**Layer:** implementation
**Implements:** l1-quality-assurance.md

## Overview

The Cargo test suite and the contributor verification checklist: which test file covers which layer, the shared helpers (stdio session driver, fake editor bridge, stand-in engine executables), the opt-in real-editor tests, and how they are run locally and in CI.

## Related Specifications

- [l1-quality-assurance.md](l1-quality-assurance.md) - Parent concept.
- [l2-ci-release-pipeline.md](l2-ci-release-pipeline.md) - Where the suite runs automatically.
- [l2-release-branches.md](l2-release-branches.md) - The line identity files the version sync test checks.

## 1. Motivation

Tells a contributor where a new test belongs and which environment variables the opt-in tests need, so new behaviour lands with a test in the right file.

## 2. Constraints & Assumptions

- Opt-in tests are marked ignored and read `GODOT_PATH` (and, on Windows, `GODOT_CONSOLE_PATH`, whose console binary is used to start the editor so its log can be shown on failure). A `.env` file fills in only variables that were not exported, so testing a specific Godot version means exporting both.
- A `.env` with double-quoted Windows paths does not load; export the variables instead.
- Editor end-to-end tests run with `--test-threads=2` in CI.

## 4. Invariant Compliance

| L1 Invariant | Implementation |
| --- | --- |
| QA-1 Test first | Contributor guide requires a failing test in the matching file before implementation; the 2026-10-14 remediation followed red → green → mutation check. |
| QA-2 Full pipeline before commit | Contributor checklist: `cargo fmt --all -- --check`, `cargo clippy --all-targets --all-features -- -D warnings`, `cargo test --all-targets`, `godot --headless --check-only` for each addon script (after one `--import`). |
| QA-3 Editor changes on a real editor | [MODIFIED] `editor_e2e_test` and `addon_export_test` (ignored by default) are mandatory when the addon changes, on every branch the change lands on, with the Godot build named in that branch's `.github/godot-version`. |
| QA-4 Real dependencies opt-in | `tests/common/mod.rs` provides `MockBridge` (scriptable WebSocket editor), `fake_godot` (prints to stdout/stderr), `sleeping_godot` (stays alive), `project_dir`, `free_port`, `fake_gh` / `FakeGh` (a scripted stand-in for the GitHub client, built from `examples/fake_gh.rs`), and a `Session` that drives the real server binary over stdio. |
| QA-5 Guards proven | Guard tests: loopback + token (`bridge_is_loopback_only_and_requires_the_token`), stdout purity (`cli_fallback_keeps_stdout_clean`), path confinement and injection (`tools_test.rs`, `paths.rs` unit tests incl. a Unix symlink test), detachment (`detach_test.rs`, process-group unit test), undo fidelity and scene confinement (e2e). |
| QA-6 Every platform, every push | [MODIFIED] `ci.yml` runs on pushes to `master` and `godot-v*` and on pull requests: matrix on ubuntu-24.04, windows-latest, macos-latest plus a Linux Godot job with the build from `.github/godot-version`. |
| QA-7 Version-portable editor code | [MODIFIED] Covered by running the e2e job on each line's Godot build; the addon guards APIs missing from the line's M.N.0 with `has_method()` (the guards per line are listed in [l2-release-branches.md](l2-release-branches.md) §5.3). |

## 5. Detailed Design

### 5.1 Files

```plaintext
tests/
├── common/mod.rs              Session, MockBridge, fake engines, temp projects
├── stdio_protocol_test.rs     protocol, concurrency, cancellation, config, stdout purity
├── tools_test.rs              host-side tools: create/validate script, project resolution, launch/run
├── feedback_test.rs           agent feedback channel: notes, drafts, scrubbing, consent gate, stand-in gh submission
├── bridge_client_test.rs      multiplexing, reconnection, timeout, error classes
├── detach_test.rs             launched processes do not hold the server's stderr
├── server_test.rs             frame and result helpers, catalog shape
├── version_sync_test.rs       server version == addon version; line identity files agree
├── steward_test.rs            repo-steward: readiness on a fixture repository, workflow least privilege
├── editor_e2e_test.rs         (ignored) real headless editor: security, scene/property tools, undo, resave
└── addon_export_test.rs       (ignored) export policy unit check + real pack export without the addon
examples/
└── fake_gh.rs                 stand-in for the GitHub client; not part of any release artifact
```

Unit tests live next to the code in `src/paths.rs`, `src/validation.rs`, `src/cli_runner.rs`, `src/config.rs`, `src/feedback.rs` and `src/issues.rs`.

### 5.2 Where a New Test Goes

| Behaviour | File |
| --- | --- |
| JSON-RPC framing, session, configuration | `tests/stdio_protocol_test.rs` |
| A tool's host-side logic | `tests/tools_test.rs` |
| Agent feedback channel (notes, drafts, scrubbing, submission) | `tests/feedback_test.rs` |
| Bridge client behaviour | `tests/bridge_client_test.rs` |
| Editor command behaviour | `tests/editor_e2e_test.rs` |
| Export exclusion | `tests/addon_export_test.rs` |
| Process independence | `tests/detach_test.rs` |
| Release line identity (version string, minimum engine version, engine build) | `tests/version_sync_test.rs` |
| Repo steward scripts and workflows (readiness, least privilege, tracked skill) [ADDED] | `tests/steward_test.rs` |

## 6. Implementation Notes

- Not covered locally on the maintainer's Windows host: Linux and macOS (CI covers them), the bridge accept-backlog ceiling (about 64–96 simultaneous MCP processes, an engine limit), and token-file permissions on a multi-user Linux host.

## Canonical References

| Alias | Path | Purpose |
| --- | --- | --- |
| `[HELPERS]` | `tests/common/mod.rs` | Shared test infrastructure |
| `[PROTO]` | `tests/stdio_protocol_test.rs` | Protocol tests |
| `[TOOLS]` | `tests/tools_test.rs` | Tool tests |
| `[FEEDBACK]` | `tests/feedback_test.rs` | Agent feedback channel tests |
| `[CLIENT]` | `tests/bridge_client_test.rs` | Bridge client tests |
| `[DETACH]` | `tests/detach_test.rs` | Process detachment |
| `[FRAMES]` | `tests/server_test.rs` | Frame helper tests |
| `[VERSION]` | `tests/version_sync_test.rs` | Version and line identity sync |
| `[STEWARD]` | `tests/steward_test.rs` | Repo steward readiness and workflow guards |
| `[E2E]` | `tests/editor_e2e_test.rs` | Real-editor tests |
| `[EXPORT]` | `tests/addon_export_test.rs` | Export tests |
| `[GUIDE]` | `AGENTS.md` | Verification checklist |

## Document History

| Version | Date | Author | Description |
| --- | --- | --- | --- |
| 1.0.0 | 2026-10-16 | Agent (magic.analyze first-time) | Reverse-engineered from `tests/` and the contributor guide at 94e4540; uncovered areas carried over from the 2026-10-14 QA report. |
| 1.1.0 | 2026-10-16 | Agent (magic.spec) | [ADDED] The feedback channel's test file, its unit-test locations, routing row and reference. |
| 1.1.1 | 2026-10-16 | Agent (magic.run) | [ADDED] The stand-in GitHub client helper and its example target. |
| 1.2.0 | 2026-10-20 | Agent (magic.spec) | [MODIFIED] QA-3, QA-6, QA-7 rows per release line; [ADDED] line identity checks in the version sync test and its routing row. Stable → RFC → Stable after Post-Update Review. |
| 1.3.0 | 2026-10-20 | Agent (magic.spec) | [ADDED] The repo steward's test file, its routing row and reference. Stable → RFC → Stable after Post-Update Review. |
