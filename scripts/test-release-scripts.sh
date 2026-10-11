#!/usr/bin/env bash
# Tests for the release scripts using temporary fixture repositories only.
# Fixtures use either synthetic license identifiers (LicenseRef-TestFixture-*)
# or the project's own license files (MIT OR Apache-2.0).
#   scripts/test-release-scripts.sh
set -euo pipefail
shopt -s nullglob
here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/.." && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
failures=0
pass() { echo "ok   $*"; }
fail() { echo "FAIL $*"; failures=$((failures + 1)); }
# A failing positive check shows the command's output.
expect_ok() {
    local name=$1 out line status=0; shift
    out=$("$@" 2>&1) || status=$?
    if ((status == 0)); then pass "$name"; else
        fail "$name (exit $status)"
        while IFS= read -r line; do echo "     | $line"; done <<<"$out"
    fi
}
expect_fail() { local name=$1; shift; if "$@" >/dev/null 2>&1; then fail "$name"; else pass "$name"; fi; }
# Exit status 1 and a blocker matching the (grep -E) pattern: a crash or a
# failure for an unrelated reason does not count as the expected denial.
expect_blocked() {
    local name=$1 pattern=$2 out status=0; shift 2
    out=$("$@" 2>&1) || status=$?
    if ((status == 1)) && grep -Eq -- "$pattern" <<<"$out"; then pass "$name"; else fail "$name (exit $status: $out)"; fi
}

fixture() {
    local d="$work/$1"
    rm -rf "$d"
    mkdir -p "$d/scripts" "$d/docs" "$d/packaging/notices"
    cp "$here"/*.sh "$here"/*.py "$d/scripts/"
    cat > "$d/Cargo.toml" <<TOML
[package]
name = "tabtrail"
version = "0.1.0"
$2

[dependencies]
TOML
    printf '[[package]]\nname = "tabtrail"\nversion = "0.1.0"\n' > "$d/Cargo.lock"
    printf '# Changelog\n\n## [0.1.0] - unreleased\n\n- test\n' > "$d/CHANGELOG.md"
    for f in README.md docs/SECURITY.md docs/ARCHITECTURE.md; do echo "fixture" > "$d/$f"; done
    echo "$d"
}

maintainer="Fixture Maintainer <fixture@fixture.test>"
ready() { # everything except the license
    echo "$maintainer" > "$1/packaging/MAINTAINER"
    echo "fixture static runtime notices" > "$1/packaging/notices/STATIC-RUNTIME-NOTICES.txt"
}

dual() { # the project's dual license, ready for publication on GitHub
    local d
    d=$(fixture "$1" 'license = "MIT OR Apache-2.0"
publish = false')
    ready "$d"
    cp "$repo/LICENSE-MIT" "$repo/LICENSE-APACHE" "$d/"
    echo "$d"
}

gate() { "$1/scripts/release-gate.sh" "$2" "$1"; }
policy() { "$1/scripts/check-release-policy.sh" "$1"; }

# --- release gate ---------------------------------------------------------
d=$(fixture none "publish = false")
ready "$d"
expect_fail "gate: no license field" gate "$d" github

d=$(fixture spdx 'license = "LicenseRef-TestFixture-A"')
ready "$d"
expect_fail "gate: license field but no license file" gate "$d" github
echo "fixture license text A" > "$d/LICENSE"
expect_ok "gate: license field + LICENSE only (no COPYING)" gate "$d" github
expect_ok "gate: crates open without publish = false" gate "$d" crates
rm "$d/packaging/MAINTAINER"
expect_fail "gate: missing maintainer metadata" gate "$d" github
ready "$d"
rm "$d/packaging/notices/STATIC-RUNTIME-NOTICES.txt"
expect_fail "gate: missing static runtime notices" gate "$d" github

d=$(fixture copying 'license = "LicenseRef-TestFixture-A"
publish = false')
ready "$d"
echo "fixture copying text" > "$d/COPYING"
expect_ok "gate: COPYING only (no LICENSE)" gate "$d" github
expect_fail "gate: crates blocked by publish = false" gate "$d" crates

d=$(fixture empty 'license = ""')
ready "$d"; echo x > "$d/LICENSE"
expect_fail "gate: empty license string" gate "$d" github

d=$(fixture lf 'license-file = "LICENSE.txt"')
ready "$d"; echo x > "$d/LICENSE-OTHER"
expect_fail "gate: license-file pointing to a missing file" gate "$d" github
echo x > "$d/LICENSE.txt"
expect_ok "gate: existing license-file" gate "$d" github

d=$(fixture deps-only '')
ready "$d"; echo x > "$d/LICENSE"
printf '\n[dependencies.foo]\nlicense = "MIT"\n' >> "$d/Cargo.toml"
expect_fail "gate: a license key outside [package] does not count" gate "$d" github
expect_fail "gate: unknown target" gate "$d" pypi

# --- the project's dual license: MIT OR Apache-2.0 ------------------------
d=$(dual dual)
expect_ok "gate: valid MIT OR Apache-2.0 opens github" gate "$d" github
expect_blocked "gate: crates denied by publish = false" 'publish = false' gate "$d" crates
expect_ok "policy: github open, crates deferred" policy "$d"
for f in LICENSE-MIT LICENSE-APACHE; do
    mv "$d/$f" "$work/held"
    expect_blocked "gate: dual license without $f" "$f" gate "$d" github
    : > "$d/$f"
    expect_blocked "gate: dual license with empty $f" "$f" gate "$d" github
    printf '\n  \n' > "$d/$f"
    expect_blocked "gate: dual license with blank $f" "$f" gate "$d" github
    expect_fail "policy: dual license with blank $f" policy "$d"
    mv "$work/held" "$d/$f"
done
expect_ok "gate: dual license restored" gate "$d" github
sed '1s/.*/Copyright <YEAR> <COPYRIGHT HOLDER>/' "$repo/LICENSE-MIT" > "$d/LICENSE-MIT"
expect_blocked "gate: MIT text with template copyright line" 'LICENSE-MIT' gate "$d" github
cp "$repo/LICENSE-APACHE" "$d/LICENSE-MIT"
expect_blocked "gate: LICENSE-MIT holding another license" 'LICENSE-MIT' gate "$d" github
cp "$repo/LICENSE-MIT" "$d/LICENSE-MIT"
cp "$repo/LICENSE-MIT" "$d/LICENSE-APACHE"
expect_blocked "gate: LICENSE-APACHE holding another license" 'LICENSE-APACHE' gate "$d" github
cp "$repo/LICENSE-APACHE" "$d/"
: > "$d/COPYING"
expect_blocked "gate: any empty license file" 'COPYING' gate "$d" github
rm "$d/COPYING"
sed -i 's/^license = .*/license = "MIT AND Apache-2.0"/' "$d/Cargo.toml"
expect_ok "gate: license files checked for AND expressions too" gate "$d" github
rm "$d/LICENSE-APACHE"
expect_blocked "gate: AND expression without LICENSE-APACHE" 'LICENSE-APACHE' gate "$d" github

# --- maintainer metadata ----------------------------------------------------
d=$(dual maint)
rm "$d/packaging/MAINTAINER"
expect_blocked "gate: absent maintainer" 'packaging/MAINTAINER' gate "$d" github
expect_fail "policy: absent maintainer" policy "$d"
for bad in "" "   " "Name <contact>" "Tabtrail maintainers (unassigned) <maintainers@tabtrail.invalid>" \
    "Someone <someone@example.com>" "TODO <todo@fixture.test>" "no address at all" "<only@fixture.test>" \
    "Two Lines <a@fixture.test>"$'\n'"Second <b@fixture.test>"; do
    printf '%s\n' "$bad" > "$d/packaging/MAINTAINER"
    expect_blocked "gate: maintainer '${bad//$'\n'/\\n}'" 'packaging/MAINTAINER' gate "$d" github
done
echo "$maintainer" > "$d/packaging/MAINTAINER"
expect_ok "gate: maintainer restored" gate "$d" github

# --- static runtime notices -------------------------------------------------
d=$(dual notices)
rm "$d/packaging/notices/STATIC-RUNTIME-NOTICES.txt"
expect_blocked "gate: dual license without static runtime notices" 'runtime notices' gate "$d" github
: > "$d/packaging/notices/STATIC-RUNTIME-NOTICES.txt"
expect_blocked "gate: empty static runtime notices" 'runtime notices' gate "$d" github
expect_fail "policy: missing static runtime notices" policy "$d"

# --- the CI publication policy: github open, crates.io deferred ------------
d=$(dual policy-crates-open)
sed -i '/^publish = false$/d' "$d/Cargo.toml"
expect_ok "gate: crates opens once publish = false is removed" gate "$d" crates
expect_blocked "policy: rejects an open crates.io gate while publication is deferred" 'crates' policy "$d"
d=$(dual policy-extra-blocker)
sed -i '/^license = /d' "$d/Cargo.toml"
expect_fail "policy: rejects a closed github gate" policy "$d"

# --- version check --------------------------------------------------------
d=$(fixture version '')
expect_ok "version: matching tag" "$d/scripts/check-version.sh" v0.1.0
expect_fail "version: mismatching tag" "$d/scripts/check-version.sh" v0.1.1
expect_fail "version: tag without v" "$d/scripts/check-version.sh" 0.1.0
sed -i 's/## \[0.1.0\]/## [0.0.9]/' "$d/CHANGELOG.md"
expect_fail "version: missing changelog section" "$d/scripts/check-version.sh" v0.1.0

# --- packaging keeps every license file -----------------------------------
d=$(dual pkg)
bin="$work/tabtrail"
printf '#!/bin/sh\necho "tabtrail 0.1.0"\n' > "$bin"; chmod +x "$bin"
echo "fixture third-party notices" > "$work/tpn.txt"
mkdir -p "$work/rustdoc/licenses"; echo "fixture rust copyright" > "$work/rustdoc/COPYRIGHT.html"
echo "fixture MIT" > "$work/rustdoc/licenses/MIT.txt"
package() { # <fixture> <out>
    TABTRAIL_ROOT="$1" THIRD_PARTY_NOTICES="$work/tpn.txt" RUST_NOTICES_DIR="$work/rustdoc" \
        "$1/scripts/package.sh" x86_64-unknown-linux-musl "$bin" "$2"
}
same() { # <name> <expected file> <actual file>
    if cmp -s "$2" "$3"; then pass "$1"; else fail "$1"; fi
}
# Prints the single artifact matching a glob; a missing (or ambiguous)
# artifact is a test failure, never a crash of this script.
artifact() { # <name> <glob matches...>
    local name=$1; shift
    if (($# == 1)) && [[ -f $1 ]]; then echo "$1"; else echo "$name: expected one artifact, found $#" >&2; return 1; fi
}
with_umask() { local mask=$1; shift; (umask "$mask"; "$@"); }
# `rpm -qlvp`/`tar tv`/`dpkg-deb --contents` entries whose mode is not one of
# the public payload modes.
nonpublic_modes() { grep -Ev '^(drwxr-xr-x|-rw-r--r--|-rwxr-xr-x) ' || true; }
out="$work/out"
expect_ok "package: builds" package "$d" "$out"
x="$work/tar-x"; mkdir -p "$x"
if tarball=$(artifact "package: tarball" "$out"/tabtrail-0.1.0-x86_64-unknown-linux-musl.tar.gz); then
    tar -C "$x" -xzf "$tarball"
else
    fail "package: tarball built"
fi
t="$x/tabtrail-0.1.0-x86_64-unknown-linux-musl"
for f in THIRD-PARTY-NOTICES.txt notices/rust-toolchain/COPYRIGHT.html \
    notices/rust-toolchain/licenses/MIT.txt notices/STATIC-RUNTIME-NOTICES.txt; do
    if [[ -s $t/$f ]]; then pass "tarball has $f"; else fail "tarball has $f"; fi
done
for f in LICENSE-MIT LICENSE-APACHE; do same "tarball has the full $f" "$repo/$f" "$t/$f"; done
if ! command -v dpkg-deb >/dev/null; then
    echo "skip deb checks (dpkg-deb not installed)"
elif deb=$(artifact "package: .deb" "$out"/*.deb); then
    contents=$(dpkg-deb --contents "$deb")
    for f in copyright THIRD-PARTY-NOTICES.txt notices/STATIC-RUNTIME-NOTICES.txt; do
        if grep -q "usr/share/doc/tabtrail/$f\$" <<<"$contents"; then pass "deb has $f"; else fail "deb has $f"; fi
    done
    x="$work/deb-x"; dpkg-deb -x "$deb" "$x"
    for f in LICENSE-MIT LICENSE-APACHE; do same "deb has the full $f" "$repo/$f" "$x/usr/share/doc/tabtrail/$f"; done
    copyright="$x/usr/share/doc/tabtrail/copyright"
    if grep -Fq "$(sed -n '3p' "$repo/LICENSE-MIT")" "$copyright" \
        && grep -Fq 'END OF TERMS AND CONDITIONS' "$copyright" \
        && grep -Fqx 'License: MIT OR Apache-2.0' "$copyright" \
        && grep -Fqx "Upstream-Contact: $maintainer" "$copyright"; then
        pass "deb copyright states the license and keeps both full texts"
    else
        fail "deb copyright states the license and keeps both full texts"
    fi
    info=$(dpkg-deb --field "$deb" Maintainer)
    if [[ $info == "$maintainer" ]]; then pass "deb Maintainer from packaging/MAINTAINER"; else fail "deb Maintainer ($info)"; fi
else
    fail "package: .deb built"
fi
rpm_tools=true
for tool in rpm rpmbuild rpm2cpio cpio; do command -v "$tool" >/dev/null || rpm_tools=false; done
if ! command -v rpmbuild >/dev/null; then
    echo "skip rpm checks (rpmbuild not installed)"
elif ! $rpm_tools; then
    fail "rpm checks need rpm, rpm2cpio and cpio next to rpmbuild"
elif rpm=$(artifact "package: .rpm" "$out"/*.rpm); then
    files=$(rpm -qlp "$rpm" 2>/dev/null)
    licenses=$(rpm -qLp "$rpm" 2>/dev/null)
    for f in LICENSE-MIT LICENSE-APACHE; do
        if grep -qx "/usr/share/licenses/tabtrail/$f" <<<"$licenses"; then pass "rpm marks $f as %license"; else fail "rpm marks $f as %license"; fi
    done
    if grep -qx /usr/share/doc/tabtrail/THIRD-PARTY-NOTICES.txt <<<"$files"; then pass "rpm has THIRD-PARTY-NOTICES.txt"; else fail "rpm has THIRD-PARTY-NOTICES.txt"; fi
    x="$work/rpm-x"; mkdir -p "$x"
    (cd "$x" && rpm2cpio "$rpm" | cpio -idm --quiet)
    for f in LICENSE-MIT LICENSE-APACHE; do same "rpm has the full $f" "$repo/$f" "$x/usr/share/licenses/tabtrail/$f"; done
    tags=$(rpm -qp --queryformat '%{LICENSE}|%{PACKAGER}' "$rpm" 2>/dev/null)
    if [[ $tags == "MIT OR Apache-2.0|$maintainer" ]]; then pass "rpm License and Packager tags"; else fail "rpm License and Packager tags ($tags)"; fi
else
    fail "package: .rpm built"
fi
# Packages are never built without the project license or maintainer.
d=$(dual pkg-nolicense)
sed -i '/^license = /d' "$d/Cargo.toml"
expect_blocked "package: refuses without a license expression" 'license' package "$d" "$work/out-nolicense"
d=$(dual pkg-nofiles)
rm "$d"/LICENSE-*
expect_blocked "package: refuses without license files" 'license' package "$d" "$work/out-nofiles"
d=$(dual pkg-nomaint)
rm "$d/packaging/MAINTAINER"
expect_blocked "package: refuses without maintainer" 'maintainer' package "$d" "$work/out-nomaint"
d=$(dual pkg) # restore the fixture used below
if sums=$(cd "$out" && sha256sum --check SHA256SUMS 2>&1); then
    pass "checksums verify"
else
    fail "checksums verify: $sums"
fi

# --- reproducible packages for a fixed toolchain and SOURCE_DATE_EPOCH ----
epoch_package() { SOURCE_DATE_EPOCH=1700000000 package "$@"; }
for i in 1 2; do
    expect_ok "reproducible: build $i" epoch_package "$d" "$work/repro$i"
    sleep 1.1 # a leaked wall-clock time would differ between the builds
done
for f in "$work"/repro1/*; do
    name=$(basename "$f")
    if cmp -s "$f" "$work/repro2/$name"; then pass "reproducible: $name"; else fail "reproducible: $name"; fi
done

# --- restrictive umask: public payload modes, identical bytes -------------
# The caller's umask (e.g. 027 or 077 in a login shell) must neither break
# packaging (dpkg-deb rejects a 0750 DEBIAN directory) nor leak into the
# payload: the packages must equal the umask 022 build above byte for byte,
# also when the checkout itself was created under that umask.
for mask in 027 077; do
    d=$(umask "$mask"; dual "umask-$mask")
    o="$work/umask-out-$mask"
    expect_ok "umask $mask: package builds" with_umask "$mask" epoch_package "$d" "$o"
    for f in "$work"/repro1/*; do
        name=$(basename "$f")
        same "umask $mask: $name identical to the umask 022 build" "$f" "$o/$name"
        mode=$(stat -c %a "$o/$name" 2>/dev/null || echo missing)
        if [[ $mode == 644 ]]; then pass "umask $mask: $name is world-readable"; else fail "umask $mask: $name mode $mode"; fi
    done
    mode=$(stat -c %a "$o")
    if [[ $mode == 755 ]]; then pass "umask $mask: artifact directory is public"; else fail "umask $mask: artifact directory mode $mode"; fi
    if tarball=$(artifact "umask $mask: tarball" "$o"/*.tar.gz); then
        bad=$(tar tvzf "$tarball" | nonpublic_modes)
        if [[ -z $bad ]]; then pass "umask $mask: tarball payload modes"; else fail "umask $mask: tarball payload modes: $bad"; fi
    else
        fail "umask $mask: tarball built"
    fi
    if ! command -v dpkg-deb >/dev/null; then
        :
    elif deb=$(artifact "umask $mask: .deb" "$o"/*.deb); then
        bad=$(dpkg-deb --contents "$deb" | nonpublic_modes)
        if [[ -z $bad ]]; then pass "umask $mask: deb payload modes"; else fail "umask $mask: deb payload modes: $bad"; fi
    else
        fail "umask $mask: .deb built"
    fi
    if ! $rpm_tools; then
        :
    elif rpm=$(artifact "umask $mask: .rpm" "$o"/*.rpm); then
        bad=$(rpm -qlvp "$rpm" | nonpublic_modes)
        if [[ -z $bad ]]; then pass "umask $mask: rpm payload modes"; else fail "umask $mask: rpm payload modes: $bad"; fi
    else
        fail "umask $mask: .rpm built"
    fi
done

# --- static runtime notices must match the building toolchain ------------
d=$(dual prov)
printf 'rust-commit: %s\n' "$(printf 'a%.0s' {1..40})" > "$d/packaging/notices/STATIC-RUNTIME-NOTICES.txt"
fakebin="$work/fakebin"; mkdir -p "$fakebin"
cat > "$fakebin/rustc" <<FAKE
#!/bin/sh
case "\$1" in
  --print) echo "$work/rustdoc-sysroot" ;;
  -vV) echo "rustc 0.0.0"; echo "commit-hash: \$FAKE_COMMIT" ;;
esac
FAKE
chmod +x "$fakebin/rustc"
mkdir -p "$work/rustdoc-sysroot/share/doc/rust"; echo "fixture" > "$work/rustdoc-sysroot/share/doc/rust/COPYRIGHT.html"
pkg() { PATH="$fakebin:$PATH" FAKE_COMMIT=$1 TABTRAIL_ROOT="$d" THIRD_PARTY_NOTICES="$work/tpn.txt" \
    "$d/scripts/package.sh" "$2" "$bin" "$work/prov-out-$1-$2"; }
expect_blocked "package: musl notices for another rustc commit" 'static runtime notices are for rust' pkg "$(printf 'b%.0s' {1..40})" x86_64-unknown-linux-musl
expect_ok "package: musl notices for the building rustc commit" pkg "$(printf 'a%.0s' {1..40})" x86_64-unknown-linux-musl
listing=""
if tarball=$(artifact "provenance: tarball" "$work/prov-out-$(printf 'a%.0s' {1..40})-x86_64-unknown-linux-musl"/*.tar.gz); then
    listing=$(tar tzf "$tarball")
fi
if grep -q '/notices/rust-toolchain/PROVENANCE.txt$' <<<"$listing"; then pass "toolchain provenance recorded"; else fail "toolchain provenance recorded"; fi

# --- the crate package includes the project license files ---------------
if command -v cargo >/dev/null; then
    c="$work/crate"
    mkdir -p "$c/src"
    # The real manifest (include list, license and all), without
    # dependencies so the check runs offline.
    awk '/^\[dependencies\]/{skip=1; next} /^\[/{skip=0} !skip' "$repo/Cargo.toml" > "$c/Cargo.toml"
    echo 'pub fn f() {}' > "$c/src/lib.rs"
    echo 'fn main() {}' > "$c/src/main.rs"
    echo "fixture" > "$c/README.md"
    cp "$repo/LICENSE-MIT" "$repo/LICENSE-APACHE" "$c/"
    echo "fixture license text" > "$c/LICENSE-FIXTURE"
    echo "fixture copying text" > "$c/COPYING"
    if list=$(cd "$c" && cargo package --list --allow-dirty --offline 2>/dev/null); then
        for f in LICENSE-MIT LICENSE-APACHE LICENSE-FIXTURE COPYING; do
            if grep -qx "$f" <<<"$list"; then pass "crate includes $f"; else fail "crate includes $f"; fi
        done
    else
        fail "cargo package --list on the manifest fixture"
    fi
else
    echo "skip crate include checks (cargo not installed)"
fi

echo
if ((failures)); then echo "$failures release-script test(s) failed"; exit 1; fi
echo "release-script tests passed"
