"""A pattern starts at its point-anchored end, whichever end that is.

`(:Hub)-[:CLAIMS]->(:Doc {id: e.id})` under `UNWIND` used to start at the
hub and walk every hub edge per input row: the planner read `{id: e.id}` as
barely selective and a one-node hub type tied even a constant id. The route
is pinned through EXPLAIN (the MATCH step lists its node labels in match
order); the answers are pinned as absolute values so a shared executor
defect cannot hide behind the differential corpus.
"""

from __future__ import annotations

import pytest

import kglite


@pytest.fixture
def graph() -> kglite.KnowledgeGraph:
    # Hub 0 claims docs 10..19 (w = id); Other 1 claims docs 12, 15, 18 (w = -id).
    g = kglite.KnowledgeGraph()
    g.cypher(
        "CREATE (h:Hub {id: 0}), (o:Other {id: 1}) "
        "WITH h, o UNWIND range(10, 19) AS i CREATE (d:Doc {id: i}) "
        "CREATE (h)-[:CLAIMS {w: i}]->(d) "
        "FOREACH (_ IN CASE WHEN i % 3 = 0 THEN [1] ELSE [] END | "
        "CREATE (o)-[:CLAIMS {w: -i}]->(d))"
    ).to_list()
    return g


def _steps(g: kglite.KnowledgeGraph, query: str, params: dict | None = None) -> list[str]:
    return [step["operation"] for step in g.cypher("EXPLAIN " + query, params=params).to_list()]


def _rows(g: kglite.KnowledgeGraph, query: str, params: dict | None = None) -> list[tuple]:
    return sorted(tuple(row.values()) for row in g.cypher(query, params=params).to_list())


BATCH = {"batch": [{"id": 12}, {"id": 15}, {"id": 99}]}

# (query, params, the MATCH step EXPLAIN must show, expected rows)
ANCHORED = [
    pytest.param(
        "UNWIND $batch AS e MATCH (:Hub)-[r:CLAIMS]->(d:Doc {id: e.id}) RETURN d.id, r.w",
        BATCH,
        "Match :Doc, :Hub",
        [(12, 12), (15, 15)],
        id="unwind-map-member",
    ),
    pytest.param(
        "UNWIND $batch AS e MATCH (d:Doc {id: e.id})<-[r:CLAIMS]-(:Hub) RETURN d.id, r.w",
        BATCH,
        "Match :Doc, :Hub",
        [(12, 12), (15, 15)],
        id="written-anchor-first",
    ),
    pytest.param(
        "UNWIND $ids AS x MATCH (:Hub)-[r:CLAIMS]->(d:Doc {id: x}) RETURN d.id, r.w",
        {"ids": [11, 99]},
        "Match :Doc, :Hub",
        [(11, 11)],
        id="unwind-scalar",
    ),
    pytest.param(
        "MATCH (:Hub)-[r:CLAIMS]->(d:Doc {id: $x}) RETURN d.id, r.w",
        {"x": 13},
        "Match :Doc, :Hub",
        [(13, 13)],
        id="param",
    ),
    pytest.param(
        "WITH 14 AS x MATCH (:Hub)-[r:CLAIMS]->(d:Doc {id: x}) RETURN d.id, r.w",
        None,
        "Match :Doc, :Hub",
        [(14, 14)],
        id="with-bound",
    ),
    pytest.param(
        "MATCH (:Hub)-[r:CLAIMS]->(d:Doc {id: 16}) RETURN d.id, r.w",
        None,
        "Match :Doc, :Hub",
        [(16, 16)],
        id="constant",
    ),
    pytest.param(
        "UNWIND [12, 13, 99] AS x OPTIONAL MATCH (:Other)-[r:CLAIMS]->(d:Doc {id: x}) RETURN x, r.w",
        None,
        "OptionalMatch :Doc, :Other",
        [(12, -12), (13, None), (99, None)],
        id="optional-match",
    ),
    pytest.param(
        "UNWIND [99] AS x MATCH (:Hub)-[r:CLAIMS]->(d:Doc {id: x}) RETURN d.id, r.w",
        None,
        "Match :Doc, :Hub",
        [],
        id="no-match-no-row",
    ),
]


@pytest.mark.parametrize(("query", "params", "match_step", "expected"), ANCHORED)
def test_point_anchored_end_starts_the_pattern(graph, query, params, match_step, expected):
    assert match_step in _steps(graph, query, params)
    assert _rows(graph, query, params) == expected


def test_bound_endpoint_starts_a_later_clause(graph):
    query = "UNWIND $batch AS e MATCH (d:Doc {id: e.id}) MATCH (h:Hub)-[r:CLAIMS]->(d) RETURN d.id, r.w"
    # The later clause's pattern is reversed to start at the bound `d`.
    assert "OptimizerPass optimize_pattern_start_node" in _steps(graph, query, BATCH)
    assert _rows(graph, query, BATCH) == [(12, 12), (15, 15)]


def test_untyped_far_end_keeps_every_claimant(graph):
    query = "UNWIND [12, 13] AS x MATCH (h)-[r:CLAIMS]->(d:Doc {id: x}) RETURN d.id, h.id, r.w"
    assert _rows(graph, query) == [(12, 0, 12), (12, 1, -12), (13, 0, 13)]


def test_bulk_set_through_the_anchored_route(graph):
    graph.cypher(
        "UNWIND $rows AS e MATCH (:Hub)-[r:CLAIMS]->(d:Doc {id: e.id}) SET r.tag = e.tag, d.seen = true",
        params={"rows": [{"id": 10, "tag": "a"}, {"id": 18, "tag": "b"}, {"id": 99, "tag": "z"}]},
    ).to_list()
    assert _rows(graph, "MATCH (h)-[r:CLAIMS]->(d:Doc) WHERE r.tag IS NOT NULL RETURN h.id, d.id, r.tag") == [
        (0, 10, "a"),
        (0, 18, "b"),
    ]
    assert _rows(graph, "MATCH (d:Doc) WHERE d.seen RETURN d.id") == [(10,), (18,)]
