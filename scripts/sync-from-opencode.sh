#!/usr/bin/env bash
set -euo pipefail

# Pull the latest upstream opencode RELEASE into this repo's root.
#
# norm is a fork of anomalyco/opencode (opencode.ai): upstream history
# was merged into this repo at the root, so pulling a newer upstream
# ref is an ordinary merge with shared ancestry. By default this syncs
# to upstream's newest `vX.Y.Z` release tag (known-good snapshots)
# rather than tip of their `dev` branch; set OPENCODE_REF to override
# with any tag or branch. Keep norm's divergence surgical
# (bootstrap/provider layer, branding) so these merges stay cheap.
# norm-only files (owallet/, scripts/, .github/workflows/owallet-*.yml)
# never conflict — upstream doesn't have them.
#
# Usage: scripts/sync-from-opencode.sh
#   OPENCODE_URL override the source repo (default: anomalyco/opencode)
#   OPENCODE_REF override the ref to sync to (tag or branch;
#                default: latest vX.Y.Z release tag)

url=${OPENCODE_URL:-https://github.com/anomalyco/opencode}
ref=${OPENCODE_REF:-}

cd "$(dirname "$0")/.."

if [ -z "$ref" ]; then
  ref=$(git ls-remote --tags "$url" 'v*' | grep -v '\^{}' \
    | awk -F/ '{print $NF}' | grep -E '^v[0-9]+\.[0-9]+\.[0-9]+$' \
    | sort -V | tail -1)
  [ -n "$ref" ] || { echo "No vX.Y.Z release tags found at $url" >&2; exit 1; }
  echo "Latest upstream release: $ref"
fi

echo "Fetching $url $ref..."
git fetch --no-tags "$url" "$ref"

if git merge-base --is-ancestor FETCH_HEAD HEAD; then
  echo "Already up to date."
  exit 0
fi

git merge --no-edit -m "Sync from upstream opencode ($ref)" FETCH_HEAD
echo "Merged. Review, run the test suites, then push."
