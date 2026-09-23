#!/usr/bin/env bash
# Reproduce the P3 relationship-embedding WAL release measurements.
# This runner records performance evidence only; it is not a correctness gate.
set -euo pipefail

usage() {
  echo "usage: $0 LABEL" >&2
  echo "writes bounded raw logs, extracted CSV, and a manifest under dev-docs/bench/out" >&2
  exit 2
}

[[ $# -eq 1 ]] || usage
label=$1
[[ "$label" =~ ^[a-z0-9][a-z0-9._-]{0,47}$ ]] || usage

root=$(cd "$(dirname "$0")/../../.." && pwd)
out_dir="$root/dev-docs/bench/out"
prefix="$out_dir/p3-edge-wal-$label"
mkdir -p "$out_dir"

codec_filter=measure_edge_wal_delta_matrix
capture_filter=measure_edge_wal_capture_boundary
codec_header='kind,members,dimension,variant,bytes,statistic,rounds,warmups,value_ns,durability'
capture_header='members,dimension,variant,bytes,statistic,rounds,warmups,value_ns'
input_files=(
  Cargo.lock
  Cargo.toml
  dev-docs/bench/scripts/run_edge_wal_p3.sh
  crates/kglite/src/graph/edge_embedding_wal_perf_tests.rs
  crates/kglite/src/graph/edge_embedding_wal_capture_perf_tests.rs
  crates/kglite/src/graph/edge_embeddings.rs
  crates/kglite/src/graph/mutation/wal_replay/edge_embedding_delta.rs
  crates/kglite/src/graph/mutation/wal_replay.rs
  crates/kglite/src/graph/storage/recording.rs
  crates/kglite/src/graph/storage/recording/wal_capture.rs
  crates/kglite/src/graph/wal.rs
  crates/kglite/src/graph/wal_edge_embeddings.rs
)

for suffix in manifest.txt codec-run1.raw.txt codec-run1.csv codec-run2.raw.txt codec-run2.csv capture-run1.raw.txt capture-run1.csv capture-run2.raw.txt capture-run2.csv; do
  [[ ! -e "$prefix-$suffix" ]] || {
    echo "refusing to overwrite $prefix-$suffix" >&2
    exit 2
  }
done

cd "$root"
make check-free-space

run_harness() {
  local filter=$1
  local header=$2
  local stem=$3
  local raw="$prefix-$stem.raw.txt"
  local csv="$prefix-$stem.csv"
  local -a command=(cargo test -p kglite --release --lib "$filter" -- --ignored --nocapture)
  printf 'running:'
  printf ' %q' "${command[@]}"
  printf '\nraw: %s\ncsv: %s\n' "$raw" "$csv"
  "${command[@]}" >"$raw" 2>&1
  awk -v header="$header" '
    $0 == header { printing=1 }
    printing && ($0 == header || $0 ~ /^[a-z0-9_]+,[0-9]+,[0-9]+,/ || $0 ~ /^[0-9]+,[0-9]+,/) { print }
  ' "$raw" >"$csv"
  [[ $(head -n 1 "$csv") == "$header" ]]
  [[ $(wc -l <"$csv") -gt 1 ]]
}

for run in 1 2; do
  run_harness "$codec_filter" "$codec_header" "codec-run$run"
  run_harness "$capture_filter" "$capture_header" "capture-run$run"
done

status_sha256=$(git status --porcelain=v1 | shasum -a 256 | awk '{print $1}')
diff_sha256=$(git diff HEAD | shasum -a 256 | awk '{print $1}')
input_sha256=$(shasum -a 256 "${input_files[@]}" | shasum -a 256 | awk '{print $1}')
{
  echo 'purpose=P3 relationship-embedding WAL release performance measurement; not a correctness gate'
  echo 'profile=release'
  echo 'warmups=20'
  echo 'rounds=200'
  echo 'runs=2'
  echo 'codec_primary=encode min; append mean'
  echo 'capture_primary=mean; min retained as diagnostic'
  echo "head=$(git rev-parse HEAD)"
  echo "status_sha256=$status_sha256"
  echo "diff_sha256=$diff_sha256"
  echo "input_sha256=$input_sha256"
  printf 'input_files='
  printf '%s,' "${input_files[@]}"
  printf '\n'
  echo "codec_command=cargo test -p kglite --release --lib $codec_filter -- --ignored --nocapture"
  echo "capture_command=cargo test -p kglite --release --lib $capture_filter -- --ignored --nocapture"
  for run in 1 2; do
    echo "codec_run${run}_raw=$prefix-codec-run$run.raw.txt"
    echo "codec_run${run}_csv=$prefix-codec-run$run.csv"
    echo "capture_run${run}_raw=$prefix-capture-run$run.raw.txt"
    echo "capture_run${run}_csv=$prefix-capture-run$run.csv"
  done
} >"$prefix-manifest.txt"

echo "manifest: $prefix-manifest.txt"
