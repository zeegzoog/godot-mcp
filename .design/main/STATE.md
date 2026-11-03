# Project State

<!-- STATE.md — live project memory. Read FIRST in every workflow session. -->
<!-- Maximum 100 lines. Agent updates AFTER each completed action. -->

**Workspace:** main
**Updated:** 2026-11-02 21:35
**Phase:** 10 — Project Adapter
**Status:** Active

## Current Position

- **Task:** T-11E01 Carry the product commits since 0.12.0
- **Spec:** l2-release-branches.md; l1-release-lines.md; l2-project-adapter.md §5.5
- **Next Action:** Run /magic.task main to update the plan

## Progress

```
Overall: [10/10] ████████ 100%
```

## Recent Decisions

<!-- Last 3-5 locked decisions. Older entries are dropped (not archived) — see PLAN.md / CHANGELOG.md for phase history. -->

- 2026-11-02 **Decision:** Phase 11 complete. Provides: editor setting conversion equal to the file fallback, the planning-owed label of the steward, the product commits since 0.12.0 on the 4.7, 4.6 and 4.5 lines with three portability fixes; nothing pushed
- 2026-11-02 **Decision:** Phase 10 complete. Provides: godot_game_state and godot_game_command, the adapter registry and loader, project.state and project.command held and in Play, proof on the 4.7, 4.6 and 4.5 lines with their own engines (it was carried with 0.12.0)
- 2026-10-22 **Decision:** Phase 9 complete. Provides: named project wins over the editor, honest stop and status, extended game_call refusal list, authoring fallbacks that match the editor, feedback mode that fails closed, pointer warning outside the viewport; carried to 4.7, 4.6 and 4.5; pushing gated on approval
- 2026-10-22 **Decision:** Phase 9 work done except the carry: 14 of 15 tasks. Provides: named project wins over the editor, honest stop and status, extended game_call refusal list, authoring fallbacks that match the editor, feedback mode that fails closed, pointer warning outside the viewport; no commit made
- 2026-10-21 **Decision:** Phase 8 complete. Provides: godot_read_script and godot_edit_script (editor path through the script editor, file fallback), carried to the 4.7, 4.6 and 4.5 lines; pushing gated on approval

## Blockers

<!-- Empty if none. Format: [severity] description -->

## Blocking Constraints

<!-- Anti-patterns discovered through real failures. MANDATORY reading. -->
<!-- Agent MUST explicitly acknowledge each constraint before working. -->
<!-- Format: - [C-NNN] **Title**: what must not be done and why -->

## Session Continuity

**Bootstrap Mode:** false
