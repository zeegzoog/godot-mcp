#!/usr/bin/env bash
# Release readiness: are the unreleased changes on the default branch enough for a release,
# which version would it be, and what would each release target contain?
#
#   readiness.sh [--default-branch <name>] [--publish]
#
# The first output line is "state=<now|due|accumulating|none> version=<x.y.z>"; the notice
# follows in Markdown. --publish keeps the repository's one open issue labelled
# release-readiness current: creates it, replaces its text, comments when the state rises,
# closes it once nothing is waiting. It never tags or publishes a release.
#
# STEWARD_OFFLINE=1 makes no network call: no priority look-up and no publishing.
set -euo pipefail
# Git for Windows' bash rewrites arguments that look like paths ("ref:dir/file") unless told not to.
export MSYS_NO_PATHCONV=1 MSYS2_ARG_CONV_EXCL='*'

here=$(cd "$(dirname "$0")" && pwd)
offline=${STEWARD_OFFLINE:-0}
publish=0
branch=""
while [ $# -gt 0 ]; do
	case "$1" in
	--publish) publish=1 ;;
	--default-branch)
		branch=${2:?--default-branch needs a branch name}
		shift
		;;
	*)
		echo "usage: readiness.sh [--default-branch <name>] [--publish]" >&2
		exit 2
		;;
	esac
	shift
done

p() { bash "$here/profile.sh" get "$1"; }

[ -n "$branch" ] || branch=$(bash "$here/context.sh" get default_branch)
# A branch name never starts with "-" and never holds "..": anything else could reach git as an
# option (git log --output=<file> writes a file).
case "$branch" in
-* | *..* | *[!A-Za-z0-9._/-]*) echo "readiness.sh: '$branch' is not a branch name" >&2; exit 2 ;;
esac
[ -n "$branch" ] || { echo "readiness.sh: cannot tell the default branch; pass --default-branch" >&2; exit 2; }
ref=$branch
git rev-parse --verify --quiet "refs/remotes/origin/$branch" >/dev/null && ref="origin/$branch"
git rev-parse --verify --quiet "$ref^{commit}" >/dev/null || {
	echo "readiness.sh: branch '$branch' not found; fetch it or pass --default-branch" >&2
	exit 2
}

# Ref of a release target's branch: the remote one when it exists.
branch_ref() {
	if git rev-parse --verify --quiet "refs/remotes/origin/$1" >/dev/null; then
		printf 'origin/%s\n' "$1"
	elif git rev-parse --verify --quiet "refs/heads/$1" >/dev/null; then
		printf '%s\n' "$1"
	fi
}

tag_pattern=$(p release.tag_pattern)
skip_subjects=$(p release.skip_subjects)
ignore_paths=$(p release.ignore_paths)
[ -n "$ignore_paths" ] || ignore_paths=$(bash "$here/context.sh" get planning_paths)
changes_threshold=$(p release.changes_threshold)
age_days=$(p release.age_days)
p1_days=$(p release.p1_days)
now=$(date +%s)

# The highest release tag in semantic version order.
last_tag=$(git tag -l | tr -d '\r' | grep -E -- "$tag_pattern" |
	awk '{ v = $0; sub(/^[^0-9]*/, "", v); split(v, n, "."); printf "%09d%09d%09d\t%s\n", n[1], n[2], n[3], $0 }' |
	sort | tail -n 1 | cut -f 2) || true
if [ -n "$last_tag" ]; then
	last_version=$(printf '%s\n' "$last_tag" | sed 's/^[^0-9]*//')
	range="$last_tag..$ref"
	last_date=$(git log -1 --format=%cs "$last_tag")
else
	last_version=0.0.0
	range=$ref
	last_date=1970-01-01
fi

skipped_subject() {
	local subject=$1 prefix
	for prefix in $skip_subjects; do
		case "$subject" in "$prefix"*) return 0 ;; esac
	done
	return 1
}

# True when every file the commit changes lies under one of the ignored paths.
only_ignored_paths() {
	local sha=$1 file path hit
	[ -n "$ignore_paths" ] || return 1
	git rev-parse --verify --quiet "$sha^1" >/dev/null || return 1
	while IFS= read -r file; do
		[ -n "$file" ] || continue
		hit=1
		for path in $ignore_paths; do
			path=${path%/}
			case "$file" in "$path" | "$path"/*) hit=0 ;; esac
		done
		[ "$hit" = 0 ] || return 1
	done <<EOF
$(git diff --name-only "$sha^1" "$sha" | tr -d '\r')
EOF
	return 0
}

# Prints "<kind>" for a commit: breaking, feat, fix, perf, internal or unclassified.
kind_of() {
	local sha=$1 subject=$2 type bang
	if ! printf '%s\n' "$subject" | grep -Eq '^[a-z]+(\([^)]*\))?!?: '; then
		echo unclassified
		return
	fi
	type=$(printf '%s\n' "$subject" | sed -E 's/^([a-z]+).*/\1/')
	bang=$(printf '%s\n' "$subject" | sed -E 's/^[a-z]+(\([^)]*\))?(!?):.*/\2/')
	if [ "$bang" = "!" ] || git log -1 --format=%B "$sha" | grep -q '^BREAKING[ -]CHANGE:'; then
		echo breaking
		return
	fi
	case "$type" in feat | fix | perf) echo "$type" ;; *) echo internal ;; esac
}

user_facing=""
user_count=0
feats=0
fixes=0
breaking=0
internal=0
unclassified=0
oldest=$now
while IFS="$(printf '\t')" read -r sha stamp subject; do
	[ -n "$sha" ] || continue
	skipped_subject "$subject" && continue
	only_ignored_paths "$sha" && continue
	kind=$(kind_of "$sha" "$subject")
	case "$kind" in
	internal) internal=$((internal + 1)) ;;
	unclassified) unclassified=$((unclassified + 1)) ;;
	*)
		user_count=$((user_count + 1))
		case "$kind" in feat) feats=$((feats + 1)) ;; breaking) breaking=$((breaking + 1)) ;; *) fixes=$((fixes + 1)) ;; esac
		[ "$stamp" -lt "$oldest" ] && oldest=$stamp
		user_facing="$user_facing- $subject (\`$(git rev-parse --short "$sha")\`)
"
		;;
	esac
done <<EOF
$(git log --first-parent --format='%H%x09%ct%x09%s' --end-of-options "$range" | tr -d '\r')
EOF

# Fixes for P0 and P1 items: issues with that priority closed as completed since the last
# release. A P1 counts towards "due" once it was closed at least p1_days ago.
p0=0
p1_aged=0
repo=""
if [ "$offline" != 1 ]; then
	repo=$(bash "$here/context.sh" get repo)
	p0=$(gh issue list -R "$repo" --state closed --label "priority: P0" \
		--search "closed:>=$last_date reason:completed" --json number --jq length 2>/dev/null | tr -d '\r') || p0=0
	p1_aged=$(gh issue list -R "$repo" --state closed --label "priority: P1" \
		--search "closed:>=$last_date reason:completed" --json closedAt \
		--jq "[.[] | select((now - (.closedAt | fromdateiso8601)) >= ($p1_days * 86400))] | length" 2>/dev/null | tr -d '\r') || p1_aged=0
fi

oldest_days=$(((now - oldest) / 86400))
if [ "${p0:-0}" -gt 0 ]; then
	state=now
elif [ "${p1_aged:-0}" -gt 0 ] || [ "$user_count" -ge "$changes_threshold" ] ||
	{ [ "$user_count" -gt 0 ] && [ "$oldest_days" -ge "$age_days" ]; }; then
	state=due
elif [ "$user_count" -gt 0 ]; then
	state=accumulating
else
	state=none
fi

IFS=. read -r major minor patch <<EOF
$last_version
EOF
if [ "$breaking" -gt 0 ]; then
	if [ "$major" -eq 0 ]; then bump=minor; else bump=major; fi
elif [ "$feats" -gt 0 ]; then
	bump=minor
else
	bump=patch
fi
case "$bump" in
major) next="$((major + 1)).0.0" ;;
minor) next="$major.$((minor + 1)).0" ;;
patch) next="$major.$minor.$((patch + 1))" ;;
esac
[ "$state" != none ] || next=""

plural() { if [ "$1" -eq 1 ]; then printf '%s %s' "$1" "$2"; else printf '%s %s' "$1" "$3"; fi; }

targets_section() {
	local file branch_pattern tag_pattern_t build_file listed target b bref tag build own
	file=$(p release.targets_file)
	[ -n "$file" ] || return 0
	branch_pattern=$(p release.target_branch)
	tag_pattern_t=$(p release.target_tag)
	build_file=$(p release.target_build_file)
	listed=$(git show "$ref:$file" 2>/dev/null || cat "$file" 2>/dev/null) || {
		printf '## Release targets\n\nThe targets file `%s` was not found.\n\n' "$file"
		return 0
	}
	printf '## Release targets\n\nEvery release contains every target listed in `%s`.\n\n' "$file"
	printf '| Target | Branch | Verified against | Own changes since %s |\n| --- | --- | --- | --- |\n' "${last_tag:-the start}"
	while IFS= read -r target; do
		target=$(printf '%s' "$target" | tr -d '\r' | sed 's/#.*//; s/[[:space:]]//g')
		[ -n "$target" ] || continue
		b=$(printf '%s' "$branch_pattern" | sed "s/{t}/$target/g")
		bref=$(branch_ref "$b")
		if [ -z "$bref" ]; then
			printf '| %s | `%s` | — | branch not found |\n' "$target" "$b"
			continue
		fi
		build="—"
		if [ -n "$build_file" ]; then
			build=$(git show "$bref:$build_file" 2>/dev/null | tr -d '\r' | head -n 1 | sed 's/[[:space:]]*$//') || build="—"
			build="\`${build:-—}\`"
		fi
		own="none"
		tag=$(printf '%s' "$tag_pattern_t" | sed "s/{version}/$last_version/g; s/{t}/$target/g")
		if [ -n "$last_tag" ] && git rev-parse --verify --quiet "refs/tags/$tag" >/dev/null; then
			local lines="" sha subject
			while IFS="$(printf '\t')" read -r sha subject; do
				[ -n "$sha" ] || continue
				skipped_subject "$subject" && continue
				only_ignored_paths "$sha" && continue
				lines="$lines${lines:+<br>}$subject (\`$(git rev-parse --short "$sha")\`)"
			done <<EOF
$(git log --first-parent --format='%H%x09%s' "$tag..$bref" | tr -d '\r')
EOF
			[ -z "$lines" ] || own=$lines
		else
			own="first release of this target"
		fi
		printf '| %s | `%s` | %s | %s |\n' "$target" "$b" "$build" "$own"
	done <<EOF
$listed
EOF
	printf '\nThe default branch `%s` is the development line; it is not a release target.\n\n' "$branch"
}

notice() {
	local repo_name=${repo:-project} procedure since
	since=${last_tag:-the first commit}
	printf '## Recommendation\n\n'
	case "$state" in
	now) printf '**Release now: v%s** (%s). An unreleased fix resolves a P0 item.\n\n' "$next" "$bump" ;;
	due) printf '**A release is due: v%s** (%s).\n\n' "$next" "$bump" ;;
	accumulating) printf 'Changes are accumulating for v%s (%s); no release is needed yet.\n\n' "$next" "$bump" ;;
	none) printf 'Nothing user-facing has changed since %s.\n\n' "$since" ;;
	esac
	if [ "$state" != none ]; then
		printf 'Since %s: %s, %s, %s. The oldest change is %s old.' "$since" \
			"$(plural "$feats" feature features)" "$(plural "$fixes" fix fixes)" "$(plural "$breaking" "breaking change" "breaking changes")" \
			"$(plural "$oldest_days" day days)"
		[ "$offline" = 1 ] || printf ' Fixed P0 items: %s; P1 items fixed at least %s days ago: %s.' "$p0" "$p1_days" "$p1_aged"
		printf '\n\n## User-facing changes\n\n%s\n' "$user_facing"
		targets_section
		procedure=$(p release.procedure)
		printf '## How to release\n\n'
		if [ -n "$procedure" ]; then
			printf 'Ask your agent to release v%s, following %s. The procedure checks every target, and a person approves every tag before it is pushed.\n\n' "$next" "$procedure"
		else
			printf 'Release v%s with the project'"'"'s release procedure. A person approves every tag before it is pushed.\n\n' "$next"
		fi
	fi
	printf '## Not counted\n\n%s (documentation, tests, automation, refactoring) and %s without a Conventional Commit type.\n\n' \
		"$(plural "$internal" "internal commit" "internal commits")" "$(plural "$unclassified" commit commits)"
	printf '_Kept current by the repo steward, an AI assistant working for the %s maintainers. It never publishes a release._\n' "$repo_name"
	printf '<!-- repo-steward:v1 kind=readiness state=%s version=%s -->\n' "$state" "${next:-none}"
}

body=$(notice)
printf 'state=%s version=%s\n%s\n' "$state" "$next" "$body"

[ "$publish" = 1 ] || exit 0
if [ "$offline" = 1 ]; then
	echo "readiness.sh: --publish needs the network; STEWARD_OFFLINE=1 is set" >&2
	exit 2
fi

rank() { case "$1" in now) echo 3 ;; due) echo 2 ;; accumulating) echo 1 ;; *) echo 0 ;; esac; }
label=release-readiness
gh label create "$label" -R "$repo" --color 0e8a16 \
	--description "Release readiness notice kept current by the steward" >/dev/null 2>&1 || true
open=$(gh issue list -R "$repo" --label "$label" --state open --limit 1 --json number --jq '.[0].number // empty' | tr -d '\r')
title="Release readiness: v$next ($state)"
body_file=$(mktemp)
trap 'rm -f "$body_file"' EXIT
printf '%s\n' "$body" >"$body_file"
# Git for Windows' mktemp prints /tmp/..., which a native Windows gh cannot open.
gh_body_file=$body_file
if command -v cygpath >/dev/null 2>&1; then gh_body_file=$(cygpath -w "$body_file"); fi

if [ "$state" = none ]; then
	if [ -n "$open" ]; then
		gh issue close "$open" -R "$repo" --reason completed --comment \
			"Nothing user-facing is waiting since ${last_tag:-the first commit}; closing this notice.

_Kept current by the repo steward, an AI assistant working for the $repo maintainers._
<!-- repo-steward:v1 kind=readiness state=none -->" >/dev/null
		echo "closed the readiness notice #$open"
	fi
	exit 0
fi

if [ -z "$open" ]; then
	url=$(gh issue create -R "$repo" --title "$title" --label "$label" --body-file "$gh_body_file" | tr -d '\r')
	echo "opened the readiness notice $url"
	exit 0
fi

previous=$(gh issue view "$open" -R "$repo" --json body --jq .body | tr -d '\r' |
	sed -n 's/.*kind=readiness state=\([a-z]*\).*/\1/p' | tail -n 1)
gh issue edit "$open" -R "$repo" --title "$title" --body-file "$gh_body_file" >/dev/null
echo "updated the readiness notice #$open"
if [ "$(rank "$state")" -gt "$(rank "${previous:-none}")" ]; then
	gh issue comment "$open" -R "$repo" --body "Release readiness rose to **$state**: v$next.

_Kept current by the repo steward, an AI assistant working for the $repo maintainers._
<!-- repo-steward:v1 kind=readiness state=$state -->" >/dev/null
	echo "commented: state rose from ${previous:-none} to $state"
fi
