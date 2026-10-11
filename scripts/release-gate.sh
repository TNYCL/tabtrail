#!/usr/bin/env bash
# Fail-closed gate for *publication* (GitHub release upload, crates.io).
# Building, testing and packaging never depend on this gate.
#   scripts/release-gate.sh github|crates [repository-root]
# Exits 1, listing every blocker, unless:
#   - Cargo.toml [package] has a non-empty `license` (SPDX expression) or an
#     existing `license-file`;
#   - the repository has at least one LICENSE*/LICENCE*/COPYING* file and
#     none of them is blank; when the expression names several licenses,
#     each has its own full text (MIT: LICENSE-MIT, Apache-2.0:
#     LICENSE-APACHE, otherwise LICENSE-<id>), and the MIT/Apache-2.0 files
#     hold those licenses (MIT with a filled-in copyright line);
#   - packaging/notices/STATIC-RUNTIME-NOTICES.txt (notices for the
#     statically linked runtime of the release binaries) is not empty;
#   - packaging/MAINTAINER is exactly one `Name <address@domain>` line and
#     not a placeholder;
#   - for crates.io: `publish = false` has been removed.
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
blank() { ! grep -q '[^[:space:]]' "$1"; }

blockers=()
if [[ -z $license && -z $license_file ]]; then
    blockers+=("no license expression in Cargo.toml")
fi
if [[ -n $license_file && ! -f "$root/$license_file" ]]; then
    blockers+=("license-file '$license_file' does not exist")
fi
license_files=("$root"/LICENSE* "$root"/LICENCE* "$root"/COPYING*)
if ((${#license_files[@]} == 0)); then
    blockers+=("no LICENSE*/LICENCE*/COPYING* file in the repository")
fi
for f in "${license_files[@]}"; do
    if [[ ! -f $f ]] || blank "$f"; then blockers+=("license file $(basename "$f") is empty or not a regular file"); fi
done

# License identifiers of the SPDX expression (exceptions after WITH are not
# licenses of their own).
ids=()
read -ra words <<<"${license//[()]/ }"
for ((i = 0; i < ${#words[@]}; i++)); do
    case ${words[i]} in
        OR | AND) ;;
        WITH) i=$((i + 1)) ;;
        *) ids+=("${words[i]}") ;;
    esac
done
text_file() { # conventional file name of a license's full text
    case $1 in
        MIT) echo LICENSE-MIT ;;
        Apache-2.0) echo LICENSE-APACHE ;;
        *) echo "LICENSE-$1" ;;
    esac
}
if ((${#ids[@]} > 1)); then
    for id in "${ids[@]}"; do
        f=$(text_file "$id")
        if [[ ! -f "$root/$f" ]] || blank "$root/$f"; then
            blockers+=("$id is part of '$license' but $f with its full text is missing or empty")
        fi
    done
fi
mit="$root/LICENSE-MIT"
if [[ -f $mit ]] && ! blank "$mit"; then
    if ! grep -q 'Permission is hereby granted, free of charge' "$mit"; then
        blockers+=("LICENSE-MIT does not contain the MIT license text")
    fi
    if ! grep -Eq '^Copyright (\([cC]\) )?[0-9]{4}([-, ]+[0-9]{4})* [^<[[:space:]]' "$mit" \
        || grep -Eq '<YEAR>|<COPYRIGHT HOLDER>|\[year\]|\[fullname\]' "$mit"; then
        blockers+=("LICENSE-MIT has no filled-in copyright line (year and holder)")
    fi
fi
apache="$root/LICENSE-APACHE"
if [[ -f $apache ]] && ! blank "$apache"; then
    if ! grep -q 'Apache License' "$apache" || ! grep -q 'Version 2.0, January 2004' "$apache" \
        || ! grep -q 'END OF TERMS AND CONDITIONS' "$apache"; then
        blockers+=("LICENSE-APACHE does not contain the full Apache License 2.0 text")
    fi
fi

notices="$root/packaging/notices/STATIC-RUNTIME-NOTICES.txt"
if [[ ! -f $notices ]] || blank "$notices"; then
    blockers+=("static runtime notices (musl libc, LLVM libunwind, crt objects) not provided in packaging/notices/")
fi

maintainer_file="$root/packaging/MAINTAINER"
if [[ ! -f $maintainer_file ]] || blank "$maintainer_file"; then
    blockers+=("package maintainer/contact metadata not set (packaging/MAINTAINER)")
else
    maintainer=$(grep '[^[:space:]]' "$maintainer_file")
    domain=${maintainer##*@}; domain=${domain%>}
    # Patterns in variables: a backslash-escaped `<` in `=~` would become a
    # GNU word-boundary operator.
    name_address='^[^[:space:]<>][^<>]*[^[:space:]<>] <[^[:space:]<>@]+@[^[:space:]<>@]+\.[^[:space:]<>@.]+>$'
    reserved_domain='(^|\.)(invalid|example|localhost|example\.com|example\.net|example\.org)$'
    if [[ $maintainer == *$'\n'* ]]; then
        blockers+=("packaging/MAINTAINER must be a single 'Name <address>' line")
    elif ! [[ $maintainer =~ $name_address ]]; then
        blockers+=("packaging/MAINTAINER is not a 'Name <address@domain>' line: $maintainer")
    elif [[ ${domain,,} =~ $reserved_domain ]] \
        || grep -Eqi '\b(unassigned|placeholder|todo|fixme|changeme|tbd)\b|your name' <<<"$maintainer"; then
        blockers+=("packaging/MAINTAINER is a placeholder: $maintainer")
    fi
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
