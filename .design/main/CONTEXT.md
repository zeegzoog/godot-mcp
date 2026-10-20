# Project Context

**Generated:** 2026-10-20

## Active Technologies

- Rust

## Core Project Structure

```plaintext
.
├── .agents/
│   ├── rules/
│   └── workflows/
├── .design/
│   ├── .version
│   ├── INDEX.md
│   ├── RULES.md
│   ├── main/
│   └── workspace.json
├── .env.example
├── .gitattributes
├── .github/
│   ├── ISSUE_TEMPLATE/
│   ├── godot-version
│   ├── scripts/
│   └── workflows/
├── .gitignore
├── .magic/
├── .markdownlint.json
├── AGENTS.md
├── CHANGELOG.md
├── CONTRIBUTING.md
├── Cargo.lock
├── Cargo.toml
├── LICENSE
├── README.md
├── SECURITY.md
├── addons/
│   └── godot_mcp/
├── examples/
│   └── fake_gh.rs
├── project.godot
├── src/
│   ├── bridge.rs
│   ├── cli_runner.rs
│   ├── config.rs
│   ├── error.rs
│   ├── feedback.rs
│   ├── issues.rs
│   ├── journal.rs
│   ├── lib.rs
│   ├── main.rs
│   ├── paths.rs
│   ├── protocol.rs
│   ├── server.rs
│   ├── tools/
│   ├── tools.rs
│   ├── util.rs
│   └── validation.rs
└── tests/
    ├── addon_export_test.rs
    ├── bridge_client_test.rs
    ├── common/
    ├── detach_test.rs
    ├── editor_e2e_test.rs
    ├── feedback_test.rs
    ├── server_test.rs
    ├── stdio_protocol_test.rs
    ├── tools_test.rs
    └── version_sync_test.rs
```

## Recent Changes

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
