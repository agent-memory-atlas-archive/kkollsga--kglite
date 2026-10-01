#!/bin/bash
cd "$(dirname "$0")"; P=..
for w in cand v019 v019 cand; do
  n=$((++i)); echo "# $(date +%H:%M:%S) $w run$n $(uptime | sed 's/.*load/load/')" >> state.log
  $P/$w/bin/python -m pytest test_bench_core.py -m benchmark -p no:cacheprovider -o addopts="" -k "return_node_10k or two_edge_relationship_text_filter or two_edge_distinct_filtered_path or return_id_10k or test_bench_traversal or var_length_1_8" \
    --benchmark-min-rounds=100 --benchmark-warmup=on --benchmark-warmup-iterations=20 --benchmark-json=ab_${w}_$n.json -q > ab_${w}_$n.log 2>&1
  echo "$w run$n rc=$?" >> state.log
done
echo finished > ab.done
