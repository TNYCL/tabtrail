#!/usr/bin/env bash
# Builds distributable packages from an already built binary.
#   scripts/package.sh <rust-target-triple> <path/to/tabtrail> <out-dir>
# Produces in <out-dir>:
#   tabtrail-<version>-<target>.tar.gz
#   tabtrail_<version>_<deb-arch>.deb          (if dpkg-deb is available)
#   tabtrail-<version>-1.<rpm-arch>.rpm        (if rpmbuild is available)
# and (re)writes SHA256SUMS for everything in <out-dir>.
# Packages install only the binary and documentation: shell integration is
# enabled per user with `tabtrail install`, never by package scripts.
# Every package carries THIRD-PARTY-NOTICES.txt (licenses of the linked
# dependencies). Tabtrail's own license is an open owner decision: until it
# is chosen, packages carry no project license file and the RPM License tag
# is the draft marker below; publication is blocked by release-gate.sh.
set -euo pipefail
shopt -s nullglob
target=${1:?target}; bin=${2:?binary}; out=${3:?out dir}
root=${TABTRAIL_ROOT:-$(cd "$(dirname "$0")/.." && pwd)}
version=$(sed -n 's/^version = "\(.*\)"$/\1/p;T;q' "$root/Cargo.toml")
license=$(sed -n 's/^license *= *"\(.*\)" *$/\1/p;T;q' "$root/Cargo.toml")
# Draft marker until the owner selects a license (publication is gated).
license=${license:-LicenseRef-Unselected-Draft}
# Owner-provided contact; the placeholder blocks publication (release-gate.sh).
maintainer=${PACKAGE_MAINTAINER:-}
if [[ -z $maintainer && -s "$root/packaging/MAINTAINER" ]]; then
    maintainer=$(sed -n '1{p;q}' "$root/packaging/MAINTAINER")
fi
maintainer=${maintainer:-Tabtrail maintainers (unassigned) <maintainers@tabtrail.invalid>}
case $target in
    x86_64-*) deb_arch=amd64; rpm_arch=x86_64 ;;
    aarch64-*) deb_arch=arm64; rpm_arch=aarch64 ;;
    *) echo "error: unsupported target $target" >&2; exit 1 ;;
esac
mkdir -p "$out"
out=$(cd "$out" && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

reported=$("$bin" --version)
[[ $reported == "tabtrail $version" ]] || { echo "error: $bin reports '$reported', expected tabtrail $version" >&2; exit 1; }

# THIRD_PARTY_NOTICES may point to a pre-generated file (e.g. when packaging
# on a machine without cargo); otherwise it is generated from cargo metadata.
if [[ -n ${THIRD_PARTY_NOTICES:-} ]]; then
    cp "$THIRD_PARTY_NOTICES" "$work/THIRD-PARTY-NOTICES.txt"
else
    python3 "$root/scripts/third-party-notices.py" "$work/THIRD-PARTY-NOTICES.txt" "$target" >/dev/null
fi
# Notices for the Rust standard library shipped with the toolchain that
# built the binary (RUST_NOTICES_DIR overrides, e.g. without rustc).
rust_notices=${RUST_NOTICES_DIR:-$(rustc --print sysroot 2>/dev/null)/share/doc/rust}
mkdir -p "$work/notices/rust-toolchain"
copyrights=("$rust_notices"/COPYRIGHT*)
if ((${#copyrights[@]} == 0)); then
    echo "error: no Rust toolchain notices (COPYRIGHT*) in $rust_notices" >&2
    exit 1
fi
for f in "${copyrights[@]}" "$rust_notices"/LICENSE*; do cp "$f" "$work/notices/rust-toolchain/"; done
if [[ -d "$rust_notices/licenses" ]]; then cp -r "$rust_notices/licenses" "$work/notices/rust-toolchain/"; fi
if [[ -z ${RUST_NOTICES_DIR:-} ]]; then
    rustc -vV > "$work/notices/rust-toolchain/PROVENANCE.txt"
    # The static runtime notices are specific to the toolchain commit.
    runtime_notices="$root/packaging/notices/STATIC-RUNTIME-NOTICES.txt"
    if [[ $target == *-musl && -s $runtime_notices ]]; then
        recorded=$(sed -n 's/^rust-commit: //p;T;q' "$runtime_notices")
        building=$(sed -n 's/^commit-hash: //p;T;q' "$work/notices/rust-toolchain/PROVENANCE.txt")
        if [[ $recorded != "$building" ]]; then
            echo "error: static runtime notices are for rust $recorded, but rustc is $building;" >&2
            echo "       build with that toolchain or run scripts/fetch-runtime-notices.sh" >&2
            exit 1
        fi
    fi
else
    echo "toolchain notices provided via RUST_NOTICES_DIR" > "$work/notices/rust-toolchain/PROVENANCE.txt"
fi
# Statically linked C runtime notices, once provided (see RELEASING.md).
for f in "$root"/packaging/notices/*; do cp "$f" "$work/notices/"; done
cp "$work/THIRD-PARTY-NOTICES.txt" "$work/notices/"

docs=(README.md CHANGELOG.md docs/SECURITY.md docs/ARCHITECTURE.md)
# The owner's project license files, all of them (e.g. dual licenses).
licfiles=("$root"/LICENSE* "$root"/LICENCE* "$root"/COPYING*)

# tarball
name="tabtrail-$version-$target"
mkdir -p "$work/$name/doc"
install -m 0755 "$bin" "$work/$name/tabtrail"
for d in "${docs[@]}"; do install -m 0644 "$root/$d" "$work/$name/doc/"; done
cp -r "$work/notices" "$work/$name/notices"
install -m 0644 "$work/THIRD-PARTY-NOTICES.txt" "$work/$name/"
for l in "${licfiles[@]}"; do install -m 0644 "$l" "$work/$name/"; done
tar -C "$work" --owner=0 --group=0 --numeric-owner --sort=name \
    --mtime="@${SOURCE_DATE_EPOCH:-0}" -czf "$out/$name.tar.gz" "$name"

# .deb
if command -v dpkg-deb >/dev/null; then
    d="$work/deb"
    mkdir -p "$d/DEBIAN" "$d/usr/bin" "$d/usr/share/doc/tabtrail"
    install -m 0755 "$bin" "$d/usr/bin/tabtrail"
    for doc in "${docs[@]}"; do install -m 0644 "$root/$doc" "$d/usr/share/doc/tabtrail/"; done
    install -m 0644 "$work/THIRD-PARTY-NOTICES.txt" "$d/usr/share/doc/tabtrail/"
    cp -r "$work/notices" "$d/usr/share/doc/tabtrail/notices"
    # Every license file is kept under its own name, and `copyright`
    # concatenates all of them (never overwritten by the last one).
    {
        echo "tabtrail $version"
        echo "Project license: ${license:-not yet selected by the owner}"
        echo "Third-party notices: /usr/share/doc/tabtrail/THIRD-PARTY-NOTICES.txt and notices/"
        for l in "${licfiles[@]}"; do
            install -m 0644 "$l" "$d/usr/share/doc/tabtrail/"
            printf '\n===== %s =====\n' "$(basename "$l")"
            cat "$l"
        done
    } > "$d/usr/share/doc/tabtrail/copyright"
    cat > "$d/DEBIAN/control" <<CONTROL
Package: tabtrail
Version: $version
Architecture: $deb_arch
Maintainer: $maintainer
Section: utils
Priority: optional
Homepage: https://github.com/TNYCL/tabtrail
Description: discovery-based Tab completion for Bash, Zsh and Fish
 Tabtrail adds completions for commands that have no native completion,
 discovered from installed manual pages and (for trusted executables)
 help output. Enable it per user with: tabtrail install
CONTROL
    dpkg-deb --root-owner-group --build "$d" "$out/tabtrail_${version}_${deb_arch}.deb" >/dev/null
fi

# .rpm
if command -v rpmbuild >/dev/null; then
    r="$work/rpm"
    mkdir -p "$r"/{BUILD,RPMS,SOURCES,SPECS,SRPMS}
    cp "$bin" "$r/SOURCES/tabtrail"
    for doc in "${docs[@]}"; do cp "$root/$doc" "$r/SOURCES/"; done
    cp "$work/THIRD-PARTY-NOTICES.txt" "$r/SOURCES/"
    cp -r "$work/notices" "$r/SOURCES/notices"
    mkdir -p "$r/SOURCES/licenses"
    license_files_entry=""
    for l in "${licfiles[@]}"; do
        cp "$l" "$r/SOURCES/licenses/"
        license_files_entry="%{_datadir}/licenses/tabtrail"
    done
    cat > "$r/SPECS/tabtrail.spec" <<SPEC
Name: tabtrail
Version: $version
Release: 1
Summary: Discovery-based Tab completion for Bash, Zsh and Fish
License: $license
URL: https://github.com/TNYCL/tabtrail
BuildArch: $rpm_arch
%description
Tabtrail adds completions for commands that have no native completion.
Enable it per user with: tabtrail install
%prep
%build
%install
install -D -m 0755 %{_sourcedir}/tabtrail %{buildroot}%{_bindir}/tabtrail
install -d %{buildroot}%{_docdir}/tabtrail
install -m 0644 %{_sourcedir}/*.md %{_sourcedir}/THIRD-PARTY-NOTICES.txt %{buildroot}%{_docdir}/tabtrail/
cp -r %{_sourcedir}/notices %{buildroot}%{_docdir}/tabtrail/notices
if [ -n "\$(ls -A %{_sourcedir}/licenses)" ]; then
    install -d %{buildroot}%{_datadir}/licenses/tabtrail
    cp %{_sourcedir}/licenses/* %{buildroot}%{_datadir}/licenses/tabtrail/
fi
%files
%{_bindir}/tabtrail
%{_docdir}/tabtrail
${license_files_entry}
SPEC
    # Reproducibility: with SOURCE_DATE_EPOCH set, use it as the build time,
    # clamp payload mtimes to it and use a fixed build host name.
    # rpm >= 4.20 replaced %clamp_mtime_to_source_date_epoch by
    # %build_mtime_policy.
    repro=()
    if [[ -n ${SOURCE_DATE_EPOCH:-} ]]; then
        rpm_ver=$(rpmbuild --version | sed -n 's/^RPM version \([0-9]*\)\.\([0-9]*\).*/\1 \2/p;T;q')
        read -r rpm_major rpm_minor <<<"${rpm_ver:-0 0}"
        if ((rpm_major > 4 || (rpm_major == 4 && rpm_minor >= 20))); then
            repro=(--define "build_mtime_policy clamp_to_source_date_epoch")
        else
            repro=(--define "clamp_mtime_to_source_date_epoch 1")
        fi
        repro+=(--define "use_source_date_epoch_as_buildtime 1" --define "_buildhost reproducible")
    fi
    rpmbuild --quiet --define "_topdir $r" --define "debug_package %{nil}" \
        --define "__strip /bin/true" --define "_build_id_links none" "${repro[@]}" \
        --target "$rpm_arch" -bb "$r/SPECS/tabtrail.spec" >/dev/null
    find "$r/RPMS" -name '*.rpm' -exec cp {} "$out/" \;
fi

sums=$(cd "$out" && find . -maxdepth 1 -type f ! -name SHA256SUMS -printf '%f\n' | LC_ALL=C sort | xargs -r sha256sum)
printf '%s\n' "$sums" > "$out/SHA256SUMS"
echo "packaged into $out:"
cat "$out/SHA256SUMS"
