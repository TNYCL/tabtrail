#!/usr/bin/env bash
# Clean-environment smoke test of a release tarball (and .deb/.rpm if given).
#   scripts/smoke-test.sh <tabtrail-*.tar.gz> [<tabtrail_*.deb> [<tabtrail-*.rpm>]]
# Uses a temporary HOME; never touches the caller's dotfiles. Also checks
# that every artifact carries the repository's license texts byte for byte
# and the maintainer/license metadata.
set -euo pipefail
shopt -s nullglob
tarball=${1:?tarball}; deb=${2:-}; rpm=${3:-}
root=$(cd "$(dirname "$0")/.." && pwd)
licfiles=("$root"/LICENSE* "$root"/LICENCE* "$root"/COPYING*)
((${#licfiles[@]})) || { echo "no license files in $root" >&2; exit 1; }
maintainer=${PACKAGE_MAINTAINER:-$(sed -n '/[^[:space:]]/{p;q}' "$root/packaging/MAINTAINER")}
license=$(awk '/^\[/{p = ($0 == "[package]")} p' "$root/Cargo.toml" | sed -n 's/^license *= *"\(.*\)" *$/\1/p;T;q')
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
same_licenses() { # <directory holding the shipped license files> <artifact>
    for l in "${licfiles[@]}"; do
        cmp "$l" "$1/$(basename "$l")" || { echo "$2: $(basename "$l") missing or different" >&2; exit 1; }
    done
}
export HOME="$work/home" XDG_CONFIG_HOME='' XDG_CACHE_HOME='' XDG_DATA_HOME=
mkdir -p "$HOME"
tar -C "$work" -xzf "$tarball"
bin=$(find "$work" -maxdepth 2 -type f -name tabtrail -print -quit)
test -s "$(dirname "$bin")/THIRD-PARTY-NOTICES.txt"
same_licenses "$(dirname "$bin")" "$tarball"
"$bin" --version
printf '# pre-existing user line\n' > "$HOME/.bashrc"
cp "$HOME/.bashrc" "$work/bashrc.orig"
"$bin" install --shell bash
grep -q '^# >>> tabtrail initialize >>>$' "$HOME/.bashrc"
bash -n "$HOME/.local/share/tabtrail/shell/tabtrail.bash"
out=$("$bin" complete --shell bash --before 'nonexistent-cmd --x' | tr '\0' '|')
[[ $out == tabtrail1\|f\|0\| ]] || { echo "unexpected protocol output: $out" >&2; exit 1; }
"$bin" uninstall --shell bash
cmp "$HOME/.bashrc" "$work/bashrc.orig"
if [[ -n $deb ]]; then
    dpkg-deb --info "$deb" >/dev/null
    # Capture first: `cmd | grep -q` would SIGPIPE cmd and fail under pipefail.
    contents=$(dpkg-deb --contents "$deb")
    grep -q './usr/bin/tabtrail$' <<<"$contents"
    grep -q './usr/share/doc/tabtrail/THIRD-PARTY-NOTICES.txt$' <<<"$contents"
    dpkg-deb -x "$deb" "$work/deb"
    same_licenses "$work/deb/usr/share/doc/tabtrail" "$deb"
    grep -Fqx "License: $license" "$work/deb/usr/share/doc/tabtrail/copyright"
    [[ $(dpkg-deb --field "$deb" Maintainer) == "$maintainer" ]] || { echo "$deb: wrong Maintainer" >&2; exit 1; }
fi
if [[ -n $rpm ]]; then
    rpm=$(realpath "$rpm")
    tags=$(rpm -qp --queryformat '%{LICENSE}|%{PACKAGER}' "$rpm")
    [[ $tags == "$license|$maintainer" ]] || { echo "$rpm: License|Packager is '$tags'" >&2; exit 1; }
    marked=$(rpm -qLp "$rpm")
    mkdir -p "$work/rpm"
    (cd "$work/rpm" && rpm2cpio "$rpm" | cpio -idm --quiet)
    for l in "${licfiles[@]}"; do
        grep -qx "/usr/share/licenses/tabtrail/$(basename "$l")" <<<"$marked" \
            || { echo "$rpm: $(basename "$l") is not a %license file" >&2; exit 1; }
    done
    same_licenses "$work/rpm/usr/share/licenses/tabtrail" "$rpm"
fi
echo "smoke test passed"
