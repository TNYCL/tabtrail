#!/usr/bin/env bash
# Clean-environment smoke test of a release tarball (and .deb if given).
#   scripts/smoke-test.sh <tabtrail-*.tar.gz> [<tabtrail_*.deb>]
# Uses a temporary HOME; never touches the caller's dotfiles.
set -euo pipefail
tarball=${1:?tarball}; deb=${2:-}
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
export HOME="$work/home" XDG_CONFIG_HOME='' XDG_CACHE_HOME='' XDG_DATA_HOME=
mkdir -p "$HOME"
tar -C "$work" -xzf "$tarball"
bin=$(find "$work" -maxdepth 2 -type f -name tabtrail -print -quit)
test -s "$(dirname "$bin")/THIRD-PARTY-NOTICES.txt"
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
fi
echo "smoke test passed"
