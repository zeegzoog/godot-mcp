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
#   act.sh sensitive <path>...      print the paths a contributor's merge may not touch (exit 1 when any)
#   act.sh pins-only < patch        succeed when a workflow patch only moves `uses:` pins within one major version
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

# Paths a contributor's change must never reach the default branch through the steward alone:
# what runs while the project builds (review.sensitive_paths), the steward's and the agents' own
# instructions and scripts, and the dependency manifests. A name ending in "/" is a directory
# anywhere in the tree; any other name is a file of that name anywhere. Compared in lower case.
protected_paths() {
	local entry
	for entry in $(bash "$here/profile.sh" get review.sensitive_paths) .github/ .agents/ .claude/ Cargo.toml Cargo.lock; do
		printf '%s\n' "$entry" | tr '[:upper:]' '[:lower:]'
	done | sort -u
}

# Prints each given path that is protected; the status is 1 when there is one.
protected_hits() {
	local entries path lower entry hit=0
	entries=$(protected_paths)
	for path in "$@"; do
		lower=$(printf '%s' "$path" | tr '[:upper:]' '[:lower:]')
		while IFS= read -r entry; do
			[ -n "$entry" ] || continue
			case "$entry" in
			*/) case "$lower" in "$entry"* | *"/$entry"*) printf '%s\n' "$path" && hit=1 && break ;; esac ;;
			*) case "$lower" in "$entry" | *"/$entry") printf '%s\n' "$path" && hit=1 && break ;; esac ;;
			esac
		done <<<"$entries"
	done
	return $hit
}

# The part of the protected set that a merge by the steward never reaches, whatever it decides:
# its own rules, scripts and workflows, everything else under .agents/ and .github/, the tool
# settings, the Cargo configuration, build scripts (they run code whenever the project is built)
# and every workflow but ci.yml (the release, cleanup and steward workflows run with the project's
# secrets and publish). What stays reachable is small and structured: ci.yml, the release skill's
# SKILL.md, Cargo.toml and Cargo.lock. They are merged after a read of every changed line, and only
# when danger_in_patch finds that every changed line has one of the plain shapes it knows. Same name
# rules as protected_paths; prints each locked path (nothing when none).
locked_hits() {
	local path lower entry
	for path in "$@"; do
		lower=$(printf '%s' "$path" | tr '[:upper:]' '[:lower:]')
		case "$lower" in .github/workflows/ci.yml | .agents/skills/release/skill.md) continue ;; esac
		for entry in .agents/ .github/ .claude/ .cargo/ build.rs; do
			case "$entry" in
			*/) case "$lower" in "$entry"* | *"/$entry"*) printf '%s\n' "$path" && break ;; esac ;;
			*) case "$lower" in "$entry" | *"/$entry") printf '%s\n' "$path" && break ;; esac ;;
			esac
		done
	done | sort -u
	return 0
}

# Succeeds when the line matches one of the extended regular expressions given after it.
matches_a_shape() {
	local line=$1 re
	shift
	for re in "$@"; do
		if printf '%s' "$line" | grep -Eq "$re"; then return 0; fi
	done
	return 1
}

# A line of Cargo.toml that names one of the package's own settings, not a dependency: the
# shape of `name = "1.0"` fits both, and only a release changes the package.
cargo_package_key() {
	local key
	key=$(printf '%s' "$1" | sed -E 's/^[[:space:]]*([A-Za-z0-9_-]+)[[:space:]]*=.*/\1/')
	case "$key" in version | edition | rust-version | name | license | readme | repository | description | authors | resolver | publish | build | links | homepage | documentation | include | exclude | default-run | categories | keywords) return 0 ;; esac
	return 1
}

# Reads the added lines of a Cargo.lock patch ($1) and succeeds only when every package block it
# adds or changes has the checksum crates.io publishes for that name and version. Offline, or
# when crates.io does not answer, it is a no: the caller fails closed.
lock_checksums_verified() {
	local line name="" version="" sum answer
	while IFS= read -r line; do
		case "$line" in
		'name = "'*) name=${line#name = \"}; name=${name%\"} ;;
		'version = "'*) version=${line#version = \"}; version=${version%\"} ;;
		'checksum = "'*)
			sum=${line#checksum = \"}
			sum=${sum%\"}
			[ -n "$name" ] && [ -n "$version" ] || return 1
			answer=$(curl -fsS --max-time 20 -A "godot-mcp-steward (repository maintenance)" \
				"https://crates.io/api/v1/crates/$name/$version" 2>/dev/null | grep -o '"checksum":"[0-9a-f]*"' | head -1) || return 1
			[ "$answer" = "\"checksum\":\"$sum\"" ] || return 1
			;;
		esac
	done <<<"$1"
	return 0
}

# Reads the patch of one protected file on stdin and prints each changed line that is not one of
# the plain shapes that file may gain, with the reason; status 1 when it prints any. The shapes
# are a list of what contributors really change: a syntax-check line or a `cargo test --test`
# step of ci.yml, a syntax-check line of the release skill, a plain registry dependency of
# Cargo.toml, a crates.io package block of Cargo.lock whose checksum is the published one. Every
# changed line, in any file, is also refused when it is shaped like a credential or like an
# instruction to an agent. A patch that GitHub did not show is not scanned: the caller fails
# closed on an empty patch.
danger_in_patch() {
	local path=$1 lower patch added removed line bad=0 gd
	local -a shapes=()
	local removals_ok=0
	lower=$(printf '%s' "$path" | tr '[:upper:]' '[:lower:]')
	patch=$(cat)
	added=$(printf '%s\n' "$patch" | grep -E '^\+' | grep -vE '^\+\+\+' | sed 's/^+//' || true)
	removed=$(printf '%s\n' "$patch" | grep -E '^-' | grep -vE '^---' | sed 's/^-//' || true)
	gd='[A-Za-z0-9_./-]+\.gd'
	case "$lower" in
	.github/workflows/ci.yml)
		shapes=(
			'^[[:space:]]*\$GODOT_PATH --headless --check-only --path \. --script addons/godot_mcp/'"$gd"'[[:space:]]*$'
			'^[[:space:]]*(-[[:space:]]+)?name:[[:space:]]+[A-Za-z0-9 ,.()/_-]+[[:space:]]*$'
			'^[[:space:]]*run:[[:space:]]+cargo test --test [a-z0-9_]+( -- --ignored)?( --test-threads=[0-9]+)?[[:space:]]*$'
		)
		;;
	.agents/skills/release/skill.md)
		shapes=('^[[:space:]]*godot --headless --check-only --path \. --script addons/godot_mcp/'"$gd"'[[:space:]]*$')
		;;
	*cargo.toml)
		removals_ok=1
		local ver='"[0-9][0-9A-Za-z.+*~^<>=, -]*"'
		shapes=(
			'^[[:space:]]*[A-Za-z0-9_-]+[[:space:]]*=[[:space:]]*'"$ver"'[[:space:]]*$'
			'^[[:space:]]*[A-Za-z0-9_-]+[[:space:]]*=[[:space:]]*\{[[:space:]]*version[[:space:]]*=[[:space:]]*'"$ver"'([[:space:]]*,[[:space:]]*default-features[[:space:]]*=[[:space:]]*(true|false))?([[:space:]]*,[[:space:]]*features[[:space:]]*=[[:space:]]*\[("[A-Za-z0-9_/-]+"(,[[:space:]]*)?)*\])?[[:space:]]*\}[[:space:]]*$'
			'^[[:space:]]*[A-Za-z0-9_-]+[[:space:]]*=[[:space:]]*\{[[:space:]]*version[[:space:]]*=[[:space:]]*'"$ver"'([[:space:]]*,[[:space:]]*default-features[[:space:]]*=[[:space:]]*(true|false))?[[:space:]]*,[[:space:]]*features[[:space:]]*=[[:space:]]*\[[[:space:]]*$'
			'^[[:space:]]*"[A-Za-z0-9_/-]+",?[[:space:]]*$'
			'^[[:space:]]*\][[:space:]]*\}?[[:space:]]*$'
			"^\\[(target\\.'cfg\\([a-z_ =\"()]+\\)'\\.)?(dev-)?dependencies\\]\$"
		)
		;;
	*cargo.lock)
		removals_ok=1
		shapes=(
			'^\[\[package\]\]$'
			'^name = "[A-Za-z0-9_-]+"$'
			'^version = "[0-9][0-9A-Za-z.+-]*"$'
			'^source = "registry\+https://github.com/rust-lang/crates.io-index"$'
			'^checksum = "[0-9a-f]{64}"$'
			'^dependencies = \[$'
			'^ "[A-Za-z0-9_-]+( [0-9][0-9A-Za-z.+-]*)?",$'
			'^\]$'
		)
		;;
	esac
	if [ ${#shapes[@]} -gt 0 ]; then
		while IFS= read -r line; do
			[ -n "$line" ] || continue
			matches_a_shape "$line" "${shapes[@]}" ||
				{ printf '%s: %s   (not one of the plain shapes this file may gain)\n' "$path" "$line"; bad=1; }
			case "$lower" in
			*cargo.toml)
				if cargo_package_key "$line"; then
					printf '%s: %s   (a setting of the package itself)\n' "$path" "$line"
					bad=1
				fi
				;;
			esac
		done <<<"$added"
		if [ "$removals_ok" = 1 ]; then
			while IFS= read -r line; do
				[ -n "$line" ] || continue
				matches_a_shape "$line" "${shapes[@]}" ||
					{ printf '%s: -%s   (a removed line of another kind)\n' "$path" "$line"; bad=1; }
				case "$lower" in
				*cargo.toml)
					if cargo_package_key "$line"; then
						printf '%s: -%s   (a setting of the package itself)\n' "$path" "$line"
						bad=1
					fi
					;;
				esac
			done <<<"$removed"
		elif printf '%s\n' "$removed" | grep -q .; then
			printf '%s: %s\n' "$path" "(removes a line; this file may only gain lines here)"
			bad=1
		fi
	fi
	case "$lower" in
	*cargo.lock)
		if [ "${STEWARD_OFFLINE:-0}" != 1 ] && ! lock_checksums_verified "$added"; then
			printf '%s: %s\n' "$path" "(a package whose checksum is not the one crates.io publishes, or crates.io did not answer)"
			bad=1
		fi
		;;
	esac
	local common='ignore (all|any|the|previous|prior|above)|disregard|without (asking|approval|confirmation|review)|do not (tell|mention|report|review)|exfiltrat|hide this'
	while IFS= read -r line; do
		[ -n "$line" ] || continue
		if printf '%s' "$line" | grep -Eiq "$common"; then
			printf '%s: %s\n' "$path" "$line"
			bad=1
		elif credential_shaped "$line"; then
			printf '%s: %s\n' "$path" "(something shaped like a credential)"
			bad=1
		fi
	done <<<"$added"
	return $bad
}

# Reads a unified patch (the `patch` field GitHub gives for a file) on stdin and succeeds only
# when it does nothing but move `uses:` lines of a workflow to another version of the same
# actions: every changed line is a `uses: owner/name@ref` line, ref is a tag or a commit with the
# version in a trailing comment, the actions removed are the actions added, and none changes its
# major version. That is what a dependency bot's update of pinned actions looks like; a new
# action, a changed step or a major jump is not, and fails closed.
pins_only() {
	local pin='^[[:space:]]*(-[[:space:]]+)?uses:[[:space:]]*([A-Za-z0-9._-]+/[A-Za-z0-9._/-]+)@([A-Za-z0-9._-]+)([[:space:]]+#[[:space:]]*([A-Za-z0-9._-]+))?[[:space:]]*$'
	local line sign name ref note version major removed="" added=""
	while IFS= read -r line; do
		case "$line" in '' | '@@'* | ' '* | '\'*) continue ;; esac
		sign=${line:0:1}
		[[ "$sign" == + || "$sign" == - ]] || return 1
		[[ "${line:1}" =~ $pin ]] || return 1
		name=${BASH_REMATCH[2]}
		ref=${BASH_REMATCH[3]}
		note=${BASH_REMATCH[5]:-}
		if [[ "$ref" =~ ^[0-9a-f]{40}$ ]]; then version=$note; else version=$ref; fi
		[[ "$version" =~ ^v?([0-9]+)(\.[0-9]+){0,2}$ ]] || return 1
		major=${BASH_REMATCH[1]}
		if [ "$sign" = - ]; then removed+="$name $major"$'\n'; else added+="$name $major"$'\n'; fi
	done
	# Line for line: the same actions, the same number of times, not merely the same set.
	[ -n "$added" ] && [ "$(printf '%s' "$removed" | sort)" = "$(printf '%s' "$added" | sort)" ]
}

# Reads the same patch and prints "owner/name commit version" for every added line that pins a
# commit. The version is only a comment in the file; pins_match_tags checks it against GitHub.
added_commits() {
	local pin='^[[:space:]]*(-[[:space:]]+)?uses:[[:space:]]*([A-Za-z0-9._-]+/[A-Za-z0-9._/-]+)@([0-9a-f]{40})[[:space:]]+#[[:space:]]*([A-Za-z0-9._-]+)[[:space:]]*$'
	local line
	while IFS= read -r line; do
		[[ "${line:0:1}" == + ]] || continue
		if [[ "${line:1}" =~ $pin ]]; then
			printf '%s %s %s\n' "${BASH_REMATCH[2]}" "${BASH_REMATCH[3]}" "${BASH_REMATCH[4]}"
		fi
	done
	return 0
}

# True when every commit a patch pins is the commit of the tag its comment names. GitHub is
# asked; an answer that does not come is a no.
pins_match_tags() {
	local action sha tag resolved
	while read -r action sha tag; do
		resolved=$(gh api "repos/$(printf '%s' "$action" | cut -d/ -f1,2)/commits/$tag" --jq .sha 2>/dev/null | tr -d '\r') || return 1
		[ "$resolved" = "$sha" ] || return 1
	done < <(added_commits <<<"$1")
	return 0
}

# A merge the script itself refuses, whatever the agent decided: a pull request by someone
# without write permission that changes a protected path (new and old names of every file). The
# check fails closed: when GitHub does not answer, or the list of files is not the whole list,
# the merge does not happen. It leaves the head commit it looked at in `head_sha`; the merge is
# made for exactly that commit, so a push after the check turns the merge down.
head_sha=""
reviewed_statement=""
guard_merge() {
	[ "${STEWARD_OFFLINE:-0}" != 1 ] || return 0
	local info author is_bot changed permission rows count files hits paths bot allowed named from_bot=0 path
	info=$(gh pr view "$1" -R "$repo" --json author,headRefOid,changedFiles \
		--jq '[.author.login, .author.is_bot, .headRefOid, .changedFiles] | @tsv' | tr -d '\r') ||
		fail "cannot read #$1; nothing was merged"
	IFS=$'\t' read -r author is_bot head_sha changed <<<"$info"
	[ -n "$author" ] && [ -n "$head_sha" ] || fail "cannot read the author and head of #$1; nothing was merged"
	if [ "$is_bot" = true ]; then
		# A dependency bot is governed by autonomy.merge_bots and must be one the profile names. Its
		# files are still checked: only the dependency manifests and lockfiles are its business.
		[ "$(bash "$here/profile.sh" get autonomy.merge_bots)" = allowed ] ||
			fail "the profile asks for a maintainer's approval before a bot's pull request is merged"
		bot=${author#app/}
		bot=${bot%\[bot\]}
		for allowed in $(bash "$here/profile.sh" get limits.allowed_bots); do
			[ "$allowed" != "$bot[bot]" ] || named=1
		done
		[ "${named:-0}" = 1 ] || fail "$author is not in limits.allowed_bots; nothing was merged"
		from_bot=1
	else
		permission=$(bash "$here/api.sh" permission "$author") ||
			fail "cannot read the permission of $author; nothing was merged"
		case "$permission" in admin | maintain | write) return 0 ;; esac
	fi
	rows=$(gh api --paginate "repos/$repo/pulls/$1/files" --jq '.[] | [.filename, (.previous_filename // "")] | @tsv' | tr -d '\r') ||
		fail "cannot list the files of #$1; nothing was merged"
	count=$(printf '%s\n' "$rows" | grep -c .) || true
	# GitHub lists at most 3000 files; a shorter list than the pull request has would hide some.
	case "$changed" in '' | *[!0-9]*) fail "cannot read the number of files of #$1; nothing was merged" ;; esac
	{ [ "$count" -eq "$changed" ] && [ "$changed" -lt 3000 ]; } ||
		fail "the file list of #$1 ($count of $changed) is not the whole change; nothing was merged"
	if [ "$from_bot" = 1 ]; then
		# A bot's business: the dependency manifests and lockfiles, and the version in the `uses:`
		# lines of a workflow (pins_only). Everything else is checked like a contributor's change.
		local kept=() old patch
		while IFS=$'\t' read -r path old; do
			[ -n "$path" ] || continue
			# A renamed file is never exempt: its old name is checked as well (below).
			if [ -z "$old" ]; then
				case "$(printf '%s' "${path##*/}" | tr '[:upper:]' '[:lower:]')" in cargo.toml | cargo.lock) continue ;; esac
				if [[ "$path" =~ ^\.github/workflows/[A-Za-z0-9._-]+\.ya?ml$ ]]; then
					patch=$(gh api "repos/$repo/pulls/$1/files" --paginate \
						--jq ".[] | select(.filename == \"$path\") | .patch // \"\"" | tr -d '\r') || patch=""
					if [ -n "$patch" ] && pins_only <<<"$patch" && pins_match_tags "$patch"; then continue; fi
				fi
			fi
			kept+=("$path")
			[ -z "$old" ] || kept+=("$old")
		done <<<"$rows"
		[ ${#kept[@]} -gt 0 ] || return 0
		paths=("${kept[@]}")
	else
		files=$(printf '%s\n' "$rows" | tr '\t' '\n' | grep . || true)
		mapfile -t paths <<<"$files"
	fi
	if hits=$(protected_hits "${paths[@]}"); then return 0; fi
	# A protected path. Some are never the steward's to merge; the others are merged after it has
	# read every changed line of them (--protected-reviewed names what they do) and the content
	# scan finds nothing in the files themselves.
	local locked scan_failed=0 hit_path patch danger
	locked=$(locked_hits "${paths[@]}")
	[ -z "$locked" ] ||
		fail "#$1 changes a path the steward never merges ($(printf '%s' "$locked" | head -3 | tr '\n' ' ')): its own rules and automation, the agent and tool settings, the release workflows. Label it 'status: needs-maintainer' and list it in the report"
	[ -n "$reviewed_statement" ] ||
		fail "#$1 changes a protected path ($(printf '%s' "$hits" | head -3 | tr '\n' ' ')). Read every changed line of it, then merge with --protected-reviewed \"<one line: what the protected changes do>\""
	while IFS= read -r hit_path; do
		[ -n "$hit_path" ] || continue
		patch=$(gh api "repos/$repo/pulls/$1/files" --paginate \
			--jq ".[] | select(.filename == \"$hit_path\") | .patch // \"\"" | tr -d '\r') || patch=""
		if [ -z "$patch" ]; then
			printf 'act.sh: GitHub shows no patch of %s (renamed, binary or too large); a protected file that cannot be read line by line is not merged\n' "$hit_path" >&2
			scan_failed=1
			continue
		fi
		if ! danger=$(danger_in_patch "$hit_path" <<<"$patch"); then
			printf '%s\n' "$danger" | head -5 >&2
			scan_failed=1
		fi
	done <<<"$hits"
	[ "$scan_failed" = 0 ] ||
		fail "#$1: the content scan of its protected files found what a change there must not do (above); nothing was merged. Label it 'status: needs-maintainer' and list it in the report"
	printf 'act.sh: protected-path merge of #%s after review: %s\n' "$1" "$reviewed_statement" >&2
	return 0
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
pins-only)
	[ $# -eq 0 ] || fail "usage: pins-only < patch"
	pins_only
	;;
sensitive)
	[ $# -ge 1 ] || fail "usage: sensitive <path>..."
	if protected_hits "$@"; then exit 0; else exit 1; fi
	;;
locked)
	[ $# -ge 1 ] || fail "usage: locked <path>..."
	locked=$(locked_hits "$@")
	[ -z "$locked" ] || { printf "%s\n" "$locked"; exit 1; }
	;;
patch-danger)
	[ $# -eq 1 ] || fail "usage: patch-danger <path> < patch"
	danger_in_patch "$1"
	;;
approve-run)
	[ $# -eq 1 ] || fail "usage: approve-run <run-id>"
	number "$1"
	run gh api -X POST "repos/$repo/actions/runs/$1/approve" --silent
	;;
merge)
	{ [ $# -eq 3 ] || { [ $# -eq 5 ] && [ "$4" = --protected-reviewed ]; }; } || fail "usage: merge <pr> squash|merge|rebase <subject> [--protected-reviewed <statement>]"
	number "$1"
	one_line "$3" subject
	if [ $# -eq 5 ]; then one_line "$5" statement; reviewed_statement=$5; fi
	case "$2" in squash | merge | rebase) guard_merge "$1" ;; esac
	case "$2" in
	squash | merge | rebase)
		if [ -n "$head_sha" ]; then
			run gh pr merge "$1" -R "$repo" "--$2" --subject "$3" --match-head-commit "$head_sha"
		else
			run gh pr merge "$1" -R "$repo" "--$2" --subject "$3"
		fi
		;;
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
