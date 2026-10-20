# Release Readiness

`scripts/readiness.sh` answers: do the unreleased changes on the default branch call for a release, which version would it be, and what would each release target contain? Its first output line is `state=<state> version=<x.y.z>`; the notice follows in Markdown. With `--publish` it keeps the repository's one open issue labelled `release-readiness` current. You never tag or publish a release; the notice tells the maintainers when to start one.

## What Counts

- The range: from the highest tag matching `release.tag_pattern` to the default branch, first-parent commits only.
- Left out: commits whose subject starts with one of `release.skip_subjects`, and commits that touch only `release.ignore_paths` (by default the planning ecosystem's paths).
- User-facing: Conventional Commit types `feat`, `fix`, `perf`, and any commit marked breaking (`!` after the type, or a `BREAKING CHANGE:` footer). Everything else is internal; subjects without a type are counted separately so a maintainer can check them.
- Priority: issues labelled `priority: P0` or `priority: P1` closed as completed since the last release.

## States

| State | Condition (profile numbers in brackets) |
| --- | --- |
| now | a P0 issue was closed as completed since the last release |
| due | a P1 issue closed as completed at least [`release.p1_days`] days ago; or at least [`release.changes_threshold`] user-facing changes; or the oldest user-facing change is at least [`release.age_days`] days old |
| accumulating | at least one user-facing change |
| none | no user-facing change since the last release |

The version: a breaking change bumps the major number (the minor number before 1.0), a feature the minor number, anything else the patch number. The project's release procedure takes the final decision; a number the maintainer names wins.

## Release Targets

When `release.targets_file` is set, the notice lists every target in it: the branch (`release.target_branch`), what it is verified against (`release.target_build_file` read from that branch), and the commits on that branch since its tag for the last release (`release.target_tag`) — changes that concern that target only. A target without such a tag is listed as its first release.

## Publishing

- No open notice and a state other than none: open one, titled `Release readiness: v<version> (<state>)`.
- An open notice: replace its title and text; add a comment only when the state rose (accumulating → due → now), so maintainers are notified once per rise.
- State none and an open notice: close it with a comment.

The readiness workflow runs the script on pushes to the default branch, weekly, and after each published release; you run it at step 10 of a run.

## In the Report

Give the state, the version and the notice link. When the state is due or now, add one line: "Start the release with <`release.procedure`>; it stops for your approval before any tag."
