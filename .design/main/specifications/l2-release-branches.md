# Release Branches & Backports

**Version:** 1.1.0
**Status:** Stable
**Layer:** implementation
**Implements:** l1-release-lines.md

## Overview

How the Godot release lines are realized in this repository: branch names, the version string, the tags, the files that carry a line's identity and the test that keeps them consistent, how CI picks the Godot build of a line, how the release workflow builds every line into one release, how a change is carried between branches, and how the agent release skill walks a maintainer through carrying, releasing, cutting and retiring lines.

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
- Version string (SemVer 2.0 with build metadata naming the line): `X.Y.Z+godot.M.N` on a release line (`0.5.1+godot.4.6`), `X.Y.Z-dev+godot.M.N` on the development line (`0.5.1-dev+godot.4.8`). Build metadata does not change SemVer precedence, which is why lines released together share `X.Y.Z`. Cargo accepts build metadata in a package version and passes it through `CARGO_PKG_VERSION`; the crate is not published to crates.io.
- Two kinds of tags. A **line tag** is `v` + the line's version string (`v0.5.1+godot.4.6`) on the line's release commit; it anchors that line's sources and starts nothing. The **release tag** is the plain number `vX.Y.Z` (`v0.5.1`) on the `master` commit that holds the release notes; it starts the release workflow and names the release. GitHub accepts `+` in tag names and download URLs.
- The release notes of all lines are one changelog on `master`, with a section `## [X.Y.Z] - YYYY-MM-DD` per release. The `CHANGELOG.md` of a release branch is frozen at the history of its first release.
- `.github/lines` on `master` lists the supported lines, one `M.N` per line; a release contains exactly these. Procedures E and F edit it.
- [DR] Changes are carried with `git cherry-pick -x` (it appends `(cherry picked from commit <sha>)`), newest line first — Godot's own backport practice; merge-forward is rejected (parent §7).
- Changes to `.design/` are committed separately from product changes, so a product commit carries to a release line without touching planning files.
- A release commit (the version bump of a line, the changelog section on `master`) is made on each branch separately and is never carried.
- Repository settings (branch protection, default branch, private vulnerability reporting) are changed by a maintainer through the GitHub web UI or their own `gh` session; agents list the steps and do not change settings.

## 4. Invariant Compliance

| L1 Invariant | Implementation |
| --- | --- |
| RL-1 One line per Godot minor version | Branches `godot-v4.5`, `godot-v4.6`, `godot-v4.7`, each cut from `master`. A branch protection rule for `godot-v*` (maintainer setting, §5.8) forbids force pushes and deletion, because tags and published releases point into these branches. |
| RL-2 Development line on the default branch | `master` carries `X.Y.Z-dev+godot.4.8` and is not in `.github/lines`, so no release is built from it. `release-set.sh` refuses a release tag whose commit is not on `origin/master` and a line tag whose commit is not on its branch (§5.7). Cutting a line and moving `master` to the next Godot version is skill procedure E. |
| RL-3 The line is part of the version | One string in `Cargo.toml` and `addons/godot_mcp/plugin.cfg`; it reaches the `initialize` reply's `serverInfo.version` and the feedback report through `CARGO_PKG_VERSION`. Line tag `v{version}`, release tag and title `vX.Y.Z`, changelog heading `## [X.Y.Z] - YYYY-MM-DD`, file names with `-godot{M}.{N}` (§5.7). `tests/version_sync_test.rs` checks the two strings are equal and carry `+godot.M.N`. |
| RL-4 Line-exact runtime gate | `plugin.gd` `MIN_MAJOR`/`MIN_MINOR` equal the line (§5.2); an older engine version prints an error and does not start the bridge; a newer one prints a warning naming the line to install and starts. Details in [l2-godot-editor-addon.md](l2-godot-editor-addon.md). |
| RL-5 Line-exact verification | `.github/godot-version` holds the exact Godot build of the line (`4.6.3-stable`; on `master` `4.8-beta1`); the CI Godot job downloads that build. `tests/version_sync_test.rs` checks that its minor version equals the line. |
| RL-6 Route by affected lines | Line-only change: committed to that `godot-v*` branch only. Shared change: committed to the newest affected branch (`master` when affected), then applied with `git cherry-pick -x` to each older affected branch, newest to oldest (§5.4); a pull request for a carried copy is titled `[M.N] <original title>`. |
| RL-7 No silent gaps | Skill procedure C step 2 lists, for each line, the product commits on newer branches that have no equivalent on it (§5.5) and shows the lists with the release approval request; the maintainer's approval confirms every entry left uncarried. |
| RL-8 One release, all lines | Skill procedure C step 3: the next number is computed from the highest `X.Y.Z` among all tags (release tags, line tags, and the tags from before the lines), bumped by the kind of change. `release-set.sh` refuses a release unless every line in `.github/lines` has the line tag `vX.Y.Z+godot.M.N` on its branch with both manifests at exactly that version; the workflow then builds every line (§5.7). |
| RL-9 The latest release has every line | The workflow publishes one release with `make_latest: true` holding the files of every line in `.github/lines`. `README.md` installation steps take the latest release and the files ending in `-godot{M}.{N}` for the user's Godot version. |
| RL-10 One published line table | The same table in `README.md` (Installation), `SECURITY.md` (Supported Versions) and `CONTRIBUTING.md` (Branches and Godot Versions); procedures E and F change all three, and `.github/lines`, on `master` in one commit and carry the three documents to every line. |
| RL-11 Few line-specific files | The identity files are exactly those in §5.2. Forced differences are listed in §5.3 of this specification, which lives on `master`. |
| RL-12 Retirement by decision | Skill procedure F: on a maintainer's decision, remove the line from `.github/lines` (it leaves the next release) and mark it retired in the three tables; the branch, its protection rule and its releases stay. |

## 5. Detailed Design

### 5.1 Branch Map

| Branch | Line | Version string | `.github/godot-version` | In releases |
| --- | --- | --- | --- | --- |
| `godot-v4.5` | 4.5 | `X.Y.Z+godot.4.5` | `4.5.2-stable` | yes |
| `godot-v4.6` | 4.6 | `X.Y.Z+godot.4.6` | `4.6.3-stable` | yes |
| `godot-v4.7` | 4.7 | `X.Y.Z+godot.4.7` | `4.7.2-stable` | yes |
| `master` | 4.8 (development) | `X.Y.Z-dev+godot.4.8` | `4.8-beta1` | no |

The `godot-version` values are the newest builds of each line on 2026-10-20; procedure G moves them forward.

### 5.2 Line Identity Files

```plaintext
Cargo.toml                    [package] version = "X.Y.Z+godot.M.N"   (development: "X.Y.Z-dev+godot.M.N")
Cargo.lock                    the godot-mcp package entry, updated by cargo
addons/godot_mcp/plugin.cfg   version="<same string>"
addons/godot_mcp/plugin.gd    MIN_MAJOR = M, MIN_MINOR = N
project.godot                 config/features=PackedStringArray("M.N")
.github/godot-version         exact Godot build verified against, e.g. 4.6.3-stable or 4.8-beta1
```

`CHANGELOG.md` is not an identity file: the release notes are one changelog on `master`.

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
[REFERENCE] before a release, for line 4.6: P = its previous line tag (the cut commit for a first release)
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
| C. Release | 1 preflight on every line with its Godot build, and the security policy and contributor guide still true · 2 uncarried-change check per line (§5.5) · 3 next number (RL-8) · 4 on each line a release commit that sets both manifests to `X.Y.Z+godot.M.N` (a line without a change of its own gets only this) · 5 on `master` the changelog section and its release commit · 6 approval showing the version, every line's commit and the uncarried lists · 7 push the branches, wait for green CI, create and push the line tags, then the release tag `vX.Y.Z` on `master` · 8 watch the run and verify the release | one published release holding every line |
| D. Release after a change to one line | The same as C: the other lines only advance their version | one published release |
| E. Cut a line | When Godot M.N becomes stable: branch `godot-vM.N` from `master`, set identity (§5.2), drop guards M.N no longer needs (§5.3), add `M.N` to `.github/lines`, update the three line tables on `master` and carry them to every line, move `master` to `M.(N+1)` and its newest pre-release | new branch with green CI; the line joins the next release through C after approval |
| F. Retire a line | Maintainer decision; remove from `.github/lines`, mark retired in the three tables; nothing deleted | updated tables on every line |
| G. Move to a new Godot patch | Update `.github/godot-version`, run the editor tests, fix and release through C only if needed | updated engine build |

Standing rules in the skill: tagging and pushing need the explicit approval of a person who has seen every version and commit (DS-7); no force push, no tag deletion, no release deletion; a failed release is fixed forward with the next number.

### 5.7 Workflows

`.github/workflows/ci.yml`:

- Runs on pushes to `master` and `godot-v*` and on every pull request.
- The Godot job reads `.github/godot-version` and downloads `Godot_v{build}_linux.x86_64.zip` from the `godotengine/godot-builds` release `{build}` (stable and pre-release builds share this pattern).

`.github/workflows/release.yml`:

- Triggers on the pattern `v[0-9]+.[0-9]+.[0-9]+` (plain release tags). The line tags `vX.Y.Z+godot.M.N` do not match it. A `+` is valid after a character class and invalid after `*`: a pattern with `*+` makes GitHub reject the whole file, which `tests/version_sync_test.rs` guards against.
- `plan` runs `.github/scripts/release-set.sh vX.Y.Z` (full history): the tag is exactly `vX.Y.Z`, its commit is on `origin/master`, `release-notes.sh` finds a non-empty `## [X.Y.Z]` section, and every line in `.github/lines` has its branch, its line tag on that branch, and both manifests at `X.Y.Z+godot.M.N`. It outputs the version and the lines as JSON for the matrices, and exits non-zero naming the violated rule, so it also runs locally exactly as in the workflow.
- `verify` (one job per line, from the line tag): format, lint and tests.
- `addon` and `build` (per line, and per line and platform for `build`, from the line tag): package `godot-mcp-addon-godot{M}.{N}.zip` and `godot-mcp-{target}-godot{M}.{N}.{zip|tar.gz}` and keep them as workflow artifacts.
- `publish` collects every artifact and creates one release named by the tag, with the changelog section as its text and `make_latest: true`.

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
6. Stop for approval: pushing the three branches, then the repository settings (§5.8), then the first releases. Release 0.5.0 was made once per line (tags `v0.5.0+godot.M.N`, one release each, from the earlier design), then deleted on the maintainer's decision with its tags kept; from 0.5.1 on a release is one release (procedure C).

## 7. Drawbacks & Alternatives

- **`git cherry` for RL-7 misses changes carried with conflicts**, whose patch differs; the `-x` line is the fallback match (§5.5). A tracking label per line was considered and rejected: it needs upkeep on every pull request.
- **Godot build derived from the version string instead of `.github/godot-version`:** one file fewer, but the exact patch build (`4.6.3-stable`) is not part of the version, and moving to a new Godot patch would need a product version change.
- **Line tags that start a release each (the design of 0.5.0):** simple, but three releases per number; the release list is hard to read and "latest" holds one Godot version only. Replaced by one release tag.
- **A manifest-driven list of lines instead of `.github/lines`:** one file fewer, but a retired or not yet released line would be guessed from branch names; the explicit list makes cutting and retiring a visible change.
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
| `[RELEASE]` | `.github/workflows/release.yml` | Plan, per-line build, one release |
| `[SET]` | `.github/scripts/release-set.sh` | Release checks shared by the workflow and local runs |
| `[LINES]` | `.github/lines` | The supported lines every release contains |
| `[SKILL]` | `.agents/skills/release/SKILL.md` | Carry, release, cut, retire procedures |

## Document History

| Version | Date | Author | Description |
| --- | --- | --- | --- |
| 1.0.0 | 2026-10-20 | Agent (magic.spec) | Created with the release lines concept; branch names, version string, identity files, carrying, workflows and skill structure. Promoted to Stable after Post-Update Review. |
| 1.0.1 | 2026-10-20 | Agent (magic.task) | Line checks of the release workflow placed in a script that also runs locally. |
| 1.0.2 | 2026-10-20 | Agent (magic.run) | [MODIFIED] Tag trigger without `+`: GitHub rejected the workflow file with it (found at the first release). |
| 1.0.3 | 2026-10-20 | Agent (magic.spec) | [MODIFIED] Release title `godot-mcp X.Y.Z (Godot M.N)`; installers select a line's release by its tag, not by its title. |
| 1.1.0 | 2026-10-20 | Agent (magic.spec) | [MODIFIED] One release per number holding the files of every supported line: release tag `vX.Y.Z`, line tags as anchors, `.github/lines`, `release-set.sh`, plan/verify/build/publish workflow, procedures C and D, one changelog on `master`. Replaces one release per line (0.5.0), after comparing with Godot add-ons that ship binaries. |
