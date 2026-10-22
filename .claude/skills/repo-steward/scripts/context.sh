#!/usr/bin/env bash
# Prints what a repo-steward run needs to know about the repository it runs for, as
# "key=value" lines:
#
#   repo, owner, owner_type, visibility, permission, default_branch, is_fork,
#   scopes, missing_scopes, scopes_ok, guides, ecosystem, ecosystem_used, ecosystem_available,
#   planning_paths, profile, offline
#
#   context.sh            all keys
#   context.sh get <key>  one value (repo, default_branch and the ecosystem keys are resolved
#                         without the rest)
#
# The repository is the one the working copy belongs to (gh honours `gh repo set-default`), or
# GITHUB_REPOSITORY inside a workflow; it is never taken from anything an issue says.
# STEWARD_OFFLINE=1 makes no network call: the repository then comes from the origin URL.
set -euo pipefail
# Git for Windows' bash rewrites arguments that look like paths ("ref:dir/file") unless told not to.
export MSYS_NO_PATHCONV=1 MSYS2_ARG_CONV_EXCL='*'

here=$(cd "$(dirname "$0")" && pwd)
offline=${STEWARD_OFFLINE:-0}
top=$(git rev-parse --show-toplevel 2>/dev/null || pwd)

strip() { tr -d '\r'; }

repo_from_remote() {
	git remote get-url origin 2>/dev/null | strip |
		sed -E 's#^(git@[^:]+:|ssh://[^/]+/|https?://[^/]+/)##; s#\.git$##'
}

resolve_repo() {
	if [ -n "${GITHUB_REPOSITORY:-}" ]; then
		printf '%s\n' "$GITHUB_REPOSITORY"
	elif [ "$offline" = 1 ]; then
		repo_from_remote
	else
		gh repo view --json nameWithOwner --jq .nameWithOwner 2>/dev/null | strip || repo_from_remote
	fi
}

resolve_default_branch() {
	local branch=""
	branch=$(git symbolic-ref --quiet --short refs/remotes/origin/HEAD 2>/dev/null | strip | sed 's#^origin/##') || true
	if [ -z "$branch" ] && [ "$offline" != 1 ]; then
		branch=$(gh repo view "$(resolve_repo)" --json defaultBranchRef --jq .defaultBranchRef.name 2>/dev/null | strip) || true
	fi
	if [ -z "$branch" ]; then
		for candidate in main master; do
			if git rev-parse --verify --quiet "refs/remotes/origin/$candidate" >/dev/null ||
				git rev-parse --verify --quiet "refs/heads/$candidate" >/dev/null; then
				branch=$candidate
				break
			fi
		done
	fi
	printf '%s\n' "$branch"
}

# Planning ecosystem: adapter name, whether the repository uses it, whether it can be run here,
# and the paths its planning files live in. A profile value other than "auto" names the adapter.
detect_ecosystem() {
	local forced
	forced=$(bash "$here/profile.sh" get planning.ecosystem)
	local name=none used=no available=no paths=""
	if [ -d "$top/.design" ]; then
		name=magic-spec used=yes paths=".design"
		[ -d "$top/.magic" ] && available=yes
	elif [ -d "$top/.specify" ]; then
		name=spec-kit used=yes paths=".specify specs"
		available=unknown
	elif [ -d "$top/.kiro/specs" ]; then
		name=kiro used=yes available=yes paths=".kiro"
	elif [ -d "$top/openspec" ]; then
		name=openspec used=yes paths="openspec"
		command -v openspec >/dev/null 2>&1 && available=yes
	fi
	if [ "$forced" != auto ]; then
		[ "$forced" = "$name" ] || { name=$forced used=yes available=unknown paths=""; }
	fi
	printf 'ecosystem=%s\necosystem_used=%s\necosystem_available=%s\nplanning_paths=%s\n' \
		"$name" "$used" "$available" "$paths"
}

guides() {
	local found=""
	for guide in $(bash "$here/profile.sh" get project.guides); do
		[ -f "$top/$guide" ] && found="$found${found:+ }$guide"
	done
	printf '%s\n' "$found"
}

# Token scopes of the gh login. A workflow token has none to report: "unknown".
scopes() {
	local line
	line=$(gh auth status 2>&1 | strip | grep -i 'token scopes' | head -n 1) || true
	if [ -z "$line" ]; then
		echo unknown
		return
	fi
	printf '%s\n' "$line" | sed -E "s/.*[Ss]copes:[[:space:]]*//; s/'//g; s/,[[:space:]]*/,/g"
}

all() {
	local repo owner
	repo=$(resolve_repo)
	owner=${repo%%/*}
	printf 'repo=%s\nowner=%s\n' "$repo" "$owner"
	if [ "$offline" = 1 ]; then
		printf 'owner_type=unknown\nvisibility=unknown\npermission=unknown\nis_fork=unknown\n'
		printf 'scopes=unknown\nmissing_scopes=unknown\nscopes_ok=unknown\n'
	else
		gh repo view "$repo" --json visibility,viewerPermission,isFork \
			--jq '"visibility=\(.visibility)\npermission=\(.viewerPermission)\nis_fork=\(.isFork)"' | strip
		printf 'owner_type=%s\n' "$(gh api "users/$owner" --jq .type 2>/dev/null | strip || echo unknown)"
		local have missing=""
		have=$(scopes)
		printf 'scopes=%s\n' "$have"
		if [ "$have" != unknown ]; then
			for needed in repo project workflow; do
				case ",$have," in *",$needed,"*) ;; *) missing="$missing${missing:+,}$needed" ;; esac
			done
			printf 'missing_scopes=%s\nscopes_ok=%s\n' "$missing" "$([ -z "$missing" ] && echo yes || echo no)"
		else
			printf 'missing_scopes=unknown\nscopes_ok=unknown\n'
		fi
	fi
	printf 'default_branch=%s\n' "$(resolve_default_branch)"
	printf 'guides=%s\n' "$(guides)"
	detect_ecosystem
	local profile="$top/.github/steward.yml"
	[ -n "${STEWARD_PROFILE:-}" ] && profile=$STEWARD_PROFILE
	if [ -f "$profile" ]; then printf 'profile=%s\n' "$profile"; else printf 'profile=none\n'; fi
	printf 'offline=%s\n' "$offline"
}

case "${1:-}" in
"") all ;;
get)
	[ $# -eq 2 ] || { echo "usage: context.sh get <key>" >&2; exit 2; }
	case "$2" in
	repo) resolve_repo ;;
	default_branch) resolve_default_branch ;;
	ecosystem | ecosystem_used | ecosystem_available | planning_paths)
		detect_ecosystem | awk -v want="$2" 'index($0, want "=") == 1 { print substr($0, length(want) + 2) }'
		;;
	*)
		value=$(all | awk -v want="$2" 'index($0, want "=") == 1 { print substr($0, length(want) + 2); hit = 1 } END { exit !hit }') || {
			echo "context.sh: unknown key '$2'" >&2
			exit 2
		}
		printf '%s\n' "$value"
		;;
	esac
	;;
*)
	echo "usage: context.sh [get <key>]" >&2
	exit 2
	;;
esac
