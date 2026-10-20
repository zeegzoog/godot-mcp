# Community Health Files (GitHub)

**Version:** 1.0.1
**Status:** Stable
**Layer:** implementation
**Implements:** l1-community-health.md

## Overview

How the security policy and the contributor guide are realized on GitHub: `SECURITY.md` and `CONTRIBUTING.md` at the repository root, the private vulnerability reporting setting they rely on, the links from the readme, the issue chooser and the agent guide, and the release-procedure steps that keep them current.

## Related Specifications

- [l1-community-health.md](l1-community-health.md) - Parent concept.
- [l2-release-branches.md](l2-release-branches.md) - The line table and the carrying procedure both files describe.
- [l2-test-suite.md](l2-test-suite.md) - The verification commands the contributor guide lists.

## 1. Motivation

GitHub shows these files only under fixed names and takes the security policy from the default branch. This specification fixes the files, sections and settings, so an agent that fills or updates them produces what GitHub surfaces and what the concept requires.

## 2. Constraints & Assumptions

- GitHub looks for `SECURITY.md` and `CONTRIBUTING.md` in `.github/`, the root or `docs/`; the root is used (both stubs are already there and the root is what a person browsing the repository sees first).
- The repository is public, has no Discussions, and private vulnerability reporting is off (checked 2026-10-20); a maintainer turns it on (§5.4).
- The reporting link is `https://github.com/zeegzoog/godot-mcp/security/advisories/new`.
- Toolchain minimum from `Cargo.toml`: Rust 1.85 (`rust-version`), edition 2024.
- Commit messages in the history follow Conventional Commits with a scope (`fix(issues): …`, `ci: …`, `build(deps): …`).
- Both files are Markdown checked against `.markdownlint.json`; no horizontal rules.

## 4. Invariant Compliance

| L1 Invariant | Implementation |
| --- | --- |
| CH-1 Discoverable | `SECURITY.md` and `CONTRIBUTING.md` at the root (GitHub links them in the Security tab and on the new issue and pull request pages). `.github/ISSUE_TEMPLATE/config.yml` gets a contact link "Report a security vulnerability" to the reporting link. `README.md` links `SECURITY.md` from its Security section and `CONTRIBUTING.md` from its Development section. |
| CH-2 Private first | `SECURITY.md` § Reporting a Vulnerability: the reporting link as the only channel; never issues, pull requests or `godot_note_feedback` / `godot_submit_feedback`; what to include (godot-mcp version string, Godot version, OS, steps, impact, proof of concept if any); targets 7 / 14 / 90 days; coordinated disclosure through a GitHub Security Advisory; credit in the advisory unless declined. |
| CH-3 Supported versions are the line table | `SECURITY.md` § Supported Versions holds the line table of [l2-release-branches.md](l2-release-branches.md) §5.1 in the form of §5.1 below. |
| CH-4 Scope stated | `SECURITY.md` § Scope: in-scope and out-of-scope lists as in the parent, each item one line, the in-scope items naming the guarantee in plain words (loopback-only bridge, bridge token, project-confined paths, no argument or code injection, scrubbed feedback). |
| CH-5 Complete contribution path | `CONTRIBUTING.md` sections in the order of the parent §5.2. |
| CH-6 Written for people and agents | Both files: imperative bullets, copyable commands in fenced blocks with a language tag, tables for every mapping, no images; `CONTRIBUTING.md` § For AI Agents points to `AGENTS.md`. |
| CH-7 One owner per fact | `CONTRIBUTING.md` owns setup, verification commands, test placement, commit and pull request rules; `README.md` § Development shrinks to a link, its Releasing paragraph to a link to `CONTRIBUTING.md` § Releases. `AGENTS.md` keeps its inline command list (agents need it loaded) and states that it mirrors `CONTRIBUTING.md` § Verification. `README.md` § Security owns the user-facing description of the security measures; `SECURITY.md` links to it instead of restating it. |
| CH-8 Updated by the change that dates them | The release skill's cut and retire procedures (E, F) and its preflight step name the sections of §5.3 in the parent to update; `AGENTS.md` § Completion Protocol gets one checklist item: "Security policy and contributor guide still true after this change". Both files are shared files: changed on `master` and carried to every line. |
| CH-9 Product-facing security policy | `SECURITY.md` names no file under `.design/`, no specification and no task; `CONTRIBUTING.md` § Planning and Specifications may name `.design/` and the planning workflow. |

## 5. Detailed Design

### 5.1 Supported Versions Table

Columns: Godot · Branch · Security fixes · Install. One row per release line of [l2-release-branches.md](l2-release-branches.md) §5.1, newest first; then the development line; then one row for unsupported releases. The row forms:

| Row | Security fixes | Install |
| --- | --- | --- |
| Release line, e.g. `4.6.x` · `godot-v4.6` | yes, in the next 4.6 release | the newest release whose tag ends with `+godot.4.6` |
| Development line, e.g. `4.8 (pre-release)` · `master` | fixed here first; no releases | build from source |
| Unsupported: `0.4.0 and older` (one build for 4.5 and newer) | no | the release for your Godot version |

Until the first line releases are published, the Install cell of every release line reads "0.4.0, the last release before the lines", and the unsupported row reads "0.3.0 and older".

### 5.2 Contributor Guide: Branch Decision Table

| The change … | Open the pull request against |
| --- | --- |
| fixes a problem that exists on `master` | `master`; maintainers carry it to the older lines it affects |
| fixes a problem that exists only on some release lines | the newest of those `godot-v*` branches |
| exists only because of one Godot version | that version's `godot-v*` branch only |
| adds a feature or an improvement | `master` |
| changes shared documentation or CI | `master` |

### 5.3 Files

```plaintext
SECURITY.md                        security policy (root)
CONTRIBUTING.md                    contributor guide (root)
README.md                          links both; Development and Releasing reduced to links
AGENTS.md                          links CONTRIBUTING.md; one completion checklist item
.github/ISSUE_TEMPLATE/config.yml  contact link to private vulnerability reporting
```

### 5.4 Repository Setting (Maintainer)

Private vulnerability reporting: Settings → Code security → Private vulnerability reporting → Enable, or with the maintainer's own `gh` session `gh api -X PUT repos/zeegzoog/godot-mcp/private-vulnerability-reporting`. While it is off, the reporting link shows no form, so it is turned on before, or together with, pushing `SECURITY.md`.

## 6. Implementation Notes

1. Write both files on `master` as part of the line-aware change ([l2-release-branches.md](l2-release-branches.md) §6 step 2), so the release lines inherit them at the cut.
2. The contributor guide's Planning and Specifications section replaces the planned planning-workflow section in `AGENTS.md`; `AGENTS.md` links to it.
3. Check both files with the repository's markdownlint configuration.

## 7. Drawbacks & Alternatives

- **Files in `.github/`:** keeps the root short, but people browsing the code look at the root first.
- **A test that the supported-versions table names the branch's own line:** catches a forgotten update on a new branch, but checks prose structure from code; the cut procedure's checklist covers it, and a test can follow if a forgotten update is ever observed.

## Canonical References

| Alias | Path | Purpose |
| --- | --- | --- |
| `[SECURITY]` | `SECURITY.md` | Security policy |
| `[CONTRIBUTING]` | `CONTRIBUTING.md` | Contributor guide |
| `[README]` | `README.md` | Links and user-facing security measures |
| `[AGENTS]` | `AGENTS.md` | Agent guide |
| `[CHOOSER]` | `.github/ISSUE_TEMPLATE/config.yml` | Issue chooser contact links |
| `[SKILL]` | `.agents/skills/release/SKILL.md` | Update steps on cut, retire and release |

## Document History

| Version | Date | Author | Description |
| --- | --- | --- | --- |
| 1.0.0 | 2026-10-20 | Agent (magic.spec) | Created with the community health concept; files, sections, links, the reporting setting and update steps. Promoted to Stable after Post-Update Review. |
| 1.0.1 | 2026-10-20 | Agent (magic.spec) | [MODIFIED] The Install cell names the release by the tag suffix instead of the title. |
