"""Path-binding cells: what binding `p`, and reading `[r*]` as a list, cost.

Outside the frozen core harness, because CI benchmarks `test_bench_core.py` on
the published 0.13.2 wheel with `--require-exact-set`: a new core cell needs a
versioned capture on both platforms first, and the path-reading twins return
different answers on releases that ran the fused WHERE before `p` was bound.

The leading MATCH binds `p` per row before its fused WHERE only when the WHERE
may read `p`. The path-free cell is the one that guards that decision: a
1%-selective predicate on a path-assigned pattern, where binding early would
build a path for the 99% of rows the predicate rejects. The twin reads `p` and
so must bind; the control has no path at all.
"""

import pandas as pd
import pytest

from kglite import KnowledgeGraph

pytestmark = pytest.mark.benchmark

NODES = 20_000
FANOUT = 3
SELECTED = NODES * FANOUT // 100


@pytest.fixture(scope="module")
def link_graph():
    graph = KnowledgeGraph()
    graph.add_nodes(pd.DataFrame({"nid": range(NODES), "x": range(NODES)}), "Item", "nid", "nid")
    src = [i for i in range(NODES) for _ in range(FANOUT)]
    dst = [(i * 7 + j * 13) % NODES for i in range(NODES) for j in range(1, FANOUT + 1)]
    graph.add_connections(pd.DataFrame({"src": src, "dst": dst}), "LINKS", "Item", "src", "Item", "dst")
    return graph


def _count(graph, query):
    return graph.cypher(query).to_list()[0]["n"]


def test_bench_path_where_path_free(benchmark, link_graph):
    """`p` assigned, the fused WHERE never reads it: no early path per row."""
    query = "MATCH p=(a:Item)-[:LINKS]->(b:Item) WHERE a.x % 100 = 7 RETURN count(*) AS n"
    assert benchmark(_count, link_graph, query) == SELECTED


def test_bench_path_where_reads_path(benchmark, link_graph):
    """The twin: the fused WHERE reads `p`, so every row binds it first."""
    query = "MATCH p=(a:Item)-[:LINKS]->(b:Item) WHERE a.x % 100 = 7 AND length(p) = 1 RETURN count(*) AS n"
    assert benchmark(_count, link_graph, query) == SELECTED


def test_bench_path_where_control_no_path(benchmark, link_graph):
    """Control: the same MATCH and WHERE with no path variable."""
    query = "MATCH (a:Item)-[:LINKS]->(b:Item) WHERE a.x % 100 = 7 RETURN count(*) AS n"
    assert benchmark(_count, link_graph, query) == SELECTED


# A variable-length relationship variable reads as a list of relationships,
# built only when the variable is read. The named-but-unread cell must cost
# what the anonymous control costs; the twin reads `r` and builds every list.
SEGMENT_SEEDS = 200
SEGMENT_PATHS = SEGMENT_SEEDS * (FANOUT + FANOUT**2 + FANOUT**3)


def test_bench_var_length_named_unread(benchmark, link_graph):
    query = f"MATCH (a:Item)-[r:LINKS*1..3]->(b:Item) WHERE a.x < {SEGMENT_SEEDS} RETURN count(*) AS n"
    assert benchmark(_count, link_graph, query) == SEGMENT_PATHS


def test_bench_var_length_reads_list(benchmark, link_graph):
    query = f"MATCH (a:Item)-[r:LINKS*1..3]->(b:Item) WHERE a.x < {SEGMENT_SEEDS} RETURN sum(size(r)) AS n"
    assert benchmark(_count, link_graph, query) == SEGMENT_SEEDS * (FANOUT + 2 * FANOUT**2 + 3 * FANOUT**3)


def test_bench_var_length_anonymous_control(benchmark, link_graph):
    query = f"MATCH (a:Item)-[:LINKS*1..3]->(b:Item) WHERE a.x < {SEGMENT_SEEDS} RETURN count(*) AS n"
    assert benchmark(_count, link_graph, query) == SEGMENT_PATHS
