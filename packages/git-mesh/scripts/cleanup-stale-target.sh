#!/usr/bin/env bash
# Remove ./target if it is a stale symlink from pre-per-worktree-target days.
# Idempotent and silent on the happy path.
node -e "try { if (require('fs').lstatSync('target').isSymbolicLink()) { require('fs').unlinkSync('target'); } } catch (e) { if (e.code !== 'ENOENT') throw e; }"
