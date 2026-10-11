#!/usr/bin/env bash
# Fail-closed gate for *publication* (GitHub release upload, crates.io).
# Building, testing and packaging never depend on this gate.
#   scripts/release-gate.sh github|crates [repository-root]
# Exits non-zero, listing every blocker, until the owner has:
#   - chosen a license: non-empty `license` or an existing `license-file` in
#     Cargo.toml, plus at least one LICENSE*/LICENCE*/COPYING* file,
#   - provided notices for the statically linked runtime of the release
#     binaries (packaging/notices/STATIC-RUNTIME-NOTICES.txt, see RELEASING.md),
#   - set package maintainer/contact metadata (packaging/MAINTAINER),
#   - for crates.io: removed `publish = false`.
set -euo pipefail
shopt -s nullglob
target=${1:?usage: release-gate.sh github|crates [root]}
root=${2:-$(cd "$(dirname "$0")/.." && pwd)}
manifest="$root/Cargo.toml"
[[ $target == github || $target == crates ]] || { echo "error: unknown target '$target'" >&2; exit 2; }

# Only the [package] table is considered.
package_table=$(awk '/^\[/{in_pkg = ($0 == "[package]")} in_pkg' "$manifest")
field() { sed -n "s/^$1 *= *\"\\(.*\\)\" *\$/\\1/p;T;q" <<<"$package_table"; }
license=$(field license)
license_file=$(field license-file)

blockers=()
if [[ -z $license && -z $license_file ]]; then
    blockers+=("no license selected in Cargo.toml (owner decision pending)")
fi
if [[ -n $license_file && ! -f "$root/$license_file" ]]; then
    blockers+=("license-file '$license_file' does not exist")
fi
license_files=("$root"/LICENSE* "$root"/LICENCE* "$root"/COPYING*)
if ((${#license_files[@]} == 0)); then
    blockers+=("no LICENSE*/LICENCE*/COPYING* file in the repository")
fi
if [[ ! -s "$root/packaging/notices/STATIC-RUNTIME-NOTICES.txt" ]]; then
    blockers+=("static runtime notices (musl libc, LLVM libunwind, crt objects) not provided in packaging/notices/")
fi
if [[ ! -s "$root/packaging/MAINTAINER" ]]; then
    blockers+=("package maintainer/contact metadata not set (packaging/MAINTAINER)")
fi
if [[ $target == crates ]] && grep -Eq '^publish *= *false' <<<"$package_table"; then
    blockers+=("Cargo.toml has publish = false")
fi
if ((${#blockers[@]})); then
    echo "publication blocked for '$target':" >&2
    printf '  - %s\n' "${blockers[@]}" >&2
    exit 1
fi
echo "release gate for '$target': open"
