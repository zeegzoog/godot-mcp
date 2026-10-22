# Master Task Index (Registry)

**Version:** 1.7.1
**Generated:** 2026-10-21
**Based on:** .design/main/PLAN.md v1.7.1
**Based on RULES:** .design/RULES.md v1.4.0
**Execution Mode:** Parallel
**Status:** Active

## Overview

Tactical registry of all phases and their statuses. Check individual phase files in `tasks/` for atomic checklists.

<!-- Describes this file, not the plan: the phase table carries status, and what a phase produced lives in its phase file, the changelog and the archive. Do not append to this section. -->

## Active Phases

| Phase | Description | Status |
| --- | --- | --- |
| [Phase 2](archives/tasks/phase-2.md) | Close the gaps found by the first analysis: source-archive hygiene, token-file permissions proof, stale dev-project comment (the specification-workflow guidance moved to Phase 4) | `Done (Archived)` |
| [Phase 3](archives/tasks/phase-3.md) | Agent feedback channel: note, draft and approved submission tools with scrubbing, a stand-in `gh` for tests, user documentation and an issue template | `Done (Archived)` |
| [Phase 4](archives/tasks/phase-4.md) | Godot release lines: identity per line, line-aware CI and release workflow, release skill for carrying and per-line releases, the cut of `godot-v4.5`/`4.6`/`4.7`, security policy and contributor guide; publishing gated on approval | `Done (Archived)` |
| [Phase 5](archives/tasks/phase-5.md) | Repository stewardship: the portable repo-steward skill (scripts, instructions, templates), its installation here (profile, workflows, issue forms, pull request template, guide changes) with tests, a report-mode run; pushing, GitHub setup and the first acting run gated on approval | `Done (Archived)` |
| [Phase 6](archives/tasks/phase-6.md) | Game runtime, authoring and code intelligence: the contributor pull request series (running-game tools, debugger, language-server client, authoring tools, scene reload) reviewed, fixed, merged and carried to the release lines, and its specifications | `Done (Archived)` |
| [Phase 7](archives/tasks/phase-7.md) | Controlled time: hold a running game at the start of a physics tick over the debugger channel, exact and conditional held steps, tick-scheduled input, refusal of time-driven operations while held, exactness through a server-declared fixed rate; feasibility test first, then the lines | `Done (Archived)` |
| [Phase 8](archives/tasks/phase-8.md) | Script reading and editing: a feasibility check of the script editor, specification notes, editor handlers and host tools with a file fallback (read, replace one fragment, refuse while the editor holds unsaved typing), user-facing text, validation, then the release lines; pushing gated on approval | `Done (Archived)` |
| [Phase 9](archives/tasks/phase-9.md) | Simulation QA remediation: the named project wins over the editor, honest stop and status answers, schema ranges, hints by failure class, fallbacks that accept what the editor accepts, a feedback mode that fails closed, language-server project check, the game-call refusal list and typed arguments, pointer input without a window, then user-facing text, validation and the release lines; pushing gated on approval | `Done (Archived)` |

## Meta Information

- **Last Updated**: 2026-10-21
- **Maintainer**: Oleg Alexandrov
