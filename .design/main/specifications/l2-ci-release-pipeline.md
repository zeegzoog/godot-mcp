# CI & Release Pipeline

**Version:** 1.4.0
**Status:** Stable
**Layer:** implementation
**Implements:** l1-distribution.md

## Overview

The GitHub Actions workflows, helper script, manifests and agent skill that build, verify and publish godot-mcp releases, and the repository hygiene that keeps machine-specific files and development scaffolding out of what is shipped.

## Related Specifications

- [l1-distribution.md](l1-distribution.md) - Parent concept.
- [l2-test-suite.md](l2-test-suite.md) - The checks the pipeline runs.
- [l2-release-branches.md](l2-release-branches.md) - The per-line branches, version string, workflow checks and release procedures this pipeline runs under.

## 1. Motivation

Release mechanics are spread over several files (two workflows, a shell script, two manifests, a changelog, an agent skill, ignore rules). This specification lists which file enforces which distribution rule.

## 2. Constraints & Assumptions

- Runners: `ubuntu-24.04` (pinned), `windows-latest`, `macos-latest`; GitHub's own actions by major version (`actions/checkout@v7`, `upload-artifact@v7`, `download-artifact@v8`); every other action (`dtolnay/rust-toolchain`, `Swatinem/rust-cache`, `softprops/action-gh-release`) pinned to a full commit SHA with its version in a comment, which `tests/steward_test.rs` checks for every workflow. `.github/dependabot.yml` proposes weekly updates of those pins and of the Rust dependencies. The toolchain action is given `toolchain: stable` because, pinned by SHA, it no longer learns the toolchain from its ref.
- [MODIFIED] CI installs, for the editor job, the Godot build named in `.github/godot-version` from the official builds repository; each line names its own build ([l2-release-branches.md](l2-release-branches.md) §5.7).
- [MODIFIED] The editor job syntax-checks every addon script with `--check-only`, the new operation files (`editor/import_ops.gd`, `editor/shader_ops.gd`) included; the list in `ci.yml` and the one in `AGENTS.md` and `CONTRIBUTING.md` name the same scripts.
- [ADDED] Branch hygiene: `branch-cleanup.yml` runs `.github/scripts/prune-branches.sh` when a pull request from this repository is merged (for its head branch) and every Monday (for every branch), and by hand, where a dry run is the default. A branch is deleted only when a pull request from it was merged and it still points at the last commit that request merged. Never deleted: the default branch, the release lines (`godot-v*`), protected branches, a branch with an open pull request, one whose request was closed unmerged, one with no request, one with commits after its merge; branches of forks are not in this repository. The job has `contents: write` and `pull-requests: read` only.
- Release profile: `lto = true`, `codegen-units = 1`, `strip = true`.
- The changelog follows Keep a Changelog headings (`## [X.Y.Z] - YYYY-MM-DD`, `## [Unreleased]`) with compare links at the bottom.
- Assumption (forecast): the specification-driven workflow's automatic changelog writer is switched off for this project, because the changelog doubles as the published release text. Runner-up: keep it on and hand-clean every release. Override: `finalization.autoChangelog` in the workspace configuration.

## 4. Invariant Compliance

| L1 Invariant | Implementation |
| --- | --- |
| DS-1 One version | `tests/version_sync_test.rs` compares `Cargo.toml` and `addons/godot_mcp/plugin.cfg` and checks the line identity files ([l2-release-branches.md](l2-release-branches.md) §5.2); the release skill bumps both manifests in one commit per line. `release-set.sh` refuses a release unless every line's tag carries manifests at exactly `X.Y.Z+godot.M.N`. |
| DS-2 Self-contained binaries | Release `build` matrix runs `cargo build --release --locked` for the four target triples with the release profile and archives the single executable per target. |
| DS-3 Gated publication | `release.yml` `plan` job runs `release-set.sh` (plain tag, commit on `master`, non-empty changelog section through `release-notes.sh`, every supported line tagged and consistent); `verify` runs fmt/clippy/test for every line; packaging, builds and publication depend on them. The development line's `-dev` versions are never in `.github/lines`. |
| DS-4 User-facing notes only | Notes are hand-written per release (release skill step 3); the automatic changelog writer of the planning workflow is disabled (see Constraints). |
| DS-5 Separate, editor-only addon | `zip -r godot-mcp-addon-godot{M}.{N}.zip addons/godot_mcp` per line; export exclusion is in the addon (see [l2-godot-editor-addon.md](l2-godot-editor-addon.md)); README documents the disabled-plugin case. |
| DS-6 No machine-specific state | `.gitignore` excludes `.env*` (except `.env.example`), `.mcp.json`, `target/`, `bin/`, `dist/`, `.godot/`, `export_presets.cfg`, local binaries; README tells users to commit `.mcp.json.example` instead. |
| DS-7 Human confirmation | The release skill stops before any `git tag`/`git push` of a release and requires explicit approval; one approval lists every line's commit and every tag. |
| DS-9 A build names its commit | [ADDED] `build.rs` reads `git rev-parse --short=9 HEAD` (or the variable `GODOT_MCP_BUILD`, which wins) into `GODOT_MCP_BUILD`, and runs again when HEAD, the branch files or `packed-refs` change (found with `git rev-parse --git-path`, so a worktree works). `src/version.rs` `build_version()` adds it to `CARGO_PKG_VERSION` as `.g<id>` after existing build metadata, else `+g<id>`, only for 7 to 40 lowercase hexadecimal characters. The initialize answer's `serverInfo.version` and the feedback report's server version use it; the addon's version, read from `plugin.cfg`, has no commit. `tests/version_test.rs`. |
| DS-10 The release number follows the changes | [ADDED] The release skill (C step 3) takes the number from the `version=` of the first line of `.agents/skills/repo-steward/scripts/readiness.sh`, which reads the Conventional Commit types since the newest release tag. "Between Releases" in the skill moves `master`'s `-dev` number to it in a `chore(release)` commit of its own; `tests/version_sync_test.rs` checks that the skill names the script and the section, and that a `-dev` version is newer than the newest release. |
| DS-8 Scaffold-free artifacts | Binary and addon archives contain only built executables and `addons/godot_mcp`. The engine directories are git-ignored. `.gitattributes` marks the planning directory `export-ignore`, so GitHub's automatic source archives of a tag leave it out. |

## 5. Detailed Design

### 5.1 Files

```plaintext
.github/
├── godot-version               exact Godot build of this line, read by the editor job
├── workflows/ci.yml            fmt, clippy, tests (3 OS), editor job on the line's Godot build
├── workflows/release.yml       plan → per-line verify, addon, build → one GitHub Release
├── workflows/branch-cleanup.yml  deletes finished branches after a merge and weekly [ADDED]
├── scripts/prune-branches.sh   which branches are finished, and the deletion (dry run available) [ADDED]
├── scripts/release-notes.sh    print the changelog section for a tag; fail if missing/empty
├── scripts/release-set.sh      checks a release tag and prints its version and lines
└── lines                       supported Godot lines, one per line (master)
.agents/skills/release/SKILL.md carry, release, cut and retire procedures for agents
CHANGELOG.md                     user-facing notes, one section per version
Cargo.toml / Cargo.lock          server version, dependencies, release profile
addons/godot_mcp/plugin.cfg      addon version
.gitignore / .gitattributes      ignore rules, LF line endings
```

## 6. Implementation Notes

- `git archive HEAD | tar -t` listing no planning directory is the check for DS-8.

## 7. Drawbacks & Alternatives

- **release-please / changesets** for automated notes: would generate notes from commit messages, which here are often internal; hand-written notes stay user-focused.

## Canonical References

| Alias | Path | Purpose |
| --- | --- | --- |
| `[CI]` | `.github/workflows/ci.yml` | Push/PR pipeline |
| `[RELEASE]` | `.github/workflows/release.yml` | Tag-triggered release |
| `[NOTES]` | `.github/scripts/release-notes.sh` | Changelog section extraction |
| `[LINE]` | `.github/godot-version` | Godot build the line's editor job installs |
| `[SET]` | `.github/scripts/release-set.sh` | Release checks |
| `[BUILD]` | `build.rs` | The commit a build names (DS-9) |
| `[VERSION]` | `src/version.rs` | The reported version (DS-9) |
| `[NEXT]` | `.agents/skills/repo-steward/scripts/readiness.sh` | The number the commits imply (DS-10) |
| `[CLEANUP]` | `.github/workflows/branch-cleanup.yml` | Branch cleanup triggers and permissions |
| `[PRUNE]` | `.github/scripts/prune-branches.sh` | Which branches are finished; the protected ones |
| `[LINES]` | `.github/lines` | Supported lines |
| `[SKILL]` | `.agents/skills/release/SKILL.md` | Agent release procedure |
| `[CHANGELOG]` | `CHANGELOG.md` | Release notes |
| `[MANIFEST]` | `Cargo.toml` | Version and release profile |
| `[LOCK]` | `Cargo.lock` | Pinned dependency graph |
| `[IGNORE]` | `.gitignore` | Repository hygiene |
| `[ATTR]` | `.gitattributes` | Line endings, archive attributes |
| `[LICENSE]` | `LICENSE` | MIT license shipped with the source |

## Document History

| Version | Date | Author | Description |
| --- | --- | --- | --- |
| 1.0.0 | 2026-10-16 | Agent (magic.analyze first-time) | Reverse-engineered from the workflows, script and release skill at 94e4540; DS-8 gap recorded. |
| 1.0.1 | 2026-10-16 | Agent (magic.run) | DS-8 gap closed by the `export-ignore` attribute. |
| 1.1.0 | 2026-10-20 | Agent (magic.spec) | [MODIFIED] Line-aware: editor job reads `.github/godot-version`; DS-1, DS-3, DS-5, DS-7 rows per line; files and references updated. Stable → RFC → Stable after Post-Update Review. |
| 1.2.0 | 2026-10-20 | Agent (magic.spec) | [MODIFIED] DS-1, DS-3, DS-5, DS-7 and the files for one release holding every supported line; CI no longer checks the changelog (the release workflow does). |
| 1.2.1 | 2026-10-20 | Agent (magic.run) | [MODIFIED] Third-party actions pinned to commit SHAs, checked by a test; Dependabot for actions and Cargo. |
| 1.3.0 | 2026-11-02 | Agent (magic.spec) | Reality sync after ventilation. [ADDED] Branch hygiene: the cleanup workflow and its script, which branches they never delete, and their permissions. [MODIFIED] The editor job checks every addon script, the shader and import operation files included. Stable → RFC → Stable after Post-Update Review. |
| 1.4.0 | 2026-11-02 | Agent (magic.run) | [ADDED] The DS-9 and DS-10 rows: the commit in the reported version through `build.rs` and `src/version.rs`, and the release number from the readiness script with the development version moved by the release skill. `-dev` on `master` is now 0.13.0 (it said 0.12.1 while the commits since 0.12.0 held features). Stable → RFC → Stable after Post-Update Review. |
