#!/usr/bin/env bash
# Run norm's own TUI test suite (packages/tui/test) against opentui's wasm core.
#
# The installed @opentui/core (bun's store copy, which every consumer —
# @opentui/solid, @opentui/keymap, packages/tui — resolves to) is swapped for
# this package's patched dist/ for the duration of the run and restored on
# exit. The native platform library is hidden at the same time, so a test that
# silently fell back to the native core would fail instead of passing.
#
# Files run one at a time (bun's single-process run of the whole suite crashes
# after the first file on the native core too). Prints "<file> <pass> <fail>
# <exit>" per file and exits non-zero if any file failed.
#
#   bash scripts/build.sh && bash scripts/test-norm-tui.sh
set -euo pipefail

here="$(cd "$(dirname "$0")/.." && pwd)"
norm="$(cd "$here/../.." && pwd)"
dist="$here/dist"
[ -f "$dist/opentui.wasm" ] || { echo "no $dist/opentui.wasm — run scripts/build.sh first" >&2; exit 2; }

store="$(readlink -f "$norm/packages/tui/node_modules/@opentui/core")"
[ -f "$store/package.json" ] || { echo "@opentui/core is not installed (bun install at the repo root)" >&2; exit 2; }
native="$(dirname "$store")/core-$(uname -s | tr A-Z a-z | sed 's/darwin/darwin/')-$(uname -m | sed 's/x86_64/x64/;s/aarch64/arm64/')"
backup="$(mktemp -d)/core"

cp -a "$store" "$backup"
restore() {
  rm -rf "$store" && cp -a "$backup" "$store"
  [ -e "$native.hidden" ] && mv "$native.hidden" "$native"
  rm -rf "$(dirname "$backup")"
}
trap restore EXIT
rm -rf "$store" && cp -a "$dist/core" "$store"
[ -e "$native" ] && mv "$native" "$native.hidden"

export OPENTUI_BACKEND=wasm OPENTUI_WASM_PATH="$dist/opentui.wasm"
cd "$norm/packages/tui"
status=0 total_pass=0 total_fail=0
while IFS= read -r file; do
  set +e
  out="$(timeout 300 bun test --timeout 30000 "$file" 2>&1)"; code=$?
  set -e
  pass="$(grep -oE '^ *[0-9]+ pass' <<<"$out" | grep -oE '[0-9]+' | tail -1 || true)"
  fail="$(grep -oE '^ *[0-9]+ fail' <<<"$out" | grep -oE '[0-9]+' | tail -1 || true)"
  echo "$file ${pass:-?} ${fail:-?} $code"
  total_pass=$((total_pass + ${pass:-0})); total_fail=$((total_fail + ${fail:-0}))
  if [ "$code" != 0 ] || [ "${fail:-0}" != 0 ] || [ -z "$pass" ]; then status=1; fi
done < <(find test -name '*.test.ts' -o -name '*.test.tsx' | sort)
echo "total: $total_pass pass, $total_fail fail (wasm core)"
exit $status
