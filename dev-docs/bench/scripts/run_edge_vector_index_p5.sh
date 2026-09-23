#!/usr/bin/env bash
# Reproduce the P5 edge/node exact-versus-HNSW release measurement.
set -euo pipefail

usage() {
  echo "usage: $0 LABEL" >&2
  exit 2
}

[[ $# -eq 1 ]] || usage
label=$1
[[ "$label" =~ ^[a-z0-9][a-z0-9._-]{0,47}$ ]] || usage

root=$(cd "$(dirname "$0")/../../.." && pwd)
out_dir="$root/dev-docs/bench/out"
prefix="$out_dir/p5-edge-vector-index-$label"
filter=measure_edge_vector_index_matrix
header='run,entity,method,vectors,dimension,top_k,metric,mean_us,min_us,recall_at_k'
input_files=(
  Cargo.lock
  Cargo.toml
  dev-docs/bench/scripts/run_edge_vector_index_p5.sh
  crates/kglite/src/graph/edge_vector_index_perf_tests.rs
  crates/kglite/src/graph/edge_vector_index.rs
  crates/kglite/src/graph/algorithms/hnsw.rs
  crates/kglite/src/graph/algorithms/vector.rs
  crates/kglite/src/graph/edge_embeddings.rs
  crates/kglite/src/graph/embeddings.rs
)

mkdir -p "$out_dir"
for suffix in manifest.txt run1.raw.txt run1.csv run2.raw.txt run2.csv; do
  [[ ! -e "$prefix-$suffix" ]] || {
    echo "refusing to overwrite $prefix-$suffix" >&2
    exit 2
  }
done

cd "$root"
make check-free-space
for run in 1 2; do
  raw="$prefix-run$run.raw.txt"
  csv="$prefix-run$run.csv"
  KGLITE_BENCH_RUN="$run" cargo test -p kglite --release --lib "$filter" -- --ignored --nocapture >"$raw" 2>&1
  awk -v header="$header" '
    $0 == header { printing=1; print; next }
    printing && $0 ~ /^[12],(edge|node),(exact|ann),/ { print }
  ' "$raw" >"$csv"
  [[ $(head -n 1 "$csv") == "$header" ]]
  [[ $(wc -l <"$csv") -eq 5 ]]
done

status_sha256=$(git status --porcelain=v1 | shasum -a 256 | awk '{print $1}')
diff_sha256=$(git diff HEAD | shasum -a 256 | awk '{print $1}')
input_sha256=$(shasum -a 256 "${input_files[@]}" | shasum -a 256 | awk '{print $1}')
{
  echo 'purpose=P5 edge/node exact-versus-HNSW release performance measurement; not a correctness gate'
  echo 'profile=release'
  echo 'warmups=20'
  echo 'rounds=200'
  echo 'runs=2'
  echo 'timing_primary=mean and min reported; compare like-for-like entity controls'
  echo 'recall_oracle=ANN top-k overlap with exact top-k on seeded corpus'
  echo "head=$(git rev-parse HEAD)"
  echo "status_sha256=$status_sha256"
  echo "diff_sha256=$diff_sha256"
  echo "input_sha256=$input_sha256"
  printf 'input_files='
  printf '%s,' "${input_files[@]}"
  printf '\n'
  echo "command=KGLITE_BENCH_RUN=<1|2> cargo test -p kglite --release --lib $filter -- --ignored --nocapture"
  for run in 1 2; do
    echo "run${run}_raw=$prefix-run$run.raw.txt"
    echo "run${run}_csv=$prefix-run$run.csv"
  done
} >"$prefix-manifest.txt"

echo "manifest: $prefix-manifest.txt"
