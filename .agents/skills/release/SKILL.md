---
name: release
description: Release godot-mcp for one or more Godot versions, and move fixes and improvements between them. Use when the user asks to release, cut, publish or ship a version ("ship v0.5.1", "release for Godot 4.6"), to backport, carry or cherry-pick a change to other Godot versions, to create the branch for a new Godot version that became stable, to retire a Godot version, or to move a line to a new Godot patch release.
---

# Release godot-mcp

## The Model

Read this first; every procedure below uses these terms.

- **Line**: godot-mcp is built per Godot minor version `M.N` (for example `4.6`). Each supported version has a release line on the branch `godot-vM.N`. `master` is the development line: it targets the next Godot version, which has no stable release yet, and it is never released.
- **Version string**: `X.Y.Z+godot.M.N` on a release line (`0.5.0+godot.4.6`), `X.Y.Z-dev+godot.M.N` on `master`. The same string is in `Cargo.toml` and `addons/godot_mcp/plugin.cfg`. The tag is `v` plus the version string: `v0.5.0+godot.4.6`.
- **Identity files**: the only files that differ between lines on purpose — `Cargo.toml` and `Cargo.lock` (version), `addons/godot_mcp/plugin.cfg` (version), `addons/godot_mcp/plugin.gd` (`MIN_MAJOR`, `MIN_MINOR`), `project.godot` (`config/features`), `.github/godot-version` (the exact Godot build the line is tested against), `CHANGELOG.md` (the line's releases). `cargo test --test version_sync_test` checks that they agree.
- **Carry**: apply a change that landed on a newer line to an older line as a new commit, with `git cherry-pick -x` (it appends `(cherry picked from commit <sha>)`).
- **Numbering**: every release takes the next version number above the highest one released on any line, including the releases from before the lines. A fix bumps the patch number; a feature or a breaking change bumps the minor number (the project is before 1.0). Lines released in one session for the same change set share the number; a line-only release gets its own.
- **Line table**: `README.md` (Installation), `SECURITY.md` (Supported Versions) and `CONTRIBUTING.md` (Branches and Godot Versions) list the lines. All three change together, and each file is identical on every branch.

Pushing a tag publishes a GitHub Release that cannot be taken back. Every step that pushes or tags stops for the user's explicit approval first.

## A. Find the Affected Lines

1. Take the test that reproduces the problem or exercises the change; write it first if it does not exist.
2. Run it on every candidate branch with that branch's Godot build. A worktree per branch keeps `master`'s working tree untouched:

   ```bash
   git worktree add ../godot-mcp-4.6 godot-v4.6
   cd ../godot-mcp-4.6
   cat .github/godot-version                     # the Godot build to test this line with
   GODOT_PATH=/path/to/that/godot cargo test --test <test-file> <test-name> -- --ignored
   ```

3. A line on which the test fails is affected. A problem that exists only because of one Godot version affects only that line.

## B. Land and Carry a Change

1. Land the change, with its test, on the newest affected line: `master` when it is affected, else the newest affected `godot-v*` branch. CI must be green there.
2. Commit changes to the planning files (`.design/`) separately from product changes; only product commits are carried.
3. Carry the change to each older affected line, newest to oldest:

   ```bash
   git switch godot-v4.7
   git cherry-pick -x <commit>
   ```

   - Resolve a conflict for that line, then make the test pass with that line's Godot build.
   - When the carried copy goes through a pull request, title it `[4.7] <original title>`.
4. Skip a line on which the test does not reproduce the problem.
5. Never carry a release commit (version bump and changelog section): each line makes its own.

## C. Release One Line

Work on the line's branch or its worktree. The examples release line 4.6 as 0.5.1.

1. **Preflight.** All of these must pass; stop and report on the first failure:

   ```bash
   git status --porcelain                        # must print nothing
   git fetch origin --tags
   cargo fmt --all -- --check
   cargo clippy --all-targets --all-features -- -D warnings
   cargo test --all-targets
   ```

   Then, with `GODOT_PATH` (and on Windows `GODOT_CONSOLE_PATH`) set to the build named in `.github/godot-version`:

   ```bash
   godot --headless --path . --import
   godot --headless --check-only --path . --script addons/godot_mcp/plugin.gd
   godot --headless --check-only --path . --script addons/godot_mcp/bridge_server.gd
   godot --headless --check-only --path . --script addons/godot_mcp/export_plugin.gd
   GODOT_PATH=/path/to/godot cargo test --test editor_e2e_test -- --ignored
   GODOT_PATH=/path/to/godot cargo test --test addon_export_test -- --ignored
   ```

   Also confirm that `SECURITY.md` and `CONTRIBUTING.md` are still true: the supported lines, the reporting link, the checks.

2. **Find uncarried changes**: product commits that landed on a newer line since this line's previous release and have no copy here. List every newer branch in `for newer in …`:

   ```bash
   line=4.6
   branch="godot-v$line"
   prev=$(git tag -l "v*+godot.$line" --sort=-creatordate | head -n 1)
   since=$(git log -1 --format=%cI "${prev:-$(git merge-base "$branch" master)}")
   for newer in godot-v4.7 master; do
       base=$(git merge-base "$branch" "$newer")
       git log --no-merges --since="$since" --format='%H %s' "$base..$newer" -- . ':!.design' |
           while read -r sha subject; do
               case "$subject" in "chore(release)"*|"chore(line)"*) continue ;; esac
               git cherry "$branch" "$newer" "$base" | grep -qx -- "- $sha" && continue
               git log "$branch" --format=%h --grep="cherry picked from commit $sha" | grep -q . && continue
               echo "$newer ${sha:0:9} $subject"
           done
   done
   ```

   Release commits and the commits that set up another line are skipped by the filter. For each listed commit, either carry it now (procedure B) or write down why it does not apply to this line, for example that it uses an API this line's Godot version does not have. The list goes into the approval request (step 6).

3. **Choose the version number.** The highest `X.Y.Z` released so far, on any line:

   ```bash
   git tag -l 'v*' | sed 's/^v//; s/[-+].*//' | sort -t. -k1,1n -k2,2n -k3,3n | tail -n 1
   ```

   Bump it by the kind of change (see Numbering) and append the line: `0.5.1+godot.4.6`. A version the user names wins if it is above that number.

4. **Write the changelog section** in this branch's `CHANGELOG.md`, above the previous release:
   - Heading `## [0.5.1+godot.4.6] - YYYY-MM-DD`, then Added / Changed / Fixed / Security, written for users: behaviour changes first, nothing about planning or tasks.
   - A compare link at the bottom, with `+` written as `%2B` in the URL: `[0.5.1+godot.4.6]: https://github.com/zeegzoog/godot-mcp/compare/v0.5.0%2Bgodot.4.6...v0.5.1%2Bgodot.4.6`.
   - Check it: `bash .github/scripts/release-notes.sh v0.5.1+godot.4.6`.

5. **Bump both manifests and commit**:
   - `Cargo.toml`: `version = "0.5.1+godot.4.6"`; `addons/godot_mcp/plugin.cfg`: `version="0.5.1+godot.4.6"`.
   - Run `cargo build` (updates `Cargo.lock`) and `cargo test --test version_sync_test`.

   ```bash
   git add CHANGELOG.md Cargo.toml Cargo.lock addons/godot_mcp/plugin.cfg
   git commit -m "chore(release): godot-mcp 0.5.1 for Godot 4.6"
   ```

6. **Stop for approval.** Show the user the line, the version, the commit (`git log -1 --oneline`), the list from step 2 with the reason each listed commit does not apply, and the commands of step 7. Continue only after an explicit yes.

7. **Push the branch, then the tag**:

   ```bash
   git push origin godot-v4.6
   gh run watch $(gh run list --branch godot-v4.6 --workflow ci.yml -L 1 --json databaseId -q '.[0].databaseId')
   git tag -a "v0.5.1+godot.4.6" -m "godot-mcp 0.5.1 for Godot 4.6"
   bash .github/scripts/release-line.sh "v0.5.1+godot.4.6"   # the release workflow's own checks
   git push origin "v0.5.1+godot.4.6"
   ```

   Push the tag only when the CI run ended green and `release-line.sh` passed.

8. **Watch and verify the release**:

   ```bash
   gh run watch $(gh run list --workflow release.yml -L 1 --json databaseId -q '.[0].databaseId')
   gh release view "v0.5.1+godot.4.6"
   ```

   - All six jobs succeed: `verify`, `addon` and the four builds (`x86_64-pc-windows-msvc`, `x86_64-unknown-linux-gnu`, `x86_64-apple-darwin`, `aarch64-apple-darwin`).
   - The title is `godot-mcp 0.5.1 for Godot 4.6`, the text is the changelog section, and five files end in `-godot4.6`: four server archives and `godot-mcp-addon-godot4.6.zip`.
   - The release is marked "Latest" only when 4.6 is the newest line that has releases.

## D. Release Several Lines Together

Use this when one change set (for example a fix carried to every line) is released on several lines.

1. Run C steps 1–2 on every line.
2. Choose one number for all of them (C step 3); each line appends its own `+godot.M.N`.
3. Run C steps 4–5 on every line: each line gets its own changelog section and release commit.
4. Ask for one approval that lists every line with its tag, its commit and its uncarried list.
5. Run C steps 7–8 line by line, newest line first, so "Latest" never points at an older line in between.

## E. Cut a Line for a New Godot Version

When Godot `M.N` becomes stable. The example cuts line 4.8; until now `master` targeted 4.8.

1. On `master`, set `.github/godot-version` to the stable build (`4.8-stable`; later patches are `4.8.1-stable` and so on) and run the C step 1 preflight with it. Fix anything that fails first.
2. Create the branch and give it its identity:

   ```bash
   git switch -c godot-v4.8 master
   ```

   - `Cargo.toml` and `plugin.cfg`: drop `-dev`, keep the line: `0.6.0+godot.4.8`. The first release still takes its number in C step 3.
   - `MIN_MAJOR`/`MIN_MINOR`, `config/features` and `.github/godot-version` already name 4.8; leave them.
   - `CHANGELOG.md`: replace the note under the introduction with one saying that this file lists the releases of the Godot 4.8 line, and that releases up to 0.4.0 were one build for every Godot version.
   - Run `cargo build` and `cargo test --test version_sync_test`, then commit `chore(line): start the Godot 4.8 line`.
3. Move `master` to the next Godot version, 4.9:
   - Version `0.6.0-dev+godot.4.9`, `MIN_MINOR = 9`, `config/features` `"4.9"`, and `.github/godot-version` set to the newest 4.9 pre-release on <https://github.com/godotengine/godot-builds/releases>.
   - If Godot has published no 4.9 pre-release yet, leave `master` on 4.8 and do this step when the first one appears.
   - Calls that needed a `has_method()` guard only because of 4.8 can become direct calls on `master`.
4. Update the line table on `master`: a row for the new line, the development row moved to 4.9. Change all three files (`README.md`, `SECURITY.md`, `CONTRIBUTING.md`) in one commit, then carry that commit to every release line (procedure B).
5. Run the C step 1 preflight on `godot-v4.8` with its build.
6. Stop for approval to push `master` and `godot-v4.8`. After the push, the first release of the new line follows procedure C, with its own approval.

## F. Retire a Line

Only when the user explicitly decides to retire a line.

1. In the line table of the three files on `master`, mark the line as retired: no further fixes; upgrade Godot, or keep the line's last release. Commit, and carry the commit to every remaining line.
2. Keep the branch, its tags and its releases. Delete nothing.
3. Stop for approval to push.

## G. Move a Line to a New Godot Patch Release

When Godot publishes a patch for a line's version (`4.6.4-stable`), or a newer pre-release for `master`:

1. On that line's branch, set `.github/godot-version` to the new build.
2. Run the C step 1 preflight with the new build.
3. If everything passes, commit `ci(line): test the Godot 4.6 line against 4.6.4-stable` and push after approval; no release is needed.
4. If something fails, fix it through procedures A and B, then release the line through C.

## Rules

- Never force-push, delete a tag or delete a release. A broken release is fixed forward with the next number.
- Never tag on `master`: `release-line.sh` and the release workflow refuse a development version.
- The tag format is exactly `vX.Y.Z+godot.M.N`; the release workflow starts on tags matching `v*.*.*godot.*` (a filter pattern cannot contain `+`) and `release-line.sh` then enforces the exact form.
- Keep files other than the identity files identical on every line, unless a Godot version forces a difference.
- Optional, after a release: refresh the git-ignored local `bin/` copy from `target/release` for local MCP client configs.
