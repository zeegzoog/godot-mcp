# Community Health Files (GitHub)

**Version:** 1.2.0
**Status:** Stable
**Layer:** implementation
**Implements:** l1-community-health.md

## Overview

How the security policy and the contributor guide are realized on GitHub: `SECURITY.md` and `CONTRIBUTING.md` at the repository root, the private vulnerability reporting setting they rely on, the links from the readme, the issue chooser and the agent guide, and the release-procedure steps that keep them current.

## Related Specifications

- [l1-community-health.md](l1-community-health.md) - Parent concept.
- [l2-release-branches.md](l2-release-branches.md) - The supported range and the landing and release procedures both files describe.
- [l2-test-suite.md](l2-test-suite.md) - The verification commands the contributor guide lists.
- [l2-repo-steward-godot-mcp.md](l2-repo-steward-godot-mcp.md) - The issue forms, the pull request template and the facts of the Issues and Pull Requests section.

## 1. Motivation

GitHub shows these files only under fixed names and takes the security policy from the default branch. This specification fixes the files, sections and settings, so an agent that fills or updates them produces what GitHub surfaces and what the concept requires.

## 2. Constraints & Assumptions

- GitHub looks for `SECURITY.md` and `CONTRIBUTING.md` in `.github/`, the root or `docs/`; the root is used (both stubs are already there and the root is what a person browsing the repository sees first).
- The repository is public, has no Discussions, and private vulnerability reporting is off (checked 2026-10-20); a maintainer turns it on (§5.4).
- The reporting link is `https://github.com/zeegzoog/godot-mcp/security/advisories/new`.
- Toolchain minimum from `Cargo.toml`: Rust 1.89 (`rust-version`), edition 2024.
- Commit messages in the history follow Conventional Commits with a scope (`fix(issues): …`, `ci: …`, `build(deps): …`).
- Both files are Markdown checked against `.markdownlint.json`; no horizontal rules.

## 4. Invariant Compliance

| L1 Invariant | Implementation |
| --- | --- |
| CH-1 Discoverable | `SECURITY.md` and `CONTRIBUTING.md` at the root (GitHub links them in the Security tab and on the new issue and pull request pages). `.github/ISSUE_TEMPLATE/config.yml` gets a contact link "Report a security vulnerability" to the reporting link. `README.md` links `SECURITY.md` from its Security section and `CONTRIBUTING.md` from its Development section. |
| CH-2 Private first | `SECURITY.md` § Reporting a Vulnerability: the reporting link as the only channel; never issues, pull requests or `godot_note_feedback` / `godot_submit_feedback`; what to include (godot-mcp version string, Godot version, OS, steps, impact, proof of concept if any); targets 7 / 14 / 90 days; coordinated disclosure through a GitHub Security Advisory; credit in the advisory unless declined. |
| CH-3 Supported versions are stated once | `SECURITY.md` § Supported Versions holds the supported range of [l2-release-branches.md](l2-release-branches.md) §5.2 in the form of §5.1 below. |
| CH-4 Scope stated | `SECURITY.md` § Scope: in-scope and out-of-scope lists as in the parent, each item one line, the in-scope items naming the guarantee in plain words (loopback-only bridge, bridge token, project-confined paths, no argument or code injection, scrubbed feedback). |
| CH-5 Complete contribution path | `CONTRIBUTING.md` sections in the order of the parent §5.2. § Issues and Pull Requests takes its labels, timings and autonomy from `.github/steward.yml` and its wording from [l2-repo-steward-godot-mcp.md](l2-repo-steward-godot-mcp.md) §5.9; § For AI Agents keeps "Never push, tag or publish a release without the explicit approval of your user", with the exception for the actions the repo-steward skill's autonomy settings allow (its defaults, as adjusted in `.github/steward.yml`). The issue forms and the pull request template appear on the new issue and new pull request pages next to the agent feedback template; blank issues stay enabled. |
| CH-6 Written for people and agents | Both files: imperative bullets, copyable commands in fenced blocks with a language tag, tables for every mapping, no images; `CONTRIBUTING.md` § For AI Agents points to `AGENTS.md`. |
| CH-7 One owner per fact | `CONTRIBUTING.md` owns setup, verification commands, test placement, commit and pull request rules; `README.md` § Development shrinks to a link, its Releasing paragraph to a link to `CONTRIBUTING.md` § Releases. `AGENTS.md` keeps its inline command list (agents need it loaded) and states that it mirrors `CONTRIBUTING.md` § Verification. `README.md` § Security owns the user-facing description of the security measures; `SECURITY.md` links to it instead of restating it. |
| CH-8 Updated by the change that dates them | The release skill's procedures C and D (support a new Godot version, drop the oldest) and its preflight step name the sections of §5.3 in the parent to update; `AGENTS.md` § Completion Protocol gets one checklist item: "Security policy and contributor guide still true after this change". Both files are changed on `master` only. |
| CH-9 Product-facing security policy | `SECURITY.md` names no file under `.design/`, no specification and no task; `CONTRIBUTING.md` § Planning and Specifications may name `.design/` and the planning workflow. |

## 5. Detailed Design

### 5.1 Supported Versions Table

Columns: Godot · Security fixes · Install. Only the latest release receives security fixes, and one release serves every supported Godot version, so the rows describe Godot versions and old releases:

| Row | Security fixes | Install |
| --- | --- | --- |
| The supported range, e.g. `4.5 to 4.8` | yes, in the next release | the files of the latest release |
| Newer than the newest tested version | the addon starts with a warning; fixes as above | the files of the latest release |
| Older than the oldest supported version | no | — |
| Releases built per Godot version, e.g. `0.15.0 and older` | no | update to the latest release |

Under the table: the releases built per Godot version came from the retired branches `godot-v4.5`, `godot-v4.6` and `godot-v4.7`, which are kept and get no further fixes; and how to read the installed version (`version=` in `plugin.cfg`, plus the commit a running server adds).

### 5.2 Contributor Guide: Branch and Godot Versions

The section states, in imperative bullets:

- every pull request goes against `master`, and every release is made from it;
- the supported range, where it is declared (`MIN_MINOR` and `TESTED_MINOR` in `plugin.gd`) and that CI runs the end-to-end suites on every version in it;
- a change must work on every supported version: what an older version lacks is called behind `has_method()`, an answer field that only newer versions have is optional, and a behaviour that really differs names its version in the test;
- the retired release-line branches are history and get no commits.

### 5.3 Files

```plaintext
SECURITY.md                        security policy (root)
CONTRIBUTING.md                    contributor guide (root)
README.md                          links both; Development and Releasing reduced to links
AGENTS.md                          links CONTRIBUTING.md; one completion checklist item
.github/ISSUE_TEMPLATE/config.yml  contact link to private vulnerability reporting; blank issues enabled
.github/ISSUE_TEMPLATE/*.yml       bug and feature forms (content: l2-repo-steward-godot-mcp.md §5.8)
.github/PULL_REQUEST_TEMPLATE.md   pull request description (content: l2-repo-steward-godot-mcp.md §5.8)
```

### 5.4 Repository Setting (Maintainer)

Private vulnerability reporting: Settings → Code security → Private vulnerability reporting → Enable, or with the maintainer's own `gh` session `gh api -X PUT repos/zeegzoog/godot-mcp/private-vulnerability-reporting`. While it is off, the reporting link shows no form, so it is turned on before, or together with, pushing `SECURITY.md`.

## 6. Implementation Notes

1. Both files are written and changed on `master`, the only branch; a change to the supported range updates them in the same commit as the range files (release skill procedures C and D).
2. The contributor guide's Planning and Specifications section replaces the planned planning-workflow section in `AGENTS.md`; `AGENTS.md` links to it.
3. Check both files with the repository's markdownlint configuration.

## 7. Drawbacks & Alternatives

- **Files in `.github/`:** keeps the root short, but people browsing the code look at the root first.
- **A test that the supported-versions table names the declared range:** catches a forgotten update when the range changes, but checks prose structure from code; the checklists of skill procedures C and D cover it, and a test can follow if a forgotten update is ever observed.

## Canonical References

| Alias | Path | Purpose |
| --- | --- | --- |
| `[SECURITY]` | `SECURITY.md` | Security policy |
| `[CONTRIBUTING]` | `CONTRIBUTING.md` | Contributor guide |
| `[README]` | `README.md` | Links and user-facing security measures |
| `[AGENTS]` | `AGENTS.md` | Agent guide |
| `[CHOOSER]` | `.github/ISSUE_TEMPLATE/config.yml` | Issue chooser contact links |
| `[SKILL]` | `.agents/skills/release/SKILL.md` | Update steps when the range changes and on release |
| `[AGENT-SETUP]` | `.agents/skills/agents-init/SKILL.md` | The setup step of a fresh clone that CONTRIBUTING.md and AGENTS.md name for agents |

## Document History

| Version | Date | Author | Description |
| --- | --- | --- | --- |
| 1.0.0 | 2026-10-20 | Agent (magic.spec) | Created with the community health concept; files, sections, links, the reporting setting and update steps. Promoted to Stable after Post-Update Review. |
| 1.0.1 | 2026-10-20 | Agent (magic.spec) | [MODIFIED] The Install cell names the release by the tag suffix instead of the title. |
| 1.0.2 | 2026-10-20 | Agent (magic.spec) | [MODIFIED] The Install cell names the files of the latest release, which holds every line. |
| 1.1.0 | 2026-10-20 | Agent (magic.spec) | [MODIFIED] CH-5 row and files for the Issues and Pull Requests section, the issue forms, the pull request template and the steward exception in § For AI Agents. Stable → RFC by the parent's amendment (C12) → Stable after Post-Update Review. |
| 1.1.1 | 2026-11-02 | Agent (magic.spec) | Toolchain minimum 1.87 after the ruzstd and libc updates. No requirement changed. |
| 1.1.2 | 2026-11-05 | Agent (magic.spec) | The agent setup skill that both guides name is in Canonical References. No requirement changed. Removed the `[ADDED]` and `[MODIFIED]` markers from the body: the history rows carry what changed. No requirement changed. |
| 1.1.3 | 2026-11-19 | Agent (magic.spec) | Toolchain minimum 1.89 after the uuid update. No requirement changed. |
| 1.2.0 | 2026-11-20 | Agent (magic.spec) | Modified for one build serving every supported Godot version: the supported-versions table holds the range and old releases instead of one row per line, the branch decision table becomes a single rule (every pull request to `master`, written for every supported version), CH-8 names the skill's procedures C and D, both files are changed on `master` only. Stable → RFC by the parent's amendment (C12) → Stable after Post-Update Review. |
