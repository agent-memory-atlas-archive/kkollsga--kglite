"""``WITH ... LIMIT k`` over a row-preserving projection stops the MATCH at k rows.

``push_limit_into_match`` used to recognise only ``MATCH ... RETURN ... LIMIT``.
A ``WITH`` between the MATCH and the LIMIT hid the cap, so
``MATCH (n) WITH n LIMIT 1 RETURN n`` materialised every node (and
every relationship or path for the other two forms) before keeping one.

The work bound is read from ``PROFILE`` (rows produced by the MATCH), not from
wall time. The goldens are exact counts and per-row value invariants: which
rows a bare LIMIT keeps is unspecified, so no assertion names one.

Bail shapes (aggregate, DISTINCT, ORDER BY) change the row count or order
before the cap, so they must still see every row; their goldens are the
values only an uncapped MATCH can produce.
"""

from __future__ import annotations

import pytest

import kglite

N = 40


@pytest.fixture(scope="module", params=["default", "mapped", "disk"])
def ring(request, tmp_path_factory):
    storage = request.param
    if storage == "disk":
        graph = kglite.KnowledgeGraph(storage="disk", path=str(tmp_path_factory.mktemp("ring") / "g"))
    else:
        graph = kglite.KnowledgeGraph(storage=storage)
    graph.cypher(f"UNWIND range(1, {N}) AS i CREATE (:P {{id: i, x: i % 4}})").to_list()
    # Every node has exactly one outgoing edge, so a later MATCH never drops a row.
    graph.cypher(
        f"UNWIND range(1, {N}) AS i MATCH (a:P {{id: i}}), (b:P {{id: i % {N} + 1}}) CREATE (a)-[:R]->(b)"
    ).to_list()
    return graph


def _match_rows(graph, query):
    profile = graph.cypher("PROFILE " + query).profile
    return next(e["rows_out"] for e in profile if e["clause"].startswith("Match"))


@pytest.mark.parametrize(
    ("query", "expected_rows"),
    [
        ("MATCH (n) WITH n LIMIT 1 RETURN n", 1),
        ("MATCH ()-[r]->() WITH r LIMIT 1 RETURN r", 1),
        ("MATCH p = ()-[]->() WITH p LIMIT 1 RETURN p", 1),
        ("MATCH (n:P) WITH n, n.x AS v LIMIT 5 RETURN n, v", 5),
        ("MATCH (n:P) WITH n.id AS id LIMIT 6 RETURN id", 6),
        ("MATCH (n:P) WHERE n.x = 2 WITH n LIMIT 4 RETURN n", 4),
    ],
)
def test_with_limit_caps_the_match(ring, query, expected_rows):
    assert len(ring.cypher(query).to_list()) == expected_rows
    assert _match_rows(ring, query) == expected_rows


def test_with_limit_values_are_the_projected_values(ring):
    rows = ring.cypher("MATCH (n:P) WITH n, n.x AS v LIMIT 5 RETURN n.id AS id, v").to_list()
    assert len(rows) == 5
    assert len({r["id"] for r in rows}) == 5
    assert all(1 <= r["id"] <= N and r["v"] == r["id"] % 4 for r in rows)


def test_with_skip_limit_counts(ring):
    assert len(ring.cypher("MATCH (n:P) WITH n SKIP 3 LIMIT 4 RETURN n").to_list()) == 4
    # SKIP + LIMIT past the end keeps only the tail.
    assert len(ring.cypher(f"MATCH (n:P) WITH n SKIP {N - 5} LIMIT 10 RETURN n").to_list()) == 5
    assert len(ring.cypher(f"MATCH (n:P) WITH n SKIP {N} LIMIT 10 RETURN n").to_list()) == 0
    assert _match_rows(ring, "MATCH (n:P) WITH n SKIP 3 LIMIT 4 RETURN n") == 7


def test_with_limit_zero_and_oversized(ring):
    assert ring.cypher("MATCH (n:P) WITH n LIMIT 0 RETURN n").to_list() == []
    assert len(ring.cypher(f"MATCH (n:P) WITH n LIMIT {N * 3} RETURN n").to_list()) == N


def test_with_limit_then_a_later_match(ring):
    q = "MATCH (n:P) WITH n LIMIT 3 MATCH (n)-[:R]->(m) RETURN n.id AS a, m.id AS b"
    rows = ring.cypher(q).to_list()
    assert len(rows) == 3
    assert len({r["a"] for r in rows}) == 3
    assert all(r["b"] == r["a"] % N + 1 for r in rows)
    assert _match_rows(ring, q) == 3


def test_with_limit_then_where_filters_after_the_cap(ring):
    q = "MATCH (n:P) WITH n LIMIT 4 WHERE n.x >= 0 RETURN n.id AS id"
    assert len(ring.cypher(q).to_list()) == 4
    # A filter that nothing passes leaves nothing, whatever the cap kept.
    assert ring.cypher("MATCH (n:P) WITH n LIMIT 4 WHERE n.x > 99 RETURN n").to_list() == []


def test_aggregate_with_still_sees_every_row(ring):
    rows = ring.cypher("MATCH (n:P) WITH n.x AS x, count(*) AS c LIMIT 2 RETURN x, c").to_list()
    assert len(rows) == 2
    assert all(r["c"] == N // 4 for r in rows)
    assert ring.cypher("MATCH (n:P) WITH count(*) AS c LIMIT 1 RETURN c").to_list() == [{"c": N}]


def test_distinct_with_still_sees_every_row(ring):
    # Four distinct x values among 40 nodes: a cap at 3 MATCH rows would give <= 3 anyway,
    # so ask for all four and require them.
    rows = ring.cypher("MATCH (n:P) WITH DISTINCT n.x AS x LIMIT 4 RETURN x").to_list()
    assert sorted(r["x"] for r in rows) == [0, 1, 2, 3]
    assert len(ring.cypher("MATCH (n:P) WITH DISTINCT n.x AS x LIMIT 3 RETURN x").to_list()) == 3


def test_order_by_with_keeps_top_k(ring):
    rows = ring.cypher("MATCH (n:P) WITH n ORDER BY n.id DESC LIMIT 3 RETURN n.id AS id").to_list()
    assert [r["id"] for r in rows] == [N, N - 1, N - 2]
    rows = ring.cypher("MATCH (n:P) WITH n.id AS id ORDER BY id SKIP 2 LIMIT 3 RETURN id").to_list()
    assert [r["id"] for r in rows] == [3, 4, 5]
