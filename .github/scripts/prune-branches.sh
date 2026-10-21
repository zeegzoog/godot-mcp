#!/usr/bin/env bash
# Deletes the branches of this repository whose work is finished: a pull request from the branch
# was merged and the branch still points at the last commit that pull request merged.
#
# Never deleted: the default branch, the release lines (godot-v*), protected branches, a branch
# with an open pull request, a branch whose pull request was closed without a merge, a branch
# that has no pull request, and a branch that got new commits after its merge. Branches from
# forks are not touched: they are not in this repository.
#
# Needs the GitHub CLI, GH_TOKEN (contents: write) and GITHUB_REPOSITORY. Set DRY_RUN=true to
# only print what would be deleted.
#
# Usage: .github/scripts/prune-branches.sh [branch]
#   without an argument: checks every branch; with one: checks only that branch
set -euo pipefail

repo="${GITHUB_REPOSITORY:?GITHUB_REPOSITORY is not set}"
owner="${repo%%/*}"
dry_run="${DRY_RUN:-false}"
only="${1:-}"

default=$(gh api "repos/$repo" --jq .default_branch)

# Prints the reason a branch must stay, or nothing when it is finished.
keep_reason() {
    local name="$1" sha="$2" protected="$3"

    [ "$name" != "$default" ] || { echo "default branch"; return; }
    [[ "$name" != godot-v* ]] || { echo "release line"; return; }
    [ "$protected" != "true" ] || { echo "protected"; return; }

    local open
    open=$(gh api "repos/$repo/pulls?state=open&head=$owner:$name&per_page=1" --jq length)
    [ "$open" = "0" ] || { echo "has an open pull request"; return; }

    local merged
    merged=$(gh api "repos/$repo/pulls?state=closed&head=$owner:$name&per_page=100" \
        --jq "[.[] | select(.merged_at != null and .head.sha == \"$sha\")] | length")
    [ "$merged" != "0" ] || echo "no merged pull request ends at $sha"
}

if [ -n "$only" ]; then
    branches=$(gh api "repos/$repo/branches/$only" \
        --jq '[.name, .commit.sha, .protected] | @tsv')
else
    branches=$(gh api --paginate "repos/$repo/branches?per_page=100" \
        --jq '.[] | [.name, .commit.sha, .protected] | @tsv')
fi

deleted=0
while IFS=$'\t' read -r name sha protected; do
    [ -n "$name" ] || continue
    reason=$(keep_reason "$name" "$sha" "$protected")
    if [ -n "$reason" ]; then
        echo "keep    $name ($reason)"
        continue
    fi
    if [ "$dry_run" = "true" ]; then
        echo "would delete $name"
    else
        gh api -X DELETE "repos/$repo/git/refs/heads/$name" > /dev/null
        echo "deleted $name"
    fi
    deleted=$((deleted + 1))
done <<< "$branches"

echo "finished branches: $deleted"
