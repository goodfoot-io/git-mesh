#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SOURCE="$REPO_ROOT/packages/git-mesh/package.json"
LEVEL="${1:-patch}"

case "$LEVEL" in
  major|minor|patch) ;;
  *)
    echo "Usage: yarn bump [major|minor|patch]" >&2
    exit 1
    ;;
esac

NEW_VERSION=$(node -e "
  const fs = require('fs');
  const raw = fs.readFileSync('$SOURCE', 'utf8');
  const pkg = JSON.parse(raw);
  const [maj, min, pat] = pkg.version.split('.').map(Number);
  const next = '$LEVEL' === 'major' ? [maj + 1, 0, 0]
             : '$LEVEL' === 'minor' ? [maj, min + 1, 0]
             : [maj, min, pat + 1];
  const newVersion = next.join('.');
  const updated = raw.replace(/\"version\": \"[^\"]+\"/, JSON.stringify('version') + ': ' + JSON.stringify(newVersion));
  fs.writeFileSync('$SOURCE', updated);
  console.log(newVersion);
")

echo "Bumped packages/git-mesh to $NEW_VERSION"
echo ""
bash "$REPO_ROOT/scripts/sync-versions.sh"
