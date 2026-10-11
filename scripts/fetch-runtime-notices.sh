#!/usr/bin/env bash
# Regenerates packaging/notices/STATIC-RUNTIME-NOTICES.txt for the Rust
# toolchain that builds the static musl release binaries.
#
#   scripts/fetch-runtime-notices.sh [RUST_COMMIT]   (default: `rustc -vV` commit-hash)
#
# Besides Rust crates and the standard library (covered by
# THIRD-PARTY-NOTICES.txt and the toolchain's own share/doc/rust notices),
# Rust's self-contained musl targets link, per src/bootstrap at that commit:
#   - libc.a, crt1.o, Scrt1.o, rcrt1.o, crti.o, crtn.o  -> musl
#     (version from src/ci/docker/scripts/musl-toolchain.sh, used by the
#      dist-x86_64-musl and dist-arm-linux-musl images)
#   - crtbegin*.o / crtend*.o -> LLVM compiler-rt/lib/builtins
#   - libunwind.a             -> LLVM libunwind
#     (llvm-project = the src/llvm-project submodule commit)
# Texts are taken from the musl release tarball that Rust's CI downloads and
# from the pinned rust-lang/llvm-project commit. This documents third-party
# licenses only; it says nothing about Tabtrail's own license.
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
commit=${1:-$(rustc -vV | sed -n 's/^commit-hash: //p')}
[[ $commit =~ ^[0-9a-f]{40}$ ]] || { echo "error: need a full Rust commit hash" >&2; exit 1; }
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
fetch() { curl -sSfL --proto '=https' --tlsv1.2 --retry 2 --max-time 120 -o "$2" "$1"; }
raw=https://raw.githubusercontent.com

fetch "$raw/rust-lang/rust/$commit/src/ci/docker/scripts/musl-toolchain.sh" "$work/musl-toolchain.sh"
musl_ver=$(sed -n 's/.*MUSL_VER=\([0-9][0-9.]*\).*/\1/p;T;q' "$work/musl-toolchain.sh")
[[ -n $musl_ver ]] || { echo "error: MUSL_VER not found in musl-toolchain.sh" >&2; exit 1; }

fetch "https://api.github.com/repos/rust-lang/rust/contents/src/llvm-project?ref=$commit" "$work/llvm.json"
llvm_sha=$(python3 -c 'import json,sys; d=json.load(open(sys.argv[1])); assert d["type"]=="submodule"; print(d["sha"])' "$work/llvm.json")
[[ $llvm_sha =~ ^[0-9a-f]{40}$ ]] || { echo "error: llvm-project submodule not found" >&2; exit 1; }

musl_url="https://www.musl-libc.org/releases/musl-$musl_ver.tar.gz"
fetch "$musl_url" "$work/musl.tar.gz"
musl_sha=$(sha256sum "$work/musl.tar.gz" | cut -d' ' -f1)
tar -xzf "$work/musl.tar.gz" -C "$work" "musl-$musl_ver/COPYRIGHT"
unwind_url="$raw/rust-lang/llvm-project/$llvm_sha/libunwind/LICENSE.TXT"
crt_url="$raw/rust-lang/llvm-project/$llvm_sha/compiler-rt/LICENSE.TXT"
fetch "$unwind_url" "$work/libunwind-LICENSE.TXT"
fetch "$crt_url" "$work/compiler-rt-LICENSE.TXT"

out="$root/packaging/notices/STATIC-RUNTIME-NOTICES.txt"
mkdir -p "$(dirname "$out")"
{
    echo "Static runtime notices for tabtrail release binaries (*-unknown-linux-musl)"
    echo "==========================================================================="
    echo
    echo "rust-commit: $commit"
    echo "musl-version: $musl_ver"
    echo "musl-source: $musl_url"
    echo "musl-source-sha256: $musl_sha"
    echo "llvm-project-commit: $llvm_sha"
    echo "libunwind-license-source: $unwind_url"
    echo "libunwind-license-sha256: $(sha256sum "$work/libunwind-LICENSE.TXT" | cut -d' ' -f1)"
    echo "compiler-rt-license-source: $crt_url"
    echo "compiler-rt-license-sha256: $(sha256sum "$work/compiler-rt-LICENSE.TXT" | cut -d' ' -f1)"
    echo
    echo "The static binaries link these runtime objects from the Rust toolchain's"
    echo "self-contained musl directory: libc.a and crt1.o/Scrt1.o/rcrt1.o/crti.o/crtn.o"
    echo "(musl $musl_ver), crtbegin*.o/crtend*.o (LLVM compiler-rt) and libunwind.a"
    echo "(LLVM libunwind). Their notices follow verbatim."
    printf '\n\n===== musl %s: COPYRIGHT =====\n\n' "$musl_ver"
    cat "$work/musl-$musl_ver/COPYRIGHT"
    printf '\n\n===== LLVM libunwind (llvm-project %s): LICENSE.TXT =====\n\n' "$llvm_sha"
    cat "$work/libunwind-LICENSE.TXT"
    printf '\n\n===== LLVM compiler-rt (llvm-project %s): LICENSE.TXT =====\n\n' "$llvm_sha"
    cat "$work/compiler-rt-LICENSE.TXT"
} > "$out"
echo "wrote $out (rust $commit, musl $musl_ver, llvm-project $llvm_sha)"
