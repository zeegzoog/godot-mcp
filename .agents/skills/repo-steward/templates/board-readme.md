<!-- markdownlint-disable-file MD041 -->
This board shows how the issues and pull requests of {repo} move from report to resolution. An AI assistant (the repo steward) keeps it current on behalf of the maintainers; a maintainer's change always wins.

## Columns

| Column | An item enters when | It leaves when |
| --- | --- | --- |
| Triage | it is opened or reopened | it has a type and a priority, or it is closed |
| Backlog | it is accepted and prioritized, or waits for information | it is reproduced or specified and nothing blocks it |
| Ready | it can be worked on now | someone starts it |
| In Progress | work has started (Started is set) | a pull request for it is opened |
| In Review | a pull request for it is open | it is merged, or changes are needed |
| Done | it is merged or resolved (Finished is set) | it is archived after {done_retention_days} days |

Items closed as duplicate or not planned leave the board.

## Work-in-progress limits

- In Progress: at most {wip_in_progress} items.
- In Review: at most {wip_in_review} items.
- An expedite (P0) item may exceed a limit by one. Items waiting for information or blocked do not count.
- A blocked item stays in its column; the Blocked field says why.

## Priorities and classes of service

| Priority | Class of service | Meaning |
| --- | --- | --- |
| P0 | Expedite | security exposure, data loss, the product unusable, a broken release, failing checks on the development line |
| P1 | Standard, next | a core function broken without a workaround, or a regression |
| P2 | Standard | broken with a workaround, misleading behaviour or documentation, a wanted feature |
| P3 | Intangible | minor or cosmetic, nice to have, questions |

Work is pulled by priority, oldest first within a priority.

## Flow measures

- Lead time: from opening to Finished.
- Cycle time: from Started to Finished.
- Each steward run posts a status update with the counts per column and the times of the items finished since the previous run.
