#!/usr/bin/env bash
# Asserts the repository's current publication policy, so that a regression
# (missing license text, notices or maintainer metadata, or a reintroduced
# `publish = false`) fails CI instead of being reported informationally:
#   - GitHub release publication: scripts/release-gate.sh github passes;
#   - crates.io publication (approved by the owner): scripts/release-gate.sh
#     crates passes.
#   scripts/check-release-policy.sh [repository-root]
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
root=${1:-$(cd "$here/.." && pwd)}

"$here/release-gate.sh" github "$root"
"$here/release-gate.sh" crates "$root"
echo "publication policy: github and crates.io gates open"
