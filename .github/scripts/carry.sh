#!/usr/bin/env bash
# Carries the product commits that landed on master to every release line in .github/lines, as one
# pull request per line (procedure B of .agents/skills/release/SKILL.md, done by a script).
#
#   carry.sh <commit | from..to> [--dry-run]
#
# For each line the script takes origin/godot-v<line>, picks the commits that line does not hold
# yet with `git cherry-pick -x`, pushes the branch carry/<short sha>-<line> and opens a pull request
# titled "[<line>] <subject>". The line keeps its own CHANGELOG.md and .design/ (the release notes
# and the plan live on master): whatever a commit changes there is dropped from the copy, and a
# file the line does not have is not added.
#
# A commit is left to a person, with an issue that names it and the commands, when it conflicts in
# a product file or changes a workflow file (the repository token may not push those). The commits
# before it are still carried, and the issue lists it with every commit after it, in order.
#
# Not carried: release commits (`chore(release)`, `chore(line)`), `docs(spec)`, and commits that
# change nothing but .design/ and CHANGELOG.md. A commit the line already holds (its message says
# "picked from commit <sha>") or that an open carry pull request holds is skipped, so running the
# script twice opens nothing twice.
#
# --dry-run makes the branches in the local repository and prints what it would push, open and
# report; it never calls `gh` and never pushes. The pull request is started with the repository
# token, whose pushes start no workflow, so the script starts CI on the branch by hand. Exit status:
# 2 for a refused argument, 1 when a line could not be pushed or its pull request opened, else 0.
set -euo pipefail
# Git for Windows' bash rewrites arguments that look like paths ("ref:dir/file") unless told not to.
export MSYS_NO_PATHCONV=1 MSYS2_ARG_CONV_EXCL='*'

fail() {
	echo "carry.sh: $*" >&2
	exit 2
}

[ "$#" -ge 1 ] && [ "$#" -le 2 ] || fail "usage: carry.sh <commit | from..to> [--dry-run]"
range="$1"
dry_run=0
case "${2:-}" in
"") ;;
--dry-run) dry_run=1 ;;
*) fail "unknown argument '$2'; usage: carry.sh <commit | from..to> [--dry-run]" ;;
esac
# The argument becomes a revision: only commit ids and a range of them are accepted, so it can
# never be read as an option.
if ! printf '%s' "$range" | grep -Eq '^[0-9a-f]{7,40}(\.\.[0-9a-f]{7,40})?$'; then
	fail "usage: carry.sh <commit | from..to> [--dry-run] (a commit id or a range of commit ids)"
fi
[ -f .github/lines ] || fail ".github/lines is missing: run it from the repository root"
lines=$(cat .github/lines)
for line in $lines; do
	[[ $line =~ ^[0-9]+\.[0-9]+$ ]] || fail "a line in .github/lines is not M.N: $line"
done

case "$range" in
*..*) commits=$(git rev-list --reverse --no-merges "$range") || fail "cannot list the commits of $range" ;;
*)
	commits=$(git rev-parse --verify "$range^{commit}") || fail "$range is not a commit"
	if git rev-parse --verify --quiet "$commits^2" >/dev/null; then
		fail "$range is a merge commit; carry the commits it merged"
	fi
	;;
esac

# Commits worth carrying, oldest first. Only commits of master are carried.
carry_list=""
for sha in $commits; do
	git merge-base --is-ancestor "$sha" origin/master || fail "${sha:0:9} is not on origin/master"
	subject=$(git log -1 --format=%s "$sha")
	case "$subject" in
	"chore(release)"* | "chore(line)"* | "docs(spec)"*) continue ;;
	esac
	paths=$(git -c core.quotePath=false show --no-renames --format= --name-only "$sha" | grep -vE '^(\.design/|CHANGELOG\.md$)' || true)
	[ -n "$paths" ] || continue
	carry_list="$carry_list $sha"
done
if [ -z "$carry_list" ]; then
	echo "nothing to carry in $range"
	exit 0
fi
newest=${carry_list##* }
short=${newest:0:9}

# A subject goes into a code span, so it cannot ping anyone or pass for a marker.
code() { printf '`%s`' "${1//\`/\'}"; }

# <line> <reason> <sha>...: opens an issue that names the commits and the commands to run by hand.
report_manual() {
	local line=$1 reason=$2 first=$3 sha
	shift 2
	echo "MANUAL $line ${first:0:9} $reason"
	[ "$dry_run" = 1 ] && return 0
	local title="Carry ${first:0:9} to Godot $line by hand: $(git log -1 --format=%s "$first")"
	local list="" picks=""
	for sha in "$@"; do
		list="$list
- \`${sha:0:9}\` $(code "$(git log -1 --format=%s "$sha")")"
		picks="$picks $sha"
	done
	if [ -n "$(gh issue list --state open --search "\"${first:0:9}\" \"Godot $line\" in:title" --json number --jq '.[].number' 2>/dev/null)" ]; then
		return 0
	fi
	gh issue create --title "$title" --body "The carry workflow did not carry these master commits to \`godot-v$line\`: $reason.
$list

Carry them by hand, oldest first (procedure B of the release skill):

\`\`\`bash
git switch -c carry/${first:0:9}-$line origin/godot-v$line
git cherry-pick -x$picks
\`\`\`

Resolve the conflicts for that line, run its checks with the Godot build in \`.github/godot-version\`, and open the pull request as \`[$line] <subject>\`. Push a workflow file with your own credentials.

_Written by the carry workflow of the repository, an automated script. A maintainer reads this thread and can step in._
<!-- repo-steward:v1 kind=carry state=needs-manual round=0 -->" >/dev/null || echo "::warning::could not open the issue for ${first:0:9} on $line"
}

# <dir> <sha>: cherry-picks one commit into the worktree. Returns 0 when it was carried, 1 when
# nothing of it is left for this line, 2 when a person has to do it (the reason is in $reason).
pick_one() {
	local dir=$1 sha=$2 file stages protected
	reason=""
	if ! git -C "$dir" cherry-pick -x "$sha" >/dev/null 2>&1; then
		local manual=""
		while IFS= read -r -d '' file; do
			stages=$(git -C "$dir" ls-files -u -z -- "$file" | tr '\0' '\n' | awk '{print $3}' | sort -u | tr -d '\n')
			case "$file" in
			CHANGELOG.md | .design/*)
				# The line keeps its own copy; a file it does not have stays absent.
				if [ "$stages" = "13" ]; then
					git -C "$dir" rm -q -f -- "$file"
				else
					git -C "$dir" checkout --ours -- "$file"
					git -C "$dir" add -- "$file"
				fi
				;;
			*)
				if [ "$stages" = "13" ]; then
					# The line deleted or never had the file: its change is not carried.
					git -C "$dir" rm -q -f -- "$file"
					dropped="$dropped
- \`${file//\`/\'}\`"
				else
					manual="$manual ${file}"
				fi
				;;
			esac
		done < <(git -C "$dir" diff -z --name-only --diff-filter=U)
		if [ -n "$manual" ]; then
			git -C "$dir" cherry-pick --abort
			reason="it conflicts in${manual}"
			return 2
		fi
		if git -C "$dir" diff --cached --quiet; then
			git -C "$dir" cherry-pick --skip >/dev/null 2>&1 || git -C "$dir" reset -q --hard HEAD
			return 1
		fi
		GIT_EDITOR=true git -C "$dir" cherry-pick --continue >/dev/null
	fi
	# Whatever the commit changed in the line's own files is taken out of the copy again.
	protected=0
	while IFS= read -r -d '' file; do
		protected=1
		if git -C "$dir" cat-file -e "HEAD~1:$file" 2>/dev/null; then
			git -C "$dir" checkout HEAD~1 -- "$file"
		else
			git -C "$dir" rm -q -f -- "$file"
		fi
	done < <(git -C "$dir" diff -z --name-only HEAD~1 HEAD -- CHANGELOG.md .design)
	if [ "$protected" = 1 ]; then
		if git -C "$dir" diff --cached --quiet HEAD~1; then
			git -C "$dir" reset -q --hard HEAD~1
			return 1
		fi
		git -C "$dir" commit -q --amend --no-edit
	fi
	# What is left of the commit must not touch a workflow file: the repository token may not push
	# one. A steward workflow the line does not have was dropped above and does not count.
	if git -C "$dir" -c core.quotePath=false diff --name-only HEAD~1 HEAD | grep -q '^\.github/workflows/'; then
		git -C "$dir" reset -q --hard HEAD~1
		reason="it changes a workflow file, which the repository token may not push"
		return 2
	fi
	return 0
}

process_line() { # <line>
	local line=$1 branch="godot-v$1" work="carry/$short-$1" sha dir
	local todo="" picked="" left="" stop="" rc dropped=""
	if ! git rev-parse --verify --quiet "refs/remotes/origin/$branch" >/dev/null; then
		echo "skip $line: origin/$branch does not exist"
		return 0
	fi
	if git rev-parse --verify --quiet "refs/remotes/origin/$work" >/dev/null; then
		echo "skip $line: $work is already pushed"
		return 0
	fi
	for sha in $carry_list; do
		if [ -n "$(git log "origin/$branch" --format=%h --grep="picked from commit $sha" -F)" ]; then
			echo "skip ${sha:0:9} on $line: already carried"
			continue
		fi
		if [ "$dry_run" = 0 ] && [ "$(gh pr list --state open --base "$branch" --search "${sha:0:9} in:body" --json number --jq length 2>/dev/null || echo 0)" != 0 ]; then
			echo "skip ${sha:0:9} on $line: an open pull request carries it"
			continue
		fi
		todo="$todo $sha"
	done
	[ -n "$todo" ] || return 0

	dir=$(mktemp -d)
	git branch -q -D "$work" 2>/dev/null || true
	git worktree add -q -b "$work" "$dir" "origin/$branch" || return 1
	for sha in $todo; do
		if [ -n "$stop" ]; then
			left="$left $sha"
			continue
		fi
		rc=0
		pick_one "$dir" "$sha" || rc=$?
		case "$rc" in
		0) picked="$picked $sha" ;;
		2)
			stop=$sha
			left=" $sha"
			;;
		esac
	done
	if [ -n "$stop" ]; then
		# shellcheck disable=SC2086
		report_manual "$line" "$reason" $left
	fi
	if [ -z "$picked" ]; then
		git worktree remove --force "$dir"
		git branch -q -D "$work"
		return 0
	fi
	if [ "$dry_run" = 1 ]; then
		echo "READY $work ($(echo $picked | wc -w | tr -d ' ') commit(s), base origin/$branch)"
		git worktree remove --force "$dir"
		return 0
	fi
	if ! git -C "$dir" push -q origin "$work"; then
		echo "::error::could not push $work"
		git worktree remove --force "$dir"
		git branch -q -D "$work"
		return 1
	fi
	git worktree remove --force "$dir"
	local newest_picked=${picked##* } body
	body="Carries to \`$branch\` the master commits below (procedure B of the release skill), each with \`git cherry-pick -x\`. CHANGELOG.md and the planning files keep this line's own copy.
$(for sha in $picked; do echo "- \`${sha:0:9}\` $(code "$(git log -1 --format=%s "$sha")")"; done)"
	if [ -n "$dropped" ]; then
		body="$body

Not carried, because this line does not have the file:$dropped"
	fi
	if [ -n "$left" ]; then
		body="$body

Left to a person (see the issue): $(for sha in $left; do printf '`%s` ' "${sha:0:9}"; done)"
	fi
	body="$body

_Opened by the carry workflow of the repository, an automated script. A maintainer reads this thread and can step in._
<!-- repo-steward:v1 kind=carry state=in-review round=0 -->"
	if ! gh pr create --base "$branch" --head "$work" --title "[$line] $(git log -1 --format=%s "$newest_picked")" --body "$body" >/dev/null; then
		echo "::error::could not open the pull request for $work"
		git push -q origin --delete "$work" || true
		git branch -q -D "$work" || true
		return 1
	fi
	# The lines get the workflow_dispatch trigger of ci.yml only after it was carried once by hand.
	gh workflow run ci.yml --ref "$work" >/dev/null || echo "::warning::CI was not started on $work; start it by hand"
	echo "OPENED $work -> $branch ($(echo $picked | wc -w | tr -d ' ') commit(s))"
}

status=0
for line in $lines; do
	process_line "$line" || status=1
done
exit "$status"
