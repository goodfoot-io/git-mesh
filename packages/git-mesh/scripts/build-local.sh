#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."

target_root="${GIT_MESH_CARGO_TARGET_ROOT:-./target-cache}"
built="$target_root/build/release/git-mesh"

mkdir -p "$HOME/.local/bin" "$HOME/.local/share/man/man1"

env CARGO_BUILD_JOBS=1 CARGO_TARGET_DIR="$target_root/build" cargo build --release
install -m 0755 "$built" "$HOME/.local/bin/git-mesh"

path_bin="$(command -v git-mesh || true)"
if [ -n "$path_bin" ]; then
  target="$(readlink -f "$path_bin")"
  if [ "$target" != "$HOME/.local/bin/git-mesh" ]; then
    install -m 0755 "$built" "$target"
  fi
fi

yarn build:man
install -m 0644 man/git-mesh.1 "$HOME/.local/share/man/man1/git-mesh.1"
