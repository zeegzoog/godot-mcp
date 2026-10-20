---
phase: 2
name: "Alignment & Hardening"
status: Done
subsystem: "repository hygiene, addons/godot_mcp, tests"
requires: []
provides:
  - "Planning directory kept out of source archives"
  - "Owner-only token file asserted on Unix by the editor e2e test"
  - "Development project header comment corrected"
key_files:
  created: []
  modified: []
patterns_established: []
duration_minutes: ~
---

# Stage 2 Tasks — Alignment & Hardening

**Phase:** 2
**Status:** Done
**Strategic Goal:** Every Stable specification's invariants hold without a recorded gap, and contributors know that planning goes through the specification-driven workflow.

## Atomic Checklist

<!-- A line is `- [ ]` (open) or `- [x]` (done). A Blocked task keeps `- [ ]`: it is blocked by its `Status` in Detailed Tracking, and the `[!]` the workflows write after "Blocked" spells that status — it is not a checkbox state. -->

- [x] [T-2A01] Mark the planning directory `export-ignore` for source archives
- [x] [T-2B01] Test that the bridge token file is owner-only on Unix
- [x] [T-2C01] Correct the stale class_name note in the development project file
- [x] [T-2D01] Add a specification-workflow section to the contributor guide (Cancelled: moved to T-4C02)
- [x] [T-2T01] Validate Phase 2 against the specifications and the verification pipeline

## Detailed Tracking

### [T-2A01] Mark the planning directory `export-ignore` for source archives

- **Spec:** l2-ci-release-pipeline.md §4 (DS-8), l1-distribution.md §3 DS-8
- **Status:** Done
- **Assignment:** Agent
- **Verify:** `git archive --format=tar HEAD | tar -t | grep -c '^\.design/'` prints `0` after the change is committed (non-zero before); `cargo test --all-targets` still passes.
- **Handoff:** Update the DS-8 row of l2-ci-release-pipeline.md (remove "Gap") via `/magic.spec amend`.
- **Notes:** Add one attribute line to `.gitattributes`. Do not touch `.gitignore`: the planning directory must stay committed so the team shares it; only its presence in tag source archives is the problem.
- **Changes:** `.gitattributes` marks `/.design` `export-ignore`; `git archive HEAD` listed 34 planning files before and 0 after; specification updated (DS-8 gap closed, v1.0.1).

### [T-2B01] Test that the bridge token file is owner-only on Unix

- **Spec:** l2-godot-editor-addon.md §6 (known limitation), l1-editor-bridge.md §3 BR-2
- **Status:** Done
- **Assignment:** Agent
- **Verify:** New assertion in `tests/editor_e2e_test.rs` (`bridge_is_loopback_only_and_requires_the_token`, `#[cfg(unix)]` block) that the token file mode is `0o600`; passes in the CI Linux Godot 4.5 job; fails locally when `FileAccess.set_unix_permissions` is removed from the addon (mutation check, then restored).
- **Handoff:** If the assertion fails on 4.5, fix the addon before marking Done; then remove the limitation note via `/magic.spec amend l2-godot-editor-addon.md`.
- **Notes:** Linux/macOS only; on Windows the block compiles out. Needs `GODOT_PATH` for local runs (see l2-test-suite.md §2).
  Blocked: the assertion and its mutation check run only on Unix; no Linux toolchain or Linux Godot on this host. Proven by the Linux CI job once the branches are published (T-4F02); then set Done.
- **Changes:** `#[cfg(unix)]` assertion in the editor e2e test that the bridge token file mode is 0600. Passed in the Linux CI Godot job on master (4.8-beta1) and on godot-v4.5, 4.6, 4.7 with their own builds.

### [T-2C01] Correct the stale class_name note in the development project file

- **Spec:** l2-godot-editor-addon.md §2 (no `class_name` convention)
- **Status:** Done
- **Assignment:** Agent
- **Verify:** `project.godot` header comment no longer claims the addon declares `class_name`; `godot --headless --path . --import` and the three `--check-only` commands from the contributor guide still succeed.
- **Handoff:** None.
- **Notes:** Comment-only change. Restate the reason in plain language (the root project exists so the addon can be enabled and syntax-checked in isolation); no references to planning artifacts.
- **Changes:** The header comment no longer claims `class_name` declarations; `--import` and the three `--check-only` commands pass on Godot 4.5.2-stable (exit 0).

### [T-2D01] Add a specification-workflow section to the contributor guide

- **Spec:** l2-test-suite.md §4 (QA-1, QA-2 via the contributor checklist), l1-quality-assurance.md §3
- **Status:** Cancelled — the section belongs to the new `CONTRIBUTING.md` (§ Planning and Specifications, l2-community-health-files.md §6 note 2) and is written by T-4C02; `AGENTS.md` links to it.
- **Assignment:** Agent
- **Verify:** `AGENTS.md` has a section stating: planning and design go through the `/magic.*` workflow, which takes priority over ad-hoc plans; `.design/` is committed and shared; the engine (`.magic/` and the workflow wrappers) is installed locally from its release archive and is git-ignored; product files never reference planning artifacts. `npx markdownlint-cli2 AGENTS.md` (or the repo's `.markdownlint.json` rules) reports no new issues.
- **Handoff:** None.
- **Notes:** The contributor guide is a process document, so naming the workflow is allowed there; keep README.md and CHANGELOG.md free of it.

### [T-2T01] Validate Phase 2 against the specifications and the verification pipeline

- **Spec:** l1-quality-assurance.md §3 QA-2, QA-3
- **Status:** Done
- **Assignment:** Agent
- **Verify:** `cargo fmt --all -- --check`, `cargo clippy --all-targets --all-features -- -D warnings`, `cargo test --all-targets` pass; editor e2e and export tests pass with Godot 4.5-stable and the local current build (addon test file changed in T-2B01); `node .magic/scripts/executor.js check-prerequisites --json --workspace=main` reports `ok: true`.
- **Handoff:** `/magic.task main` to re-plan once all specs are fully satisfied.
- **Notes:** Run last; depends on T-2A01..T-2C01. The Unix token-file assertion of T-2B01 is proven by the Linux CI job, which runs after T-4F02 publishes the branches.
- **Changes:** Pipeline run per line with its own Godot build (see Phase 4 validation); fmt, clippy, tests, check-only, editor e2e and export tests pass on master, godot-v4.7, 4.6 and 4.5; CI green on all four branches; check-prerequisites ok.
