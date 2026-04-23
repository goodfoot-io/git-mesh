#!/usr/bin/env bash
#
# Install the built VSIX extension into the current editor (Cursor or VSCode).
#
# This script is called after `vsce package` to install the extension
# into the running editor instance.
#

set -e

# Get the directory where this script lives
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PACKAGE_DIR="$(dirname "$SCRIPT_DIR")"

# Get version from package.json
VERSION=$(node -p "require('$PACKAGE_DIR/package.json').version")
VSIX_FILE="$PACKAGE_DIR/git-mesh-${VERSION}.vsix"

# Check if VSIX file exists
if [ ! -f "$VSIX_FILE" ]; then
  echo "Error: VSIX file not found: $VSIX_FILE"
  exit 1
fi

echo "Found VSIX: $VSIX_FILE"

# Detect which editor CLI to use
# Priority: cursor > code (since this is primarily developed for Cursor)
EDITOR_CLI=""
if command -v cursor &> /dev/null; then
  EDITOR_CLI="cursor"
elif command -v code &> /dev/null; then
  EDITOR_CLI="code"
else
  echo "Warning: Neither 'cursor' nor 'code' CLI found in PATH."
  echo "VSIX was built successfully but could not be installed automatically."
  echo "Install manually: Extensions > ... > Install from VSIX..."
  exit 0
fi

echo "Using editor CLI: $EDITOR_CLI"

# Install the extension
echo "Installing extension..."
INSTALL_OUTPUT=$("$EDITOR_CLI" --install-extension "$VSIX_FILE" --force 2>&1)
INSTALL_EXIT_CODE=$?

echo "$INSTALL_OUTPUT"

if [ $INSTALL_EXIT_CODE -ne 0 ]; then
  echo "Error: Extension installation failed (exit code $INSTALL_EXIT_CODE)."
  exit $INSTALL_EXIT_CODE
fi

if echo "$INSTALL_OUTPUT" | grep -qi "failed installing"; then
  echo "Error: Extension installation reported a failure."
  exit 1
fi

echo "Extension installed successfully!"
echo ""
echo "Note: You may need to reload the editor window for changes to take effect."
echo "Use Command Palette > 'Developer: Reload Window' or restart the editor."
