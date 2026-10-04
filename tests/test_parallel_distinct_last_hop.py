"""A deduplicating parallel last hop drops repeated targets per block, then globally.

`push_distinct_into_match` lets the matcher keep one match per DISTINCT target
at the last hop. Above the parallel threshold the hop runs in blocks of
partial matches; each block drops targets it already produced before building
their matches, and one ordered pass drops the repeats across blocks. The rows
and their order must be those of the statement with the hint pass disabled
(every match built, deduplicated afterwards).
"""

from __future__ import annotations

import pytest

import kglite

HINT = "push_distinct_into_match"
FUSION = "fuse_chain_path_count"
A, M, B, C = 600, 3, 40, 60
FAN = 5  # each :B reaches FAN consecutive :C from 3*b, wrapping at C


@pytest.fixture(scope="module", params=["default", "mapped", "disk"])
def wide(request, tmp_path_factory):
    if request.param == "disk":
        graph = kglite.KnowledgeGraph(storage="disk", path=str(tmp_path_factory.mktemp("pdl") / "g.kgl"))
    else:
        graph = kglite.KnowledgeGraph(storage=request.param)
    for label, n in (("A", A), ("M", M), ("B", B), ("C", C)):
        graph.cypher(f"UNWIND range(0, {n - 1}) AS i CREATE (:{label} {{id: i}})").to_list()
    graph.cypher("MATCH (a:A), (m:M) CREATE (a)-[:R]->(m)").to_list()
    graph.cypher("MATCH (m:M), (b:B) CREATE (m)-[:S]->(b)").to_list()
    rows = [{"b": b, "c": (b * 3 + k) % C} for b in range(B) for k in range(FAN)]
    graph.cypher(
        "UNWIND $rows AS r MATCH (b:B {id: r.b}), (c:C {id: r.c}) CREATE (b)-[:T]->(c)",
        params={"rows": rows},
    ).to_list()
    return graph


CHAIN = "MATCH (a:A)-[:R]->(m:M)-[:S]->(b:B)-[:T]->(c:C) "

# Each statement must stay on the matcher (no chain fusion) so the parallel
# last hop is the code under test: a DISTINCT row list, and WHERE-guarded counts.
SHAPES = [
    (f"{CHAIN}RETURN DISTINCT c.id AS i", C),
    (f"{CHAIN}WHERE b.id >= 0 RETURN count(DISTINCT c) AS n", None),
    (f"{CHAIN}WHERE a.id >= 0 RETURN count(DISTINCT c) AS n, min(c.id) AS lo", None),
]


@pytest.mark.parametrize("query,rows", SHAPES)
def test_blockwise_dedup_keeps_the_rows_and_order_of_the_unhinted_path(wide, query, rows):
    hinted = wide.cypher(query).to_list()
    plain = wide.cypher(query, disabled_passes=[HINT, FUSION]).to_list()
    assert hinted == plain
    if rows is not None:
        assert len(hinted) == rows
        assert sorted(r["i"] for r in hinted) == list(range(C))
    else:
        assert hinted[0]["n"] == C


def test_the_match_count_is_above_the_parallel_threshold(wide):
    # 600 * 3 * 40 partial matches reach the last hop: more than 8192, and more
    # than eight dedup blocks, so a target repeats across blocks.
    n = wide.cypher(f"{CHAIN}WHERE a.id >= 0 RETURN count(*) AS n").to_list()[0]["n"]
    assert n == A * M * B * FAN
    assert A * M * B > 8 * 8192
