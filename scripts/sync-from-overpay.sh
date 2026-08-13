#!/usr/bin/env bash
set -euo pipefail

# Pull the latest owallet-rs/ history from fathom-x/overpay into this
# repo's owallet/ directory.
#
# owallet/ began as `git subtree split -P owallet-rs` of overpay. The
# split is deterministic: re-splitting a newer overpay main regenerates
# identical commit ids for the history already imported here, plus new
# commits on top — so the merge below shares ancestry with this repo and
# behaves like an ordinary pull (the -Xsubtree option maps the split's
# root-level paths onto owallet/). Conflicts can only come from files
# norm itself has diverged on (owallet/README.md, owallet/CLAUDE.md, the
# vendored nip98 fixture); resolve keeping norm's standalone wording.
#
# Usage: scripts/sync-from-overpay.sh
#   OVERPAY_URL    override the source repo (default: fathom-x/overpay)
#   OVERPAY_BRANCH override the source branch (default: main)

url=${OVERPAY_URL:-https://github.com/fathom-x/overpay}
branch=${OVERPAY_BRANCH:-main}

cd "$(dirname "$0")/.."

echo "Fetching $url $branch..."
git fetch "$url" "$branch"

# `git subtree split` insists the prefix directory exists in the working
# tree, which it never does here — so run the split from a temporary
# detached worktree of the fetched overpay commit. Worktrees share this
# repo's object store, so the split result is immediately mergeable.
echo "Splitting owallet-rs/ history..."
wt=$(mktemp -d)
trap 'git worktree remove --force "$wt" 2>/dev/null || true' EXIT
git worktree add --detach --force "$wt" FETCH_HEAD >/dev/null
split=$(git -C "$wt" subtree split --prefix=owallet-rs HEAD)
echo "Split head: $split"

if git merge-base --is-ancestor "$split" HEAD; then
  echo "Already up to date."
  exit 0
fi

git merge -Xsubtree=owallet --no-edit \
  -m "Sync owallet from overpay ($(git rev-parse --short FETCH_HEAD))" "$split"
echo "Merged. Review, run the test suite, then push."
