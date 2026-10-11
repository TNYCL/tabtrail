#!/usr/bin/env bash
# Verifies that a release tag matches the crate version.
#   scripts/check-version.sh v0.1.0
set -euo pipefail
tag=${1:?usage: check-version.sh <tag>}
root=$(cd "$(dirname "$0")/.." && pwd)
version=$(sed -n 's/^version = "\(.*\)"$/\1/p;T;q' "$root/Cargo.toml")
[[ -n $version ]] || { echo "error: no version in Cargo.toml" >&2; exit 1; }
if [[ $tag != "v$version" ]]; then
    echo "error: tag '$tag' does not match Cargo.toml version '$version' (expected 'v$version')" >&2
    exit 1
fi
# Cargo.lock must record the same version for the package itself.
lock_version=$(awk '/^name = "tabtrail"$/{getline; sub(/^version = "/,""); sub(/"$/,""); print; exit}' "$root/Cargo.lock")
if [[ $lock_version != "$version" ]]; then
    echo "error: Cargo.lock has tabtrail $lock_version, Cargo.toml has $version" >&2
    exit 1
fi
if ! grep -q "^## \[$version\]" "$root/CHANGELOG.md"; then
    echo "error: CHANGELOG.md has no '## [$version]' section" >&2
    exit 1
fi
echo "version $version matches tag $tag"
