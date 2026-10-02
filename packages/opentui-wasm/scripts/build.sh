#!/usr/bin/env bash
# Build opentui with the wasm patch set.
#
#   bash scripts/build.sh
#
# 1. clone anomalyco/opentui into .work/opentui (or $OPENTUI_WORK) at the
#    commit pinned in UPSTREAM and apply patches/0*.patch
# 2. find or download the pinned Zig into .work/ (or $OPENTUI_TOOLS)
# 3. bun install there and apply the bun-ffi-structs patch
# 4. zig build -Dtarget=wasm32-wasi (falls back to git-cloning the Zig
#    package dependencies when Zig's own fetcher cannot get through a proxy)
# 5. build the @opentui/core and @opentui/solid packages
# 6. copy into dist/: opentui.wasm, core/ (index.browser.js, wasm.js,
#    opentui.wasm, plus the usual bun/node entries), solid/
#
# Re-running is incremental: the checkout is only reset when the pin or the
# patches change (stamp file in the checkout).
set -euo pipefail

PKG="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck disable=SC1091
eval "$(grep -E '^[A-Z_]+=' "$PKG/UPSTREAM")"

WORK="${OPENTUI_WORK:-$PKG/.work/opentui}"
TOOLS="${OPENTUI_TOOLS:-$PKG/.work}"
DIST="$PKG/dist"
OPTIMIZE="${OPENTUI_WASM_OPTIMIZE:-ReleaseSmall}"

log() { printf '\033[1m==> %s\033[0m\n' "$*"; }

# --- Zig ---------------------------------------------------------------------
find_zig() {
  if command -v zig >/dev/null 2>&1 && [ "$(zig version)" = "$ZIG_VERSION" ]; then
    command -v zig
    return
  fi
  local arch os
  arch="$(uname -m)"
  case "$(uname -s)" in
    Linux) os=linux ;;
    Darwin) os=macos ;;
    *) echo "unsupported host OS for the Zig download: $(uname -s)" >&2; exit 1 ;;
  esac
  case "$arch" in arm64) arch=aarch64 ;; esac
  local name="zig-${arch}-${os}-${ZIG_VERSION}"
  if [ ! -x "$TOOLS/$name/zig" ]; then
    mkdir -p "$TOOLS"
    log "downloading Zig $ZIG_VERSION ($name)" >&2
    curl -fsSL "https://ziglang.org/download/${ZIG_VERSION}/${name}.tar.xz" -o "$TOOLS/$name.tar.xz"
    tar -xJf "$TOOLS/$name.tar.xz" -C "$TOOLS"
    rm -f "$TOOLS/$name.tar.xz"
  fi
  echo "$TOOLS/$name/zig"
}

ZIG="$(find_zig)"
log "zig: $ZIG ($("$ZIG" version))"

# --- opentui checkout + patches -----------------------------------------------
patch_files=("$PKG"/patches/0*.patch)
stamp="$(cat "$PKG/UPSTREAM" "${patch_files[@]}" | sha256sum | cut -d' ' -f1)"

if [ ! -d "$WORK/.git" ]; then
  log "cloning $OPENTUI_REPO into $WORK"
  mkdir -p "$(dirname "$WORK")"
  git clone --quiet "$OPENTUI_REPO" "$WORK"
fi

if [ "$(cat "$WORK/.opentui-wasm-stamp" 2>/dev/null || true)" != "$stamp" ]; then
  if ! git -C "$WORK" cat-file -e "$OPENTUI_COMMIT^{commit}" 2>/dev/null; then
    git -C "$WORK" fetch --quiet origin "$OPENTUI_TAG" || git -C "$WORK" fetch --quiet origin "$OPENTUI_COMMIT"
  fi
  log "applying ${#patch_files[@]} patches onto $OPENTUI_TAG ($OPENTUI_COMMIT)"
  # The checkout is build scratch owned by this script.
  git -C "$WORK" checkout --quiet --force -B opentui-wasm "$OPENTUI_COMMIT"
  git -C "$WORK" -c user.name="opentui-wasm build" -c user.email="opentui-wasm@localhost" \
    am --quiet --committer-date-is-author-date "${patch_files[@]}"
  echo "$stamp" > "$WORK/.opentui-wasm-stamp"
fi

# --- JS dependencies -----------------------------------------------------------
log "bun install (opentui workspace)"
(cd "$WORK" && bun install --frozen-lockfile >/dev/null 2>&1 || bun install >/dev/null)

bfs_dir="$(cd "$WORK/packages/core/node_modules/bun-ffi-structs" && pwd -P)"
bfs_version="$(node -p "require('$bfs_dir/package.json').version")"
if [ "$bfs_version" != "$BUN_FFI_STRUCTS_VERSION" ]; then
  echo "bun-ffi-structs is $bfs_version, the patch is for $BUN_FFI_STRUCTS_VERSION" >&2
  exit 1
fi
if ! grep -q "__BUN_FFI_STRUCTS_BACKEND__" "$bfs_dir/dist/index.js"; then
  log "patching bun-ffi-structs@$bfs_version"
  patch --quiet -p1 -d "$bfs_dir" < "$PKG/patches/bun-ffi-structs@${BUN_FFI_STRUCTS_VERSION}.patch"
fi

# --- Zig package dependencies ---------------------------------------------------
ZIG_DIR="$WORK/packages/core/src/zig"
if ! (cd "$ZIG_DIR" && "$ZIG" build --fetch >/dev/null 2>&1); then
  # Zig's HTTP client does not get through every proxy; fetch the same
  # sources with git and hand them to `zig fetch`, which checks the hashes
  # against build.zig.zon.
  log "zig build --fetch failed; fetching Zig dependencies with git"
  deps_dir="$TOOLS/zig-deps"
  mkdir -p "$deps_dir"
  grep -oE '\.url = "[^"]+"' "$ZIG_DIR/build.zig.zon" | sed -E 's/.*"(.*)"/\1/' | while read -r url; do
    case "$url" in
      git+*)
        repo="${url#git+}"; ref="${repo##*#}"; repo="${repo%%#*}" ;;
      https://github.com/*/archive/*.tar.gz)
        repo="${url%/archive/*}"; ref="$(basename "$url" .tar.gz)" ;;
      *)
        echo "don't know how to fetch $url" >&2; exit 1 ;;
    esac
    name="$(basename "$repo")-$ref"
    if [ ! -d "$deps_dir/$name" ]; then
      rm -rf "$deps_dir/$name.git"
      git clone --quiet "$repo" "$deps_dir/$name.git"
      git -C "$deps_dir/$name.git" checkout --quiet "$ref"
      mkdir -p "$deps_dir/$name"
      git -C "$deps_dir/$name.git" archive HEAD | tar -x -C "$deps_dir/$name"
      rm -rf "$deps_dir/$name.git"
    fi
    "$ZIG" fetch "$deps_dir/$name" >/dev/null
  done
fi

# --- wasm core ------------------------------------------------------------------
log "zig build -Dtarget=wasm32-wasi -Doptimize=$OPTIMIZE"
(cd "$ZIG_DIR" && "$ZIG" build -Dtarget=wasm32-wasi -Doptimize="$OPTIMIZE")
WASM="$ZIG_DIR/lib/wasm32-wasi/opentui.wasm"

# --- JS packages ----------------------------------------------------------------
log "building @opentui/core"
(cd "$WORK/packages/core" && bun scripts/build.ts --lib >/dev/null)
log "building @opentui/solid"
(cd "$WORK/packages/solid" && bun scripts/build.ts >/dev/null)

# --- dist -------------------------------------------------------------------------
log "writing $DIST"
rm -rf "$DIST"
mkdir -p "$DIST"
cp "$WASM" "$DIST/opentui.wasm"
cp -R "$WORK/packages/core/dist" "$DIST/core"
cp -R "$WORK/packages/solid/dist" "$DIST/solid"

wasm_bytes="$(wc -c < "$DIST/opentui.wasm")"
wasm_gzip="$(gzip -9 -c "$DIST/opentui.wasm" | wc -c)"
log "done: opentui.wasm ${wasm_bytes} bytes (${wasm_gzip} gzipped), core/ and solid/ in $DIST"
