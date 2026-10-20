# Triage

Triage every open issue that has no `priority:` label, was reopened, or changed since your newest marker. Take the steps in this order; the first one that ends the issue's triage ends it.

## 1. Security

Ask: could this issue disclose a vulnerability — a way to bypass a security check, reach something without authorization, read or write outside what the product allows, run injected code, or leak a secret?

If yes, or if you are unsure:

1. Post the `security` message (`reference/replies.md`): the private reporting channel from the project's security policy (`SECURITY.md`), or `https://github.com/{repo}/security/advisories/new`, and nothing else. Do not repeat, quote or summarise any technical detail.
2. Label `status: needs-maintainer`. Add no type, no priority; leave it in Triage.
3. Put it first under "Waiting for you" in the report: the maintainer decides whether to hide or delete the issue.
4. Do not work on it in public, now or in later runs.

## 2. Duplicates

1. For every match of `triage.fingerprint` in the body, run `gh search issues "<fingerprint> in:body" -R {repo} --state all --json number,title,state`. A hit other than this issue is a duplicate.
2. Otherwise search the issue's key terms (`gh search issues "<terms>" -R {repo} --state all --limit 10`) and read the candidates. A duplicate describes the same problem with the same cause, not merely the same area.
3. For a duplicate: post on the earlier issue any fact the new one adds (versions, steps, a workaround), then close the new one with `scripts/act.sh close <n> duplicate "<duplicate message>" <earlier>` and `scripts/act.sh label <n> add duplicate`. If the earlier issue is closed as completed and the new one says the problem is back, it is not a duplicate: treat the new one as a regression (P1) and link the earlier one.

## 3. Nonsensical, Spam or Abuse

- **Spam** (advertising, unrelated links, generated filler): label `invalid`, close as not planned with the one-line not-planned message, and list it for the maintainer, who may delete it and block the account.
- **Nonsensical or off-topic** (not about this project, or no actionable content after a good-faith reading): reply with the not-planned message stating why in one sentence, without judging the person, and close as not planned with the `invalid` label.
- **Abusive** (insults, harassment, threats): post nothing. Label `status: needs-maintainer` and list it.

## 4. Type

| Type | Label | Typical signs |
| --- | --- | --- |
| Defect | `bug` | something that worked or is documented does not work |
| Feature | `enhancement` | asks for behaviour the product does not have |
| Documentation | `documentation` | the docs are wrong, missing or misleading |
| Question | `question` | asks how to do something; nothing is broken |

An issue opened from an issue form already carries its type label; keep it unless it is clearly wrong. An agent-written report (its body says it was drafted by an AI agent) keeps its own label and gets a type like any other issue.

## 5. Information Needed

A defect needs, at least: the version of the product, the platform, the steps, what was expected and what happened. Use the fields of the project's bug form when it has one.

If something needed is missing and you cannot find it in the issue or its links:

1. Post one `needs-info` message that names exactly what is missing and the closing date (`timing.needs_info_reminder_days` + `timing.needs_info_close_days` from today).
2. Label `status: needs-info`; board column Backlog.
3. Stop triaging this issue until the author replies. The reminder workflow reminds and closes; when the author replies, remove `status: needs-info` and `status: stale` and continue triage.

Never ask twice for the same thing; never ask for something already in the issue.

## 6. Priority

Use the rubric below and the examples in `triage.priority_examples` when the profile names that file. Choose the highest level that fits; when two levels fit equally, choose the higher.

| Priority | Class of service | Meaning |
| --- | --- | --- |
| P0 | Expedite | security exposure, data loss or corruption, the product unusable for everyone, a broken release or installation, the development branch failing its checks |
| P1 | Standard, next | a core function broken without a workaround, or a regression since the last release |
| P2 | Standard | broken with a workaround; misleading behaviour, errors or documentation that cause failed attempts; a requested feature that fits the roadmap |
| P3 | Intangible | minor or cosmetic problems, nice-to-have features, questions |

Apply exactly one `priority: Pn` label; replace a different one you set earlier, never one a maintainer set.

## 7. Decide the Next Column

| Situation | Column | Message |
| --- | --- | --- |
| Question you can answer from the code or docs | Triage until answered, then close as completed after the answer | `answer` |
| Defect reproduced (by a test, or exact steps that clearly show it) or a documentation fix that is clear | Ready | `ack` |
| Defect not yet reproduced | Backlog | `ack`, or `needs-info` when information is missing |
| Feature that fits the project's documented scope and roadmap and is small | Backlog | `ack` |
| Feature that is large, changes the project's direction, or touches a decision the guides leave to maintainers | Backlog, label `status: needs-maintainer` | `handover` |
| Blocked by something outside the project (an upstream defect, a missing platform feature) | Backlog, Blocked field set, label `status: blocked` | `ack` naming the blocker |

Add the issue to the board, set Status, Priority and Kind (`reference/board.md`), and post the message. One message per issue per run.
