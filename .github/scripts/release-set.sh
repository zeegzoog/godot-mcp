#!/usr/bin/env bash
# Checks that a release tag (vX.Y.Z) can publish a release and prints, as key=value lines for
# $GITHUB_OUTPUT, what the release workflow needs:
#   version=X.Y.Z
#
# A release is built from the tagged commit of master: its Cargo.toml and addon say X.Y.Z, and
# CHANGELOG.md holds the notes of that version. Fails, naming the broken rule, when the release
# must not go out.
#
# Needs the full history and the branches of origin.
#
# Usage: .github/scripts/release-set.sh v0.16.0
set -euo pipefail

tag="${1:?usage: release-set.sh vX.Y.Z}"
version="${tag#v}"

fail() {
    echo "$tag cannot be released: $*" >&2
    exit 1
}

[[ "$tag" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]] ||
    fail "a release tag is exactly vX.Y.Z"

git merge-base --is-ancestor "$tag^{commit}" origin/master ||
    fail "the tagged commit is not on origin/master"

bash "$(dirname "$0")/release-notes.sh" "$tag" > /dev/null ||
    fail "CHANGELOG.md has no notes for it"

cargo_version=$(git show "$tag:Cargo.toml" | grep -m1 '^version' | cut -d'"' -f2)
addon_version=$(git show "$tag:addons/godot_mcp/plugin.cfg" | grep -m1 '^version=' | cut -d'"' -f2)
[ "$cargo_version" = "$version" ] ||
    fail "Cargo.toml at $tag says $cargo_version, expected $version"
[ "$addon_version" = "$version" ] ||
    fail "plugin.cfg at $tag says $addon_version, expected $version"

echo "version=$version"
