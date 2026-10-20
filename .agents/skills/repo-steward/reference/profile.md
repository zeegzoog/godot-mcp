# Profile

The profile is `.github/steward.yml` in the repository (or the file named by `STEWARD_PROFILE`). Every key is optional. `scripts/profile.sh list` prints each key with its value and whether it came from the profile or from the default; the defaults live in that script.

## Format

A YAML subset the scripts read without a YAML library:

```yaml
board:
  wip_in_progress: 2          # a comment after a value needs a space before "#"
  text_fields: Lines Owner    # a list is one space-separated string
release:
  tag_pattern: '^v[0-9]+\.[0-9]+\.[0-9]+$'   # quote values that contain " #"
```

Top-level names are sections; each key is indented once under its section. No nested sections, no YAML lists, no anchors. An unknown key is reported by `profile.sh list` and ignored.

## Keys

| Key | Meaning |
| --- | --- |
| `tracker.language` | Language of every message you post |
| `project.guides` | Guides read at the start of a run, highest precedence first |
| `planning.ecosystem` | `auto` (detect), an adapter name from `reference/changes.md`, `none`, or the exact command text to use; leave it at `auto` unless detection is wrong |
| `board.title` | Title the board is found by; `{repo}` is the repository name |
| `board.public` | `true`, `false`, or `auto` (same visibility as the repository) |
| `board.wip_in_progress`, `board.wip_in_review` | Work-in-progress limits |
| `board.text_fields` | Extra text fields the project needs on the board |
| `board.done_retention_days` | Days a Done item stays before the maintainer's auto-archive setting removes it |
| `triage.fingerprint` | Extended regular expression of machine fingerprints in reports |
| `triage.priority_examples` | Path of a Markdown file with the project's examples per priority |
| `review.sensitive_paths` | Paths whose change makes a contributor's workflow run need a maintainer's approval |
| `merge.method` | `squash`, `merge` or `rebase`; must be allowed by the repository |
| `autonomy.*` | Levels of `SKILL.md` § Autonomy; values `allowed` or `approval` |
| `timing.needs_info_reminder_days`, `timing.needs_info_close_days` | Days to the reminder, then days from the reminder to the close |
| `limits.max_actions` | Outward actions per run |
| `limits.dispute_rounds` | Exchanges of disagreement before you hand an item to a maintainer |
| `limits.allowed_bots` | Bot accounts whose items you handle |
| `release.procedure` | Where the project's release procedure is written; the readiness notice points there |
| `release.tag_pattern` | Extended regular expression of plain release tags |
| `release.ignore_paths` | Commits touching only these paths do not count for readiness; empty means the planning ecosystem's paths |
| `release.skip_subjects` | Subject prefixes of release bookkeeping commits |
| `release.changes_threshold`, `release.age_days`, `release.p1_days` | Readiness numbers (`reference/readiness.md`) |
| `release.targets_file` | File on the default branch with one release target per line (`#` comments allowed) |
| `release.target_branch`, `release.target_tag` | Patterns with `{t}` (the target) and `{version}` (the last release) |
| `release.target_build_file` | File read from each target branch: what the target is verified against |

## When the Profile Is Missing

Every key takes its default, and the report lists them under "Defaults used". `setup` writes a profile from `templates/steward.yml` with the values it can discover.
