#!/usr/bin/env bash
set -o pipefail

{
  yarn typecheck &&
  yarn lint &&
  yarn test &&
  SKIP_INSTALL=1 yarn build
} 2>&1 | tee yarn-validate-output.log

EXIT_CODE=${PIPESTATUS[0]}
echo "Exit code: $EXIT_CODE" | tee -a yarn-validate-output.log
exit $EXIT_CODE
