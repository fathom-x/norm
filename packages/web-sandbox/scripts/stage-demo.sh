#!/usr/bin/env bash
# Stage what the demo service image serves into .demo/ at the repo root:
#   .demo/web      this package's pages (landing + /sandbox/)
#   .demo/browser  the browser build (packages/web-tui) for /browser/
# The Dockerfile copies .demo/ (dist/ folders are dockerignored).
#
# Needs the browser build's own inputs: packages/opentui-wasm/dist (its
# scripts/build.sh) and owallet-web (`bun run build:owallet` in
# packages/web-tui: Rust + wasm32 target, clang, wasm-bindgen-cli).
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
repo="$(cd "$here/../.." && pwd)"
out="$repo/.demo"
rm -rf "$out" && mkdir -p "$out"

(cd "$here" && bun run build)
cp -R "$here/dist/web" "$out/web"

[ -d "$repo/packages/opentui-wasm/dist" ] ||
  { echo "stage-demo: packages/opentui-wasm/dist is missing — bash packages/opentui-wasm/scripts/build.sh" >&2; exit 1; }
[ -f "$repo/packages/web-tui/src/owallet-web/owallet_web_bg.wasm" ] ||
  { echo "stage-demo: owallet-web is not built — (cd packages/web-tui && bun run build:owallet)" >&2; exit 1; }
(cd "$repo/packages/web-tui" && NORM_WEB_BASE=/browser/ bun x vite build --outDir "$out/browser" --emptyOutDir)
echo "stage-demo: staged $(du -sh "$out" | cut -f1) in $out"
