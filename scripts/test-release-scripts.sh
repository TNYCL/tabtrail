#!/usr/bin/env bash
# Tests for the release scripts using synthetic, temporary fixture
# repositories only. The license identifiers used here are test fixtures
# (LicenseRef-TestFixture-*), not a license choice for Tabtrail.
#   scripts/test-release-scripts.sh
set -euo pipefail
shopt -s nullglob
here=$(cd "$(dirname "$0")" && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
failures=0
pass() { echo "ok   $*"; }
fail() { echo "FAIL $*"; failures=$((failures + 1)); }
expect_ok() { local name=$1; shift; if "$@" >/dev/null 2>&1; then pass "$name"; else fail "$name"; fi; }
expect_fail() { local name=$1; shift; if "$@" >/dev/null 2>&1; then fail "$name"; else pass "$name"; fi; }

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

ready() { # everything except the license
    echo "Fixture Maintainer <fixture@example.invalid>" > "$1/packaging/MAINTAINER"
    echo "fixture static runtime notices" > "$1/packaging/notices/STATIC-RUNTIME-NOTICES.txt"
}

gate() { "$1/scripts/release-gate.sh" "$2" "$1"; }

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

# --- version check --------------------------------------------------------
d=$(fixture version '')
expect_ok "version: matching tag" "$d/scripts/check-version.sh" v0.1.0
expect_fail "version: mismatching tag" "$d/scripts/check-version.sh" v0.1.1
expect_fail "version: tag without v" "$d/scripts/check-version.sh" 0.1.0
sed -i 's/## \[0.1.0\]/## [0.0.9]/' "$d/CHANGELOG.md"
expect_fail "version: missing changelog section" "$d/scripts/check-version.sh" v0.1.0

# --- packaging keeps every license file -----------------------------------
d=$(fixture pkg 'license = "LicenseRef-TestFixture-A OR LicenseRef-TestFixture-B"')
ready "$d"
echo "fixture license text A" > "$d/LICENSE-A"
echo "fixture license text B" > "$d/LICENSE-B"
bin="$work/tabtrail"
printf '#!/bin/sh\necho "tabtrail 0.1.0"\n' > "$bin"; chmod +x "$bin"
echo "fixture third-party notices" > "$work/tpn.txt"
mkdir -p "$work/rustdoc/licenses"; echo "fixture rust copyright" > "$work/rustdoc/COPYRIGHT.html"
echo "fixture MIT" > "$work/rustdoc/licenses/MIT.txt"
out="$work/out"
if TABTRAIL_ROOT="$d" THIRD_PARTY_NOTICES="$work/tpn.txt" RUST_NOTICES_DIR="$work/rustdoc" \
    "$d/scripts/package.sh" x86_64-unknown-linux-musl "$bin" "$out" >/dev/null 2>&1; then
    pass "package: builds"
else
    fail "package: builds"
fi
listing=$(tar tzf "$out"/tabtrail-0.1.0-x86_64-unknown-linux-musl.tar.gz)
for f in LICENSE-A LICENSE-B THIRD-PARTY-NOTICES.txt notices/rust-toolchain/COPYRIGHT.html \
    notices/rust-toolchain/licenses/MIT.txt notices/STATIC-RUNTIME-NOTICES.txt; do
    if grep -q "/$f\$" <<<"$listing"; then pass "tarball has $f"; else fail "tarball has $f"; fi
done
if command -v dpkg-deb >/dev/null; then
    deb=("$out"/*.deb)
    contents=$(dpkg-deb --contents "${deb[0]}")
    for f in LICENSE-A LICENSE-B copyright THIRD-PARTY-NOTICES.txt notices/STATIC-RUNTIME-NOTICES.txt; do
        if grep -q "usr/share/doc/tabtrail/$f\$" <<<"$contents"; then pass "deb has $f"; else fail "deb has $f"; fi
    done
    x="$work/deb-x"; dpkg-deb -x "${deb[0]}" "$x"
    copyright=$(cat "$x/usr/share/doc/tabtrail/copyright")
    if grep -q "text A" <<<"$copyright" && grep -q "text B" <<<"$copyright"; then
        pass "deb copyright keeps both license texts"
    else
        fail "deb copyright keeps both license texts"
    fi
    info=$(dpkg-deb --field "${deb[0]}" Maintainer)
    if [[ $info == "Fixture Maintainer <fixture@example.invalid>" ]]; then
        pass "deb maintainer from packaging/MAINTAINER"
    else
        fail "deb maintainer ($info)"
    fi
else
    echo "skip deb checks (dpkg-deb not installed)"
fi
if command -v rpm >/dev/null && command -v rpmbuild >/dev/null; then
    rpms=("$out"/*.rpm)
    files=$(rpm -qlp "${rpms[0]}" 2>/dev/null)
    for f in /usr/share/licenses/tabtrail/LICENSE-A /usr/share/licenses/tabtrail/LICENSE-B \
        /usr/share/doc/tabtrail/THIRD-PARTY-NOTICES.txt; do
        if grep -qx "$f" <<<"$files"; then pass "rpm has $f"; else fail "rpm has $f"; fi
    done
    lic=$(rpm -qp --queryformat '%{LICENSE}' "${rpms[0]}" 2>/dev/null)
    if [[ $lic == "LicenseRef-TestFixture-A OR LicenseRef-TestFixture-B" ]]; then
        pass "rpm License tag"
    else
        fail "rpm License tag ($lic)"
    fi
else
    echo "skip rpm checks (rpmbuild not installed)"
fi
if sums=$(cd "$out" && sha256sum --check SHA256SUMS 2>&1); then
    pass "checksums verify"
else
    fail "checksums verify: $sums"
fi

# --- reproducible packages for a fixed toolchain and SOURCE_DATE_EPOCH ----
twice() {
    for i in 1 2; do
        TABTRAIL_ROOT="$d" THIRD_PARTY_NOTICES="$work/tpn.txt" RUST_NOTICES_DIR="$work/rustdoc" \
            SOURCE_DATE_EPOCH=1700000000 "$d/scripts/package.sh" x86_64-unknown-linux-musl "$bin" "$work/repro$i" >/dev/null 2>&1
        sleep 1.1 # a leaked wall-clock time would differ between the builds
    done
}
twice
for f in "$work"/repro1/*; do
    name=$(basename "$f")
    if cmp -s "$f" "$work/repro2/$name"; then pass "reproducible: $name"; else fail "reproducible: $name"; fi
done

# --- static runtime notices must match the building toolchain ------------
d=$(fixture prov 'license = "LicenseRef-TestFixture-A"')
ready "$d"
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
expect_fail "package: musl notices for another rustc commit" pkg "$(printf 'b%.0s' {1..40})" x86_64-unknown-linux-musl
expect_ok "package: musl notices for the building rustc commit" pkg "$(printf 'a%.0s' {1..40})" x86_64-unknown-linux-musl
listing=$(tar tzf "$work/prov-out-$(printf 'a%.0s' {1..40})-x86_64-unknown-linux-musl"/*.tar.gz)
if grep -q '/notices/rust-toolchain/PROVENANCE.txt$' <<<"$listing"; then pass "toolchain provenance recorded"; else fail "toolchain provenance recorded"; fi

# --- the crate package includes owner license files -----------------------
if command -v cargo >/dev/null; then
    c="$work/crate"
    mkdir -p "$c/src"
    # The real manifest (include list and all), with a fixture license and
    # without dependencies so the check runs offline.
    awk '/^\[dependencies\]/{skip=1; next} /^\[/{skip=0} !skip' "$here/../Cargo.toml" \
        | sed 's/^publish = false$/publish = false\nlicense = "LicenseRef-TestFixture-A"/' > "$c/Cargo.toml"
    echo 'pub fn f() {}' > "$c/src/lib.rs"
    echo 'fn main() {}' > "$c/src/main.rs"
    echo "fixture" > "$c/README.md"
    echo "fixture license text" > "$c/LICENSE-FIXTURE"
    echo "fixture copying text" > "$c/COPYING"
    if list=$(cd "$c" && cargo package --list --allow-dirty --offline 2>/dev/null); then
        for f in LICENSE-FIXTURE COPYING; do
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
