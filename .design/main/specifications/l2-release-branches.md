# Release Branches & Backports

**Version:** 1.0.3
**Status:** Stable
**Layer:** implementation
**Implements:** l1-release-lines.md

## Overview

How the Godot release lines are realized in this repository: branch names, the version string, the files that carry a line's identity and the test that keeps them consistent, how CI picks the Godot build of a line, how the release workflow checks and names a line's release, how a change is carried between branches, and how the agent release skill walks a maintainer through carrying, releasing, cutting and retiring lines.

## Related Specifications

- [l1-release-lines.md](l1-release-lines.md) - Parent concept.
- [l2-ci-release-pipeline.md](l2-ci-release-pipeline.md) - The workflows and release files this specification makes line-aware.
- [l2-test-suite.md](l2-test-suite.md) - Where the line identity test lives.
- [l2-godot-editor-addon.md](l2-godot-editor-addon.md) - The addon's version gate and newer-version warning.
- [l2-community-health-files.md](l2-community-health-files.md) - The security policy and contributor guide that carry the line table.

## 1. Motivation

The concept fixes what a line is. This specification fixes the names, files and commands, so that an agent cutting a line, carrying a fix or releasing does the same thing every time and a mistake fails a check instead of reaching users.

## 2. Constraints & Assumptions

- Git and GitHub: branches, annotated tags, GitHub Releases published by the existing release workflow, `gh` for inspection.
- `M.N` is the Godot major.minor version of a line (`4.6`). Branch names: `godot-v{M}.{N}` (`godot-v4.5`, `godot-v4.6`, `godot-v4.7`). The development line is the default branch `master`. The `godot-` prefix keeps branch names apart from product tags (`v*`); `v{M}.{N}` follows Godot's own file naming (`Godot_v4.6-stable`).
- Version string (SemVer 2.0 with build metadata naming the line): `X.Y.Z+godot.M.N` on a release line (`0.5.0+godot.4.6`), `X.Y.Z-dev+godot.M.N` on the development line (`0.5.0-dev+godot.4.8`). Build metadata does not change SemVer precedence, which is why lines released together share `X.Y.Z`. Cargo accepts build metadata in a package version and passes it through `CARGO_PKG_VERSION`; the crate is not published to crates.io.
- Tag = `v` + version string (`v0.5.0+godot.4.6`). GitHub accepts `+` in tag names and release download URLs.
- [DR] Changes are carried with `git cherry-pick -x` (it appends `(cherry picked from commit <sha>)`), newest line first — Godot's own backport practice; merge-forward is rejected (parent §7).
- Changes to `.design/` are committed separately from product changes, so a product commit carries to a release line without touching planning files.
- A release commit (changelog section and version bump) is made on each line separately and is never carried.
- Repository settings (branch protection, default branch, private vulnerability reporting) are changed by a maintainer through the GitHub web UI or their own `gh` session; agents list the steps and do not change settings.

## 4. Invariant Compliance

| L1 Invariant | Implementation |
| --- | --- |
| RL-1 One line per Godot minor version | Branches `godot-v4.5`, `godot-v4.6`, `godot-v4.7`, each cut from `master`. A branch protection rule for `godot-v*` (maintainer setting, §5.8) forbids force pushes and deletion, because tags and published releases point into these branches. |
| RL-2 Development line on the default branch | `master` carries `X.Y.Z-dev+godot.4.8`. The release workflow refuses a version with a pre-release identifier and a tag whose commit is not on `origin/godot-v{M}.{N}` (§5.7). Cutting a line and moving `master` to the next Godot version is skill procedure E. |
| RL-3 The line is part of the version | One string in `Cargo.toml` and `addons/godot_mcp/plugin.cfg`; it reaches the `initialize` reply's `serverInfo.version` and the feedback report through `CARGO_PKG_VERSION`. Tag `v{version}`, release title `godot-mcp X.Y.Z (Godot M.N)`, changelog heading `## [X.Y.Z+godot.M.N] - YYYY-MM-DD`, file names with `-godot{M}.{N}` (§5.7). `tests/version_sync_test.rs` checks the two strings are equal and carry `+godot.M.N`. |
| RL-4 Line-exact runtime gate | `plugin.gd` `MIN_MAJOR`/`MIN_MINOR` equal the line (§5.2); an older engine version prints an error and does not start the bridge; a newer one prints a warning naming the line to install and starts. Details in [l2-godot-editor-addon.md](l2-godot-editor-addon.md). |
| RL-5 Line-exact verification | `.github/godot-version` holds the exact Godot build of the line (`4.6.3-stable`; on `master` `4.8-beta1`); the CI Godot job downloads that build. `tests/version_sync_test.rs` checks that its minor version equals the line. |
| RL-6 Route by affected lines | Line-only change: committed to that `godot-v*` branch only. Shared change: committed to the newest affected branch (`master` when affected), then applied with `git cherry-pick -x` to each older affected branch, newest to oldest (§5.4); a pull request for a carried copy is titled `[M.N] <original title>`. |
| RL-7 No silent gaps | Skill procedure C step 2 lists the product commits on newer branches that have no equivalent on the line (§5.5) and shows the list with the release approval request; the maintainer's approval confirms every entry left uncarried. |
| RL-8 One numbering for all lines | Skill procedure C step 3: the next number is computed from the highest `X.Y.Z` among all tags matching `v*` (line tags and the tags from before the lines), bumped by the kind of change; the release workflow refuses a tag that is not `v{version}`. |
| RL-9 Default download is the newest stable line | `make_latest` is `true` only when no existing tag `v*+godot.*` names a higher line than the tag being released (§5.7), so a freshly cut line takes "latest" with its first release, not before. `README.md` installation steps choose the release by the user's Godot version. |
| RL-10 One published line table | The same table in `README.md` (Installation), `SECURITY.md` (Supported Versions) and `CONTRIBUTING.md` (Branches and Godot Versions); procedures E and F change all three on `master` in one commit and carry it to every line. |
| RL-11 Few line-specific files | The identity files are exactly those in §5.2. Forced differences are listed in §5.3 of this specification, which lives on `master`. |
| RL-12 Retirement by decision | Skill procedure F: on a maintainer's decision, mark the line retired in the three tables; the branch, its protection rule and its releases stay. |

## 5. Detailed Design

### 5.1 Branch Map

| Branch | Line | Version string (first) | `.github/godot-version` | GitHub "latest" |
| --- | --- | --- | --- | --- |
| `godot-v4.5` | 4.5 | `0.5.0+godot.4.5` | `4.5.2-stable` | no |
| `godot-v4.6` | 4.6 | `0.5.0+godot.4.6` | `4.6.3-stable` | no |
| `godot-v4.7` | 4.7 | `0.5.0+godot.4.7` | `4.7.2-stable` | yes |
| `master` | 4.8 (development) | `0.5.0-dev+godot.4.8` | `4.8-beta1` | never released |

The `godot-version` values are the newest builds of each line on 2026-10-20; procedure G moves them forward.

### 5.2 Line Identity Files

```plaintext
Cargo.toml                    [package] version = "X.Y.Z+godot.M.N"   (development: "X.Y.Z-dev+godot.M.N")
Cargo.lock                    the godot-mcp package entry, updated by cargo
addons/godot_mcp/plugin.cfg   version="<same string>"
addons/godot_mcp/plugin.gd    MIN_MAJOR = M, MIN_MINOR = N
project.godot                 config/features=PackedStringArray("M.N")
.github/godot-version         exact Godot build verified against, e.g. 4.6.3-stable or 4.8-beta1
CHANGELOG.md                  the line's own release sections (master: history up to 0.4.0 and a
                              note that later releases are listed per line and on the Releases page)
```

`tests/version_sync_test.rs` (default test run, no engine needed) checks on every branch:

- the version in `Cargo.toml` equals the version in `plugin.cfg` (DS-1);
- the version has build metadata `godot.M.N`, and a pre-release identifier only if it is `dev`;
- `MIN_MAJOR`/`MIN_MINOR` in `plugin.gd` equal `M`/`N`;
- the first entry of `config/features` in `project.godot` equals `M.N`;
- `.github/godot-version` starts with `M.N` followed by `.`, `-` or end of text.

### 5.3 Line Deviations

Differences forced by a Godot version, beyond the identity files. Kept here on `master`; a deviation is added in the same change that introduces it.

| Area | 4.5 | 4.6 | 4.7 | 4.8 (development) | Reason |
| --- | --- | --- | --- | --- | --- |
| Import-state check in `bridge_server.gd` (`is_import_running`) | `has_method` guard | `has_method` guard | direct call | direct call | `EditorFileSystem.is_importing()` exists from 4.7 |

### 5.4 Carrying a Change

```plaintext
[REFERENCE] shared fix, affects master, 4.7 and 4.6
  1. land the fix and its test on master, CI green                     -> commit F
  2. git switch godot-v4.7
     git cherry-pick -x F                                               (or a PR "[4.7] <title>")
  3. same for godot-v4.6
  4. resolve a conflict on that line; the test must pass against the line's Godot build
  5. a line on which the test does not reproduce the bug gets no copy
```

A change that touches `.design/` and product files in one commit is split before carrying; only the product commit is carried.

### 5.5 Finding Uncarried Changes (RL-7)

```plaintext
[REFERENCE] before releasing godot-v4.6, previous release tag P (the cut commit for a first release)
  for each newer branch B in (godot-v4.7, master):
    candidates = commits on B committed after P, excluding commits that touch only .design/
                 and release commits
    git cherry -v godot-v4.6 B $(git merge-base godot-v4.6 B)
      "-" = an equivalent patch is already on godot-v4.6 -> drop from candidates
    git log godot-v4.6 --grep "cherry picked from commit <sha>"
      a match = carried with a conflict -> drop from candidates
  the remaining candidates are the uncarried changes
```

The remaining list is shown with the release approval request (procedure C step 6).

### 5.6 Release Skill Structure

`.agents/skills/release/SKILL.md` is rewritten around the lines. Its description triggers on releasing, backporting or carrying a fix, cutting a branch for a new Godot version, and retiring a line. Procedures:

| Procedure | Purpose | Ends with |
| --- | --- | --- |
| A. Find the affected lines | Run the change's test on each candidate branch | the list of lines to change |
| B. Land and carry | Newest affected branch first, then `cherry-pick -x` newest to oldest (§5.4) | green CI on every affected branch |
| C. Release one line | 1 preflight on the line's Godot build, including that the security policy and contributor guide are still true · 2 uncarried-change check (§5.5) · 3 next number (RL-8) · 4 changelog section · 5 bump both manifests and commit · 6 approval showing version, commit and the uncarried list · 7 tag `v{version}` and push · 8 watch the run and verify the release | published release |
| D. Release several lines | One number for all; C per line; one approval listing every tag and commit | one release per line |
| E. Cut a line | When Godot M.N becomes stable: branch `godot-vM.N` from `master`, set identity (§5.2), drop guards M.N no longer needs (§5.3), update the three line tables on `master` and carry the commit to every line, move `master` to `M.(N+1)` and its newest pre-release | new branch with green CI; first release through C after approval |
| F. Retire a line | Maintainer decision; mark retired in the three tables; nothing deleted | updated tables on every line |
| G. Move to a new Godot patch | Update `.github/godot-version`, run the editor tests, fix and release through C only if needed | updated engine build |

Standing rules in the skill: tagging and pushing need the explicit approval of a person who has seen every version and commit (DS-7); no force push, no tag deletion, no release deletion; a failed release is fixed forward with the next number.

### 5.7 Workflows per Line

`.github/workflows/ci.yml`:

- Runs on pushes to `master` and `godot-v*` and on every pull request.
- The Godot job reads `.github/godot-version` and downloads `Godot_v{build}_linux.x86_64.zip` from the `godotengine/godot-builds` release `{build}` (stable and pre-release builds share this pattern).
- The changelog check runs only when the version has no pre-release identifier (it is skipped on `master`).

`.github/workflows/release.yml`:

- Triggers on tags matching `v*.*.*godot.*`: a GitHub filter pattern cannot contain `+` (it is a quantifier, and a pattern with it invalidates the whole workflow file), so the exact tag form is enforced by `release-line.sh` in the verify job.
- The verify job also checks: the version has build metadata `godot.M.N` and no pre-release identifier; the tagged commit is contained in `origin/godot-vM.N`.
- File names: `godot-mcp-{target}-godot{M}.{N}.{zip|tar.gz}` and `godot-mcp-addon-godot{M}.{N}.zip`.
- The line checks, the file-name suffix and the "latest" decision live in `.github/scripts/release-line.sh` (argument: the tag; prints the line, the suffix and `true`/`false` for latest; exits non-zero naming the violated rule), so they run locally exactly as in the workflow, like `release-notes.sh`.
- Release title `godot-mcp X.Y.Z (Godot M.N)` (users find a line's release by the tag ending in `+godot.M.N`, never by the title); `make_latest: true` only when no existing tag `v*+godot.*` names a line higher than `M.N`, otherwise `false`.

### 5.8 Repository Settings (Maintainer)

| Setting | Value |
| --- | --- |
| Default branch | `master` (unchanged) |
| Branch protection `godot-v*` | no force push, no deletion |
| Private vulnerability reporting | enabled (see [l2-community-health-files.md](l2-community-health-files.md)) |

## 6. Implementation Notes

1. Finish the work pending on `master`; it is then inherited by every line instead of being carried three times.
2. One change on `master`: line identity 4.8 (§5.2), `.github/godot-version`, the extended `tests/version_sync_test.rs`, the line-aware workflows (§5.7), the rewritten release skill (§5.6), the line table in `README.md`, `SECURITY.md` and `CONTRIBUTING.md`, the `AGENTS.md` wording that named 4.5 as the only tested version.
3. Cut `godot-v4.5`, `godot-v4.6` and `godot-v4.7` from that commit; on each, one commit sets the identity files to its line.
4. On `master` and `godot-v4.7`, replace the import-state guard with a direct call (§5.3); verify with the editor tests on 4.8-beta1 and 4.7.2-stable.
5. Run the full pipeline on every branch locally, including the editor tests against the branch's Godot build.
6. Stop for approval: pushing the three branches, then the repository settings (§5.8), then the first releases 0.5.0 through procedure D.

## 7. Drawbacks & Alternatives

- **`git cherry` for RL-7 misses changes carried with conflicts**, whose patch differs; the `-x` line is the fallback match (§5.5). A tracking label per line was considered and rejected: it needs upkeep on every pull request.
- **Godot build derived from the version string instead of `.github/godot-version`:** one file fewer, but the exact patch build (`4.6.3-stable`) is not part of the version, and moving to a new Godot patch would need a product version change.
- **`v4.6` as branch names:** shorter, but reads like a product tag in the `v*` namespace.

## Canonical References

| Alias | Path | Purpose |
| --- | --- | --- |
| `[MANIFEST]` | `Cargo.toml` | Line version string |
| `[ADDON]` | `addons/godot_mcp/plugin.cfg` | Addon version string |
| `[GATE]` | `addons/godot_mcp/plugin.gd` | Line minimum engine version and newer-version warning |
| `[PROJECT]` | `project.godot` | Development project's engine feature tag |
| `[LINE]` | `.github/godot-version` | Exact Godot build the line is verified against |
| `[SYNC]` | `tests/version_sync_test.rs` | Line identity consistency |
| `[CI]` | `.github/workflows/ci.yml` | Per-line pipeline |
| `[RELEASE]` | `.github/workflows/release.yml` | Per-line release checks, names and "latest" |
| `[LINESH]` | `.github/scripts/release-line.sh` | Line checks shared by the workflow and local runs |
| `[SKILL]` | `.agents/skills/release/SKILL.md` | Carry, release, cut, retire procedures |

## Document History

| Version | Date | Author | Description |
| --- | --- | --- | --- |
| 1.0.0 | 2026-10-20 | Agent (magic.spec) | Created with the release lines concept; branch names, version string, identity files, carrying, workflows and skill structure. Promoted to Stable after Post-Update Review. |
| 1.0.1 | 2026-10-20 | Agent (magic.task) | Line checks of the release workflow placed in a script that also runs locally. |
| 1.0.2 | 2026-10-20 | Agent (magic.run) | [MODIFIED] Tag trigger without `+`: GitHub rejected the workflow file with it (found at the first release). |
| 1.0.3 | 2026-10-20 | Agent (magic.spec) | [MODIFIED] Release title `godot-mcp X.Y.Z (Godot M.N)`; installers select a line's release by its tag, not by its title. |
