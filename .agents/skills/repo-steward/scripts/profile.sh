#!/usr/bin/env bash
# Reads the repo-steward profile (.github/steward.yml) and falls back to the defaults below.
#
#   profile.sh get <section>.<key>   print one value
#   profile.sh list                  print every key as "key=value (profile|default)"
#
# The profile is a YAML subset: top-level section names, each followed by indented
# "key: value" lines with scalar values. A list is one space-separated string. Values may be
# quoted; a " #" outside quotes starts a comment. STEWARD_PROFILE overrides the file path.
set -euo pipefail
# Git for Windows' bash rewrites arguments that look like paths ("ref:dir/file") unless told not to.
export MSYS_NO_PATHCONV=1 MSYS2_ARG_CONV_EXCL='*'

defaults() {
	cat <<'EOF'
tracker.language=English
project.guides=AGENTS.md CLAUDE.md CONTRIBUTING.md SECURITY.md
planning.ecosystem=auto
board.title={repo} issues and pull requests
board.public=auto
board.wip_in_progress=2
board.wip_in_review=3
board.text_fields=
board.done_retention_days=14
triage.fingerprint=gmfb[0-9a-f]{12}
triage.priority_examples=
review.sensitive_paths=.github/
merge.method=squash
autonomy.messages=allowed
autonomy.close_routine=allowed
autonomy.run_approval=allowed
autonomy.merge_own=allowed
autonomy.merge_bots=allowed
autonomy.merge_external=approval
autonomy.delete_merged_branches=allowed
timing.needs_info_reminder_days=14
timing.needs_info_close_days=7
limits.max_actions=20
limits.dispute_rounds=2
limits.allowed_bots=dependabot[bot] renovate[bot]
release.procedure=
release.tag_pattern=^v[0-9]+\.[0-9]+\.[0-9]+$
release.ignore_paths=
release.skip_subjects=chore(release)
release.changes_threshold=3
release.age_days=21
release.p1_days=3
release.targets_file=
release.target_branch=
release.target_tag=
release.target_build_file=
EOF
}

profile_file() {
	if [ -n "${STEWARD_PROFILE:-}" ]; then
		printf '%s\n' "$STEWARD_PROFILE"
		return
	fi
	local top
	top=$(git rev-parse --show-toplevel 2>/dev/null || pwd)
	printf '%s\n' "$top/.github/steward.yml"
}

# Prints "section.key<TAB>value" for every key the profile sets.
profile_values() {
	local file
	file=$(profile_file)
	[ -f "$file" ] || return 0
	awk -v q="'" '
		function unquote(v, mark,   i) {
			v = substr(v, 2); i = index(v, mark)
			return i ? substr(v, 1, i - 1) : v
		}
		{ sub(/\r$/, "") }
		/^[ \t]*#/ || /^[ \t]*$/ { next }
		/^[A-Za-z_][A-Za-z0-9_]*:[ \t]*(#.*)?$/ {
			section = $0; sub(/:.*/, "", section); next
		}
		/^[ \t]+[A-Za-z_][A-Za-z0-9_]*:/ && section != "" {
			line = $0; sub(/^[ \t]+/, "", line)
			key = line; sub(/:.*/, "", key)
			value = line; sub(/^[^:]*:[ \t]*/, "", value)
			first = substr(value, 1, 1)
			if (first == "\"" || first == q) { value = unquote(value, first) }
			else { sub(/[ \t]+#.*$/, "", value); sub(/[ \t]+$/, "", value) }
			printf "%s.%s\t%s\n", section, key, value
			next
		}
		/^[^ \t]/ { section = "" }
	' "$file"
}

get() {
	local want=$1 found
	found=$(profile_values | awk -F '\t' -v want="$want" '$1 == want { print $2; hit = 1; exit } END { exit !hit }') && {
		printf '%s\n' "$found"
		return
	}
	found=$(defaults | awk -v want="$want" 'index($0, want "=") == 1 { print substr($0, length(want) + 2); hit = 1; exit } END { exit !hit }') && {
		printf '%s\n' "$found"
		return
	}
	echo "profile.sh: unknown key '$want'; see the defaults in this script" >&2
	return 2
}

list() {
	local set
	set=$(profile_values)
	defaults | while IFS= read -r line; do
		local key=${line%%=*}
		if printf '%s\n' "$set" | awk -F '\t' -v want="$key" '$1 == want { hit = 1 } END { exit !hit }'; then
			printf '%s=%s (profile)\n' "$key" "$(get "$key")"
		else
			printf '%s (default)\n' "$line"
		fi
	done
	printf '%s\n' "$set" | awk -F '\t' 'NF' | while IFS="$(printf '\t')" read -r key _; do
		defaults | awk -v want="$key" 'index($0, want "=") == 1 { hit = 1 } END { exit !hit }' ||
			echo "profile.sh: '$key' in the profile is not a known key" >&2
	done
}

case "${1:-}" in
get)
	[ $# -eq 2 ] || { echo "usage: profile.sh get <section>.<key>" >&2; exit 2; }
	get "$2"
	;;
list) list ;;
*)
	echo "usage: profile.sh get <section>.<key> | profile.sh list" >&2
	exit 2
	;;
esac
