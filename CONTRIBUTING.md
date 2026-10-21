# Contributing to godot-mcp

Thank you for helping. This guide takes you from a first question to a merged change. It is written for people and for AI agents alike: every rule is a single instruction, and every command can be copied as it is.

| You want to | Go to |
| --- | --- |
| Ask a question or report a bug | [Issues](https://github.com/zeegzoog/godot-mcp/issues) |
| Report a security vulnerability | [SECURITY.md](SECURITY.md) (privately, never in an issue) |
| Work on the code as an AI agent | [AGENTS.md](AGENTS.md), then this guide |
| Know the license of your contribution | [License](#license) |

## Ways to Contribute

- **Questions:** open an issue. The repository has no Discussions.
- **Bug reports:** open an issue with:
  - the godot-mcp version (`version=` in `addons/godot_mcp/plugin.cfg`), the Godot version and the operating system;
  - the MCP client you use (Claude Code, Cursor, VS Code, …);
  - the steps to reproduce, what you expected and what happened;
  - the server log if it helps: run the server with `RUST_LOG=godot_mcp=debug`; the log goes to stderr.
- **Feature proposals:** open an issue before writing a larger change, so the approach can be agreed first.
- **Security vulnerabilities:** follow [SECURITY.md](SECURITY.md). Never open a public issue for them.
- **Reports from your AI assistant:** paste a report drafted by `godot_draft_feedback` into a new issue with the "Feedback from an AI assistant" template, or let the assistant submit it with your approval.

## Branches and Godot Versions

Each Godot minor version has its own release line on its own branch. `master` is the development line for the next Godot version, which has no stable release yet.

| Godot | Branch | Status |
| --- | --- | --- |
| 4.7.x | `godot-v4.7` | Release line, newest stable |
| 4.6.x | `godot-v4.6` | Release line |
| 4.5.x | `godot-v4.5` | Release line |
| 4.8 (pre-release) | `master` | Development line, no releases |

Choose the branch for your pull request:

| The change … | Open the pull request against |
| --- | --- |
| fixes a problem that exists on `master` | `master`; maintainers carry it to the older lines it affects |
| fixes a problem that exists only on some release lines | the newest of those `godot-v*` branches |
| exists only because of one Godot version | that version's `godot-v*` branch only |
| adds a feature or an improvement | `master` |
| changes shared documentation or CI | `master` |

- A branch is deleted automatically once its pull request is merged (`.github/workflows/branch-cleanup.yml`). The release lines, protected branches and branches with unmerged work are never deleted.
- Find out which lines a bug affects by running its test on each branch, not by reading the code.
- Files that differ between lines on purpose: `Cargo.toml` and `Cargo.lock` (version), `addons/godot_mcp/plugin.cfg` (version), `addons/godot_mcp/plugin.gd` (`MIN_MAJOR`, `MIN_MINOR`), `project.godot` (`config/features`) and `.github/godot-version` (the Godot build the line is tested against). Keep every other file identical on all lines unless a Godot version forces a difference.

## Development Setup

1. Install Rust 1.85 or newer: `rustup update stable`.
2. Install the Godot build named in `.github/godot-version` of the branch you work on, from <https://github.com/godotengine/godot-builds/releases>. On Windows, also keep the `_console.exe` file that comes with it.
3. Clone and build:

   ```bash
   git clone https://github.com/zeegzoog/godot-mcp.git
   cd godot-mcp
   cargo build
   ```

4. Tell the tests where Godot is: export `GODOT_PATH` (and on Windows `GODOT_CONSOLE_PATH`, so a failing editor test shows the editor log), or copy `.env.example` to `.env` and fill it in.
   - Exported variables win over `.env`; export both when you test a specific Godot version.
   - In `.env`, put Windows paths in single quotes. A double-quoted Windows path stops the whole file from loading.

## Making a Change

1. Write the test first. It must fail before your change and pass after it.
2. Put the test where its behaviour belongs:

   | Behaviour | Test file |
   | --- | --- |
   | JSON-RPC framing, session, configuration | `tests/stdio_protocol_test.rs` |
   | A tool's host-side logic | `tests/tools_test.rs` |
   | Agent feedback channel | `tests/feedback_test.rs` |
   | Bridge client | `tests/bridge_client_test.rs` |
   | Editor command behaviour | `tests/editor_e2e_test.rs` |
   | Addon kept out of exported games | `tests/addon_export_test.rs` |
   | Launched processes staying independent of the server | `tests/detach_test.rs` |
   | Running game, input, screenshots, breakpoints, GDScript diagnostics | `tests/runtime_e2e_test.rs` |
   | Version string and release line identity | `tests/version_sync_test.rs` |

   Unit tests live next to the code in `src/`.
3. Follow the engineering standards in [AGENTS.md](AGENTS.md): no `unwrap()` or `expect()` in production code, logs to stderr only, every editor mutation undoable, every path checked against the project, strict typing in GDScript.
4. Prove every security or data-safety guard with a test that fails when the guard is removed.

## Verification

Run all of these before you commit. CI runs them again on Windows, Linux and macOS.

```bash
cargo clippy --all-targets --all-features -- -D warnings
cargo fmt --all -- --check
cargo test --all-targets
```

When you changed anything in `addons/godot_mcp/`, also check the GDScript and run the editor tests with the Godot build of the branch (`.github/godot-version`):

```bash
godot --headless --path . --import   # first run only, or after script changes
godot --headless --check-only --path . --script addons/godot_mcp/plugin.gd
godot --headless --check-only --path . --script addons/godot_mcp/bridge_server.gd
godot --headless --check-only --path . --script addons/godot_mcp/export_plugin.gd
godot --headless --check-only --path . --script addons/godot_mcp/editor/game_relay.gd
godot --headless --check-only --path . --script addons/godot_mcp/core/runtime_override.gd
godot --headless --check-only --path . --script addons/godot_mcp/runtime/mcp_runtime.gd
godot --headless --check-only --path . --script addons/godot_mcp/runtime/ops/rt_hold.gd
godot --headless --check-only --path . --script addons/godot_mcp/runtime/ops/rt_input.gd
godot --headless --check-only --path . --script addons/godot_mcp/runtime/ops/rt_live.gd
godot --headless --check-only --path . --script addons/godot_mcp/runtime/ops/rt_output.gd
godot --headless --check-only --path . --script addons/godot_mcp/runtime/ops/rt_result.gd
godot --headless --check-only --path . --script addons/godot_mcp/runtime/ops/rt_view.gd
GODOT_PATH=/path/to/godot cargo test --test editor_e2e_test -- --ignored
GODOT_PATH=/path/to/godot cargo test --test addon_export_test -- --ignored
```

When the running-game tools, the debugger client, or the GDScript language-server client changed, also run that with the same Godot build:

```bash
GODOT_PATH=/path/to/godot cargo test --test runtime_e2e_test -- --ignored
```

## Commits and Pull Requests

- Write commit messages as Conventional Commits with a scope: `fix(bridge): reject a token that is not a string`. Common types: `feat`, `fix`, `docs`, `test`, `refactor`, `ci`, `build`, `chore`.
- Explain in the commit body why the change is needed when the summary does not say it.
- Keep one logical change per pull request.
- State in the pull request description:
  - what changed and why;
  - which Godot lines the change affects, and how you checked (the test, run on each branch);
  - which checks from [Verification](#verification) you ran.
- Never commit `.env`, an MCP client config with real paths (`.mcp.json`), built binaries or the `.godot/` folder. Commit examples with placeholders instead, such as `.env.example`.
- Leave version numbers and `CHANGELOG.md` sections alone: maintainers set them in release commits.
- Write code, comments, commit messages and documentation in English.

## Issues and Pull Requests

An AI assistant, the repo steward, works the issue tracker and the pull requests on behalf of the maintainers. Every message it writes says that an AI wrote it.

- **What it does:** it gives each new issue a type and a priority, asks for missing details, answers questions, closes duplicates, reviews pull requests, fixes accepted issues through pull requests of its own, and keeps the project board "godot-mcp issues and pull requests" current.
- **What the maintainers decide:** whether the project takes on a feature, whether a contributor's pull request is merged or declined, and every release.
- **Priorities:**

  | Priority | Meaning |
  | --- | --- |
  | P0 | security exposure, data loss, the product unusable, a broken release, failing CI on a branch |
  | P1 | a core function broken without a workaround, or a regression |
  | P2 | broken with a workaround, misleading behaviour or documentation, a wanted feature |
  | P3 | minor or cosmetic, nice to have, questions |

- **Labels you may see:**
  - `status: needs-info` — details are missing. A reminder follows after 14 days, and the issue is closed 7 days after the reminder unless the details arrive. Reopen it, or comment, when you have them.
  - `status: needs-maintainer` — waiting for a maintainer's decision.
  - `status: blocked` — waiting for something outside the project.
- **Reach a person:** ask for a maintainer in a comment, and the assistant hands the issue or pull request over.
- **Security problems:** never in an issue or pull request; follow [SECURITY.md](SECURITY.md).

## Carrying Changes Between Lines

A change that affects several lines lands on the newest of them first and is then carried to each older one, newest to oldest. Maintainers usually do the carrying; you may do it yourself:

```bash
git switch godot-v4.7
git cherry-pick -x <commit>    # -x records "(cherry picked from commit <commit>)"
```

- Open the carried copy as its own pull request titled `[4.7] <original title>`.
- Resolve a conflict on that line, and make the test pass with that line's Godot build.
- Skip a line on which the test does not reproduce the problem.

## Planning and Specifications

- The `.design/` folder holds the project's specifications and plan. It is committed and shared, and it is maintained on `master` only; a release branch keeps the copy from the day it was created.
- Maintainers plan and design through the Magic Spec workflow (`/magic.spec`, `/magic.task`, `/magic.run`, `/magic.analyze`), run by their AI agent. Its engine (`.magic/` and the `magic.*` workflow files) is installed locally from its release archive and is git-ignored.
- You do not need the engine to contribute. If your change alters documented behaviour, say so in the pull request, and a maintainer updates the specifications.
- Commit changes to `.design/` separately from product changes, so that product commits carry cleanly to the release lines.
- Never mention `.design/`, specification file names or task IDs in product files: code, tests, `README.md`, `CHANGELOG.md`, `SECURITY.md`.

## For AI Agents

- Read [AGENTS.md](AGENTS.md) first and follow it. It holds the engineering standards, the agent workflow and the completion checklist; the commands in [Verification](#verification) are the same as there.
- Pick the target branch with the table in [Branches and Godot Versions](#branches-and-godot-versions).
- Never push, tag or publish a release without the explicit approval of your user — except the actions the repo-steward skill's autonomy settings allow (its defaults, as adjusted in `.github/steward.yml`). Tagging and publishing a release are never among them.
- Work issues and pull requests with the repo-steward skill: [.agents/skills/repo-steward/SKILL.md](.agents/skills/repo-steward/SKILL.md).

## Releases

Maintainers publish releases with the agent skill in `.agents/skills/release/SKILL.md`. In short:

- One release holds the files of every supported Godot line, named `vX.Y.Z` (for example `v0.5.1`). Every release takes the next version number; all lines take it together, and a line without a change of its own only advances its version.
- Each line's version names the line: `0.5.1+godot.4.6` on branch `godot-v4.6`, with the line tag `v0.5.1+godot.4.6` marking its sources. The release notes are one `CHANGELOG.md` on `master`.
- A person approves every tag before it is pushed. The release workflow then builds the files of every line and publishes the release.

## License

godot-mcp is licensed under the [MIT License](LICENSE). By contributing, you agree that your contribution is licensed under the same terms. There is no separate contributor agreement.
