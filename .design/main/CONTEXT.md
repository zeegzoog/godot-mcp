# Project Context

**Generated:** 2026-11-02

## Active Technologies

- Rust

## Core Project Structure

```plaintext
.
├── .agents/
│   ├── rules/
│   ├── skills/
│   └── workflows/
├── .claude/
│   ├── commands/
│   ├── rules/
│   └── skills/
├── .design/
│   ├── .version
│   ├── INDEX.md
│   ├── RULES.md
│   ├── main/
│   └── workspace.json
├── .editorconfig
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
├── AGENTS.md
├── CHANGELOG.md
├── CLAUDE.md
├── CONTRIBUTING.md
├── Cargo.lock
├── Cargo.toml
├── LICENSE
├── README.md
├── SECURITY.md
├── addons/
│   └── godot_mcp/
├── docs/
│   ├── feature-parity.md
│   └── game-checks.md
├── examples/
│   └── fake_gh.rs
├── project.godot
├── src/
│   ├── bridge.rs
│   ├── bridge_port.rs
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
    ├── bridge-port-vectors.json
    ├── bridge_client_test.rs
    ├── common/
    ├── detach_test.rs
    ├── editor_e2e_test.rs
    ├── export_e2e_test.rs
    ├── feedback_test.rs
    ├── runtime_e2e_test.rs
    ├── scenario_e2e_test.rs
    ├── server_test.rs
    ├── stdio_protocol_test.rs
    ├── steward_test.rs
    ├── tools_test.rs
    └── version_sync_test.rs
```

## Recent Changes

### Phase 10 — 2026-11-02

- A project names one development-only adapter script in `godot_mcp/adapter`; the in-game component loads it with godot-dev-bridge's registration contract (so its adapters work unchanged), keeps what it registers and what it refused, and shows its status in the hello and in `godot_get_run_status`.
- `godot_game_state` reads the states the adapter registered and `godot_game_command` runs one of its commands by name, in a managed run and in editor Play, held or not (a held game defers what a command schedules to the next tick); answers are bounded and each failure class has its own recovery steps.
- Failures in the project's own code are reported instead of hidden: the output logger marks script errors of a called handler, and a state that breaks fails alone.
- Feasibility on a real engine first (Godot 4.8-beta1): the logger sees a handler's script error at once, so no specification amendment was needed.
- Specifications: Project Adapter and Project Adapter Runtime (new), Runtime Observability 1.3.0, Tool Catalog 1.8.0, Game Runtime Channel 1.6.0.
- Proved on every release line with its own engine (4.7.2, 4.6.3, 4.5.2): the adapter was carried with 0.12.0 and its end-to-end tests pass on each line (51 of 51 runtime tests).

### Phase 11 — 2026-11-02

- `godot_set_project_setting` with the editor accepts the same values as without it: an HTML color or a color name for a Color, the `project.godot` text for other types (never an Object), a feature override takes the type of its base setting; one test table now holds both paths.
- The repo steward labels a merged pull request `status: planning-owed` until the planning update for it is committed, and a later run with the planning ecosystem settles the labelled ones first.
- The specifications were first brought in line with the code merged since Phase 10 (the derived bridge port, the export, shader, import, InputMap and deletion tools, one-off scripts and their repairs, branch cleanup); a new concept covers export.
- Carried the product commits since 0.12.0 to the 4.7, 4.6 and 4.5 lines. Found by carrying: the shader tab has `get_base_editor` only from Godot 4.8, a new text shader gets no `.uid` from the file system update on older editors, and a reloaded scene's tab is switched to a frame later on 4.5 and 4.6; all three are fixed in one commit on `master` and carried with the rest.
