#!/usr/bin/env bash
set -euo pipefail

# This script is intended to be run from the repo root
cd "$(dirname "$0")"

echo "Installing owallet from crates/owallet..."
cargo install --path crates/owallet --features dev-envs

# Ensure cargo bin is on PATH for this session
export PATH="$HOME/.cargo/bin:$PATH"

echo "Checking installation..."
if command -v owallet >/dev/null 2>&1; then
  echo "owallet installed at: $(command -v owallet)"
else
  echo "owallet not found in PATH. Add ~/.cargo/bin to your PATH."
  exit 1
fi

owallet --version
