#!/usr/bin/env bash
# Every outward action the steward takes on issues and pull requests, with its arguments checked
# here rather than trusted: item numbers must be numbers, a message must end with the AI
# disclosure and the marker, and a message that contains anything shaped like a credential is
# refused. The agent is given this script instead of raw `gh` writes.
#
#   act.sh comment <n> <body>
#   act.sh close <n> completed|not-planned|duplicate <body> [<duplicate-of>]
#   act.sh reopen <n> <body>
#   act.sh label <n> add|remove <label>
#   act.sh review <pr> approve|request-changes|comment <body>
#   act.sh edit-comment <comment-id> <body>
#   act.sh approve-run <run-id>
#   act.sh merge <pr> squash|merge|rebase <subject>
#   act.sh open-pr <base> <head> <title> <body>
#   act.sh delete-branch <branch>   delete a branch whose pull request is merged (checks below)
#
# --dry-run (anywhere) prints the gh call instead of making it.
set -euo pipefail
# Git for Windows' bash rewrites arguments that look like paths ("ref:dir/file") unless told not to.
export MSYS_NO_PATHCONV=1 MSYS2_ARG_CONV_EXCL='*'

here=$(cd "$(dirname "$0")" && pwd)
# shellcheck source=guard.sh
. "$here/guard.sh"
dry_run=0
args=()
for arg in "$@"; do
	if [ "$arg" = --dry-run ]; then dry_run=1; else args+=("$arg"); fi
done
set -- ${args[@]+"${args[@]}"}

fail() {
	echo "act.sh: $*" >&2
	exit 2
}

number() { case "$1" in '' | *[!0-9]*) fail "'$1' is not a number" ;; esac; }

# A name of a branch, a label or a merge subject: one line, no control characters.
one_line() {
	case "$1" in *$'\n'* | *$'\r'* | *$'\t'*) fail "'$2' must be one line" ;; esac
	[ -n "$1" ] || fail "'$2' is empty"
	# A title, a branch name or a merge subject is public text too.
	if credential_shaped "$1"; then
		fail "'$2' contains something shaped like a credential; nothing was sent"
	fi
}

body() {
	local text=$1
	[ "${#text}" -le 6000 ] || fail "the message is longer than 6000 characters"
	printf '%s' "$text" | grep -q 'Written by an AI assistant' ||
		fail "the message lacks the AI disclosure line (reference/replies.md)"
	printf '%s' "$text" | awk 'NF { last = $0 } END { exit !(last ~ /^<!-- repo-steward:v1 kind=[a-z-]+ /) }' ||
		fail "the message must end with the repo-steward marker (reference/replies.md)"
	# Credentials never belong in a message, whoever asked for them.
	if credential_shaped "$text"; then
		fail "the message contains something shaped like a credential; it was not posted"
	fi
}

body_file=""
cleanup() { [ -z "$body_file" ] || rm -f "$body_file"; }
trap cleanup EXIT

# A path the gh program can open: Git for Windows' mktemp prints /tmp/..., which a native
# Windows gh cannot resolve; cygpath translates it there and is absent elsewhere.
native_path() {
	if command -v cygpath >/dev/null 2>&1; then cygpath -w "$1"; else printf '%s\n' "$1"; fi
}

# Checks the message and writes it to a file of this script's own, named by $file. It runs in
# this shell, not in a command substitution, so a refusal stops the whole script.
message_file() {
	body "$1"
	body_file=$(mktemp)
	printf '%s\n' "$1" >"$body_file"
	file=$(native_path "$body_file")
}

run() {
	if [ "$dry_run" = 1 ]; then
		printf 'would run:'
		printf ' %q' "$@"
		printf '\n'
	else
		"$@"
	fi
}

repo=$(bash "$here/context.sh" get repo)
[ -n "$repo" ] || fail "cannot tell the repository"

action=${1:-}
[ $# -eq 0 ] || shift
case "$action" in
comment)
	[ $# -eq 2 ] || fail "usage: comment <n> <body>"
	number "$1"
	message_file "$2"
	run gh issue comment "$1" -R "$repo" --body-file "$file"
	;;
close)
	[ $# -ge 3 ] && [ $# -le 4 ] || fail "usage: close <n> completed|not-planned|duplicate <body> [<duplicate-of>]"
	number "$1"
	message_file "$3"
	case "$2" in
	completed) run gh issue close "$1" -R "$repo" --reason completed --comment "$(cat "$body_file")" ;;
	not-planned) run gh issue close "$1" -R "$repo" --reason "not planned" --comment "$(cat "$body_file")" ;;
	duplicate)
		number "${4:-}"
		run gh issue close "$1" -R "$repo" --duplicate-of "$4" --comment "$(cat "$body_file")"
		;;
	*) fail "close reason must be completed, not-planned or duplicate" ;;
	esac
	;;
reopen)
	[ $# -eq 2 ] || fail "usage: reopen <n> <body>"
	number "$1"
	message_file "$2"
	run gh issue reopen "$1" -R "$repo" --comment "$(cat "$body_file")"
	;;
label)
	[ $# -eq 3 ] || fail "usage: label <n> add|remove <label>"
	number "$1"
	one_line "$3" label
	if [ "$dry_run" != 1 ]; then
		gh label list -R "$repo" --limit 500 --json name --jq '.[].name' | tr -d '\r' | grep -Fqx -- "$3" ||
			fail "label '$3' does not exist in $repo; labels.sh creates the steward's labels"
	fi
	case "$2" in
	add) run gh issue edit "$1" -R "$repo" --add-label "$3" ;;
	remove) run gh issue edit "$1" -R "$repo" --remove-label "$3" ;;
	*) fail "label action must be add or remove" ;;
	esac
	;;
review)
	[ $# -eq 3 ] || fail "usage: review <pr> approve|request-changes|comment <body>"
	number "$1"
	message_file "$3"
	case "$2" in
	approve | request-changes | comment) run gh pr review "$1" -R "$repo" "--$2" --body-file "$file" ;;
	*) fail "review must be approve, request-changes or comment" ;;
	esac
	;;
edit-comment)
	[ $# -eq 2 ] || fail "usage: edit-comment <comment-id> <body>"
	number "$1"
	message_file "$2"
	run gh api -X PATCH "repos/$repo/issues/comments/$1" -F "body=@$file" --silent
	;;
approve-run)
	[ $# -eq 1 ] || fail "usage: approve-run <run-id>"
	number "$1"
	run gh api -X POST "repos/$repo/actions/runs/$1/approve" --silent
	;;
merge)
	[ $# -eq 3 ] || fail "usage: merge <pr> squash|merge|rebase <subject>"
	number "$1"
	one_line "$3" subject
	case "$2" in
	squash | merge | rebase) run gh pr merge "$1" -R "$repo" "--$2" --subject "$3" ;;
	*) fail "merge method must be squash, merge or rebase" ;;
	esac
	;;
open-pr)
	[ $# -eq 4 ] || fail "usage: open-pr <base> <head> <title> <body>"
	one_line "$1" base
	one_line "$2" head
	one_line "$3" title
	message_file "$4"
	run gh pr create -R "$repo" --base "$1" --head "$2" --title "$3" --body-file "$file"
	;;
delete-branch)
	[ $# -eq 1 ] || fail "usage: delete-branch <branch>"
	name=$1
	# The name goes into a URL path: plain branch-name characters only, no option, no `..`.
	case "$name" in -* | *..* | */ | /* | *//* | *[!A-Za-z0-9._/-]*) fail "'$name' is not a plain branch name" ;; esac
	[ -n "$name" ] || fail "the branch name is empty"
	[ "$(bash "$here/profile.sh" get autonomy.delete_merged_branches)" = allowed ] ||
		fail "the profile asks for a maintainer's approval before a branch is deleted"
	# Release branches of the project are never deleted: the profile's pattern, `{t}` as a wildcard.
	pattern=$(bash "$here/profile.sh" get release.target_branch)
	if [ -n "$pattern" ]; then
		# shellcheck disable=SC2254
		case "$name" in ${pattern//\{t\}/*}) fail "'$name' is a release branch; it is never deleted" ;; esac
	fi
	owner=${repo%%/*}
	default=$(gh api "repos/$repo" --jq .default_branch | tr -d '\r')
	[ "$name" != "$default" ] || fail "'$name' is the default branch; it is never deleted"
	# Where the branch is now: gone is fine, protected is not.
	info=$(gh api "repos/$repo/branches/$name" --jq '[.commit.sha, .protected] | @tsv' 2>&1) || {
		case "$info" in *"Branch not found"*) echo "branch '$name' is already gone"; exit 0 ;; esac
		fail "cannot read the branch '$name': $info"
	}
	sha=$(printf '%s' "$info" | cut -f1 | tr -d '\r')
	protected=$(printf '%s' "$info" | cut -f2 | tr -d '\r')
	[ "$protected" != true ] || fail "'$name' is protected; it is never deleted"
	open=$(gh api "repos/$repo/pulls?state=open&head=$owner:$name&per_page=1" --jq length | tr -d '\r')
	[ "$open" = 0 ] || fail "'$name' has an open pull request"
	# Finished work only: a merged pull request of this repository ends exactly at the branch tip.
	merged=$(gh api "repos/$repo/pulls?state=closed&head=$owner:$name&per_page=100" \
		--jq "[.[] | select(.merged_at != null and .head.sha == \"$sha\")] | length" | tr -d '\r')
	[ "$merged" != 0 ] || fail "no merged pull request of this repository ends at the tip of '$name'; it keeps its commits"
	run gh api -X DELETE "repos/$repo/git/refs/heads/$name"
	;;
*) fail "unknown action '$action'; see the usage at the top of this script" ;;
esac
