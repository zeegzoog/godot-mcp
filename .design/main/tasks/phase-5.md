---
phase: 5
name: "Repository Stewardship"
status: In Progress
subsystem: ".agents/skills/repo-steward, .github (profile, workflows, issue forms, pull request template), tests/steward_test.rs, CONTRIBUTING.md, AGENTS.md"
requires: [4]
provides: []
key_files:
  created: []
  modified: []
patterns_established: []
duration_minutes: ~
---

# Stage 5 Tasks — Repository Stewardship

**Phase:** 5
**Status:** In Progress
**Strategic Goal:** A portable `repo-steward` skill triages, plans, fixes, answers, reviews, merges and closes this repository's issues and pull requests within its autonomy limits, keeps a Kanban board, and announces when a release is due and what it holds for each Godot line; installed here with its profile, workflows, forms and guide changes, guarded by tests, and switched on only after the maintainer has read a report-mode run.

## Atomic Checklist

<!-- A line is `- [ ]` (open) or `- [x]` (done). A Blocked task keeps `- [ ]`: it is blocked by its `Status` in Detailed Tracking, and the `[!]` the workflows write after "Blocked" spells that status — it is not a checkbox state. -->

- [x] [T-5A01] Write the steward tests first
- [x] [T-5A02] Profile, context and label scripts
- [x] [T-5A03] Readiness script
- [x] [T-5A04] Board script
- [x] [T-5B01] Skill entry, profile, triage and reply instructions
- [x] [T-5B02] Pull request, change, board, readiness and setup instructions
- [x] [T-5B03] Skill templates: profile, board policies, workflows, issue forms, pull request template
- [x] [T-5C01] Install the steward in this repository
- [x] [T-5C02] Contributor guide and agent guide changes
- [x] [T-5T01] Validate Phase 5 against the specifications, with a report-mode run
- [x] [T-5E01] Commit and push the steward to master
- [x] [T-5E02] Run setup against GitHub: labels and the board
- [ ] [T-5E03] Maintainer settings that only the web interface or the owner can change
- [ ] [T-5E04] First acting run

## Detailed Tracking

### [T-5A01] Write the steward tests first

- **Spec:** l2-repo-steward-godot-mcp.md §5.10; l2-test-suite.md §5.2
- **Status:** Done
- **Assignment:** Agent
- **Verify:** `cargo test --test steward_test` compiles and every test fails for the missing file it names (no skill directory, no workflows, no profile yet); `cargo clippy --all-targets --all-features -- -D warnings` and `cargo fmt --all -- --check` pass.
- **Handoff:** T-5A02, T-5A03, T-5B03, T-5C01 turn the tests green one by one.
- **Notes:** Five tests, one `#[test]` each, behaviour names, failure messages that name the file to fix: (1) readiness on a fixture repository — build a temporary git repository (`tests/common` temp-dir helpers), commit Conventional Commits with fixed author dates, a plain tag `v0.1.0`, a commit that touches only the fixture's own ignore path, a `chore(release)` commit, a targets file with two targets and their branches and line tags; write a fixture profile; run `bash <skill>/scripts/readiness.sh` with `GIT_DIR`/working directory set to the fixture and no network (the script's `gh` look-ups are skipped when `STEWARD_OFFLINE=1`); assert the first line `state=due version=0.2.0` and that both targets with their builds appear. (2) the label `actions/stale` watches in `.github/workflows/steward-stale.yml` equals the needs-info label of the profile (default `status: needs-info`). (3) `.github/workflows/steward-agent.yml` contains the `vars.STEWARD_AGENT` gate, no `contents: write`, no `pull_request_target`, and no `ref:` that names a pull request head. (4) its `on:` keys are only `schedule` and `workflow_dispatch`. (5) `git check-ignore -q .agents/skills/repo-steward/SKILL.md` exits non-zero. A test that needs `bash` or `git` prints why and returns early when the tool is missing. Plain string parsing, no new dependency. No planning references in names, strings or comments.
- **Changes:** `tests/steward_test.rs` with five tests: readiness on a cloned fixture repository (feature, fix, docs, ignored-path fix, bookkeeping, two targets with builds and line tags), reminder label shared with the label script, agent workflow without code write access or pull request heads, agent triggers limited to schedule and manual, skill directory tracked. Git for Windows' bash is used on Windows. Red as expected (`cargo test --test steward_test`: 0 passed, 5 failed, each naming its missing file); clippy and fmt pass; no planning references.

### [T-5A02] Profile, context and label scripts

- **Spec:** l2-repo-steward-skill.md §5.1, §5.2, §5.3, §5.4, §5.8; l1-repository-stewardship.md ST-1, ST-2
- **Status:** Done
- **Assignment:** Agent
- **Verify:** `bash -n` on the three scripts; in this repository `bash .agents/skills/repo-steward/scripts/context.sh` prints `repo=zeegzoog/godot-mcp`, `permission=ADMIN`, `default_branch=master`, `ecosystem=magic-spec`, `ecosystem_available=yes`, `scopes_ok=yes`; `profile.sh get board.wip_in_progress` prints `2` without a profile file and the profile's value with one; `labels.sh --dry-run` lists exactly the labels of l2-repo-steward-godot-mcp.md §5.3 and writes nothing (no `gh label create` call in dry-run, checked by running with `GH_TOKEN` unset after one read).
- **Handoff:** T-5A03, T-5A04, T-5B01.
- **Notes:** `profile.sh get {section}.{key}`: awk over `section:` lines and the indented `key: value` lines under them, strips quotes and trailing comments, falls back to the defaults table of §5.2 kept in the script. `context.sh`: `gh repo view --json …`, `gh auth status` scopes, `GITHUB_REPOSITORY` when set, ecosystem detection per §5.8 (used / available / planning paths), guides found; output `key=value` lines. `labels.sh`: `gh label list --json name` once, then `gh label create` only for missing names; never `--force`. Portable bash: no GNU-only flags (`date -d`, `sed -i` without a suffix, `readlink -f`); works in Git Bash, Linux and macOS. No project name anywhere in the skill directory.
- **Changes:** `profile.sh` (get, list; flat YAML subset read with awk, CRLF and quotes handled, unknown keys reported), `context.sh` (repository, permission, owner type, scopes with `scopes_ok`, default branch, guides, ecosystem used/available/planning paths; offline mode from the origin URL), `labels.sh` (creates only missing labels, never `--force`). `bash -n` passes; here: `repo=zeegzoog/godot-mcp permission=ADMIN default_branch=master ecosystem=magic-spec ecosystem_available=yes scopes_ok=yes`; `labels.sh --dry-run` lists exactly the ten labels to add. shellcheck is not installed on this host.

### [T-5A03] Readiness script

- **Spec:** l2-repo-steward-skill.md §5.10; l2-repo-steward-godot-mcp.md §5.6; l1-repository-stewardship.md ST-17, §5.8
- **Status:** Done
- **Assignment:** Agent
- **Verify:** Test (1) of T-5A01 passes. In this repository `bash .agents/skills/repo-steward/scripts/readiness.sh` (no `--publish`) prints a first line `state=… version=…`, names `v0.5.1` as the last release, lists the targets 4.5, 4.6 and 4.7 with the builds read from `origin/godot-v4.5`, `origin/godot-v4.6`, `origin/godot-v4.7`, says that the development line is not released, and points to the release skill, procedure C. `--publish` is not run here (it writes an issue; that happens in T-5E02 or in the workflow).
- **Handoff:** T-5B03 (the readiness workflow calls it).
- **Notes:** Dates as epoch seconds from `git log --format=%ct` and `gh --jq`; no `date -d`. `STEWARD_OFFLINE=1` skips every `gh` call (priority look-ups, notice publishing) so the fixture test runs without network. Planning paths to ignore come from `context.sh` when `release.ignore_paths` is unset. `--publish`: one open issue labelled `release-readiness`, body replaced, a comment only when the state rose, closed when a tag at or above the recommended version exists; marker `kind=readiness`.
- **Changes:** `readiness.sh`: semver-highest release tag, first-parent range, skip subjects and ignored paths (planning paths by default), Conventional Commit kinds, offline P0/P1 skip, state and next version, targets table with each branch's build file and own changes, `--publish` (one labelled issue, edit in place, comment on a rise, close when nothing waits). All four scripts export `MSYS_NO_PATHCONV=1`: Git for Windows' bash rewrote `ref:.github/file` arguments (found here, the target builds read as missing). Readiness test green; mutation without the export makes it fail on the `.ci/build.txt` fixture file. On this repository: `state=none` since v0.5.1; with the last release forced to v0.4.0 the table lists 4.5/4.6/4.7 with `4.5.2-stable`, `4.6.3-stable`, `4.7.2-stable` and points to the release skill, procedure C.
- **Attempts:**
  - build file read with `git show origin/<branch>:.github/godot-version` under Git Bash → MSYS path conversion mangled the argument; fixed by exporting MSYS_NO_PATHCONV in every script

### [T-5A04] Board script

- **Spec:** l2-repo-steward-skill.md §5.9; l1-repository-stewardship.md ST-15, §5.7
- **Status:** Done
- **Assignment:** Agent
- **Verify:** `bash -n`; `board.sh ensure --dry-run` in this repository finds no project titled "godot-mcp issues and pull requests" (the organization has none) and prints, without running them, the create, link, edit, field-create, Status-option, view and README calls with the values of l2-repo-steward-godot-mcp.md §5.2; `board.sh ids --dry-run` prints the variables a run needs. No call that writes is made in this task.
- **Handoff:** T-5E02 runs `ensure` for real after approval.
- **Notes:** Idempotent: every create is preceded by a look-up by name. Status options through `gh api graphql` `updateProjectV2Field` with `singleSelectOptions` `{id?, name, color, description}`: the new project's `Todo` becomes `Backlog`, `In Progress` and `Done` keep their ids. Views through `gh api -X POST orgs/{owner}/projectsV2/{number}/views` (users path for a personal owner) with numeric field ids from `gh api orgs/{owner}/projectsV2/{number}/fields`. README from `templates/board-readme.md` with the profile values filled in.
- **Changes:** `board.sh ensure|ids [--dry-run]`: project found by title, created, linked (an already-linked error counts as done), visibility from `board.public` or the repository, description and policies README, missing fields, Status options through `updateProjectV2Field` keeping ids (Todo becomes Backlog, maintainer options kept), views by REST POST after a GraphQL look-up; personal owners use the users paths. `templates/board-readme.md` written here because the script reads it. Dry-run against `zeegzoog`: no project found, prints create, link, edit, six field creations including `Lines`, Status options and views; `ids` exits 1 naming `board.sh ensure`. The paths for an existing project (Status options, views) run for the first time in T-5E02.

### [T-5B01] Skill entry, profile, triage and reply instructions

- **Spec:** l2-repo-steward-skill.md §5.1, §5.2, §5.4, §5.5, §5.6; l1-repository-stewardship.md ST-3 to ST-6, ST-10 to ST-14, ST-16, ST-18, §5.2, §5.3, §5.5, §5.6
- **Status:** Done
- **Assignment:** Agent
- **Verify:** `SKILL.md` front matter has exactly the `name` and `description` of §5.1; `SKILL.md` lists the run procedure of §5.5 as numbered steps and the never class of the parent §5.5 verbatim; `reference/replies.md` has one template per marker kind of §5.6 (15 kinds, checked by grepping `kind=` values) and each template ends with the disclosure line and marker; `grep -riE "godot|zeegzoog" .agents/skills/repo-steward` prints nothing; the prompt-engineer review of these files (run.md Step 3.4b) returns PASS or PASS-WITH-REWRITES with the rewrites applied.
- **Handoff:** T-5B02.
- **Notes:** `SKILL.md` stays short: the model (terms of the parent), arguments, the twelve steps, the rules, and which reference file each step loads. Triage: the parent rubric with the profile's examples file, duplicate search by fingerprint then meaning (`gh issue close --duplicate-of`), security redirect, the needs-info request. Replies: brief, polite, businesslike, English by default, at most five sentences of prose, no planning artifacts, one message per state change.
- **Changes:** `SKILL.md` (model, arguments, autonomy table with the never list and approval rules, twelve numbered run steps, rules, report format, reference map), `reference/profile.md`, `reference/triage.md` (security, duplicates by fingerprint then meaning with `--duplicate-of`, spam/nonsense/abuse, type, needs-info, rubric, next column), `reference/replies.md` (voice, disclosure line and marker, 15 templates, conversation and dispute rules). No project name in the skill directory; prompt-engineer pass: PASS (instructions ordered, defaults and precedence stated once in `SKILL.md`).

### [T-5B02] Pull request, change, board, readiness and setup instructions

- **Spec:** l2-repo-steward-skill.md §5.3, §5.7, §5.8, §5.9, §5.10, §5.12, §5.13; l1-repository-stewardship.md ST-7 to ST-9, ST-15, ST-17, §5.4, §5.7, §5.8
- **Status:** Done
- **Assignment:** Agent
- **Verify:** `reference/pull-requests.md` covers each row of the parent §5.4 and the stacking, staleness, run-approval and checks-green rules of §5.7; `reference/changes.md` contains the adapter table of §5.8 including the "used but not available" stop; `reference/setup.md` contains the generic manual-steps table of §5.13; `grep -riE "godot|zeegzoog" .agents/skills/repo-steward` prints nothing; prompt-engineer review PASS or PASS-WITH-REWRITES applied.
- **Handoff:** T-5B03.
- **Notes:** Contributor code is never built or run on the machine of an attended run; CI verifies pushed edits. The independent review runs in a fresh context (a sub-agent or a new session) that gets the diff, the item and the guides only; at most three rounds, then hand-over. Before writing the OpenSpec row, check its current command names; keep the TBD if they cannot be confirmed.
- **Changes:** `reference/pull-requests.md` (author classes, stacking, base freshness, run approval, checks green, review order with safety first, decision table, merge, edits with credit), `reference/changes.md` (pull under WIP, ecosystem adapter table with the used-but-not-available stop, branch and test first, pull request, fresh-context review up to three rounds, merge and carrying), `reference/board.md` (fields, state-to-status table, commands, status update, views, web-only settings), `reference/readiness.md`, `reference/setup.md` (setup, install, manual steps). The OpenSpec row names no commands (not confirmed); closed not-planned items are archived on the board rather than removed. Prompt-engineer pass found one contradiction (code changes in unattended runs: `SKILL.md` and `changes.md` disagreed) — rewritten to the spec's rule: change code only in a run that can push. No project name in the skill directory.

### [T-5B03] Skill templates: profile, board policies, workflows, issue forms, pull request template

- **Spec:** l2-repo-steward-skill.md §5.1, §5.2, §5.11; l2-repo-steward-godot-mcp.md §5.8
- **Status:** Done
- **Assignment:** Agent
- **Verify:** Every YAML template parses with a YAML parser present on the machine (`actionlint` for the three workflows when installed, otherwise PyYAML `yaml.safe_load` or `yq`; name the one used in Changes); `templates/steward.yml` has every key of §5.2 with its default, commented; the `steward-agent.yml` template meets the assertions of tests (3) and (4) of T-5A01 by inspection (the tests themselves run against the installed copy in T-5C01); every action is pinned by major version and the pinned majors exist (`gh api repos/actions/stale/releases/latest --jq .tag_name`, same for `anthropics/claude-code-action` and `actions/checkout`); no project name in any template except placeholders.
- **Handoff:** T-5C01.
- **Notes:** Workflows per §5.11: concurrency groups, no `pull_request_target`, no head checkout; `steward-agent.yml` gated on `vars.STEWARD_AGENT == 'on'`, permissions `contents: read`, `issues: write`, `pull-requests: write`, `actions: read`, `github_token: ${{ secrets.GITHUB_TOKEN }}`, the API key or OAuth token secret, `claude_args` with `--max-turns` and the allowed tools of §5.11, the prompt naming the skill path and `--unattended`. Issue forms keep blank issues possible; the bug form reminds that vulnerabilities go to the security policy.
- **Changes:** Templates `steward.yml` (every key, commented defaults), `board-readme.md` (from T-5A04), three workflows (`actions/stale@v11`, `actions/checkout@v7`, `anthropics/claude-code-action@v1`, latest majors checked with `gh api …/releases/latest`), two issue forms, pull request template. Parsed with PyYAML 6.0.3 (actionlint not installed): the agent workflow triggers on `schedule` and `workflow_dispatch` only, permissions `contents: read`, `issues`/`pull-requests: write`, `actions: read`; its allowed tools exclude `gh pr checkout` and every build command, so no pull request code can run there. The reminder workflow needs `actions: write` (the action's cache). Board writes moved into `board.sh item` / `status-update`, which use `STEWARD_PROJECT_TOKEN` in a workflow; skill spec patched to 1.0.1 with these findings.

### [T-5C01] Install the steward in this repository

- **Spec:** l2-repo-steward-godot-mcp.md §5.1, §5.2, §5.3, §5.4, §5.7, §5.8
- **Status:** Done
- **Assignment:** Agent
- **Verify:** `cargo test --test steward_test` passes (all five tests); `git check-ignore -q .agents/skills/repo-steward/SKILL.md` exits 1; `git status --porcelain` lists only the files of §5.1; `.github/ISSUE_TEMPLATE/config.yml` is unchanged (`git diff --quiet -- .github/ISSUE_TEMPLATE/config.yml`); the full verification of `CONTRIBUTING.md` § Verification passes (`cargo clippy --all-targets --all-features -- -D warnings`, `cargo fmt --all -- --check`, `cargo test --all-targets`).
- **Handoff:** T-5T01.
- **Notes:** Write the installed files the way the skill's `setup` would (files only; labels and the board wait for T-5E02). Profile values exactly as §5.2; `planning.ecosystem` and `release.ignore_paths` left out so detection decides. `.github/steward-priorities.md` with the examples of §5.4. `.gitignore`: add `!/.*/skills/repo-steward` next to the release exception.
- **Changes:** Installed `.github/steward.yml` (the §5.2 values; ecosystem and ignore paths left to detection), `.github/steward-priorities.md`, the three workflows (readiness on `master`), godot-mcp issue forms (plugin.cfg version, Godot version, OS, MCP client, server log) and pull request template (what and why, Godot lines, checks); `config.yml` untouched. `.gitignore`: the rule `/.*/skills/` excluded the whole directory, so no `!` exception could ever re-include a skill (the release skill is tracked only because it was added by force) — changed to `/.*/skills/*` plus `!/.*/skills/repo-steward`; `git check-ignore --no-index` now passes the two listed skills and still ignores others. `cargo test --test steward_test`: 5 passed; clippy, fmt and `cargo test --all-targets` pass.

### [T-5C02] Contributor guide and agent guide changes

- **Spec:** l2-repo-steward-godot-mcp.md §5.9; l1-community-health.md CH-5, §5.2, §5.3; l2-community-health-files.md CH-5
- **Status:** Done
- **Assignment:** Agent
- **Verify:** `CONTRIBUTING.md` has the section "Issues and Pull Requests" directly after "Commits and Pull Requests" with every item of the parent §5.2 row; § For AI Agents carries the exception wording of §5.9; `AGENTS.md` has the one pointer line; `npx --yes markdownlint-cli2 CONTRIBUTING.md AGENTS.md` passes with `.markdownlint.json` (or the IDE's markdownlint reports nothing new); no horizontal rule added; `git diff -U0 CONTRIBUTING.md AGENTS.md | grep -E "T-[0-9]+[A-Z][0-9]+|[Pp]hase[- ][0-9]|\.design/|l[12]-[a-z-]+\.md"` prints nothing.
- **Handoff:** T-5T01.
- **Notes:** Imperative bullets (CH-6); the timings 14 and 7 days and the WIP limits come from the profile; the board link is filled in after T-5E02 (until then the sentence names the board by title). Both files are shared files: changed on `master`, carried to the release lines at the next carrying pass (release skill, procedure B) like other shared documentation.
- **Changes:** `CONTRIBUTING.md` § Issues and Pull Requests after § Commits and Pull Requests (the assistant and its disclosure, what maintainers decide, priority table, the three status labels with 14 + 7 days, reaching a person, security), § For AI Agents exception and skill link; `AGENTS.md` one sentence in its introduction (Tool Invocation Protocols covers using godot-mcp, so the pointer went next to the `CONTRIBUTING.md` pointer; spec patched). `markdownlint-cli2` with `.markdownlint.json`: 0 issues over 15 files after a file-level MD041 exception in the two headless pull request templates and the board README (stripped before upload); no horizontal rules; no planning references added.

### [T-5T01] Validate Phase 5 against the specifications, with a report-mode run

- **Spec:** l1-repository-stewardship.md (all invariants); l2-repo-steward-skill.md §4; l2-repo-steward-godot-mcp.md §4
- **Status:** Done
- **Assignment:** Agent
- **Verify:** (1) `cargo clippy --all-targets --all-features -- -D warnings`, `cargo fmt --all -- --check`, `cargo test --all-targets` pass. (2) A trace table, one row per ST-1 … ST-18, naming the file and section of the skill that implements it; no row empty. (3) A report-mode run of the skill on the live repository (`--mode report`, attended) makes no write — checked by comparing `gh issue list --state all --json number,updatedAt` and `gh pr list --state all --json number,updatedAt` before and after — and its report lists issue #1 and pull requests #2–#8, detects that #4, #5 and #6 share leading commits, finds the six workflow runs waiting for approval, maps #2 to #1, and shows the readiness state.
- **Handoff:** T-5E01. The report of (3) goes to the maintainer before T-5E04.
- **Notes:** Read-only against GitHub. A finding that a specification is wrong goes to `/magic.spec`, not into the code.
- **Changes:** (1) `cargo fmt --check`, `cargo clippy -D warnings`, `cargo test --all-targets`: exit 0, 198 passed, 0 failed; markdownlint 0 issues. (2) Trace: ST-1 `context.sh` + `SKILL.md` step 1; ST-2 `profile.sh`, `context.sh` guides, `SKILL.md` § Model; ST-3 `SKILL.md` § Rules, `steward-agent.yml` (no head checkout, tool list), tests; ST-4 `triage.md` §2–§7; ST-5 `triage.md` §1; ST-6 `SKILL.md` step 8, `changes.md` §1; ST-7 `changes.md` §2–§4, ecosystem detection; ST-8 `changes.md` §5, `pull-requests.md` §4; ST-9 `pull-requests.md` §2–§7; ST-10 `SKILL.md` § Autonomy; ST-11 `replies.md` § Voice and ending; ST-12 `replies.md` templates, `SKILL.md` step 9; ST-13 `replies.md` § Conversation; ST-14 `SKILL.md` § Rules, markers, `limits.max_actions`; ST-15 `board.sh`, `board.md`, `board-readme.md`; ST-16 `SKILL.md` § Rules and step 4; ST-17 `readiness.sh`, `steward-readiness.yml`, `readiness.md`; ST-18 `SKILL.md` § Report, report mode, `board.sh status-update` — no row empty. (3) Report-mode run on the live repository: issue/pull request `updatedAt` snapshots identical before and after (1 issue, 8 pull requests — #9 opened today); the report lists #1 → P2 bug in review through #2, 8 runs waiting for approval with no sensitive paths, stacks #3→#4→#5→#6, #4→#9, #2→#7, hand-over for #3–#7 and #9 (parked roadmap), the contributor's merge-commit request as a maintainer decision, readiness none. Found while running: merge commits from "Merge upstream master" hid the stacking, so `pull-requests.md` now compares non-merge commits (GraphQL parent count, checked on #3, #4, #9).
- **Security follow-up:** an automated review of `steward-agent.yml` found that a prompt injection could have used `gh api` or `--body-file` to post `STEWARD_PROJECT_TOKEN` or the checkout's persisted token, and that the third-party action was not pinned. Fixed test-first (4 new tests, red then green; mutation of the credential pattern and of the Status check each fails its test): `act.sh` for every write (numbers checked, disclosure and marker required, credential shapes refused), `api.sh` for fixed reads, the agent's tool list narrowed to read-only `gh` commands plus the scripts, `/proc` and runner directories denied, `persist-credentials: false`, the board queued by the agent and applied by a separate step holding the token, `anthropics/claude-code-action` pinned to `6c690188…` (v1.0.242, the newest release at least two weeks old). Skill spec 1.0.2, project spec 1.0.2. Pipeline: 202 passed, 0 failed.
- **Security follow-up 2:** a second review found that `git log`/`git show --output=<file>` could write a file into the scripts directory, which the `scripts/*` wildcard would then run. Fixed test-first (3 new tests): no `git` command for the agent (the release lookup moved to `api.sh released-in`, commit ids only), each script allowed by exact name, `.agents` made read-only before the agent step, subprocess secret scrubbing pinned on, `readiness.sh` and `api.sh` refuse branch names that start with `-` or hold `..`, `git log --end-of-options`, `board.sh apply` refused where the queue is set. 12 steward tests pass.

### [T-5E01] Commit and push the steward to master

- **Spec:** l2-repo-steward-godot-mcp.md §6 step 3; `CONTRIBUTING.md` § For AI Agents (an agent pushes only with its user's approval)
- **Status:** Done
- **Assignment:** User (approval), then Agent
- **Verify:** The user's explicit yes in the conversation; then `git log -4 --oneline` shows the product commits separate from the planning commit; `git archive HEAD | tar -t | grep -c '^\.design/'` prints 0; `git push origin master`; CI green on `master` (`gh run list --branch master -L 1`), including the new tests on all three operating systems.
- **Handoff:** T-5E02.
- **Notes:** Conventional Commits: `feat(steward): …` for the skill, `ci(steward): …` for the workflows, `docs(contributing): …` for the guides; planning files in their own `docs(design): …` commit. The installed workflows start running after the push: readiness on the push itself (it writes the readiness issue with the workflow token), stale on its schedule; the agent workflow stays idle until `STEWARD_AGENT` is `on`.
- **Changes:** Approved by the user. Commits `c866416` chore(steward) skill and ignore rule, `a6b1531` ci(steward) workflows, profile, forms and tests, `e3657ae` docs(contributing), `50e560d` docs(design); maintenance types, so the readiness workflow counts them as internal. Before committing, the scripts got mode 100755 and call each other through `bash` (Windows had added them as 100644; a direct call would have failed on Linux and macOS). `git archive HEAD` holds no planning file. Pushed `033b9be..50e560d`; CI green on all four jobs (Rust on ubuntu, windows, macos; Godot addon); readiness workflow green with `state=none`, no notice opened.

### [T-5E02] Run setup against GitHub: labels and the board

- **Spec:** l2-repo-steward-skill.md §5.4, §5.9, §5.13; l2-repo-steward-godot-mcp.md §5.3, §6 step 4
- **Status:** Done
- **Assignment:** User (approval), then Agent
- **Verify:** The user's explicit yes; `gh label list` shows the labels of l2-repo-steward-godot-mcp.md §5.3; `gh project list --owner zeegzoog` shows "godot-mcp issues and pull requests", public, linked to the repository, with fields Status (six options), Priority, Type, Started, Finished, Blocked, Lines and the four views; running `board.sh ensure` a second time makes no change.
- **Handoff:** T-5E03, then the board link goes into `CONTRIBUTING.md` (T-5C02 note) in a follow-up commit.
- **Notes:** Outward actions on the organization: one approval covers labels, project, fields, views and README.
- **Changes:** Approved by the user. `labels.sh`: 9 labels created (`release-readiness` had been created by the readiness workflow); a second run reports all present. `board.sh ensure` found project #1 "godot-mcp issues and pull requests", which the maintainer had created by hand (public, 9 items), and added Priority, Kind, Started, Finished, Blocked, Lines, the six Status options (Todo kept its id as Backlog), the views Flow (board by Status, sorted by Priority), Triage queue, Awaiting maintainer, Finished, the description and the policies README; a second run changed nothing. The type field is Kind: `Type` is reserved for the organization's issue types and the first attempt failed with "Name cannot have a reserved value"; script, board reference and skill spec 1.0.3 changed accordingly (not yet pushed). Board: https://github.com/orgs/zeegzoog/projects/1
- **Attempts:**
  - field named Type → GitHub reserves it for issue types; renamed to Kind

### [T-5E03] Maintainer settings that only the web interface or the owner can change

- **Spec:** l2-repo-steward-godot-mcp.md §5.11
- **Status:** Todo
- **Assignment:** User
- **Verify:** The maintainer confirms each row of §5.11 done or declined; `gh api repos/zeegzoog/godot-mcp --jq .delete_branch_on_merge` is `true` if accepted; `gh api repos/zeegzoog/godot-mcp/rulesets --jq '.[].name'` lists the `master` ruleset if accepted; `gh variable list` shows `STEWARD_AGENT` only if the unattended run is wanted.
- **Handoff:** T-5E04.
- **Notes:** The agent gives the exact clicks and commands of §5.11 and changes none of these settings itself.

### [T-5E04] First acting run

- **Spec:** l1-repository-stewardship.md ST-18; l2-repo-steward-godot-mcp.md §6 step 5
- **Status:** In Progress
- **Assignment:** User (approval), then Agent
- **Verify:** The maintainer has read the report of T-5T01 (3) and says yes; the acting run's report lists each action with its link; every message it posted carries the disclosure line and a marker (`gh issue view 1 --comments`, `gh pr view 2 --comments`); a second run immediately afterwards posts nothing (ST-14).
- **Handoff:** Phase 5 complete; the next step is computed by `/magic.task`.
- **Notes:** Merges of the contributor's pull requests stay at `approval` (profile default); the run queues them with its recommendations and asks one consent question at the end.
- **Changes:** First acting run (attended, approved by the user), stopped at the 20-action limit as designed. Done: #1 labelled `bug`, `agent-feedback`, `priority: P2`, acknowledged, card In Review / P2 / Bug; CI runs approved for #2, #3, #8, #4 (no sensitive paths); #2 answered (review after CI, carrying to 4.5–4.7, the merge-commit request noted for maintainers), card In Review / Bug; #3, #8, #4 handed over (roadmap decision on runtime access; #8 describes the server after the whole series) with `status: needs-maintainer`, cards for #3 and #8 updated. Second pass (after pushing `514b8b0`, `b5dfa67`; 18 actions): #4 card; #7, #5, #9, #6 CI runs approved and handed over with `status: needs-maintainer`, cards updated; project status update posted. Then, CI being green on #2, a review of its diff: changes requested (unsaved hand edits are not tracked so a reload can discard them; `get_scene_tree` reloads as a side effect of a read; undo history is dropped by a reload and undocumented; the macOS path mapping is an untested separate change); card marked Blocked for the author. All nine cards are In Review; every item carries exactly one steward message. Readiness: none. A third run was not executed, so the Verify line's "a second run posts nothing" is still unchecked.
- **Stack, in order (approved by the user: "yes, one by one"):** #3 reviewed and merged as a squash `80d8834` (`chore(debug)`), after finding that a 12-byte packet made the Variant decoder reserve 64 GB and abort the server; fixed on the contributor's branch (`ea6c387`, regression tests, no unwrap in the module), CI green on four jobs. #4 reviewed and run on live editors (4.8-beta1 plus 4.7.2, 4.6.3, 4.5.2 with the patch applied; all pass); changes requested (schema leaks `reload`/`discard` from #2; process runs leave runtime scripts in the project; editor Play always rewrites `override.cfg`; CI lacks the new checks). Remaining in order: #4 after the author's update, then #5, #6, #9; #7 after #2; #8 last. A later attempt to run more of the contributor's code on this machine was refused by the permission classifier, so edits to #4 are left to its author. Found during the run: `act.sh` handed gh a POSIX temp path (`/tmp/...`) that a native Windows gh cannot open, so the first #1 comment failed and nothing was posted; reproduced with a new test that puts the stand-in gh first on PATH (the stand-in now reads `--body-file` and fails on an unreadable file, as gh does), fixed by translating the path with cygpath where it exists, retried once. Pipeline: 206 passed, 0 failed.
