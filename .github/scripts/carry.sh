#!/usr/bin/env bash
# Carries the product commits that landed on master to every release line in .github/lines, as one
# pull request per line (procedure B of .agents/skills/release/SKILL.md, done by a script).
#
#   carry.sh <commit | from..to> [--dry-run]
#
# For each line the script takes origin/godot-v<line>, picks the commits that line does not hold
# yet with `git cherry-pick -x`, pushes the branch carry/<short sha>-<line> and opens a pull request
# titled "[<line>] <subject>". Two kinds of conflict are settled the way a person settles them:
# CHANGELOG.md and .design/ keep the line's own copy (the release notes and the plan live on
# master), and a file the line does not have (the steward workflows) is not added. Any other
# conflict stops that line: the script opens an issue that names the commit and the commands to
# run by hand, and leaves every other line alone.
#
# Not carried: release commits (`chore(release)`, `chore(line)`), `docs(spec)`, and commits that
# change nothing but .design/ and CHANGELOG.md. A commit the line already holds (its message says
# "picked from commit <sha>") is skipped, so running the script twice opens nothing twice.
#
# --dry-run makes the branches in the local repository and prints what it would push, open and
# report; it never calls `gh` and never pushes. The pull request is started with the repository
# token, whose pushes start no workflow, so the script starts CI on the branch by hand.
set -euo pipefail
# Git for Windows' bash rewrites arguments that look like paths ("ref:dir/file") unless told not to.
export MSYS_NO_PATHCONV=1 MSYS2_ARG_CONV_EXCL='*'

fail() {
	echo "carry.sh: $*" >&2
	exit 2
}

range="${1:-}"
dry_run=0
[ "${2:-}" = "--dry-run" ] && dry_run=1
[ "$#" -le 2 ] || fail "usage: carry.sh <commit | from..to> [--dry-run]"
# The argument becomes a revision: only commit ids and a range of them are accepted, so it can
# never be read as an option.
if ! printf '%s' "$range" | grep -Eq '^[0-9a-f]{7,40}(\.\.[0-9a-f]{7,40})?$'; then
	fail "usage: carry.sh <commit | from..to> [--dry-run] (a commit id or a range of commit ids)"
fi
[ -f .github/lines ] || fail ".github/lines is missing: run it from the repository root"

case "$range" in
*..*) commits=$(git rev-list --reverse --no-merges "$range") ;;
*) commits=$(git rev-parse --verify "$range^{commit}") ;;
esac

# Commits worth carrying, oldest first.
carry_list=""
for sha in $commits; do
	subject=$(git log -1 --format=%s "$sha")
	case "$subject" in
	"chore(release)"* | "chore(line)"* | "docs(spec)"*) continue ;;
	esac
	paths=$(git show --no-renames --format= --name-only "$sha" | grep -vE '^(\.design/|CHANGELOG\.md$)' || true)
	[ -n "$paths" ] || continue
	carry_list="$carry_list $sha"
done
if [ -z "$carry_list" ]; then
	echo "nothing to carry in $range"
	exit 0
fi
newest=${carry_list##* }
short=${newest:0:9}

report_manual() { # <line> <sha> <files>
	local line=$1 sha=$2 files=$3 subject title
	subject=$(git log -1 --format=%s "$sha")
	echo "MANUAL $line ${sha:0:9} conflicts in: $files"
	[ "$dry_run" = 1 ] && return 0
	title="Carry ${sha:0:9} to Godot $line by hand: $subject"
	if [ -n "$(gh issue list --state open --search "\"${sha:0:9}\" \"Godot $line\" in:title" --json number --jq '.[].number')" ]; then
		return 0
	fi
	gh issue create --title "$title" --body "The carry workflow could not apply \`${sha:0:9}\` ($subject) to \`godot-v$line\`: it conflicts in $files.

Carry it by hand (procedure B of the release skill):

\`\`\`bash
git switch -c carry/${sha:0:9}-$line origin/godot-v$line
git cherry-pick -x $sha
\`\`\`

Resolve the conflict for that line, run its checks with the Godot build in \`.github/godot-version\`, and open the pull request as \`[$line] $subject\`.

_Written by the carry workflow of the repository, an automated script. A maintainer reads this thread and can step in._
<!-- repo-steward:v1 kind=carry state=needs-manual round=0 -->" >/dev/null
}

for line in $(cat .github/lines); do
	case "$line" in [0-9]*.[0-9]*) ;; *) fail "a line in .github/lines is not M.N: $line" ;; esac
	branch="godot-v$line"
	work="carry/$short-$line"
	if ! git rev-parse --verify --quiet "refs/remotes/origin/$branch" >/dev/null; then
		echo "skip $line: origin/$branch does not exist"
		continue
	fi
	if git rev-parse --verify --quiet "refs/remotes/origin/$work" >/dev/null; then
		echo "skip $line: $work is already pushed"
		continue
	fi
	todo=""
	for sha in $carry_list; do
		if [ -n "$(git log "origin/$branch" --format=%h --grep="picked from commit $sha" -F)" ]; then
			echo "skip ${sha:0:9} on $line: already carried"
			continue
		fi
		todo="$todo $sha"
	done
	[ -n "$todo" ] || continue

	dir=$(mktemp -d)
	git worktree add -q -b "$work" "$dir" "origin/$branch"
	picked=0
	stopped=0
	for sha in $todo; do
		if git -C "$dir" cherry-pick -x "$sha" >/dev/null 2>&1; then
			picked=$((picked + 1))
			continue
		fi
		manual=""
		for file in $(git -C "$dir" diff --name-only --diff-filter=U); do
			code=$(git -C "$dir" status --porcelain -- "$file" | cut -c1-2)
			case "$file" in
			CHANGELOG.md | .design/*)
				git -C "$dir" checkout --ours -- "$file" 2>/dev/null || git -C "$dir" rm -q -f -- "$file"
				git -C "$dir" add -A -- "$file" 2>/dev/null || true
				;;
			*)
				if [ "$code" = "DU" ]; then
					git -C "$dir" rm -q -f -- "$file"
				else
					manual="$manual $file"
				fi
				;;
			esac
		done
		if [ -n "$manual" ]; then
			git -C "$dir" cherry-pick --abort
			report_manual "$line" "$sha" "${manual# }"
			stopped=1
			break
		fi
		if git -C "$dir" diff --cached --quiet; then
			# Everything the commit changed is something this line keeps for itself.
			git -C "$dir" cherry-pick --skip >/dev/null 2>&1 || git -C "$dir" cherry-pick --abort
			continue
		fi
		GIT_EDITOR=true git -C "$dir" cherry-pick --continue >/dev/null
		picked=$((picked + 1))
	done

	if [ "$picked" -eq 0 ]; then
		git worktree remove --force "$dir"
		git branch -q -D "$work"
		continue
	fi
	subject=$(git log -1 --format=%s "${todo##* }")
	if [ "$dry_run" = 1 ]; then
		echo "READY $work ($picked commit(s), base origin/$branch)"
		git worktree remove --force "$dir"
		continue
	fi
	git -C "$dir" push -q origin "$work"
	git worktree remove --force "$dir"
	body="Carries to \`$branch\` the master commits below (procedure B of the release skill), each with \`git cherry-pick -x\`. CHANGELOG.md and the planning files keep this line's own copy.

$(for sha in $todo; do echo "- \`${sha:0:9}\` $(git log -1 --format=%s "$sha")"; done)

_Opened by the carry workflow of the repository, an automated script. A maintainer reads this thread and can step in._
<!-- repo-steward:v1 kind=carry state=in-review round=0 -->"
	gh pr create --base "$branch" --head "$work" --title "[$line] $subject" --body "$body" >/dev/null
	gh workflow run ci.yml --ref "$work" >/dev/null
	echo "OPENED $work -> $branch ($picked commit(s))"
	[ "$stopped" = 0 ] || true
done
