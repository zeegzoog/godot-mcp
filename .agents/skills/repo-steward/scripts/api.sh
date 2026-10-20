#!/usr/bin/env bash
# The GitHub API reads the steward needs beyond the plain `gh issue` and `gh pr` commands, each
# with a fixed endpoint and checked arguments, so the agent never needs a raw `gh api`.
#
#   api.sh timeline <n>             who did what on an item: "<event> <actor> <created_at> [<label>]"
#   api.sh comments <n>             "<comment-id> <author> <created_at>", oldest first
#   api.sh permission <login>       admin, maintain, write, triage, read or none
#   api.sh pr-commits <n>           a pull request's commits with one parent (merges left out)
#   api.sh required-checks <branch> the branch protection's required checks, one per line
#   api.sh released-in <sha>        the first release tag (release.tag_pattern) that contains a commit
set -euo pipefail
# Git for Windows' bash rewrites arguments that look like paths ("ref:dir/file") unless told not to.
export MSYS_NO_PATHCONV=1 MSYS2_ARG_CONV_EXCL='*'

here=$(cd "$(dirname "$0")" && pwd)

fail() {
	echo "api.sh: $*" >&2
	exit 2
}
number() { case "$1" in '' | *[!0-9]*) fail "'$1' is not a number" ;; esac; }

repo=$(bash "$here/context.sh" get repo)
owner=${repo%%/*}
name=${repo#*/}

case "${1:-}" in
timeline)
	number "${2:-}"
	gh api --paginate "repos/$repo/issues/$2/timeline" \
		--jq '.[] | "\(.event) \(.actor.login // .user.login // "-") \(.created_at // .submitted_at // "-") \(.label.name // "")"' |
		tr -d '\r'
	;;
comments)
	number "${2:-}"
	gh api --paginate "repos/$repo/issues/$2/comments" --jq '.[] | "\(.id) \(.user.login) \(.created_at)"' | tr -d '\r'
	;;
permission)
	login=${2:-}
	printf '%s' "$login" | grep -Eq '^[A-Za-z0-9][A-Za-z0-9-]{0,38}$' || fail "'$login' is not a GitHub login"
	gh api "repos/$repo/collaborators/$login/permission" --jq .permission 2>/dev/null | tr -d '\r' || echo none
	;;
pr-commits)
	number "${2:-}"
	gh api graphql \
		-f query='query($owner: String!, $name: String!, $n: Int!) { repository(owner: $owner, name: $name) { pullRequest(number: $n) { commits(first: 250) { nodes { commit { oid parents { totalCount } } } } } } }' \
		-f owner="$owner" -f name="$name" -F n="$2" \
		--jq '.data.repository.pullRequest.commits.nodes[].commit | select(.parents.totalCount == 1) | .oid' | tr -d '\r'
	;;
required-checks)
	branch=${2:-}
	case "$branch" in -* | *..* | '' | *[!A-Za-z0-9._/-]*) fail "'$branch' is not a branch name" ;; esac
	gh api "repos/$repo/branches/$branch/protection/required_status_checks" --jq '.checks[].context' 2>/dev/null |
		tr -d '\r' || true
	;;
released-in)
	sha=${2:-}
	printf '%s' "$sha" | grep -Eq '^[0-9a-f]{7,40}$' || fail "'$sha' is not a commit id"
	git cat-file -e "$sha^{commit}" 2>/dev/null || fail "commit $sha is not in this clone"
	pattern=$(bash "$here/profile.sh" get release.tag_pattern)
	git tag --contains "$sha" | tr -d '\r' | grep -E -- "$pattern" |
		awk '{ v = $0; sub(/^[^0-9]*/, "", v); split(v, n, "."); printf "%09d%09d%09d\t%s\n", n[1], n[2], n[3], $0 }' |
		sort | head -n 1 | cut -f 2
	;;
*)
	echo "usage: api.sh timeline <n> | comments <n> | permission <login> | pr-commits <n> | required-checks <branch> | released-in <sha>" >&2
	exit 2
	;;
esac
