#!/usr/bin/env bash
set -euo pipefail

usage() {
  echo "Usage: $0 <id>" >&2
  exit 2
}

die() {
  echo "snapshot.sh: $*" >&2
  exit 1
}

if [ "$#" -ne 1 ] || [ -z "${1:-}" ]; then
  usage
fi

SNAPSHOT_ID=$1

if ! REPO_ROOT=$(git rev-parse --show-toplevel 2>/dev/null); then
  die "not inside a Git repository"
fi

cd "$REPO_ROOT"
REPO_ROOT=$(pwd -P)

canonical_dir() {
  local path=$1

  case "$path" in
    /*) ;;
    *) path="$REPO_ROOT/$path" ;;
  esac

  (cd "$path" && pwd -P)
}

hash_text() {
  printf '%s' "$1" | git hash-object --stdin
}

read_state_field() {
  local file=$1
  local field=$2

  if [ ! -f "$file" ]; then
    return 0
  fi

  awk -v field="$field" '$1 == field { print $2; exit }' "$file"
}

join_alternates() {
  local value=$1

  if [ -n "${GIT_ALTERNATE_OBJECT_DIRECTORIES:-}" ]; then
    value="$value:$GIT_ALTERNATE_OBJECT_DIRECTORIES"
  fi

  printf '%s\n' "$value"
}

build_workspace_tree() (
  local object_dir=$1
  local alternates=$2
  local tmp_index
  local untracked_paths
  local real_index

  tmp_index=$(mktemp "${TMPDIR:-/tmp}/workspace-snapshot-index.XXXXXX")
  untracked_paths=$(mktemp "${TMPDIR:-/tmp}/workspace-snapshot-untracked.XXXXXX")
  trap 'rm -f "$tmp_index" "$untracked_paths"' EXIT

  real_index=$(git rev-parse --git-path index)
  case "$real_index" in
    /*) ;;
    *) real_index="$REPO_ROOT/$real_index" ;;
  esac

  if [ -f "$real_index" ]; then
    cp "$real_index" "$tmp_index"
  else
    rm -f "$tmp_index"
    env GIT_INDEX_FILE="$tmp_index" \
      GIT_OBJECT_DIRECTORY="$object_dir" \
      GIT_ALTERNATE_OBJECT_DIRECTORIES="$alternates" \
      git read-tree --empty
  fi

  env GIT_INDEX_FILE="$tmp_index" \
    GIT_OBJECT_DIRECTORY="$object_dir" \
    GIT_ALTERNATE_OBJECT_DIRECTORIES="$alternates" \
    git add -u -- .

  env GIT_INDEX_FILE="$tmp_index" \
    GIT_OBJECT_DIRECTORY="$object_dir" \
    GIT_ALTERNATE_OBJECT_DIRECTORIES="$alternates" \
    git ls-files -z --others --exclude-standard >"$untracked_paths"

  if [ -s "$untracked_paths" ]; then
    env GIT_INDEX_FILE="$tmp_index" \
      GIT_OBJECT_DIRECTORY="$object_dir" \
      GIT_ALTERNATE_OBJECT_DIRECTORIES="$alternates" \
      GIT_LITERAL_PATHSPECS=1 \
      git add --pathspec-from-file="$untracked_paths" --pathspec-file-nul
  fi

  env GIT_INDEX_FILE="$tmp_index" \
    GIT_OBJECT_DIRECTORY="$object_dir" \
    GIT_ALTERNATE_OBJECT_DIRECTORIES="$alternates" \
    git write-tree
)

GIT_DIR=$(canonical_dir "$(git rev-parse --git-dir)")
REPO_OBJECTS=$(canonical_dir "$(git rev-parse --git-path objects)")
REPO_KEY=$(hash_text "$REPO_ROOT
$GIT_DIR")
ID_KEY=$(hash_text "$SNAPSHOT_ID")

STATE_ROOT=${GIT_WORKSPACE_SNAPSHOT_DIR:-${TMPDIR:-/tmp}/git-workspace-snapshots}
STATE_DIR="$STATE_ROOT/$REPO_KEY"
STATE_FILE="$STATE_DIR/$ID_KEY.state"

mkdir -p "$STATE_DIR"

TMP_OBJECTS=
TMP_STATE=

cleanup() {
  if [ -n "${TMP_STATE:-}" ]; then
    rm -f "$TMP_STATE"
  fi
  if [ -n "${TMP_OBJECTS:-}" ]; then
    rm -rf "$TMP_OBJECTS"
  fi
}
trap cleanup EXIT

OLD_OBJECTS=$(read_state_field "$STATE_FILE" "objects")
TMP_OBJECTS=$(mktemp -d "$STATE_DIR/$ID_KEY.objects.XXXXXX")
TMP_STATE=$(mktemp "$STATE_DIR/$ID_KEY.state.XXXXXX")

TREE=$(build_workspace_tree "$TMP_OBJECTS" "$(join_alternates "$REPO_OBJECTS")")
OBJECTS_NAME=$(basename "$TMP_OBJECTS")

{
  printf 'version 1\n'
  printf 'tree %s\n' "$TREE"
  printf 'objects %s\n' "$OBJECTS_NAME"
  printf 'created_at %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
} >"$TMP_STATE"

mv -f "$TMP_STATE" "$STATE_FILE"
TMP_STATE=
TMP_OBJECTS=

if [ -n "$OLD_OBJECTS" ] && [ "$OLD_OBJECTS" != "$OBJECTS_NAME" ]; then
  case "$OLD_OBJECTS" in
    */* | *..* | '')
      ;;
    *)
      rm -rf "$STATE_DIR/$OLD_OBJECTS"
      ;;
  esac
fi

printf 'snapshot_tree=%s\n' "$TREE"
