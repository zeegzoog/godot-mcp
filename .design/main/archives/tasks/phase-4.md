---
phase: 4
name: "Godot Release Lines & Community Health"
status: Done
subsystem: "release lines (branches, workflows, release skill), addons/godot_mcp version gate, SECURITY.md, CONTRIBUTING.md"
requires: [2]
provides:
  - "Release lines godot-v4.5, godot-v4.6, godot-v4.7 and the development line on master (Godot 4.8)"
  - "Line identity in the version string with a sync test; addon version gate warns on newer Godot"
  - "Line-aware CI and release workflow with release-line.sh"
  - "Releases 0.5.0 for Godot 4.5, 4.6 and 4.7"
  - "Release skill for carrying and releasing lines; SECURITY.md and CONTRIBUTING.md"
  - "A configured GODOT_PATH is used as given"
key_files:
  created: []
  modified: []
patterns_established: []
duration_minutes: ~
---

# Stage 4 Tasks — Godot Release Lines & Community Health

**Phase:** 4
**Status:** Done
**Strategic Goal:** godot-mcp is maintained as one release line per Godot minor version (branches `godot-v4.5`, `godot-v4.6`, `godot-v4.7`) plus the development line on `master` (Godot 4.8); every line proves itself against its own Godot build, the release skill carries shared changes to every affected line and releases each line on its own, and the security policy and contributor guide tell people and AI agents how to report and how to contribute.

## Atomic Checklist

<!-- A line is `- [ ]` (open) or `- [x]` (done). A Blocked task keeps `- [ ]`: it is blocked by its `Status` in Detailed Tracking, and the `[!]` the workflows write after "Blocked" spells that status — it is not a checkbox state. -->

- [x] [T-4A01] Extend the version sync test with the line identity checks
- [x] [T-4A02] Set the development line identity to Godot 4.8
- [x] [T-4A03] Warn when the editor is newer than the line
- [x] [T-4B01] Make CI line-aware
- [x] [T-4B02] Make the release workflow line-aware
- [x] [T-4C01] Write the security policy
- [x] [T-4C02] Write the contributor guide
- [x] [T-4C03] Point the readme, agent guide, issue chooser and changelog at the lines and the new files
- [x] [T-4D01] Rewrite the release skill around the lines
- [x] [T-4E01] Commit the line-aware development line and cut the three release branches
- [x] [T-4E02] Call `is_importing` directly on the development line and line 4.7
- [x] [T-4F01] Turn on private vulnerability reporting and protect the release branches
- [x] [T-4F02] Publish the branches
- [x] [T-4F03] Publish the first line releases (0.5.0)
- [x] [T-4T01] Validate Phase 4 on every line against the specifications

## Detailed Tracking

### [T-4A01] Extend the version sync test with the line identity checks

- **Spec:** l2-release-branches.md §5.2 (RL-3, RL-5); l2-test-suite.md §5.2
- **Status:** Done
- **Assignment:** Agent
- **Verify:** Written before T-4A02: `cargo test --test version_sync_test` fails on the current tree (version `0.4.0` has no `+godot.M.N`) and passes after T-4A02. Mutation check after T-4A02, one file at a time, restoring each: `MIN_MINOR = 7` in `plugin.gd`, `"4.7"` in `project.godot`, `4.7.2-stable` in `.github/godot-version`, `0.5.0-beta+godot.4.8` in both manifests — each makes exactly one named check fail.
- **Handoff:** T-4A02.
- **Notes:** Five checks from §5.2, each its own `#[test]` with a behaviour name and a failure message that names the file to fix. Plain string parsing, no new dependency. `project.godot`: the first entry of `config/features=PackedStringArray(...)`. `.github/godot-version`: trimmed; the build must start with `M.N` followed by `.`, `-` or the end. A pre-release identifier is allowed only when it is `dev`. No planning references in names or messages.
- **Changes:** Four line identity tests in `tests/version_sync_test.rs` (version metadata and pre-release, plugin minimum, project feature, CI build), files read at run time with messages naming the file to fix. Red on the 0.4.0 tree (4 failed), green after T-4A02; each single-file mutation (MIN_MINOR 7, feature 4.7, build 4.7.2-stable, version -beta) fails exactly its own test.


### [T-4A02] Set the development line identity to Godot 4.8

- **Spec:** l2-release-branches.md §5.1, §5.2; l1-release-lines.md RL-2
- **Status:** Done
- **Assignment:** Agent
- **Verify:** `cargo build` updates the godot-mcp entry of `Cargo.lock`; `cargo test --all-targets` passes, including every check of T-4A01; with Godot 4.8-beta1: `godot --headless --path . --import` and the three `--check-only` commands of the contributor guide pass, and `GODOT_PATH=<4.8-beta1> cargo test --test editor_e2e_test -- --ignored` passes.
- **Handoff:** T-4A03.
- **Notes:** `Cargo.toml` and `plugin.cfg`: `0.5.0-dev+godot.4.8`. `plugin.gd`: `MIN_MINOR = 8`. `project.godot`: `config/features=PackedStringArray("4.8")` (after T-2C01 fixed its header comment). New file `.github/godot-version` with `4.8-beta1` and a trailing newline. Download the official Windows builds (editor and console) of 4.8-beta1 from the `godotengine/godot-builds` release into a folder outside the repository, and export `GODOT_PATH` / `GODOT_CONSOLE_PATH` (exported values win over `.env`).
- **Changes:** Development line identity: `0.5.0-dev+godot.4.8` in `Cargo.toml`, `Cargo.lock`, `plugin.cfg`; `MIN_MINOR = 8`; feature `4.8`; new `.github/godot-version` `4.8-beta1`. fmt, clippy pass; `cargo test --all-targets`: all pass except the pre-existing `without_an_editor_or_a_project_the_unknowns_are_said_so`, which also fails on clean HEAD on this host (the server finds Godot 4.7.2 in Program Files; Windows resets ProgramFiles for every child process, so the test cannot isolate it). With 4.8-beta1: import and three check-only exit 0; editor_e2e 3/3, addon_export 2/2.


### [T-4A03] Warn when the editor is newer than the line

- **Spec:** l2-godot-editor-addon.md §4 BR-9; l1-release-lines.md RL-4
- **Status:** Done
- **Assignment:** Agent
- **Verify:** The three `--check-only` commands pass with 4.8-beta1. Mutation check with 4.8-beta1, restoring after each: `MIN_MINOR = 7` → the editor log (`GODOT_CONSOLE_PATH` output of an e2e run) contains the warning naming Godot 4.7 and `editor_e2e_test` still passes; `MIN_MINOR = 9` → the log contains the refusal and the bridge does not start.
- **Handoff:** T-4E01 (the cut inherits the gate).
- **Notes:** Older engine (major or minor) → `printerr` refusal as today. Newer engine (any newer major or minor) → `push_warning("[Godot MCP] This godot-mcp build is for Godot %d.%d, but this is Godot %s. Install the godot-mcp release for Godot %d.%d.")` and continue. Strict typing; no API newer than 4.5 (the same `plugin.gd` runs on line 4.5).
- **Changes:** `plugin.gd`: older engine → refusal naming this build's line; newer engine → `push_warning` naming the line to install, bridge starts. With 4.8-beta1: MIN_MINOR 8 → no message; 7 → warning in the log and the token e2e test passes; 9 → refusal, bridge not started; restored to 8. check-only passes on 4.8-beta1 and 4.5.2.


### [T-4B01] Make CI line-aware

- **Spec:** l2-release-branches.md §5.7; l2-ci-release-pipeline.md §2, §4 DS-3
- **Status:** Done
- **Assignment:** Agent
- **Verify:** `ci.yml` parses as YAML; the run steps, executed locally in Git Bash on the T-4A02 tree: the Godot install step resolves `Godot_v4.8-beta1_linux.x86_64.zip` from `.github/godot-version` (print the URL; `curl -fsI` on it returns 200); the changelog step exits 0 and prints that it is skipped for `0.5.0-dev+godot.4.8`, and with a temporary version `0.5.0+godot.4.8` (restored afterwards) exits 1 without a changelog section. Final proof: the CI run after T-4F02.
- **Handoff:** T-4F02 proves it on GitHub.
- **Notes:** `on.push.branches: [master, 'godot-v*']`, `pull_request` unchanged. Drop the `GODOT_VERSION` env; read the file in the install step. Job and step names stay meaningful in the GitHub UI ("Install Godot $build").
- **Changes:** `ci.yml`: pushes to `master` and `godot-v*`; the Godot job installs the build named in `.github/godot-version`; the changelog check skips versions with a pre-release identifier. YAML parses; locally the changelog step exits 0 for `0.5.0-dev+godot.4.8` and 1 for `0.5.0+godot.4.8` without a section; the 4.8-beta1 Linux download URL answers 200. GitHub run pending T-4F02.


### [T-4B02] Make the release workflow line-aware

- **Spec:** l2-release-branches.md §5.7 (RL-2, RL-3, RL-8, RL-9); l2-ci-release-pipeline.md §4 DS-3, DS-5
- **Status:** Done
- **Assignment:** Agent
- **Verify:** `release.yml` parses as YAML. In a scratch repository (temporary directory, local bare `origin`) with branches `godot-v4.6`, `godot-v4.7` and matching manifests, `bash .github/scripts/release-line.sh <tag>`: prints `4.6`, `-godot4.6` and `false` for `v0.5.0+godot.4.6` when tag `v0.5.0+godot.4.7` exists, `true` when it does not; exits non-zero, naming the rule, for `v0.5.0` (no line), `v0.5.0-dev+godot.4.8` (pre-release), a tag that differs from the manifest version, and a tagged commit not contained in `origin/godot-v4.6`.
- **Handoff:** T-4F03 proves it on GitHub.
- **Notes:** Trigger `tags: ["v*.*.*+godot.*"]`. The verify job checks out with `fetch-depth: 0` and runs the script; the addon and build jobs use its suffix in file names; the release steps pass `name: godot-mcp X.Y.Z for Godot M.N` and `make_latest: "true"|"false"` to `softprops/action-gh-release@v3`. `release-notes.sh` stays as is (its `index()` match handles `+`). Script style like `release-notes.sh`: `set -euo pipefail`, usage line, messages to stderr.
- **Changes:** New `.github/scripts/release-line.sh` (tag = manifest, `+godot.M.N`, no pre-release, commit on `origin/godot-vM.N`; prints line, suffix, title, latest). `release.yml`: trigger `v*.*.*+godot.*`, verify job with full history runs the script (explicit bash shell, so pipefail keeps a failed check fatal), file names with `-godotM.N`, release title and make_latest from its output. Scratch-repository cases 8/8 pass; `release-notes.sh` extracts `+godot` sections. GitHub run pending T-4F03.


### [T-4C01] Write the security policy

- **Spec:** l2-community-health-files.md §4 CH-1–CH-4, CH-6, CH-9, §5.1; l1-community-health.md §5.1
- **Status:** Done
- **Assignment:** Agent
- **Verify:** `SECURITY.md` has the sections Supported Versions, Reporting a Vulnerability, Scope, How Fixes Are Released, For AI Agents; the table follows §5.1 with the wording for the time before the first line releases; the only reporting channel is `https://github.com/zeegzoog/godot-mcp/security/advisories/new`; `grep -nE '\.design|l[12]-[a-z-]+\.md|T-[0-9]+[A-Z][0-9]+|[Pp]hase[- ][0-9]' SECURITY.md` prints nothing; `npx --yes markdownlint-cli2 SECURITY.md` reports no errors.
- **Handoff:** T-4F01 turns the reporting channel on before the file is pushed.
- **Notes:** Link `README.md#security` for the security measures instead of restating them (CH-7). No personal e-mail address. Response targets 7 / 14 / 90 days. The For AI Agents section: stop, keep details out of issues and `godot_note_feedback` / `godot_submit_feedback`, tell the user how to report privately.
- **Changes:** `SECURITY.md`: Supported Versions (line table, pre-release wording), Reporting a Vulnerability (private advisory link only, never issues or the feedback tools, contents, 7/14/90 days, advisory and credit), Scope, How Fixes Are Released, For AI Agents; links README § Security. SDD grep empty; markdownlint 0 issues.


### [T-4C02] Write the contributor guide

- **Spec:** l2-community-health-files.md §4 CH-5–CH-8, §5.2; l1-community-health.md §5.2
- **Status:** Done
- **Assignment:** Agent
- **Verify:** `CONTRIBUTING.md` has the sections of l1-community-health.md §5.2 in that order; the branch decision table of l2-community-health-files.md §5.2; the verification commands are line-for-line the ones in `AGENTS.md` (compare the fenced command lines); the Planning and Specifications section states: `.design/` is committed and shared, it is maintained on `master` only, planning-file changes are committed separately from product changes, the planning engine is installed locally and git-ignored; `npx --yes markdownlint-cli2 CONTRIBUTING.md` reports no errors.
- **Handoff:** T-4C03 links to it.
- **Notes:** Supersedes T-2D01. Rust 1.85 (`rust-version`); editor tests with the build named in the branch's `.github/godot-version`; Conventional Commits with a scope; carrying with `git cherry-pick -x`; contributions under MIT. No Discussions: questions go to issues.
- **Changes:** `CONTRIBUTING.md` with the twelve sections in order, line table and branch decision table, test placement table, Planning and Specifications section (replaces T-2D01). Verification commands identical to `AGENTS.md` (diff of command lines empty); markdownlint 0 issues.


### [T-4C03] Point the readme, agent guide, issue chooser and changelog at the lines and the new files

- **Spec:** l2-release-branches.md §4 RL-9, RL-10, §6 step 2; l2-community-health-files.md §4 CH-1, CH-7, CH-8, §5.3
- **Status:** Done
- **Assignment:** Agent
- **Verify:** `README.md`: the Installation section has the line table (same rows as `SECURITY.md` and `CONTRIBUTING.md`), Option A step 3 picks the newest release titled "for Godot M.N" using the version found in step 2 and falls back to 0.4.0 while no line release exists, Option B names the `-godotM.N` files; § Security links `SECURITY.md`; § Development and the Releasing paragraph are links to `CONTRIBUTING.md`. `AGENTS.md`: no statement that 4.5-stable is the only tested version, line wording for the minimum version and CI, a link to `CONTRIBUTING.md`, one completion checklist item for the two policy files. `.github/ISSUE_TEMPLATE/config.yml`: a contact link to the reporting URL. `CHANGELOG.md`: a note under the intro that later releases are listed in each line's changelog and on the Releases page. The SDD grep of T-4C01 prints nothing for `README.md` and `CHANGELOG.md`; `bash .github/scripts/release-notes.sh v0.4.0` still prints the 0.4.0 notes; markdownlint reports no errors.
- **Handoff:** T-4E01.
- **Notes:** The three line tables must be identical in rows (RL-10). Keep README user-facing; no planning references.
- **Changes:** README: line table and per-version release choice in Installation, Option A picks the newest release titled for the user's Godot (fallback v0.4.0), Option B names the -godotM.N files, Security links SECURITY.md, Development and Releasing reduced to links. AGENTS.md: link to CONTRIBUTING.md, line wording for target, guards and editor tests, Policy Documents checklist item. Issue chooser contact link; CHANGELOG note on per-line changelogs. SDD grep empty; markdownlint 0 issues on five files; release-notes.sh v0.4.0 unchanged.


### [T-4D01] Rewrite the release skill around the lines

- **Spec:** l2-release-branches.md §5.4–§5.6, §4 RL-6–RL-10, RL-12; l2-community-health-files.md §4 CH-8
- **Status:** Done
- **Assignment:** Agent
- **Verify:** `.agents/skills/release/SKILL.md`: the frontmatter description triggers on releasing, backporting or carrying a fix, cutting a branch for a new Godot version and retiring a line; procedures A–G with copyable commands; procedure C has the eight numbered steps of §5.6; procedures C and D stop for explicit approval before tagging and pushing; the notes forbid force pushes, tag deletion and release deletion; every file it names exists after T-4A02 and T-4B02; no planning references (SDD grep of T-4C01); markdownlint reports no errors.
- **Handoff:** T-4E01 exercises procedure E; T-4E02 exercises procedure B.
- **Notes:** Keep the skill self-contained for an agent with no other context: define line, carry, version string and `M.N` once at the top.
- **Changes:** Rewrote `.agents/skills/release/SKILL.md` (202 lines): model and terms first, procedures A–G, C with eight numbered steps and an approval stop before any push or tag, D newest line first, rules against force push and tag or release deletion. The uncarried-changes snippet was checked in a scratch repository (finds only the uncarried fix; clean carry, carry with conflict, planning-only and release commits filtered). markdownlint 0 issues; no task, phase or specification names; two mentions of the planning folder are process instructions.


### [T-4E01] Commit the line-aware development line and cut the three release branches

- **Spec:** l2-release-branches.md §5.1, §6 steps 2–3; l1-release-lines.md §5.5
- **Status:** Done
- **Assignment:** Agent, after the user's go-ahead to commit (local only; nothing is pushed)
- **Verify:** `master` has the line-aware product commit(s) with `.design/` committed separately; `git branch --list 'godot-v*'` lists three branches cut from that commit; each branch has one identity commit (`0.5.0+godot.4.N` in both manifests and `Cargo.lock`, `MIN_MINOR = N`, `config/features` `4.N`, `.github/godot-version` `4.5.2-stable` / `4.6.3-stable` / `4.7.2-stable`); `cargo test --test version_sync_test` passes on each branch.
- **Handoff:** T-4E02.
- **Notes:** Requires Phase 2 T-2C01 and Phase 4 tracks A–D done. Use one `git worktree` per branch outside the repository folder, so `master`'s working tree is never switched. Each branch's identity commit also replaces the note under the `CHANGELOG.md` introduction with the line's own ("this file lists the releases of the Godot 4.N line; releases up to 0.4.0 were one build for every Godot version").
  Waiting for the user's go-ahead to commit: tracks A–D and T-2C01 are done in `master`'s working tree, uncommitted. Commit plan: the planning files in one commit, the product changes in another, then the cut.
- **Changes:** Product commit d25a41e and planning commit ac265ed on master; branches godot-v4.5, godot-v4.6, godot-v4.7 cut from d25a41e, one identity commit each (0.5.0+godot.4.N, minimum engine, feature tag, Godot build 4.5.2/4.6.3/4.7.2-stable, line changelog note); version_sync_test passes on each. Local only, not pushed.


### [T-4E02] Call `is_importing` directly on the development line and line 4.7

- **Spec:** l2-release-branches.md §5.3; l2-godot-editor-addon.md §2
- **Status:** Done
- **Assignment:** Agent
- **Verify:** On `master` and `godot-v4.7` `is_import_running` calls `EditorFileSystem.is_importing()` without `has_method`; `godot-v4.6` and `godot-v4.5` keep the guard (`grep -n has_method addons/godot_mcp/bridge_server.gd` on each); GDScript check, `editor_e2e_test` and `addon_export_test` pass with 4.8-beta1 on `master` and 4.7.2-stable on `godot-v4.7`; the `godot-v4.7` commit carries the `(cherry picked from commit …)` line.
- **Handoff:** T-4T01.
- **Notes:** A shared change for two lines: commit on `master` first, then `git cherry-pick -x` onto `godot-v4.7` (procedure B). Keep the function and its comment honest for the line it is on.
- **Changes:** master 24cc054 calls is_importing directly (API present in 4.7-stable, absent in 4.6-stable); carried to godot-v4.7 with cherry-pick -x. 4.5 and 4.6 keep the guard. With 4.8-beta1: check-only exit 0, editor_e2e 3/3, addon_export 2/2.


### [T-4F01] Turn on private vulnerability reporting and protect the release branches

- **Spec:** l2-community-health-files.md §5.4; l2-release-branches.md §5.8
- **Status:** Done
- **Assignment:** User (repository settings)
- **Verify:** `gh api repos/zeegzoog/godot-mcp/private-vulnerability-reporting` returns `{"enabled":true}`; a branch rule for `godot-v*` blocks deletion and force pushes (`gh api repos/zeegzoog/godot-mcp/rulesets` lists it, or the branch protection page shows it).
- **Handoff:** T-4F02.
- **Notes:** The agent lists the steps; it does not change settings.
  Private vulnerability reporting: done by the user (confirmed enabled on 2026-10-20). Remaining: the ruleset for `godot-v*` (no deletion, no force push), instructions given; tick when `gh api repos/zeegzoog/godot-mcp/rulesets` lists it.
- **Changes:** Private vulnerability reporting enabled and ruleset 'Protect Godot release lines' (godot-v*: no deletion, no force push) active; both done by the user.


### [T-4F02] Publish the branches

- **Spec:** l2-release-branches.md §6 step 6; l1-distribution.md DS-7
- **Status:** Done
- **Assignment:** Agent, after the user's explicit approval of the push
- **Verify:** `git push origin master godot-v4.7 godot-v4.6 godot-v4.5` after approval; the latest CI run of each branch is green (`gh run list --branch <branch> -L 1`), including the Godot job with that branch's build; T-2B01's Unix token-file assertion passes in the Linux job.
- **Handoff:** T-4F03.
- **Notes:** Push only after T-4F01, so the reporting link in `SECURITY.md` works from the first day.
- **Changes:** master, godot-v4.7, 4.6, 4.5 pushed after approval; CI green on all four heads (16/16 jobs), including the Linux token-file assertion.


### [T-4F03] Publish the first line releases (0.5.0)

- **Spec:** l1-release-lines.md §5.3, §5.5; l2-release-branches.md §5.6 procedure D
- **Status:** Done
- **Assignment:** Agent through the release skill, after the user's explicit approval of every tag
- **Verify:** Releases `v0.5.0+godot.4.5`, `v0.5.0+godot.4.6`, `v0.5.0+godot.4.7` exist, each titled `godot-mcp 0.5.0 for Godot 4.N` with five files suffixed `-godot4.N`; only the 4.7 release is marked latest (`gh release view` / `gh release list`); afterwards the install cells of the three line tables no longer mention 0.4.0, changed on `master` and carried to every line.
- **Handoff:** `/magic.task main update` to close the phase.
- **Notes:** Each branch gets its own changelog section `## [0.5.0+godot.4.N] - <date>`, written for users: the tag and file names changed, which release to pick, the line gate and warning.
- **Changes:** Releases v0.5.0+godot.4.7 (latest), 4.6, 4.5, each titled 'godot-mcp 0.5.0 for Godot 4.N' with 5 files suffixed -godot4.N; all release jobs green. The first 4.7 tag was deleted with the user's approval because the tagged commit's release workflow had an invalid tag filter ('+' is a quantifier); fixed with a guard test, carried to all lines, then re-tagged. Line tables in README and SECURITY.md now name the line releases.


### [T-4T01] Validate Phase 4 on every line against the specifications

- **Spec:** l1-quality-assurance.md §3 QA-2, QA-3, QA-6; l1-release-lines.md §3; l1-community-health.md §3
- **Status:** Done
- **Assignment:** Agent
- **Verify:** On `master`, `godot-v4.7`, `godot-v4.6` and `godot-v4.5`: `cargo fmt --all -- --check`, `cargo clippy --all-targets --all-features -- -D warnings`, `cargo test --all-targets`, the GDScript check, `editor_e2e_test` and `addon_export_test` with the Windows build of the version in that branch's `.github/godot-version` — all pass; every RL and CH invariant row of the two L2 specifications is checked against the files; the SDD grep of T-4C01 over the product files changed in this phase prints nothing; `node .magic/scripts/executor.js check-prerequisites --json --workspace=main` reports `ok: true`.
- **Handoff:** T-4F01 and the gated tasks after it.
- **Notes:** Runs after T-4E02 and before any push. Official Windows builds 4.5.2-stable, 4.6.3-stable, 4.7.2-stable and 4.8-beta1 (editor and console) go into a folder outside the repository.
- **Changes:** Per line with its own Windows Godot build (4.5.2, 4.6.3, 4.7.2, and 4.8-beta1 for master): fmt, clippy, check-only of three scripts, editor_e2e 3/3 and addon_export 2/2 all pass. cargo test --all-targets passes except without_an_editor_or_a_project_the_unknowns_are_said_so on every line: the server falls back to a Godot found in Program Files when GODOT_PATH points at a missing file; it also fails on the commit before this phase, so it is not caused by it. Fixed afterwards (c5243ee on master, carried with cherry-pick -x to godot-v4.7, 4.6, 4.5): a configured GODOT_PATH is used as given, so the test passes and `cargo test --all-targets` is green on all four lines.
