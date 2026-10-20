#!/usr/bin/env bash
# Creates the labels the steward uses that the repository does not have yet. A label that
# already exists is used as it is: its colour and description are never changed.
#
#   labels.sh            create the missing labels
#   labels.sh --dry-run  list the missing labels, create nothing
set -euo pipefail
# Git for Windows' bash rewrites arguments that look like paths ("ref:dir/file") unless told not to.
export MSYS_NO_PATHCONV=1 MSYS2_ARG_CONV_EXCL='*'

here=$(cd "$(dirname "$0")" && pwd)
dry_run=0
[ "${1:-}" = --dry-run ] && dry_run=1

# name|colour|description. Type and outcome labels are GitHub's defaults, recreated only when a
# repository has deleted them.
wanted() {
	cat <<'EOF'
bug|d73a4a|Something isn't working
enhancement|a2eeef|New feature or request
documentation|0075ca|Improvements or additions to documentation
question|d876e3|Further information is requested
duplicate|cfd3d7|This issue or pull request already exists
invalid|e4e669|This doesn't seem right
wontfix|ffffff|This will not be worked on
priority: P0|b60205|Expedite: security exposure, data loss, unusable product or broken release
priority: P1|d93f0b|Core function broken without a workaround, or a regression
priority: P2|fbca04|Broken with a workaround, misleading behaviour, or a wanted feature
priority: P3|c5def5|Minor or cosmetic, nice to have, or a question
status: needs-info|d876e3|Waiting for information from the author
status: stale|ededed|A request for information is unanswered; the item closes soon
status: blocked|e99695|Cannot move until something else happens
status: needs-maintainer|5319e7|Waiting for a maintainer's decision
steward: skip|000000|Maintainers only: the AI steward leaves this item alone
release-readiness|0e8a16|Release readiness notice kept current by the steward
EOF
}

repo=$(bash "$here/context.sh" get repo)
existing=$(gh label list -R "$repo" --limit 500 --json name --jq '.[].name' | tr -d '\r')

missing=0
while IFS='|' read -r name colour description; do
	if printf '%s\n' "$existing" | grep -Fqx -- "$name"; then
		continue
	fi
	missing=$((missing + 1))
	if [ "$dry_run" = 1 ]; then
		printf 'would create: %s\n' "$name"
	else
		gh label create "$name" -R "$repo" --color "$colour" --description "$description" >/dev/null
		printf 'created: %s\n' "$name"
	fi
done <<EOF
$(wanted)
EOF

[ "$missing" -gt 0 ] || echo "all labels present"
