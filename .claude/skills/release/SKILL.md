---
name: release
description: Release godot-mcp (one release holding the files for every supported Godot version), and move fixes and improvements between the Godot versions. Use when the user asks to release, cut, publish or ship a version ("ship v0.5.1", "release a fix for Godot 4.6"), to backport, carry or cherry-pick a change to other Godot versions, to create the branch for a new Godot version that became stable, to retire a Godot version, or to move a line to a new Godot patch release.
---

# Release godot-mcp

## The Model

Read this first; every procedure below uses these terms.

- **Line**: godot-mcp is built per Godot minor version `M.N` (for example `4.6`). Each supported version has a release line on the branch `godot-vM.N`. `master` is the development line: it targets the next Godot version, which has no stable release yet, and it is never released.
- **Version string**: `X.Y.Z+godot.M.N` on a release line (`0.5.1+godot.4.6`), `X.Y.Z-dev+godot.M.N` on `master`. The same string is in `Cargo.toml` and `addons/godot_mcp/plugin.cfg`.
- **Release**: one GitHub Release per number `X.Y.Z`, named `vX.Y.Z`, holding the files of every supported line: for each line `M.N` four server archives and one addon archive, all with `-godotM.N` in the name. The supported lines are listed in `.github/lines` on `master`. A line without a change of its own still takes part: its release commit only advances its version.
- **Tags**: a **line tag** `vX.Y.Z+godot.M.N` on each line's release commit anchors that line's sources and starts nothing. The **release tag** `vX.Y.Z` on the `master` release commit starts the release workflow; it builds every line from its line tag.
- **Identity files**: the only files that differ between lines on purpose — `Cargo.toml` and `Cargo.lock` (version), `addons/godot_mcp/plugin.cfg` (version), `addons/godot_mcp/plugin.gd` (`MIN_MAJOR`, `MIN_MINOR`), `project.godot` (`config/features`), `.github/godot-version` (the exact Godot build the line is tested against). `cargo test --test version_sync_test` checks that they agree. The release notes are one `CHANGELOG.md` on `master`; the copies on the lines are frozen history.
- **Carry**: apply a change that landed on a newer line to an older line as a new commit, with `git cherry-pick -x` (it appends `(cherry picked from commit <sha>)`).
- **Numbering**: every release takes the next version number above the highest one released so far, including the releases from before the single release model (0.5.0 was released once per line). A fix bumps the patch number; a feature or a breaking change bumps the minor number (the project is before 1.0). All lines take the new number together.
- **Line table**: `README.md` (Installation), `SECURITY.md` (Supported Versions) and `CONTRIBUTING.md` (Branches and Godot Versions) list the lines, like `.github/lines`. They change together, and the three documents are identical on every branch.

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
5. Never carry a release commit (a line's version bump, the changelog section on `master`): each release makes its own.

## C. Release

A release covers every line in `.github/lines`. Work on the branches or on one worktree per line. The examples release 0.5.1 with the lines 4.5, 4.6 and 4.7.

1. **Preflight, on every line.** All of these must pass; stop and report on the first failure:

   ```bash
   git status --porcelain                        # must print nothing
   git fetch origin --tags
   cargo fmt --all -- --check
   cargo clippy --all-targets --all-features -- -D warnings
   cargo test --all-targets
   ```

   Then, with `GODOT_PATH` (and on Windows `GODOT_CONSOLE_PATH`) set to the build named in that line's `.github/godot-version`:

   ```bash
   godot --headless --path . --import
   godot --headless --check-only --path . --script addons/godot_mcp/plugin.gd
   godot --headless --check-only --path . --script addons/godot_mcp/bridge_server.gd
   godot --headless --check-only --path . --script addons/godot_mcp/export_plugin.gd
   GODOT_PATH=/path/to/godot cargo test --test editor_e2e_test -- --ignored
   GODOT_PATH=/path/to/godot cargo test --test addon_export_test -- --ignored
   ```

   Also confirm that `SECURITY.md` and `CONTRIBUTING.md` are still true: the supported lines, the reporting link, the checks.

2. **Find uncarried changes, for every line**: product commits that landed on a newer branch since this line's previous release and have no copy here. The example checks line 4.6; list its newer branches in `for newer in …`:

   ```bash
   line=4.6
   branch="godot-v$line"
   prev=$(git tag -l "v*+godot.$line" --sort=-creatordate | head -n 1)
   since=$(git log -1 --format=%cI "${prev:-$(git merge-base "$branch" master)}")
   for newer in godot-v4.7 master; do
       base=$(git merge-base "$branch" "$newer")
       git log --max-parents=1 --since="$since" --format='%H %s' "$base..$newer" -- . ':!.design' |
           while read -r sha subject; do
               case "$subject" in "chore(release)"*|"chore(line)"*) continue ;; esac
               git cherry "$branch" "$newer" "$base" | grep -qx -- "- $sha" && continue
               git log "$branch" --format=%h --grep="picked from commit $sha" | grep -q . && continue
               echo "$newer ${sha:0:9} $subject"
           done
   done
   ```

   Release commits and the commits that set up another line are skipped by the filter. For each listed commit, either carry it now (procedure B) or write down why it does not apply to that line, for example that it uses an API this line's Godot version does not have. The lists go into the approval request (step 6).

3. **Choose the version number.** The highest `X.Y.Z` among all tags:

   ```bash
   git tag -l 'v*' | sed 's/^v//; s/[-+].*//' | sort -t. -k1,1n -k2,2n -k3,3n | tail -n 1
   ```

   Bump it by the kind of change (see Numbering). A version the user names wins if it is above that number.

4. **Release commit on every line.** On each line's branch set both manifests to `0.5.1+godot.M.N` (`Cargo.toml` `version = "…"`, `addons/godot_mcp/plugin.cfg` `version="…"`), even when the line has no other change. Then:

   ```bash
   cargo build                                   # updates Cargo.lock
   cargo test --test version_sync_test
   git add Cargo.toml Cargo.lock addons/godot_mcp/plugin.cfg
   git commit -m "chore(release): godot-mcp 0.5.1 for Godot 4.6"
   ```

5. **Release commit on `master`.** Write the changelog section in `CHANGELOG.md`, above the previous release:
   - Heading `## [0.5.1] - YYYY-MM-DD`, then Added / Changed / Fixed / Security, written for users: behaviour changes first, nothing about planning or tasks. Mark what applies to some lines only ("Godot 4.6 only: …").
   - No compare link: commits of different lines are not comparable.
   - Check it: `bash .github/scripts/release-notes.sh v0.5.1`.

   ```bash
   git add CHANGELOG.md
   git commit -m "chore(release): godot-mcp 0.5.1"
   ```

6. **Stop for approval.** Show the user the version, every line's release commit (`git log -1 --oneline <branch>`), the lists from step 2 with the reason each listed commit does not apply, and the commands of step 7. Continue only after an explicit yes.

7. **Push the branches, then the tags**:

   ```bash
   git push origin master godot-v4.5 godot-v4.6 godot-v4.7
   # wait until CI is green on all four branches (gh run list --branch <branch>)
   for line in 4.5 4.6 4.7; do git tag -a "v0.5.1+godot.$line" -m "godot-mcp 0.5.1 (Godot $line)" "godot-v$line"; done
   git tag -a v0.5.1 -m "godot-mcp 0.5.1" master
   git push origin v0.5.1+godot.4.5 v0.5.1+godot.4.6 v0.5.1+godot.4.7
   bash .github/scripts/release-set.sh v0.5.1   # the release workflow's own checks, run locally first
   git push origin v0.5.1
   ```

   Push the release tag only when CI is green on every branch and `release-set.sh` printed the version and all lines.

8. **Watch and verify the release**:

   ```bash
   gh run watch $(gh run list --workflow release.yml -L 1 --json databaseId -q '.[0].databaseId')
   gh release view v0.5.1
   ```

   - The jobs succeed: `plan`, then `verify`, `addon` and `build` for every line, then `publish`.
   - The release is named `v0.5.1`, marked "Latest", its text is the changelog section, and for every line it holds five files ending in `-godotM.N`: four server archives (`x86_64-pc-windows-msvc`, `x86_64-unknown-linux-gnu`, `x86_64-apple-darwin`, `aarch64-apple-darwin`) and `godot-mcp-addon-godotM.N.zip` (fifteen files for three lines).

## D. Release After a Change to One Line

A fix that concerns one Godot version is released like any other change: run procedure C. The other lines only advance their version in their release commit (step 4), and the changelog section says which lines the change concerns.

## E. Cut a Line for a New Godot Version

When Godot `M.N` becomes stable. The example cuts line 4.8; until now `master` targeted 4.8.

1. On `master`, set `.github/godot-version` to the stable build (`4.8-stable`; later patches are `4.8.1-stable` and so on) and run the C step 1 preflight with it. Fix anything that fails first.
2. Create the branch and give it its identity:

   ```bash
   git switch -c godot-v4.8 master
   ```

   - `Cargo.toml` and `plugin.cfg`: drop `-dev`, keep the line: `0.6.0+godot.4.8`. The first release still takes its number in C step 3.
   - `MIN_MAJOR`/`MIN_MINOR`, `config/features` and `.github/godot-version` already name 4.8; leave them.
   - `CHANGELOG.md` on the branch stays as it is: the release notes live on `master`.
   - Run `cargo build` and `cargo test --test version_sync_test`, then commit `chore(line): start the Godot 4.8 line`.
3. Move `master` to the next Godot version, 4.9:
   - Version `0.6.0-dev+godot.4.9`, `MIN_MINOR = 9`, `config/features` `"4.9"`, and `.github/godot-version` set to the newest 4.9 pre-release on <https://github.com/godotengine/godot-builds/releases>.
   - If Godot has published no 4.9 pre-release yet, leave `master` on 4.8 and do this step when the first one appears.
   - Calls that needed a `has_method()` guard only because of 4.8 can become direct calls on `master`.
4. On `master`, add the line to `.github/lines` and update the line table: a row for the new line, the development row moved to 4.9. Change `.github/lines` and the three documents (`README.md`, `SECURITY.md`, `CONTRIBUTING.md`) in one commit, then carry the documents to every release line (procedure B).
5. Run the C step 1 preflight on `godot-v4.8` with its build.
6. Stop for approval to push `master` and `godot-v4.8`. After the push, the new line joins the next release through procedure C, with its own approval.

## F. Retire a Line

Only when the user explicitly decides to retire a line.

1. On `master`, remove the line from `.github/lines` (it is then not in the next release) and mark it retired in the line table of the three documents: no further fixes; upgrade Godot, or keep the line's last release. Commit, and carry the documents to every remaining line.
2. Keep the branch, its tags and its releases. Delete nothing.
3. Stop for approval to push.

## G. Move a Line to a New Godot Patch Release

When Godot publishes a patch for a line's version (`4.6.4-stable`), or a newer pre-release for `master`:

1. On that line's branch, set `.github/godot-version` to the new build.
2. Run the C step 1 preflight with the new build.
3. If everything passes, commit `ci(line): test the Godot 4.6 line against 4.6.4-stable` and push after approval; no release is needed.
4. If something fails, fix it through procedures A and B, then release through C.

## Rules

- Never force-push, delete a tag or delete a release. A broken release is fixed forward with the next number.
- The release tag is exactly `vX.Y.Z` and sits on a `master` commit; a line tag `vX.Y.Z+godot.M.N` sits on its line's release commit and is pushed before the release tag. The release workflow starts on `v[0-9]+.[0-9]+.[0-9]+` only (a filter pattern cannot contain `*+`), and `release-set.sh` enforces the rest.
- A version with `-dev` is never released: `master` is not in `.github/lines`.
- Keep files other than the identity files identical on every line, unless a Godot version forces a difference.
- Optional, after a release: refresh the git-ignored local `bin/` copy from `target/release` for local MCP client configs.
