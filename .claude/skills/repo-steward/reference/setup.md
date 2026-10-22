# Setup and Install

## Setup (a repository that has the skill)

Run the steps in order; each one leaves what already exists untouched and reports it.

1. `scripts/context.sh`. Stop with the report if `permission` is not `ADMIN`, `MAINTAIN` or `WRITE`; name a missing scope with `gh auth refresh -s <scope>`.
2. **Profile.** If `.github/steward.yml` is missing, write it from `templates/steward.yml`: uncomment only the keys whose discovered value differs from the default (the board title, a release procedure the guides name, release targets the repository has). Show the file to the person.
3. **Files.** Copy from `templates/` each file the repository does not have:
   - `workflows/steward-stale.yml`, `workflows/steward-readiness.yml`, `workflows/steward-agent.yml` → `.github/workflows/`. Then: where this skill lives somewhere other than `.agents/skills/repo-steward`, replace that path; set the readiness workflow's `branches: [main]` to the default branch; and when `timing.*` differs from 14 and 7 days, change the reminder workflow's `days-before-stale`, `days-before-close` and the "7 days" in its message;
   - `ISSUE_TEMPLATE/bug_report.yml`, `ISSUE_TEMPLATE/feature_request.yml` → `.github/ISSUE_TEMPLATE/`; adjust the version fields to what the product needs reported;
   - `PULL_REQUEST_TEMPLATE.md` → `.github/PULL_REQUEST_TEMPLATE.md`, unless the repository has a pull request template elsewhere.
   An existing file is never overwritten: show the difference between it and what the template would produce, and let the person decide.
4. **Agents that read skills elsewhere.** Claude Code reads `.claude/skills/`: link this directory there (`ln -s ../../<skill path> .claude/skills/repo-steward` on Linux and macOS; `mklink /J .claude\skills\repo-steward <skill path>` on Windows). The link is local; do not commit it.
5. **GitHub** (outward actions; attended runs ask for one approval covering both): `scripts/labels.sh`, then `scripts/board.sh ensure`.
6. **Manual steps.** Print the table below with the values filled in.
7. **First run.** Run the skill with `--mode report` and show the report. Start acting only after the person has read it.

The files of step 3 and the profile are written to the working copy. Commit them following the project's rules; pushing needs the person's approval.

## Install (into another repository)

1. Copy this directory into the target repository (the same relative path, for example `.agents/skills/repo-steward/`).
2. If the target repository ignores skill directories, add an exception for this one that names its path (`!/.agents/skills/repo-steward`), so the workflows and other contributors get it while a local link elsewhere stays ignored. An ignore rule that names the directory itself (`/.*/skills/`) must first become `/.*/skills/*`: git cannot re-include anything inside an excluded directory. Check with `git check-ignore -v --no-index <skill path>/SKILL.md`, which must print nothing.
3. Run Setup there.

## Manual Steps for a Maintainer

| Step | Where |
| --- | --- |
| Column limits on the Flow view: In Progress `board.wip_in_progress`, In Review `board.wip_in_review` | Project → Flow → column menu → Set limit |
| Project workflows: Item closed → Done; Pull request merged → Done; Item reopened → Triage; Auto-archive items closed more than `board.done_retention_days` days ago | Project → ⋯ → Workflows |
| Delete head branches after merge | Settings → General → Pull Requests → Automatically delete head branches |
| A ruleset for the default branch: block force pushes and deletion; require the CI checks for pull requests | Settings → Rules → Rulesets |
| Keep approval required for workflow runs from first-time contributors | Settings → Actions → General |
| Unattended AI run (optional): secret `ANTHROPIC_API_KEY`, or `CLAUDE_CODE_OAUTH_TOKEN` from `claude setup-token`; repository variable `STEWARD_AGENT` = `on`; optional secret `STEWARD_PROJECT_TOKEN` (a fine-grained token with read and write access to the owner's Projects) for board updates from the workflow | Settings → Secrets and variables → Actions |
| Optional bot identity for attended runs: a machine account or a GitHub App with write access; export its token as `GH_TOKEN` for the run | the owner's settings |

The steward never changes these settings itself.
