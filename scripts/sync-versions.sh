#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SOURCE="$REPO_ROOT/packages/git-mesh/package.json"

if [ ! -f "$SOURCE" ]; then
  echo "Error: Source package.json not found at $SOURCE" >&2
  exit 1
fi

VERSION=$(node -e "console.log(JSON.parse(require('fs').readFileSync('$SOURCE','utf8')).version)")

if [ -z "$VERSION" ]; then
  echo "Error: Could not read version from $SOURCE" >&2
  exit 1
fi

echo "Source of truth: $SOURCE"
echo "Version: $VERSION"
echo ""

updated=0

# Update npm platform packages
for pkg_dir in "$REPO_ROOT"/npm/git-mesh-*/; do
  pkg_json="$pkg_dir/package.json"
  if [ -f "$pkg_json" ]; then
    current=$(node -e "console.log(JSON.parse(require('fs').readFileSync('$pkg_json','utf8')).version)")
    if [ "$current" != "$VERSION" ]; then
      node -e "
        const fs = require('fs');
        const pkg = JSON.parse(fs.readFileSync('$pkg_json', 'utf8'));
        pkg.version = '$VERSION';
        fs.writeFileSync('$pkg_json', JSON.stringify(pkg, null, 2) + '\n');
      "
      echo "Updated: $pkg_json ($current -> $VERSION)"
      updated=$((updated + 1))
    else
      echo "OK:      $pkg_json (already $VERSION)"
    fi
  fi
done

# Update optionalDependencies in packages/git-mesh/package.json
cli_json="$REPO_ROOT/packages/git-mesh/package.json"
if [ -f "$cli_json" ]; then
  node -e "
    const fs = require('fs');
    const pkg = JSON.parse(fs.readFileSync('$cli_json', 'utf8'));
    let changed = false;
    if (pkg.optionalDependencies) {
      for (const [name, ver] of Object.entries(pkg.optionalDependencies)) {
        if (ver !== '$VERSION') {
          pkg.optionalDependencies[name] = '$VERSION';
          changed = true;
        }
      }
    }
    if (changed) {
      fs.writeFileSync('$cli_json', JSON.stringify(pkg, null, 2) + '\n');
    }
    process.stdout.write(changed ? 'updated' : 'ok');
  "
  result=$?
  echo ""
  if [ $result -eq 0 ]; then
    echo "Updated: $cli_json optionalDependencies -> $VERSION"
    updated=$((updated + 1))
  fi
fi

# Update packages/extension/package.json
ext_json="$REPO_ROOT/packages/extension/package.json"
if [ -f "$ext_json" ]; then
  current=$(node -e "console.log(JSON.parse(require('fs').readFileSync('$ext_json','utf8')).version)")
  if [ "$current" != "$VERSION" ]; then
    node -e "
      const fs = require('fs');
      const pkg = JSON.parse(fs.readFileSync('$ext_json', 'utf8'));
      pkg.version = '$VERSION';
      fs.writeFileSync('$ext_json', JSON.stringify(pkg, null, 2) + '\n');
    "
    echo "Updated: $ext_json ($current -> $VERSION)"
    updated=$((updated + 1))
  else
    echo "OK:      $ext_json (already $VERSION)"
  fi
fi

echo ""
echo "Done. $updated file(s) updated to version $VERSION."
