#!/usr/bin/env bash
set -euo pipefail

# Pull the latest upstream opencode history into this repo's root.
#
# norm is a fork of anomalyco/opencode (opencode.ai): upstream's full
# history was merged into this repo at the root, so pulling newer
# upstream commits is an ordinary merge with shared ancestry. Keep
# norm's divergence surgical (bootstrap/provider layer, branding) so
# these merges stay cheap. norm-only files (owallet/, scripts/,
# .github/workflows/owallet-*.yml) never conflict — upstream doesn't
# have them.
#
# Usage: scripts/sync-from-opencode.sh
#   OPENCODE_URL    override the source repo (default: anomalyco/opencode)
#   OPENCODE_BRANCH override the source branch (default: dev)

url=${OPENCODE_URL:-https://github.com/anomalyco/opencode}
branch=${OPENCODE_BRANCH:-dev}

cd "$(dirname "$0")/.."

echo "Fetching $url $branch..."
git fetch "$url" "$branch"

if git merge-base --is-ancestor FETCH_HEAD HEAD; then
  echo "Already up to date."
  exit 0
fi

git merge --no-edit \
  -m "Sync from upstream opencode ($(git rev-parse --short FETCH_HEAD))" FETCH_HEAD
echo "Merged. Review, run the test suites, then push."
