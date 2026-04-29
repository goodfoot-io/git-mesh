#!/usr/bin/env bash
# Scale benchmark for git-mesh operation latency across committed mesh counts,
# anchors-per-mesh, anchor mixes, and the downstream wiki coverage workload.
#
# This companion harness uses a synthetic repository so 100/1,000/10,000 mesh
# runs are reproducible without depending on a large external clone. It drives
# the git-mesh binary on PATH and writes a CSV plus a Markdown summary.

set -euo pipefail

MESH_COUNTS=(100 1000 10000)
ANCHORS_PER_MESH=(2 3 4 5)
EDGE_ANCHORS=()
ITERATIONS=3
WIKI_QUERIES=50
ANCHOR_MIX="mixed"
OUT_CSV="./bench-mesh-scale.csv"
KEEP_FIXTURE=0
RUN_MAINTENANCE=0
PACK_REFS=0

usage() {
  cat <<EOF
Usage: $0 [options]

  --iterations N          samples per operation cell (default: $ITERATIONS)
  --mesh-counts a,b,c     mesh-count tiers (default: ${MESH_COUNTS[*]})
  --anchors a,b,c         anchors-per-mesh tiers (default: ${ANCHORS_PER_MESH[*]})
  --edge                  include edge anchor tiers 10,20,120
  --anchor-mix MIX        line, whole, or mixed (default: $ANCHOR_MIX)
  --wiki-queries N        process-per-query wiki coverage samples (default: $WIKI_QUERIES)
  --pack-refs             run git pack-refs --all after seeding
  --maintenance           run selected git maintenance tasks after seeding
  --csv PATH              CSV output path (default: $OUT_CSV)
  --keep                  keep scratch repository for profiling
  -h, --help              this help

Env:
  GIT_MESH_BIN            git-mesh binary path (default: first git-mesh on PATH)
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --iterations) ITERATIONS="$2"; shift 2;;
    --mesh-counts) IFS=, read -r -a MESH_COUNTS <<<"$2"; shift 2;;
    --anchors) IFS=, read -r -a ANCHORS_PER_MESH <<<"$2"; shift 2;;
    --edge) EDGE_ANCHORS=(10 20 120); shift;;
    --anchor-mix) ANCHOR_MIX="$2"; shift 2;;
    --wiki-queries) WIKI_QUERIES="$2"; shift 2;;
    --pack-refs) PACK_REFS=1; shift;;
    --maintenance) RUN_MAINTENANCE=1; shift;;
    --csv) OUT_CSV="$2"; shift 2;;
    --keep) KEEP_FIXTURE=1; shift;;
    -h|--help) usage; exit 0;;
    *) echo "unknown option: $1" >&2; usage; exit 2;;
  esac
done

case "$ANCHOR_MIX" in
  line|whole|mixed) ;;
  *) echo "invalid --anchor-mix: $ANCHOR_MIX" >&2; exit 2;;
esac

if [[ ${#EDGE_ANCHORS[@]} -gt 0 ]]; then
  ANCHORS_PER_MESH+=("${EDGE_ANCHORS[@]}")
fi

GIT_MESH_BIN="${GIT_MESH_BIN:-$(command -v git-mesh || true)}"
[[ -n "$GIT_MESH_BIN" ]] || { echo "git-mesh not on PATH" >&2; exit 1; }
command -v git >/dev/null || { echo "git not on PATH" >&2; exit 1; }

WORK_ROOT="$(mktemp -d -t git-mesh-scale-XXXXXX)"
cleanup() {
  if [[ "$KEEP_FIXTURE" -eq 0 ]]; then
    rm -rf "$WORK_ROOT"
  else
    echo "kept scratch: $WORK_ROOT" >&2
  fi
}
trap cleanup EXIT

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
      printf "%d %.6f %.6f %.6f %.6f %.6f %.6f\n", n, sum / n, median, pct(0.95), pct(0.99), a[1], a[n]
    }'
}

timed() {
  local t0 t1
  t0=$(date +%s.%N)
  "$@" >/dev/null 2>&1
  t1=$(date +%s.%N)
  awk -v a="$t0" -v b="$t1" 'BEGIN { printf "%.6f", b - a }'
}

create_repo() {
  local repo="$1"
  mkdir -p "$repo"
  git -C "$repo" init --quiet
  git -C "$repo" config user.email bench@example.com
  git -C "$repo" config user.name "bench"
  mkdir -p "$repo/src" "$repo/docs/wiki"
  local i
  for (( i = 0; i < 256; i++ )); do
    printf -v file_suffix '%03d' "$i"
    {
      printf 'pub fn module_%03d() -> u32 {\n' "$i"
      local line
      for (( line = 1; line <= 240; line++ )); do
        printf '    let value_%03d_%03d = %d;\n' "$i" "$line" "$line"
      done
      printf '    0\n}\n'
    } >"$repo/src/module_${file_suffix}.rs"
    printf '# Wiki %03d\n\nFragment links for module %03d.\n' "$i" "$i" >"$repo/docs/wiki/page_${file_suffix}.md"
  done
  git -C "$repo" add .
  git -C "$repo" commit --quiet -m "seed synthetic scale repo"
}

anchor_for() {
  local mesh_idx="$1" anchor_idx="$2" anchors_per="$3"
  local file_idx=$(((mesh_idx + anchor_idx) % 256))
  if [[ "$ANCHOR_MIX" == "whole" ]]; then
    printf 'src/module_%03d.rs\n' "$file_idx"
    return
  fi
  if [[ "$ANCHOR_MIX" == "mixed" && "$anchor_idx" -eq 0 ]]; then
    printf 'docs/wiki/page_%03d.md\n' "$file_idx"
    return
  fi
  if [[ "$ANCHOR_MIX" == "mixed" && "$anchors_per" -gt 2 && "$anchor_idx" -eq $((anchors_per - 1)) ]]; then
    printf 'src/module_%03d.rs\n' "$file_idx"
    return
  fi
  local start=$((2 + ((mesh_idx + anchor_idx * 13) % 180)))
  local end=$((start + 4))
  printf 'src/module_%03d.rs#L%d-L%d\n' "$file_idx" "$start" "$end"
}

seed_meshes() {
  local repo="$1" meshes="$2" anchors_per="$3"
  local i j name
  for (( i = 0; i < meshes; i++ )); do
    name="scale-${i}"
    anchors=()
    for (( j = 0; j < anchors_per; j++ )); do
      anchors+=("$(anchor_for "$i" "$j" "$anchors_per")")
    done
    (
      cd "$repo"
      "$GIT_MESH_BIN" add "$name" "${anchors[@]}" >/dev/null
      "$GIT_MESH_BIN" why "$name" -m "Synthetic scale mesh $i" >/dev/null
      "$GIT_MESH_BIN" commit "$name" >/dev/null
    )
    if (( i > 0 && i % 1000 == 0 )); then
      echo "      seeded $i meshes" >&2
    fi
  done
}

run_maintenance_if_requested() {
  local repo="$1"
  if [[ "$PACK_REFS" -eq 1 ]]; then
    git -C "$repo" pack-refs --all
  fi
  if [[ "$RUN_MAINTENANCE" -eq 1 ]]; then
    git -C "$repo" maintenance run --task=loose-objects --task=incremental-repack --task=commit-graph --task=pack-refs
  fi
}

CSV_TMP="$(mktemp)"
echo "mesh_count,anchors_per_mesh,anchor_mix,layout,op,n,mean_s,median_s,p95_s,p99_s,min_s,max_s" >"$CSV_TMP"

echo "# git-mesh scale benchmark"
echo
echo "- binary: \`$GIT_MESH_BIN\`"
echo "- version: \`$("$GIT_MESH_BIN" --version 2>/dev/null || echo unknown)\`"
echo "- iterations: $ITERATIONS"
echo "- mesh counts: ${MESH_COUNTS[*]}"
echo "- anchors/mesh: ${ANCHORS_PER_MESH[*]}"
echo "- anchor mix: $ANCHOR_MIX"
echo "- wiki queries: $WIKI_QUERIES"
echo "- layout: $([[ "$PACK_REFS" -eq 1 ]] && echo packed-refs || echo loose-refs)$([[ "$RUN_MAINTENANCE" -eq 1 ]] && echo +maintenance || true)"
echo "- date: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
echo
printf "| meshes | anchors | mix | op | n | mean | median | p95 | p99 | min | max |\n"
printf "|---:|---:|---|---|---:|---:|---:|---:|---:|---:|---:|\n"

for M in "${MESH_COUNTS[@]}"; do
  for A in "${ANCHORS_PER_MESH[@]}"; do
    REPO="$WORK_ROOT/repo-${M}-${A}-${ANCHOR_MIX}"
    echo "==> preparing synthetic repo M=$M A=$A mix=$ANCHOR_MIX" >&2
    create_repo "$REPO"
    seed_meshes "$REPO" "$M" "$A"
    run_maintenance_if_requested "$REPO"

    if [[ "$ITERATIONS" -eq 0 ]]; then
      continue
    fi

    hit_anchor="$(anchor_for 0 1 "$A")"
    if [[ "$hit_anchor" != *"#L"* ]]; then
      hit_anchor="src/module_001.rs#L2-L6"
    fi
    miss_anchor="src/no-such-file.rs#L1-L3"
    show_name="scale-0"
    new_name="scale-new-${M}-${A}"
    layout="$([[ "$PACK_REFS" -eq 1 ]] && echo packed-refs || echo loose-refs)"
    [[ "$RUN_MAINTENANCE" -eq 1 ]] && layout="${layout}+maintenance"

    for op in add why commit show ls_all_porcelain ls_filtered_hit ls_filtered_miss stale pre_commit wiki_hit_queries wiki_miss_queries; do
      samples=()
      for (( it = 0; it < ITERATIONS; it++ )); do
        case "$op" in
          add)
            ( cd "$REPO" && "$GIT_MESH_BIN" restore "${new_name}-${it}" >/dev/null 2>&1 || true )
            t=$(cd "$REPO" && timed "$GIT_MESH_BIN" add "${new_name}-${it}" "$(anchor_for "$it" 0 "$A")" "$(anchor_for "$it" 1 "$A")")
            ;;
          why)
            ( cd "$REPO" && "$GIT_MESH_BIN" add "${new_name}-${it}" "$(anchor_for "$it" 0 "$A")" >/dev/null 2>&1 || true )
            t=$(cd "$REPO" && timed "$GIT_MESH_BIN" why "${new_name}-${it}" -m "bench why")
            ;;
          commit)
            (
              cd "$REPO"
              "$GIT_MESH_BIN" restore "${new_name}-${it}" >/dev/null 2>&1 || true
              "$GIT_MESH_BIN" add "${new_name}-${it}" "$(anchor_for "$it" 0 "$A")" "$(anchor_for "$it" 1 "$A")" >/dev/null
              "$GIT_MESH_BIN" why "${new_name}-${it}" -m "bench why" >/dev/null
            )
            t=$(cd "$REPO" && timed "$GIT_MESH_BIN" commit "${new_name}-${it}")
            ;;
          show)
            t=$(cd "$REPO" && timed "$GIT_MESH_BIN" show "$show_name")
            ;;
          ls_all_porcelain)
            t=$(cd "$REPO" && timed "$GIT_MESH_BIN" ls --porcelain)
            ;;
          ls_filtered_hit)
            t=$(cd "$REPO" && timed "$GIT_MESH_BIN" ls "$hit_anchor" --porcelain)
            ;;
          ls_filtered_miss)
            t=$(cd "$REPO" && timed "$GIT_MESH_BIN" ls "$miss_anchor" --porcelain)
            ;;
          stale)
            t=$(cd "$REPO" && timed "$GIT_MESH_BIN" stale --no-exit-code)
            ;;
          pre_commit)
            t=$(cd "$REPO" && timed "$GIT_MESH_BIN" pre-commit --no-exit-code)
            ;;
          wiki_hit_queries)
            t0=$(date +%s.%N)
            for (( q = 0; q < WIKI_QUERIES; q++ )); do
              query="$(anchor_for "$q" 1 "$A")"
              [[ "$query" == *"#L"* ]] || query="$(printf 'src/module_%03d.rs#L2-L6' $(((q + 1) % 256)))"
              ( cd "$REPO" && "$GIT_MESH_BIN" ls "$query" --porcelain >/dev/null )
            done
            t1=$(date +%s.%N)
            t=$(awk -v a="$t0" -v b="$t1" 'BEGIN { printf "%.6f", b - a }')
            ;;
          wiki_miss_queries)
            t0=$(date +%s.%N)
            for (( q = 0; q < WIKI_QUERIES; q++ )); do
              ( cd "$REPO" && "$GIT_MESH_BIN" ls "src/missing_$q.rs#L1-L3" --porcelain >/dev/null )
            done
            t1=$(date +%s.%N)
            t=$(awk -v a="$t0" -v b="$t1" 'BEGIN { printf "%.6f", b - a }')
            ;;
        esac
        samples+=("$t")
      done

      read -r n mean median p95 p99 min max < <(stats "${samples[@]}")
      printf "%d,%d,%s,%s,%s,%d,%s,%s,%s,%s,%s,%s\n" \
        "$M" "$A" "$ANCHOR_MIX" "$layout" "$op" "$n" "$mean" "$median" "$p95" "$p99" "$min" "$max" >>"$CSV_TMP"
      printf "| %d | %d | %s | %s | %d | %.4f | %.4f | %.4f | %.4f | %.4f | %.4f |\n" \
        "$M" "$A" "$ANCHOR_MIX" "$op" "$n" "$mean" "$median" "$p95" "$p99" "$min" "$max"
    done
  done
done

mv "$CSV_TMP" "$OUT_CSV"
echo
echo "CSV written to $OUT_CSV"
