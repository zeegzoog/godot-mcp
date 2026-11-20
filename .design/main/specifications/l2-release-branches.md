# Supported Versions & Release Procedure

**Version:** 2.0.0
**Status:** Stable
**Layer:** implementation
**Implements:** l1-release-lines.md

## Overview

> The file name is historical: until 2026-11-20 this specification described release branches and backports. It now describes the single build.

How the supported range of Godot versions is realized in this repository: the one branch, the version string, the tags, the files that declare the range and the test that keeps them consistent, how CI runs every supported Godot build, how the release workflow builds one release from the default branch, the guards that make one build portable, and how the agent release skill walks a maintainer through landing a change, releasing, supporting a new Godot version and dropping the oldest.

## Related Specifications

- [l1-release-lines.md](l1-release-lines.md) - Parent concept.
- [l2-ci-release-pipeline.md](l2-ci-release-pipeline.md) - The workflows and release files this specification describes.
- [l2-test-suite.md](l2-test-suite.md) - Where the range consistency test lives.
- [l2-godot-editor-addon.md](l2-godot-editor-addon.md) - The addon's version gate and newer-version warning.
- [l2-community-health-files.md](l2-community-health-files.md) - The security policy and contributor guide that publish the range.

## 1. Motivation

The concept fixes what the range is and how a change is proven against it. This specification fixes the names, files and commands, so that an agent landing a change, supporting a new Godot version or releasing does the same thing every time and a mistake fails a check instead of reaching users.

## 2. Constraints & Assumptions

- Git and GitHub: one default branch `master`, annotated tags, GitHub Releases published by the release workflow, `gh` for inspection.
- Version string: SemVer 2.0 without build metadata, `X.Y.Z` in a release and `X.Y.Z-dev` on `master` between releases, in `Cargo.toml` and `addons/godot_mcp/plugin.cfg`. A running server adds the commit it was built from (`0.16.1-dev+g6295b51ab`, DS-9); the manifests carry none. Cargo does not publish the crate to crates.io.
- Tags: the release tag is `vX.Y.Z` on the `master` commit that holds the release notes and the manifests at `X.Y.Z`; it starts the release workflow and names the release. No other tag shape starts a release.
- The release notes of all versions are one changelog, `CHANGELOG.md`, with a section `## [X.Y.Z] - YYYY-MM-DD` per release.
- [DR] Releases made before this model are history: `godot-v4.5`, `godot-v4.6` and `godot-v4.7` (retired release lines of 0.5.1 to 0.15.0), their tags `vX.Y.Z+godot.M.N` (and `v0.5.0+godot.M.N`), and the releases built from them. They stay as they are and are never deleted or changed; the skill's `Never force-push, delete a tag or delete a release` covers them.
- Changes to `.design/` are committed separately from product changes.
- A release commit (the changelog section and the manifests at the release number) and the commit that moves `master` to the next `-dev` number are their own commits, with the subjects `chore(release): godot-mcp X.Y.Z` and `chore(release): move master to the next development version`.
- Repository settings (branch protection, rulesets, default branch, private vulnerability reporting) are changed by a maintainer through the GitHub web UI or their own `gh` session; agents list the steps and do not change settings.

## 4. Invariant Compliance

| L1 Invariant | Implementation |
| --- | --- |
| RL-1 One codebase | Work and releases are on `master`. `release-set.sh` refuses a release tag whose commit is not on `origin/master`. The retired `godot-v*` branches receive no commits; a branch protection rule for them (maintainer setting, §5.8) forbids force pushes and deletion, and `prune-branches.sh` never deletes them. |
| RL-2 One declared range | `plugin.gd` constants `MIN_MAJOR`, `MIN_MINOR`, `TESTED_MINOR` (§5.2). `tests/version_sync_test.rs` checks every other statement of the range against them (§5.2). |
| RL-3 The version names no Godot version | One string in `Cargo.toml` and `addons/godot_mcp/plugin.cfg`; it reaches the `initialize` reply's `serverInfo.version` and the feedback report through `CARGO_PKG_VERSION` and `src/version.rs`. Release tag and title `vX.Y.Z`, changelog heading `## [X.Y.Z] - YYYY-MM-DD`, file names without a Godot version (§5.6). `tests/version_sync_test.rs` checks that the two strings are equal, carry no build metadata, and have no pre-release identifier but `dev`. |
| RL-4 Range-exact runtime gate | `plugin.gd`: an editor older than `MIN_MAJOR`.`MIN_MINOR` prints an error and does not start the bridge; an editor newer than `TESTED_MINOR` (or of a newer major version) prints a warning that names the tested range and starts the bridge. Details in [l2-godot-editor-addon.md](l2-godot-editor-addon.md). |
| RL-5 Verified on every supported version | `ci.yml`: the `godot` job runs on the build in `.github/godot-version` (the newest tested version) and the `godot-versions` matrix runs on the builds of every older supported minor version; each installs the build from the official builds repository, imports the development project, checks every addon script and runs the editor, addon-export, running-game, scenario and export suites (§5.5). `tests/version_sync_test.rs` checks that the matrix holds every minor version from `MIN_MINOR` up to the one before `TESTED_MINOR`, and that `.github/godot-version` is of `TESTED_MINOR`. |
| RL-6 One portable change | The guard convention of §5.3, written in `AGENTS.md` and `CONTRIBUTING.md`; skill procedure A step 2. Behaviour that really differs names its version in the test (`godot_minor()` in `tests/runtime_e2e_test.rs`). |
| RL-7 Nothing unverified is released | Skill procedure B step 1 stops unless CI is green on the tip of `master`, which is the commit to release (the check compares the run's `headSha` with `origin/master`); step 6 waits for CI on the release commit before the tag is pushed. The release workflow's `verify` job runs format, lint and tests again on the tagged commit. |
| RL-8 One release, one number | Skill procedure B step 3 takes the number from the first line of `.agents/skills/repo-steward/scripts/readiness.sh` (DS-10); `release-set.sh` checks that the tag is `vX.Y.Z`, on `origin/master`, with a non-empty changelog section and both manifests at exactly `X.Y.Z`; the workflow then builds once (§5.6). |
| RL-9 The latest release serves every supported version | The workflow publishes one release with `make_latest: true` holding five files. `README.md` installation steps take the latest release's archive for the user's platform and the addon archive; no step selects a file by Godot version. |
| RL-10 One published range | The same range text in `README.md` (Installation), `SECURITY.md` (Supported Versions) and `CONTRIBUTING.md` (Branch and Godot Versions); skill procedures C and D change all three in one commit. |
| RL-11 Few places hold the range | The files of §5.2 are the only product files that name it; forced version differences in code are listed in §5.3. |
| RL-12 Support ends by decision; retired lines stay as history | Skill procedure D, on a maintainer's decision: raise `MIN_MINOR`, move the development project's feature tag, drop the builds from the matrix, update the three documents, release as a feature. The retired branches, tags and releases are not touched (Constraints). |

## 5. Detailed Design

### 5.1 Branches and Tags

| Ref | Role |
| --- | --- |
| `master` | The only branch for work; every release is made from it |
| `vX.Y.Z` | Release tag on a `master` commit; starts the release workflow |
| `godot-v4.5`, `godot-v4.6`, `godot-v4.7` | Retired release lines of releases 0.5.1 to 0.15.0; kept as history, no commits |
| `vX.Y.Z+godot.M.N`, `v0.5.0+godot.M.N` | Tags of the retired lines; never match the release workflow's tag filter |

### 5.2 Where the Range Is Kept

```plaintext
addons/godot_mcp/plugin.gd     MIN_MAJOR = 4, MIN_MINOR = 5 (oldest supported), TESTED_MINOR = 8 (newest tested)
project.godot                  config/features first entry "4.<MIN_MINOR>": the development project is saved with the oldest supported Godot
.github/godot-version          engine build of the newest tested version (4.8-beta2): CI's main job
.github/workflows/ci.yml       godot-versions matrix: the builds of every older supported minor version (4.5.2-stable, 4.6.3-stable, 4.7.2-stable)
Cargo.toml, Cargo.lock         version = "X.Y.Z" (development: "X.Y.Z-dev")
addons/godot_mcp/plugin.cfg    version="<same string>"
```

`tests/version_sync_test.rs` (default test run, no engine needed) checks:

- the version in `Cargo.toml` equals the version in `plugin.cfg` (DS-1);
- the version is `X.Y.Z` or `X.Y.Z-dev`, with no build metadata;
- `MIN_MAJOR` is 4 and `MIN_MINOR` is not above `TESTED_MINOR`;
- the first `config/features` entry of `project.godot` is `4.<MIN_MINOR>`, and the matrix contains `MIN_MINOR`;
- the minor version of `.github/godot-version` is `TESTED_MINOR`, the matrix contains every minor version from `MIN_MINOR` up to the one before it, and does not contain `TESTED_MINOR` itself (it runs once, in the main job);
- the release workflow's tag filter matches plain version tags only (§5.6);
- the release skill takes the number from the readiness script and moves `master` to the next development version, and a `-dev` version is newer than the newest release in the changelog.

### 5.3 Version-Dependent Forms

The guard convention: what the first release of the oldest supported version (4.5.0) lacks is called through `has_method()` + `call()`; an answer field that only newer versions can fill is optional. A form that depends on the Godot version is listed here in the same change that introduces it.

| Area | Form | Reason |
| --- | --- | --- |
| Import-state check in `bridge_server.gd` (`is_import_running`) | `has_method` guard | `EditorFileSystem.is_importing()` exists from 4.7 |
| Script-editor pairing | tab name compared only where the editor names its tab | the `_tab_name` metadata exists in 4.8 and not in 4.5 ([l2-godot-editor-addon.md](l2-godot-editor-addon.md)) |

### 5.4 Landing a Change

```plaintext
[REFERENCE] any change, one pass
  1. write the test first; make the change work on every supported Godot version (guard, optional field)
  2. pull request to master; CI runs the Rust checks on three systems and the end-to-end suites on every supported build
  3. a red job names the Godot build; fix it in the same pull request (a version-specific behaviour gets a test that names the version)
  4. merge; nothing is carried anywhere
```

### 5.5 CI

`.github/workflows/ci.yml` runs on pushes to `master` and `godot-v*` (the retired lines keep their own CI) and on every pull request.

| Job | What it runs |
| --- | --- |
| `rust` | format, lint and tests on `ubuntu-24.04`, `windows-latest` and `macos-latest` |
| `godot` | the newest tested Godot (the build in `.github/godot-version`): import, syntax check of every addon script, then the editor, addon-export, running-game, scenario and export suites |
| `godot-versions` | the same on `4.5.2-stable`, `4.6.3-stable` and `4.7.2-stable`, one job each; the syntax check walks every `.gd` file under `addons/godot_mcp` |

### 5.6 Release Workflow

`.github/workflows/release.yml` triggers on the pattern `v[0-9]+.[0-9]+.[0-9]+` only. A `+` is valid after a character class and invalid after `*`: a pattern with `*+` makes GitHub reject the whole file, which `tests/version_sync_test.rs` guards against.

- `plan` runs `.github/scripts/release-set.sh vX.Y.Z` (full history): the tag is exactly `vX.Y.Z`, its commit is on `origin/master`, `release-notes.sh` finds a non-empty `## [X.Y.Z]` section, and both manifests at the tag say `X.Y.Z`. It prints `version=X.Y.Z` and exits non-zero naming the violated rule, so it runs locally exactly as in the workflow; `tests/steward_test.rs` drives it on a throwaway repository.
- `verify`: format, lint and tests on the tagged commit.
- `addon`: `godot-mcp-addon.zip` (`addons/godot_mcp` only).
- `build`: one job per target (`x86_64-pc-windows-msvc`, `x86_64-unknown-linux-gnu`, `x86_64-apple-darwin`, `aarch64-apple-darwin`) runs `cargo build --release --locked` and packages `godot-mcp-<target>.zip` (Windows) or `godot-mcp-<target>.tar.gz`.
- `publish` collects the five artifacts and creates one release named by the tag, with the changelog section as its text and `make_latest: true`.

### 5.7 Release Skill Structure

`.agents/skills/release/SKILL.md` describes the model first (one codebase, the version, the release, numbering, retired lines) and then four procedures; its description triggers on releasing, supporting a new Godot version and dropping the oldest.

| Procedure | Purpose | Ends with |
| --- | --- | --- |
| A. Land a change | A pull request to `master` with its test; CI green on every supported Godot build; a version-dependent behaviour written to work on all and tested by version; `.design/` commits separate | the change on `master` |
| B. Release | 1 preflight: nothing uncommitted, CI green on the tip of `master`, `SECURITY.md` and `CONTRIBUTING.md` still true · 2 the product commits since the previous release tag · 3 the number from the readiness script (a number the user names wins if it is higher) · 4 the release commit: changelog section, both manifests at `X.Y.Z` · 5 approval by a person who has seen the version, the commit and the commands (DS-7) · 6 push `master`, wait for green CI, tag `vX.Y.Z`, run `release-set.sh` locally, push the tag · 7 watch the run and verify the release holds the five files · 8 move `master` to the next `-dev` number | one published release, `master` on the next development number |
| C. Support a new Godot version | Make `master` pass on it with the older versions still working; in one commit raise `TESTED_MINOR`, move the previous newest build into the matrix, set `.github/godot-version`; update the three documents and the changelog | the range extended, checked by `tests/version_sync_test.rs` |
| D. Drop the oldest | Maintainer decision only: raise `MIN_MINOR`, move the feature tag of `project.godot`, remove the build from the matrix, update the three documents and the changelog, release as a feature | the range shortened; the last release that supports the version stays available |

Standing rules in the skill: tagging and pushing need the explicit approval of a person who has seen the version and the commit (DS-7); no force push, no tag deletion, no release deletion; a failed release is fixed forward with the next number; a version with `-dev` is never released.

### 5.8 Repository Settings (Maintainer)

| Setting | Value |
| --- | --- |
| Default branch | `master` (unchanged) |
| Branch protection `godot-v*` | no force push, no deletion (the retired lines) |
| Required checks on `master` | the jobs named in [l2-repo-steward-godot-mcp.md](l2-repo-steward-godot-mcp.md) §5.11, which include every `godot-versions` matrix job |
| Private vulnerability reporting | enabled (see [l2-community-health-files.md](l2-community-health-files.md)) |

## 6. Implementation Notes

1. The change of 2026-11-20 was one commit on `master`: `MIN_MINOR` 5 and the new `TESTED_MINOR`, the development project saved with 4.5, the version without build metadata, one build in the release workflow, the `godot-versions` matrix and its checks, the removal of the carry workflow and script and of `.github/lines`, and the matching rewrite of the release skill, the guides and the steward profile.
2. The retired branches, tags and releases were left in place; the first release from `master` follows 0.15.0.
3. Release 0.5.0 was published once per Godot version and withdrawn; from 0.5.1 to 0.15.0 a release was one release holding the files of every line (named with `-godotM.N`); from the first release after 0.15.0 the files carry no Godot version.

## 7. Drawbacks & Alternatives

- **The release workflow does not rerun the end-to-end suites:** they need the Godot builds and take long; the skill requires green CI on the release commit before the tag is pushed, and `verify` repeats the cheap checks. Rerunning them in the workflow would double the cost of every release for a check that already passed on the same commit.
- **The newest tested version may be a pre-release:** its build can change under the same name; `.github/godot-version` names the exact build and moves only through a commit.
- **A matrix entry per Godot patch release:** more coverage, more jobs; one build per minor version is enough because patch releases keep the editor API.
- **Keeping the line branches as active:** they would invite fixes that never reach users of other versions; retired and protected instead.

## Canonical References

| Alias | Path | Purpose |
| --- | --- | --- |
| `[MANIFEST]` | `Cargo.toml` | Version string |
| `[ADDON]` | `addons/godot_mcp/plugin.cfg` | Addon version string |
| `[GATE]` | `addons/godot_mcp/plugin.gd` | Oldest supported and newest tested minor version, the version gate |
| `[PROJECT]` | `project.godot` | Development project's engine feature tag |
| `[BUILD]` | `.github/godot-version` | Engine build of the newest tested version |
| `[SYNC]` | `tests/version_sync_test.rs` | Consistency of the range and the version |
| `[CI]` | `.github/workflows/ci.yml` | Suites on every supported version |
| `[RELEASE]` | `.github/workflows/release.yml` | Plan, verify, build once, publish |
| `[SET]` | `.github/scripts/release-set.sh` | Release checks shared by the workflow and local runs |
| `[SKILL]` | `.agents/skills/release/SKILL.md` | Land, release, support a new version, drop the oldest |

## Document History

| Version | Date | Author | Description |
| --- | --- | --- | --- |
| 1.0.0 | 2026-10-20 | Agent (magic.spec) | Created with the release lines concept; branch names, version string, identity files, carrying, workflows and skill structure. Promoted to Stable after Post-Update Review. |
| 1.0.1 | 2026-10-20 | Agent (magic.task) | Line checks of the release workflow placed in a script that also runs locally. |
| 1.0.2 | 2026-10-20 | Agent (magic.run) | [MODIFIED] Tag trigger without `+`: GitHub rejected the workflow file with it (found at the first release). |
| 1.0.3 | 2026-10-20 | Agent (magic.spec) | [MODIFIED] Release title `godot-mcp X.Y.Z (Godot M.N)`; installers select a line's release by its tag, not by its title. |
| 1.1.0 | 2026-10-20 | Agent (magic.spec) | [MODIFIED] One release per number holding the files of every supported line: release tag `vX.Y.Z`, line tags as anchors, `.github/lines`, `release-set.sh`, plan/verify/build/publish workflow, procedures C and D, one changelog on `master`. Replaces one release per line (0.5.0), after comparing with Godot add-ons that ship binaries. |
| 2.0.0 | 2026-11-20 | Agent (magic.spec) | Rewritten (major) with the parent for one build: no branch per Godot version, no line tags, no `-godotM.N` file names, no `+godot.M.N` version metadata, no carry procedure, no `.github/lines`, no per-line deviations table; added the range files and their consistency test, the `godot-versions` CI matrix, a release made once from `master`, and the skill procedures A land, B release, C support a new Godot version, D drop the oldest; the retired branches, tags and releases are history. The whole file was replaced rather than edited in sections (its sections, tables and invariants all changed; it was 206 lines). RL-n rows follow the parent's renumbered meanings. Stable → RFC by the parent's amendment (C12) → Stable after Post-Update Review. |
