# Changes

How you take a Ready item from the board to a merged change. Do this only in a run that can push to the repository, and only when the planning ecosystem (below) is not "used but not available".

## 1. Pull

1. Count the items in In Progress, leaving out blocked items and items waiting for information. Stop when the count reaches `board.wip_in_progress` — except for a P0 item, which may start while the count equals the limit (In Progress then holds the limit plus one).
2. Pick the next Ready item by `--order`: priority (P0 first, then oldest), or item number.
3. Re-read it. Skip it if it is assigned to a person, labelled `steward: skip`, or was changed by a maintainer since triage.
4. Board: In Progress, Started = today. Post the `plan` message with the steps you will take.

## 2. Plan with the Project's Ecosystem

Run `scripts/context.sh get ecosystem`, `get ecosystem_used` and `get ecosystem_available`.

| Adapter | Used when | Available when | Plan and tasks |
| --- | --- | --- | --- |
| Guide-declared | a guide names its planning commands or plan location | the named command or path exists | the guide's commands; this row wins over the others |
| `magic-spec` | `.design/` exists | `.magic/` exists too | `/magic.spec` with the item's requirement and its link as raw input (it hands over to `/magic.task`), then `/magic.run`; commit planning files separately from product changes |
| `spec-kit` | `.specify/` exists | your agent has the `/speckit.*` commands | `/speckit.specify`, then `/speckit.plan`, `/speckit.tasks`, `/speckit.implement` |
| `kiro` | `.kiro/specs/` exists | always (files only) | the feature's `requirements.md`, `design.md` and `tasks.md`, in Kiro's layout |
| `openspec` | `openspec/` exists | the `openspec` command exists | a change proposal under `openspec/changes/`, following the project's OpenSpec guide |
| `exec-plans` | a plans directory a guide names (for example `docs/exec-plans/`) | always (files only) | a plan file there, in the guide's format |
| `none` | nothing above | — | the steps in your `plan` message |

- **Used but not available** (for example the planning files are committed but the engine is installed only on a maintainer's machine): do not change code in this run. Leave the item Ready and report "ecosystem unavailable".
- Use the ecosystem's own commands for its files; never hand-edit what a command maintains.
- Keep planning artifacts out of product files, commit subjects and messages you post.

## 3. Branch and Fix

1. Branch from the branch the guides' routing rules name for this change: `git switch -c steward/<n>-<short-slug> <base>`.
2. Write the test first. It must fail on the base and pass after the fix; keep the failing output for the pull request description.
3. Make the smallest change that resolves the item, following the guides' standards.
4. Run the full verification the guides list. All of it must pass; a check you cannot run here (it needs a tool or engine you do not have) is named in the pull request description.
5. Commit in the guides' convention (Conventional Commits unless they say otherwise), product and planning files in separate commits.

## 4. Pull Request

1. `git push -u origin <branch>`.
2. `scripts/act.sh open-pr <base> <branch> "<Conventional Commit subject>" "<body>"`. The body follows the repository's pull request template when there is one, and contains `Fixes #<n>`, the failing-then-passing test, and the checks run. End it with the disclosure line and the marker (`kind=plan state=in-review`).
3. Board: the item to In Review; update the `plan` message.

## 5. Independent Review

1. Start a fresh context that has not seen your reasoning: a sub-agent, or a new session. Give it only the diff (`gh pr diff`), the item, and the guides, with this task: "Review this change for correctness, tests, scope, safety and the project's rules. List each finding with file, line and the change that resolves it, or say PASS."
2. Fix every finding, push, and review again. After three rounds without PASS, hand the item to a maintainer (`handover`).

## 6. Merge and Close

Follow `reference/pull-requests.md` § 7. Then:

- Post the `fixed` message on the item, with state `unreleased`.
- If the project releases several targets from several branches, follow its release procedure to find which other branches the change affects; record them on the board (a text field the profile adds, when it has one), and carry the change only as that procedure allows — queue it for approval when it asks for one.
- Board: Done, Finished = today.
