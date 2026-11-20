---
name: release
description: Release godot-mcp (one release from master holding one server per platform and one addon, for every supported Godot version), and change which Godot versions are supported. Use when the user asks to release, cut, publish or ship a version ("ship v0.16.1"), to support a new Godot version that became stable, or to drop the oldest one.
---

# Release godot-mcp

## The Model

Read this first; every procedure below uses these terms.

- **One codebase**: `master` is the only line of development and the only source of releases. It serves every Godot minor version from `MIN_MINOR` (the addon refuses an older editor) to `TESTED_MINOR` (the newest Godot the end-to-end tests run against; a newer editor starts the bridge with a warning), both constants in `addons/godot_mcp/plugin.gd`. What an older Godot lacks is called behind `has_method()` (see AGENTS.md). CI runs the end-to-end suites on every supported minor version: the main job on the build in `.github/godot-version` (the newest), the `godot-versions` matrix of `.github/workflows/ci.yml` on the older ones.
- **Version**: `X.Y.Z` in `Cargo.toml` and `addons/godot_mcp/plugin.cfg` (equal, checked by `tests/version_sync_test.rs`) in a release, `X.Y.Z-dev` on `master` between releases. A running server adds the commit it was built from as build metadata (`0.16.1-dev+g6295b51ab`, `src/version.rs`); the manifests carry none.
- **Release**: one GitHub Release per `X.Y.Z`, named `vX.Y.Z`, with five files: `godot-mcp-<target>.zip` or `.tar.gz` for `x86_64-pc-windows-msvc`, `x86_64-unknown-linux-gnu`, `x86_64-apple-darwin` and `aarch64-apple-darwin`, and `godot-mcp-addon.zip`. The tag `vX.Y.Z` sits on the release commit on `master` and starts the release workflow.
- **Numbering**: every release takes the next version number above the highest released so far. A fix bumps the patch number; a feature or a breaking change bumps the minor number (the project is before 1.0). The readiness script computes it (step 3).
- **Retired lines**: releases up to 0.15.0 were built per Godot minor version, from the branches `godot-v4.5`, `godot-v4.6` and `godot-v4.7`, with the tags `vX.Y.Z+godot.M.N` and files ending in `-godotM.N`. The branches, the tags and those releases stay as they are and get no further commits or releases; a fix reaches every Godot version through the next release from `master`. Never delete them.

## A. Land a Change

1. Land the change, with its test, on `master` through a pull request. CI must be green: the Rust job on three systems and every Godot version.
2. A change that behaves differently on some Godot version is written to work on all of them (a call guarded by `has_method()`, an answer field that is optional) and its test says which version it concerns, as `godot_minor()` does in `tests/runtime_e2e_test.rs`.
3. Commit changes to the planning files (`.design/`) separately from product changes.

## B. Release

1. **Preflight.** CI already runs the whole preflight on `master` with every supported Godot build: `cargo fmt`, `clippy` and the unit tests on Linux, Windows and macOS, the GDScript checks, and the editor, addon-export, running-game, scenario and export end-to-end suites. Do not repeat it by hand: check that CI is green on the tip of `master`, and stop and report if it is not.

   ```bash
   git status --porcelain                        # must print nothing
   git fetch origin --tags
   gh run list --branch master --workflow ci.yml -L 1 --json headSha,status,conclusion --jq '.[0]'
   git rev-parse origin/master                   # the run's headSha must be this commit, completed and success
   ```

   A job that failed on the runner and not in the code (a network error while fetching crates, a runner that never started) is rerun with `gh run rerun <id> --failed`. Run a suite by hand only to reproduce a failure CI reported:

   ```bash
   cargo fmt --all -- --check
   cargo clippy --all-targets --all-features -- -D warnings
   cargo test --all-targets
   GODOT_PATH=/path/to/godot cargo test --test editor_e2e_test -- --ignored --test-threads=1
   ```

   Also confirm that `SECURITY.md` and `CONTRIBUTING.md` are still true: the supported Godot versions, the reporting link, the checks.

2. **Check what the release holds.** The product commits since the previous release tag are the content of the release notes:

   ```bash
   git log --format='%h %s' "$(git tag -l 'v[0-9]*.[0-9]*.[0-9]*' --sort=-v:refname | grep -v '+' | head -n 1)..origin/master" -- . ':!.design'
   ```

3. **Choose the version number.** The highest `X.Y.Z` among all tags:

   ```bash
   git tag -l 'v*' | sed 's/^v//; s/[-+].*//' | sort -t. -k1,1n -k2,2n -k3,3n | tail -n 1
   ```

   Do not judge the bump by hand: the number the commits imply is the `version=` of the first line that the steward's readiness script prints. It reads the Conventional Commit types since the newest release tag (a breaking change or a `feat` bumps the minor number before 1.0, anything else the patch; see Numbering):

   ```bash
   bash .agents/skills/repo-steward/scripts/readiness.sh | head -n 1     # state=due version=0.16.0
   ```

   A version the user names wins if it is above that number.

4. **Release commit.** On `master`:
   - Write the changelog section in `CHANGELOG.md`, above the previous release: heading `## [X.Y.Z] - YYYY-MM-DD`, then Added / Changed / Fixed / Security, written for users: behaviour changes first, nothing about planning or tasks. Mark what applies to some Godot versions only ("Godot 4.5 only: …"). Move the `[Unreleased]` link at the end of the file to `compare/vX.Y.Z...master`, and add `[X.Y.Z]: …/compare/v<previous>...vX.Y.Z` for the new version.
   - Check it: `bash .github/scripts/release-notes.sh vX.Y.Z`.
   - Set `Cargo.toml` and `addons/godot_mcp/plugin.cfg` to `X.Y.Z` and commit:

   ```bash
   cargo build                                   # updates Cargo.lock
   cargo test --test version_sync_test
   git add CHANGELOG.md Cargo.toml Cargo.lock addons/godot_mcp/plugin.cfg
   git commit -m "chore(release): godot-mcp X.Y.Z"
   ```

5. **Approval.** A person approves every tag before it is pushed. When the user asked for the release in this conversation, that is the approval: show them the version, the release commit and the commands of step 6 in the final answer, and go on. When a schedule or another agent started the release, stop here and ask.

6. **Push the branch, then the tag**:

   ```bash
   git push origin master
   # wait until CI is green on the release commit (gh run list --branch master)
   git tag -a vX.Y.Z -m "godot-mcp X.Y.Z" master
   git fetch origin --tags
   bash .github/scripts/release-set.sh vX.Y.Z   # the release workflow's own checks, run locally first
   git push origin vX.Y.Z
   ```

   Push the tag only when CI is green on the release commit and `release-set.sh` printed the version.

7. **Watch and verify the release**:

   ```bash
   gh run watch $(gh run list --workflow release.yml -L 1 --json databaseId -q '.[0].databaseId')
   gh release view vX.Y.Z
   ```

   - The jobs succeed: `plan`, then `verify`, `addon` and `build` for the four targets, then `publish`.
   - The release is named `vX.Y.Z`, marked "Latest", its text is the changelog section, and it holds five files: the four server archives and `godot-mcp-addon.zip`.

8. **Move master to the next development version.** Right after the tag is pushed, one more commit: set `Cargo.toml` and `addons/godot_mcp/plugin.cfg` to the next patch with `-dev` (`0.16.1-dev` after releasing 0.16.0), run `cargo build` and `cargo test --test version_sync_test`, and commit it as `chore(release): move master to the next development version` (0.16.1-dev). If the commits that follow include a `feat`, the next release takes a minor number and `master` says so only when the readiness script's `version=` is higher than the `-dev` core: change it then, in a commit of its own.

   Without this `master` keeps reporting the version of the release it already made.

Never force-push, delete a tag or delete a release. A broken release is fixed forward with the next number.

## C. Support a New Godot Version

When Godot `4.N` becomes stable (or a pre-release should be tested):

1. Make `master` pass on it first: install the build, run the suites locally with `GODOT_PATH` set (editor, addon-export, running-game, scenario and export), and fix what fails in a way that keeps the older versions working (guard the call, make the answer field optional).
2. In one commit: set `TESTED_MINOR` in `addons/godot_mcp/plugin.gd` to `N`; move the previous newest build into the `godot-versions` matrix of `.github/workflows/ci.yml`; set `.github/godot-version` to the new build (`4.N-stable`, later patches `4.N.1-stable`). `tests/version_sync_test.rs` checks that the three agree.
3. List the version in `README.md`, `SECURITY.md` and `CONTRIBUTING.md` (the supported-versions text), and add a changelog line under the next release.
4. A new patch release of a Godot minor version that is already supported is a change of the build in the matrix or in `.github/godot-version` only; run the suites before you commit it.

## D. Drop the Oldest Godot Version

Only when the user explicitly decides to stop supporting a Godot minor version.

1. Raise `MIN_MINOR` in `addons/godot_mcp/plugin.gd`, set `config/features` in `project.godot` to the new oldest version, and remove its build from the `godot-versions` matrix of `.github/workflows/ci.yml` (`tests/version_sync_test.rs` checks the three). The calls that were guarded by `has_method()` only for it can become direct calls.
2. Update the supported-versions text in `README.md`, `SECURITY.md` and `CONTRIBUTING.md`, and say it in the changelog under "Changed" of the next release: the last release that supports the dropped version stays available.
3. Release it as a feature (a minor number).

## Rules

- Never force-push, delete a tag or delete a release.
- The release tag is exactly `vX.Y.Z` and sits on a `master` commit; `release-set.sh` enforces the rest. The release workflow starts on `v[0-9]+.[0-9]+.[0-9]+` only.
- A version with `-dev` is never released.
- Optional, after a release: refresh the git-ignored local `bin/` copy from `target/release` for local MCP client configs.
