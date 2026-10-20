#!/usr/bin/env bash
# Checks that a release tag (vX.Y.Z+godot.M.N) belongs to a Godot release line and prints, as
# key=value lines for $GITHUB_OUTPUT, what the release workflow needs: the line, the suffix of
# the release file names, the release title and whether the release becomes GitHub's "latest"
# (only a release of the newest line that has releases does). Fails, naming the broken rule,
# when the tag must not be released.
#
# Needs the full history: the tagged commit must be on origin/godot-vM.N, and the other line
# tags decide "latest".
#
# Usage: .github/scripts/release-line.sh v0.5.0+godot.4.6
set -euo pipefail

tag="${1:?usage: release-line.sh vX.Y.Z+godot.M.N}"
version="${tag#v}"

fail() {
    echo "$tag cannot be released: $*" >&2
    exit 1
}

manifest=$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)
[ "$version" = "$manifest" ] || fail "it does not match the Cargo.toml version $manifest"

case "$version" in
    *+godot.*) ;;
    *) fail "it names no Godot line; release tags look like vX.Y.Z+godot.M.N" ;;
esac
core="${version%%+*}"
line="${version#*+godot.}"
[[ "$line" =~ ^[0-9]+\.[0-9]+$ ]] || fail "its build metadata must be godot.M.N, not ${version#*+}"
[[ "$core" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] ||
    fail "$core is a pre-release version; the development line is never released"

branch="godot-v$line"
git rev-parse --verify --quiet "refs/remotes/origin/$branch" > /dev/null ||
    fail "there is no branch origin/$branch"
git merge-base --is-ancestor "$tag^{commit}" "origin/$branch" ||
    fail "the tagged commit is not on origin/$branch"

highest=$(
    {
        echo "$line"
        git tag -l 'v*+godot.*' | sed -n 's/^v[^+]*+godot\.\([0-9]*\.[0-9]*\)$/\1/p'
    } | sort -t. -k1,1n -k2,2n | tail -n 1
)
latest=false
[ "$highest" = "$line" ] && latest=true

echo "line=$line"
echo "suffix=-godot$line"
echo "title=godot-mcp $core (Godot $line)"
echo "latest=$latest"
