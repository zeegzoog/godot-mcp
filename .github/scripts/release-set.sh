#!/usr/bin/env bash
# Checks that a release tag (vX.Y.Z) can publish a release and prints, as key=value lines for
# $GITHUB_OUTPUT, what the release workflow needs:
#   version=X.Y.Z
#   lines=["4.5","4.6","4.7"]     the Godot lines the release contains (JSON, for a build matrix)
#
# A release holds the files of every supported Godot line (.github/lines), each built from its
# line tag vX.Y.Z+godot.M.N. Fails, naming the broken rule, when the release must not go out.
#
# Needs the full history and the branches and tags of origin.
#
# Usage: .github/scripts/release-set.sh v0.5.1
set -euo pipefail

tag="${1:?usage: release-set.sh vX.Y.Z}"
version="${tag#v}"

fail() {
    echo "$tag cannot be released: $*" >&2
    exit 1
}

[[ "$tag" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]] ||
    fail "a release tag is exactly vX.Y.Z; the line tags (vX.Y.Z+godot.M.N) are not released on their own"

git merge-base --is-ancestor "$tag^{commit}" origin/master ||
    fail "the tagged commit is not on origin/master"

bash "$(dirname "$0")/release-notes.sh" "$tag" > /dev/null ||
    fail "CHANGELOG.md has no notes for it"

lines=()
while IFS= read -r line; do
    line="${line%%#*}"
    line="$(echo "$line" | tr -d '[:space:]')"
    [ -n "$line" ] && lines+=("$line")
done < <(git show "$tag:.github/lines")
[ "${#lines[@]}" -gt 0 ] || fail ".github/lines lists no Godot line"

for line in "${lines[@]}"; do
    [[ "$line" =~ ^[0-9]+\.[0-9]+$ ]] || fail ".github/lines has '$line', which is not a Godot M.N"
    branch="godot-v$line"
    line_tag="v$version+godot.$line"
    git rev-parse --verify --quiet "refs/remotes/origin/$branch" > /dev/null ||
        fail "there is no branch origin/$branch for the Godot $line line"
    git rev-parse --verify --quiet "refs/tags/$line_tag^{commit}" > /dev/null ||
        fail "the Godot $line line has no tag $line_tag"
    git merge-base --is-ancestor "$line_tag^{commit}" "origin/$branch" ||
        fail "$line_tag is not on origin/$branch"
    cargo_version=$(git show "$line_tag:Cargo.toml" | grep -m1 '^version' | cut -d'"' -f2)
    addon_version=$(git show "$line_tag:addons/godot_mcp/plugin.cfg" | grep -m1 '^version=' | cut -d'"' -f2)
    [ "$cargo_version" = "$version+godot.$line" ] ||
        fail "Cargo.toml at $line_tag says $cargo_version, expected $version+godot.$line"
    [ "$addon_version" = "$cargo_version" ] ||
        fail "plugin.cfg at $line_tag says $addon_version, Cargo.toml says $cargo_version"
done

json=$(printf '"%s",' "${lines[@]}")
echo "version=$version"
echo "lines=[${json%,}]"
