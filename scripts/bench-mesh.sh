#!/usr/bin/env bash
# Benchmark `git mesh` operation latency across repo size, mesh count, and
# anchors-per-mesh. Drives the `git-mesh` binary on the user's PATH against
# a locally cached full clone of vercel/next.js.
#
# Outputs a markdown summary to stdout and a CSV file alongside (default
# ./bench-mesh.csv). Per cell we collect N raw timings and report
# mean / median / p95 / p99 / min / max.

set -euo pipefail

# ----------------------------- Configuration ----------------------------------

CACHE_DIR="${BENCH_CACHE_DIR:-/tmp/git-mesh-bench-cache}"
NEXTJS_URL="${NEXTJS_URL:-https://github.com/vercel/next.js.git}"
NEXTJS_BARE="$CACHE_DIR/next.js.git"

# Three repo-size tiers via Next.js refs (small / medium / large tree).
REFS=(
  "v9.0.0"
  "v13.0.0"
  "canary"
)

# Sweep axes. Override with --mesh-counts, --anchors, --iterations, --full.
MESH_COUNTS=(1 10 100)
ANCHORS_PER_MESH=(2 20)
ITERATIONS=5
OUT_CSV="./bench-mesh.csv"
OPS=(add why commit show list stale)
KEEP_FIXTURE=0

usage() {
  cat <<EOF
Usage: $0 [options]

  --iterations N       samples per cell (default: $ITERATIONS)
  --mesh-counts a,b,c  mesh-count tiers (default: ${MESH_COUNTS[*]})
  --anchors a,b,c      anchors-per-mesh tiers (default: ${ANCHORS_PER_MESH[*]})
  --refs a,b,c         git refs to use as repo-size tiers (default: ${REFS[*]})
  --full               extends defaults to mesh-counts=1,10,100,1000 anchors=2,20,200
  --csv PATH           CSV output path (default: $OUT_CSV)
  --keep               keep per-iteration scratch worktrees (debug)
  -h, --help           this help

Env:
  BENCH_CACHE_DIR  cache root (default: $CACHE_DIR)
  NEXTJS_URL       upstream URL (default: $NEXTJS_URL)
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --iterations)  ITERATIONS="$2"; shift 2;;
    --mesh-counts) IFS=, read -r -a MESH_COUNTS <<<"$2"; shift 2;;
    --anchors)     IFS=, read -r -a ANCHORS_PER_MESH <<<"$2"; shift 2;;
    --refs)        IFS=, read -r -a REFS <<<"$2"; shift 2;;
    --full)        MESH_COUNTS=(1 10 100 1000); ANCHORS_PER_MESH=(2 20 200); shift;;
    --csv)         OUT_CSV="$2"; shift 2;;
    --keep)        KEEP_FIXTURE=1; shift;;
    -h|--help)     usage; exit 0;;
    *) echo "unknown option: $1" >&2; usage; exit 2;;
  esac
done

command -v git-mesh >/dev/null || { echo "git-mesh not on PATH" >&2; exit 1; }
command -v git      >/dev/null || { echo "git not on PATH"      >&2; exit 1; }

GIT_MESH_BIN="$(command -v git-mesh)"
echo "# git-mesh benchmark"
echo
echo "- binary:        \`$GIT_MESH_BIN\`"
echo "- version:       \`$(git-mesh --version 2>/dev/null || echo unknown)\`"
echo "- cache:         \`$CACHE_DIR\`"
echo "- iterations:    $ITERATIONS"
echo "- refs:          ${REFS[*]}"
echo "- mesh counts:   ${MESH_COUNTS[*]}"
echo "- anchors/mesh:  ${ANCHORS_PER_MESH[*]}"
echo "- operations:    ${OPS[*]}"
echo "- date:          $(date -u +%Y-%m-%dT%H:%M:%SZ)"
echo

# ----------------------------- Fixture management -----------------------------

mkdir -p "$CACHE_DIR"

if [[ ! -d "$NEXTJS_BARE" ]]; then
  echo "Cloning Next.js (full history) — this may take several minutes…" >&2
  git clone --bare "$NEXTJS_URL" "$NEXTJS_BARE" >&2
else
  echo "Updating Next.js cache…" >&2
  git --git-dir="$NEXTJS_BARE" remote update --prune >&2 || true
fi

# Pre-resolve refs to SHAs (so canary is pinned for the whole run).
declare -A REF_SHA
for ref in "${REFS[@]}"; do
  if sha=$(git --git-dir="$NEXTJS_BARE" rev-parse --verify "$ref^{commit}" 2>/dev/null); then
    REF_SHA["$ref"]="$sha"
  else
    echo "warning: ref $ref not found, skipping" >&2
  fi
done

# ------------------------------ Stats helper ----------------------------------

# stats <space-sep-floats>  ->  "n mean median p95 p99 min max"
stats() {
  printf '%s\n' "$@" | sort -n | awk '
    function pct(p,    rank, lo, hi, frac) {
      if (n == 1) return a[1]
      rank = p * (n - 1) + 1
      lo = int(rank); hi = lo + 1; frac = rank - lo
      if (hi > n) hi = n
      return a[lo] + frac * (a[hi] - a[lo])
    }
    { n++; a[n] = $1; sum += $1 }
    END {
      if (n == 0) { print "0 0 0 0 0 0 0"; exit }
      mean = sum / n
      median = (n % 2) ? a[int((n+1)/2)] : (a[n/2] + a[n/2+1]) / 2
      printf "%d %.6f %.6f %.6f %.6f %.6f %.6f\n", n, mean, median, pct(0.95), pct(0.99), a[1], a[n]
    }'
}

# Time a command in seconds with sub-millisecond resolution.
timed() {
  local t0 t1
  t0=$(date +%s.%N)
  "$@" >/dev/null 2>&1
  t1=$(date +%s.%N)
  awk -v a="$t0" -v b="$t1" 'BEGIN { printf "%.6f", b - a }'
}

# --------------------------- Worktree per (ref) -------------------------------

WORK_ROOT="$(mktemp -d -t git-mesh-bench-XXXXXX)"
cleanup() {
  if [[ "$KEEP_FIXTURE" -eq 0 ]]; then
    rm -rf "$WORK_ROOT"
  else
    echo "kept scratch: $WORK_ROOT" >&2
  fi
}
trap cleanup EXIT

# Pick A real anchors (line-range or whole-file) from the working tree.
gen_anchors() {
  local count="$1" workdir="$2"
  # Shuffle a stable list of tracked source files; take `count` of them.
  ( cd "$workdir" && git ls-files '*.js' '*.ts' '*.tsx' '*.jsx' '*.md' 2>/dev/null \
      | head -n 5000 | awk 'BEGIN{srand(42)} {print rand()"\t"$0}' \
      | sort | cut -f2- | head -n "$count" )
}

# Init the mesh refs/dirs that git-mesh expects in a worktree.
mesh_init_in() {
  local workdir="$1"
  ( cd "$workdir" && git config --local user.email bench@example.com \
      && git config --local user.name "bench" )
}

# Pre-populate (M-1) meshes (untimed setup) so reader ops have something to scan.
seed_meshes() {
  local workdir="$1" count="$2" anchors_per="$3"
  local i
  for (( i = 0; i < count; i++ )); do
    local name="seed-${i}"
    mapfile -t anchors < <(gen_anchors "$anchors_per" "$workdir")
    [[ ${#anchors[@]} -gt 0 ]] || continue
    ( cd "$workdir" \
        && git-mesh add "$name" "${anchors[@]}" >/dev/null 2>&1 \
        && git-mesh why "$name" -m "Seed mesh $i for benchmark" >/dev/null 2>&1 \
        && git-mesh commit "$name" >/dev/null 2>&1 ) || true
  done
}

# ----------------------------- Main sweep -------------------------------------

CSV_TMP="$(mktemp)"
echo "repo_ref,repo_sha,mesh_count,anchors_per_mesh,op,n,mean_s,median_s,p95_s,p99_s,min_s,max_s" >"$CSV_TMP"

# Markdown header
printf "| ref | meshes | anchors | op | n | mean | median | p95 | p99 | min | max |\n"
printf "|---|---:|---:|---|---:|---:|---:|---:|---:|---:|---:|\n"

for ref in "${REFS[@]}"; do
  sha="${REF_SHA[$ref]:-}"
  [[ -n "$sha" ]] || continue

  # One scratch worktree per ref, reused across (mesh_count × anchors) cells.
  WORKDIR="$WORK_ROOT/$ref"
  echo "==> preparing worktree for $ref ($sha)" >&2
  rm -rf "$WORKDIR"
  git clone --shared --no-local-tags --quiet "$NEXTJS_BARE" "$WORKDIR" >&2 2>/dev/null \
    || git clone --quiet "$NEXTJS_BARE" "$WORKDIR" >&2
  ( cd "$WORKDIR" && git checkout --quiet --detach "$sha" )
  mesh_init_in "$WORKDIR"

  for M in "${MESH_COUNTS[@]}"; do
    for A in "${ANCHORS_PER_MESH[@]}"; do
      # Fresh mesh state for each (M, A) combo.
      ( cd "$WORKDIR" && rm -rf .git/mesh .git/refs/mesh .git/packed-refs.lock 2>/dev/null || true )
      ( cd "$WORKDIR" && git for-each-ref --format='%(refname)' refs/mesh \
          | xargs -r -n1 git update-ref -d 2>/dev/null || true )

      echo "    ref=$ref M=$M A=$A — seeding $((M > 0 ? M : 0)) meshes" >&2
      if [[ "$M" -gt 0 ]]; then
        seed_meshes "$WORKDIR" "$M" "$A"
        seeded=$(cd "$WORKDIR" && git-mesh list --porcelain 2>/dev/null | awk '{print $1}' | sort -u | wc -l)
        if [[ "$seeded" -lt "$M" ]]; then
          echo "FATAL: seeded only $seeded of $M meshes (ref=$ref M=$M A=$A)" >&2
          exit 1
        fi
      fi

      # Cache one anchor list to reuse across iterations of write-ops.
      mapfile -t bench_anchors < <(gen_anchors "$A" "$WORKDIR")
      [[ ${#bench_anchors[@]} -gt 0 ]] || { echo "no anchors found, skipping" >&2; continue; }

      # Pick an existing seeded mesh name for readers (`show`).
      reader_name="seed-0"

      for op in "${OPS[@]}"; do
        samples=()
        for (( it = 0; it < ITERATIONS; it++ )); do
          name="bench-${M}-${A}-${it}"
          case "$op" in
            add)
              ( cd "$WORKDIR" && git-mesh restore "$name" >/dev/null 2>&1 || true )
              t=$(cd "$WORKDIR" && timed git-mesh add "$name" "${bench_anchors[@]}")
              ;;
            why)
              ( cd "$WORKDIR" && git-mesh add "$name" "${bench_anchors[@]}" >/dev/null 2>&1 || true )
              t=$(cd "$WORKDIR" && timed git-mesh why "$name" -m "bench why for $name")
              ;;
            commit)
              ( cd "$WORKDIR" \
                  && git-mesh add "$name" "${bench_anchors[@]}" >/dev/null 2>&1 \
                  && git-mesh why "$name" -m "bench why" >/dev/null 2>&1 ) || true
              t=$(cd "$WORKDIR" && timed git-mesh commit "$name")
              ;;
            show)
              t=$(cd "$WORKDIR" && timed git-mesh show "$reader_name")
              ;;
            list)
              t=$(cd "$WORKDIR" && timed git-mesh list)
              ;;
            stale)
              t=$(cd "$WORKDIR" && timed git-mesh stale --no-exit-code)
              ;;
          esac
          samples+=("$t")
        done

        read -r n mean median p95 p99 min max < <(stats "${samples[@]}")
        printf "%s,%s,%d,%d,%s,%d,%s,%s,%s,%s,%s,%s\n" \
          "$ref" "$sha" "$M" "$A" "$op" "$n" "$mean" "$median" "$p95" "$p99" "$min" "$max" \
          >>"$CSV_TMP"
        printf "| %s | %d | %d | %s | %d | %.4f | %.4f | %.4f | %.4f | %.4f | %.4f |\n" \
          "$ref" "$M" "$A" "$op" "$n" "$mean" "$median" "$p95" "$p99" "$min" "$max"
      done
    done
  done
done

mv "$CSV_TMP" "$OUT_CSV"
echo
echo "CSV written to $OUT_CSV"
