# Board

The board is a GitHub project owned by the repository's owner, found by `board.title`. `scripts/board.sh ensure` creates whatever is missing; `scripts/board.sh ids` prints the ids below as `key=value` lines (`project_number`, `project_id`, `field_<Name>`, `option_<Field>_<Option>`, with non-alphanumerics replaced by `_`).

## Fields

| Field | Type | Set from |
| --- | --- | --- |
| Status | single select: Triage, Backlog, Ready, In Progress, In Review, Done | the item's real state (table below) |
| Priority | single select: P0, P1, P2, P3 | the `priority:` label |
| Kind | single select: Bug, Feature, Docs, Question, Chore | the type label (`bug`, `enhancement`, `documentation`, `question`; Chore for maintenance pull requests) |
| Started | date | the day Status first became In Progress |
| Finished | date | the day Status became Done |
| Blocked | text | the reason, while the item is blocked; empty otherwise |
| `board.text_fields` | text | project-specific facts the profile adds |

Labels are the source of truth for type and priority; the fields mirror them on every run. The field is called Kind because GitHub reserves the name Type for organization issue types.

## Status from the Real State

| Real state | Status |
| --- | --- |
| open and untriaged, or reopened | Triage |
| triaged, not yet reproduced or specified, or waiting for information | Backlog |
| reproduced or specified, unblocked, not started | Ready |
| a branch for it exists (yours, or linked: `gh issue develop --list <n>`) and no pull request | In Progress |
| an open pull request resolves it, or it is itself an open pull request | In Review |
| closed as completed, or merged | Done |
| closed as not planned or as a duplicate | archived on the board (`gh project item-archive`; the issue itself stays) |

A maintainer who moves an item by hand decides: keep their column until the real state changes again.

## Commands

Make every board change through the script; it finds the ids. In the unattended workflow `STEWARD_BOARD_QUEUE` is set: the script then checks each change (this repository's items only, known fields and options) and writes it to that file, and the workflow's next step, which alone holds `STEWARD_PROJECT_TOKEN`, applies it with `board.sh apply`.

```text
scripts/board.sh item <item url> Status=Ready Priority=P2 Kind=Bug       add (safe to repeat) and set fields
scripts/board.sh item <item url> Status="In Progress" Started=2026-01-31  a date is YYYY-MM-DD
scripts/board.sh item <item url> Blocked="waits for the upstream fix"    a text
scripts/board.sh item <item url> Blocked=                                an empty value clears the field
scripts/board.sh item <item url> --archive                               take a closed item off the board
gh project item-list <number> --owner <owner> --format json --limit 500   read current items and values
```

Add `--dry-run` in report mode.

## Status Update

At the end of an acting run, when the board is writable, post one status update whose text is the report's Board line and Release line, on two lines:

```text
scripts/board.sh status-update "$(printf '%s\n%s' "<Board line>" "<Release line>")"
```

## Views

`board.sh ensure` creates, when absent: **Flow** (board, columns by Status, sorted by Priority), **Triage queue** (table, `status:Triage`), **Awaiting maintainer** (table, `label:"status: needs-maintainer"`), **Finished** (table, `status:Done`, newest Finished first).

## Settings Only the Web Interface Has

Put these in the report's "Manual steps" until the maintainer confirms them:

- Column limits on the Flow view: In Progress `board.wip_in_progress`, In Review `board.wip_in_review` (Flow → column menu → Set limit).
- Project workflows (⋯ → Workflows): Item closed → Done; Pull request merged → Done; Item reopened → Triage; Auto-archive items closed more than `board.done_retention_days` days ago.
