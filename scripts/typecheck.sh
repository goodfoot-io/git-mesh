#!/usr/bin/env bash
set -uo pipefail

# Run cargo check for the Rust CLI and the extension package typecheck in
# parallel. Let Yarn resolve package-local tool binaries for the extension.

WORKSPACE_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

PIDS=()
EXIT=0

# --- Rust CLI typecheck ---
if [ -f "$WORKSPACE_ROOT/packages/git-mesh/Cargo.toml" ]; then
  echo "Running cargo check for packages/git-mesh..."
  (cd "$WORKSPACE_ROOT/packages/git-mesh" && cargo check --quiet) &
  PIDS+=($!)
else
  echo "Warning: packages/git-mesh/Cargo.toml not found, skipping cargo check." >&2
fi

if [ -d "$WORKSPACE_ROOT/packages/extension" ]; then
  echo "Running extension typecheck..."
  (cd "$WORKSPACE_ROOT/packages/extension" && yarn typecheck) &
  PIDS+=($!)
else
  echo "Warning: packages/extension not found, skipping TypeScript typecheck." >&2
fi

for PID in "${PIDS[@]}"; do
  wait "$PID" || EXIT=1
done

exit $EXIT
