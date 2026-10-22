# Project Context

**Generated:** 2026-10-22

## Active Technologies

- Rust

## Core Project Structure

```plaintext
.
├── .agents/
│   ├── rules/
│   ├── skills/
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
│   ├── PULL_REQUEST_TEMPLATE.md
│   ├── dependabot.yml
│   ├── godot-version
│   ├── lines
│   ├── scripts/
│   ├── steward-priorities.md
│   ├── steward.yml
│   └── workflows/
├── .gitignore
├── .magic/
├── .markdownlint-cli2.jsonc
├── .vscode/
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
├── docs/
│   └── feature-parity.md
├── examples/
│   └── fake_gh.rs
├── project.godot
├── src/
│   ├── bridge.rs
│   ├── cli_runner.rs
│   ├── config.rs
│   ├── error.rs
│   ├── feedback.rs
│   ├── godot/
│   ├── issues.rs
│   ├── journal.rs
│   ├── lib.rs
│   ├── lsp/
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
    ├── runtime_e2e_test.rs
    ├── server_test.rs
    ├── stdio_protocol_test.rs
    ├── steward_test.rs
    ├── tools_test.rs
    └── version_sync_test.rs
```

## Recent Changes

- The portable repo-steward skill: scripts for context, profile, labels, readiness, board and the one way to write to GitHub, instructions and references for triage, replies, pull requests, changes, board, readiness and setup, and templates.
- Installed here with its profile, priorities, workflows (readiness, stale reminders, an agent run that stays off until switched on), issue forms and pull request template; contributor and agent guides describe it; `tests/steward_test.rs` guards the contracts.
- Labels and a public board with fields, views and policies; a release-readiness notice kept current by a workflow.
- Attended acting runs reviewed the contributor series and merged it with the review fixes, carried each change to the release lines, and made the 0.6.0 release at the maintainer's request; a later run posted the released message on the fixed issue and proved that a second pass posts nothing.
- A ruleset now protects `master`; head branches are deleted after merge; first-time contributors need approval for workflow runs.
- Found along the way: a native Windows `gh` cannot open a POSIX temporary path, so the action script translates it; GitHub reserves the field name Type, so the board field is Kind; the token needs the project scope for board steps, and a run without it skips them and says so.

### Phase 8 — 2026-10-21

- Feasibility on a real engine first (Godot 4.8-beta1): the script editor exists without a window, unsaved typing is the code editor's version differing from its saved version, a fragment replacement is one undo step, and saving gives a running tool script the new code. Found false: no call names an editor's resource, so editors are paired with paths through the two lists the script editor offers.
- `godot_read_script`: the text the editor would save (unsaved typing included), where it came from, whether it is unsaved, and a version label (64-bit FNV-1a, the same in the addon and the host). `godot_edit_script`: one fragment that must occur exactly once, an optional version check, one undoable editor action that saves, refused while the editor holds unsaved changes to the script. File fallback through an exclusive temporary file; line endings and byte order mark kept.
- Specifications: Content Authoring 1.1.0 (CA-7 to CA-10), Godot Editor Addon 1.4.2, Tool Handlers 1.3.2, Tool Catalog 1.4.0.
- Found by carrying: Godot 4.5.2, 4.6.3 and 4.7.2 give script tabs no `_tab_name` metadata; the pairing check now applies only where it exists. Found by the security review: the edit's temporary file is now created exclusively under an unpredictable name and a script that is a symbolic link is refused.
- Carried to `godot-v4.7`, `godot-v4.6` and `godot-v4.5` as three commits each, verified with each line's pinned build.
