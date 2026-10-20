# Community Health Files

**Version:** 1.1.0
**Status:** Stable
**Layer:** concept

## Overview

Defines the two policy documents a security reporter or a contributor meets first — the security policy and the contributor guide: what each must say, how they are written so that people and AI agents read them the same way, where they live so the hosting platform shows them, and which changes must update them.

## Related Specifications

- [l1-release-lines.md](l1-release-lines.md) - RL-6 and RL-10: the branch routing and the line table both documents publish.
- [l1-quality-assurance.md](l1-quality-assurance.md) - The checks the contributor guide tells contributors to run.
- [l1-editor-bridge.md](l1-editor-bridge.md) - Guarantees the security policy names as its scope.
- [l1-tool-catalog.md](l1-tool-catalog.md) - Write and launch safety, part of the security scope.
- [l1-agent-feedback.md](l1-agent-feedback.md) - The public feedback channel the security policy keeps vulnerabilities out of.
- [l1-repository-stewardship.md](l1-repository-stewardship.md) - How issues and pull requests are handled once opened, which the contributor guide describes. [ADDED]
- [l2-community-health-files.md](l2-community-health-files.md) - Implementation of this concept.

## 1. Motivation

Both documents exist only as headings. A reporter without a private channel either stays silent or opens a public issue that discloses the problem before a fix exists — and this project even gives AI agents a tool that files public issues. A contributor without a guide does not know which branch to target now that every Godot version has its own line, which checks to run, or that tests come first; an AI agent asked to contribute has the same gap and fills it by guessing. Both documents also go stale without anyone noticing: a supported-versions list that still names a retired line promises fixes that will not come.

## 2. Constraints & Assumptions

- The hosting platform shows a security policy and a contributor guide only from fixed file names and locations, and takes the security policy from the default branch.
- Readers include AI agents that load a document as context and act on it literally, without asking back.
- The project has no code of conduct and no discussion forum; questions go to the issue tracker.
- Contributions are accepted under the project's license (MIT), with no separate agreement.
- **Assumption (forecast):** vulnerabilities are reported through the hosting platform's private vulnerability reporting; no personal e-mail address is published. Runner-up: a maintainer e-mail address. Override: `/magic.spec amend l1-community-health.md "report by e-mail to <address>"`.
- **Assumption (forecast):** response targets for a single-maintainer project: acknowledgement within 7 days, first assessment within 14 days, a fix released or a disclosure date agreed within 90 days. Runner-up: no committed targets. Override: `/magic.spec amend l1-community-health.md "response targets <values>"`.

## 3. Core Invariants

- **CH-1 Discoverable.** Both documents live where the hosting platform shows them (the security tab; the new issue and new pull request pages), and the readme links to both.
- **CH-2 Private first.** The security policy names exactly one private reporting channel and tells readers never to report a vulnerability publicly — not in issues, pull requests or the agent feedback channel. It lists what a report should contain and states the response targets, how disclosure is coordinated and how reporters are credited.
- **CH-3 Supported versions are the line table.** The security policy's supported-versions table is the line table (RL-10): which lines receive security fixes, where a fix is released, and which versions are unsupported (retired lines, releases from before the lines).
- **CH-4 Scope stated.** The security policy says what counts as a vulnerability in this project — reaching the editor bridge without its secret or from another machine, writing or reading outside the project directory, passing arguments or code to the engine through tool input, leaking secrets or private paths through a feedback report — and what does not: defects in Godot itself (reported to Godot), attacks that need code already running as the same operating-system user, and an AI client doing what its user allowed.
- **CH-5 Complete contribution path.** The contributor guide takes a contributor from a first question to a merged change: where to ask, how to report a bug or propose a feature, what happens to an issue or pull request once it is opened and who answers it, how to set up and run the checks, which branch a change targets (RL-6), that a test comes first (QA-1), what a commit and a pull request must contain, how a change reaches other lines, and under which license a contribution is accepted.
- **CH-6 Written for people and agents.** Each instruction is one imperative sentence or bullet; commands are complete and copyable; mappings (line to branch to Godot version, check to command) are tables; no step depends on an image, tone or unstated context; a term is defined once, where it is first used. Rules specific to AI agents are linked to the agent guide, not restated.
- **CH-7 One owner per fact.** Each fact has one owning document and the others link to it. A command sequence an agent must have inline (the agent guide, the release procedure) may repeat the contributor guide's; a change to one updates every copy in the same change.
- **CH-8 Updated by the change that dates them.** A change that makes either document wrong — a line cut or retired, the reporting channel, the response targets, the checks, the branch model, a toolchain or engine minimum — updates the document in the same change. Both documents are identical on every line.
- **CH-9 Product-facing security policy.** The security policy refers to no planning artifact. The contributor guide may describe the planning workflow, which is part of its content.

## 5. Detailed Design

### 5.1 Security Policy Content

| Section | Content |
| --- | --- |
| Supported Versions | The line table: Godot version, branch, whether it receives security fixes, where fixes are released; unsupported versions with what to do instead |
| Reporting a Vulnerability | The private channel, the warning against public reports, what to include, the response targets, disclosure and credit |
| Scope | In scope and out of scope (CH-4) |
| How Fixes Are Released | Fixed on the newest affected line first, carried to every supported line, released per line with an advisory that names the fixed release of each line |
| For AI Agents | What an agent does when it finds or suspects a vulnerability: stop, keep details out of public channels and feedback notes, tell its user how to report privately |

### 5.2 Contributor Guide Content

| Section | Content |
| --- | --- |
| Quick links | Issues, security policy, agent guide, license |
| Ways to Contribute | Questions, bug reports (with versions), feature proposals (issue first for larger changes), security reports (security policy), agent feedback reports |
| Branches and Godot Versions | The line table and a decision table: which branch a change targets |
| Development Setup | Toolchain minimum, the Godot build of the branch, environment variables |
| Making a Change | Test first, where each kind of test goes, coding standards (link to the agent guide) |
| Verification | The full pipeline, as copyable commands |
| Commits and Pull Requests | Commit message format, one logical change per pull request, what the description states, what never to commit |
| Issues and Pull Requests | [ADDED] That an AI assistant triages, answers and reviews on the maintainers' behalf and says so in every message; what the labels and priorities mean; when an unanswered request for information closes an issue and how to reopen it; how to reach a person; that the maintainers decide on accepting features and on releases; where the board is |
| Carrying Changes Between Lines | How a merged change reaches older lines, who does it |
| Planning and Specifications | That the planning directory is shared and maintained on the default branch, how maintainers use the planning workflow, what contributors need to know without it |
| For AI Agents | Read and follow the agent guide; the commands here are the same |
| Releases | Maintainers only; where the procedure lives |
| License | Contributions are accepted under the project license |

### 5.3 Update Triggers

| Event | Security policy | Contributor guide |
| --- | --- | --- |
| Line cut or retired | Supported Versions | Branches and Godot Versions |
| Reporting channel or response targets change | Reporting a Vulnerability | Ways to Contribute (link text only) |
| New guarantee that a security report could break | Scope | — |
| Verification commands, toolchain or engine minimum change | — | Development Setup, Verification |
| Branch model or carrying procedure changes | How Fixes Are Released | Branches and Godot Versions, Carrying Changes Between Lines |
| Commit or pull request conventions change | — | Commits and Pull Requests |
| Stewardship settings change (labels, timings, autonomy, the board) | — | Issues and Pull Requests [ADDED] |

## 7. Drawbacks & Alternatives

- **E-mail reporting:** works without a platform account, but publishes a personal address and splits the conversation from the fix; kept as the forecast runner-up.
- **Code of conduct:** recommended by the hosting platform's community profile; not requested and not invented here; a separate decision.
- **One combined document:** fewer files, but the hosting platform shows each document from its own file name only.

## Canonical References

| Alias | Path | Purpose |
| --- | --- | --- |
| `[SECURITY]` | `SECURITY.md` | Security policy |
| `[CONTRIBUTING]` | `CONTRIBUTING.md` | Contributor guide |
| `[AGENTS]` | `AGENTS.md` | Agent guide the contributor guide links to |
| `[README]` | `README.md` | Links to both documents |

## Document History

| Version | Date | Author | Description |
| --- | --- | --- | --- |
| 1.0.0 | 2026-10-20 | Agent (magic.spec) | Created from the request to fill the security policy and the contributor guide following GitHub's guidance, readable by people and AI agents, and to keep them current. Promoted to Stable after Post-Update Review. |
| 1.1.0 | 2026-10-20 | Agent (magic.spec) | [ADDED] CH-5 and the contributor guide's Issues and Pull Requests section, with its update trigger, for the AI-assisted handling of issues and pull requests. Stable → RFC → Stable after Post-Update Review. |
