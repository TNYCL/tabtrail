#!/usr/bin/env bash
# Asserts the repository's current publication policy, so that a regression
# (missing license text, notices or maintainer metadata, or an accidentally
# opened registry gate) fails CI instead of being reported informationally:
#   - GitHub release publication: scripts/release-gate.sh github passes;
#   - crates.io publication is deferred by the owner: release-gate.sh crates
#     is denied by `publish = false` and by nothing else.
# When the owner approves crates.io publication, change this policy together
# with removing `publish = false`.
#   scripts/check-release-policy.sh [repository-root]
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
root=${1:-$(cd "$here/.." && pwd)}

"$here/release-gate.sh" github "$root"

status=0
out=$("$here/release-gate.sh" crates "$root" 2>&1) || status=$?
expected="publication blocked for 'crates':
  - Cargo.toml has publish = false"
if ((status == 0)); then
    echo "error: the crates.io gate is open, but crates.io publication is deferred (keep publish = false)" >&2
    exit 1
fi
if ((status != 1)) || [[ $out != "$expected" ]]; then
    echo "error: the crates.io gate must be denied only by publish = false (exit $status):" >&2
    printf '%s\n' "$out" >&2
    exit 1
fi
echo "publication policy: github gate open; crates.io deferred (denied only by publish = false)"
