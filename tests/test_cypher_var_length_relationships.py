"""Absolute goldens: a variable-length relationship variable is a list.

`MATCH (a)-[r:R*1..2]->(b)` binds `r` to the list of relationships the segment
walked, in path order (openCypher / Neo4j). It used to read as a path map, so
`size(r)` was null, `[x IN r | …]` was empty and `all(x IN r WHERE …)` was
vacuously true — a filter that silently kept every row. The unoptimised plan
shared that binding, so these are expected values, not a differential check.

The graph is `(a:A)-[:R {w:1}]->(b:B)-[:R {w:2}]->(c:B)`: from `a`, one
one-hop and one two-hop segment.
"""

from __future__ import annotations

import pytest

import kglite

SEGMENT = "MATCH (a:A)-[r:R*1..2]->(b)"


@pytest.fixture(scope="module")
def graph():
    g = kglite.KnowledgeGraph()
    g.cypher("CREATE (a:A {name:'a'})-[:R {w:1}]->(b:B {name:'b'})-[:R {w:2}]->(c:B {name:'c'})")
    return g


def _rows(graph, query, **kwargs):
    return graph.cypher(query, **kwargs).to_list()


def test_size(graph):
    rows = _rows(graph, f"{SEGMENT} RETURN b.name AS b, size(r) AS s, length(r) AS l ORDER BY b")
    assert rows == [{"b": "b", "s": 1, "l": 1}, {"b": "c", "s": 2, "l": 2}]


def test_list_comprehension(graph):
    rows = _rows(graph, f"{SEGMENT} RETURN b.name AS b, [x IN r | x.w] AS ws ORDER BY b")
    assert rows == [{"b": "b", "ws": [1]}, {"b": "c", "ws": [1, 2]}]


@pytest.mark.parametrize(
    "predicate,expected",
    [
        ("any(x IN r WHERE x.w = 2)", ["c"]),
        ("all(x IN r WHERE x.w > 5)", []),
        ("all(x IN r WHERE x.w = 1)", ["b"]),
        ("none(x IN r WHERE x.w = 2)", ["b"]),
        ("single(x IN r WHERE x.w = 1)", ["b", "c"]),
        ("size(r) = 2", ["c"]),
        ("r[0].w = 1 AND r[-1].w = 2", ["c"]),
        ("last(r).w = 2", ["c"]),
    ],
)
def test_where_over_the_list(graph, predicate, expected):
    rows = _rows(graph, f"{SEGMENT} WHERE {predicate} RETURN b.name AS b ORDER BY b")
    assert [row["b"] for row in rows] == expected


def test_return_r_is_a_list_of_relationships(graph):
    rows = _rows(graph, f"{SEGMENT} WHERE size(r) = 2 RETURN r")
    [row] = rows
    assert isinstance(row["r"], list)
    assert [(rel["type"], rel["properties"]["w"]) for rel in row["r"]] == [("R", 1), ("R", 2)]
    assert row["r"][0]["end"] == row["r"][1]["start"]


def test_return_star_carries_r(graph):
    rows = _rows(graph, f"{SEGMENT} WHERE size(r) = 1 RETURN *")
    [row] = rows
    assert sorted(row) == ["a", "b", "r"]
    assert [rel["properties"]["w"] for rel in row["r"]] == [1]


def test_with_carries_r(graph):
    rows = _rows(graph, f"{SEGMENT} WITH b, r WHERE size(r) = 2 RETURN b.name AS b, [x IN r | x.w] AS ws")
    assert rows == [{"b": "c", "ws": [1, 2]}]


def test_with_renames_r(graph):
    rows = _rows(graph, f"{SEGMENT} WITH r AS rels RETURN size(rels) AS s ORDER BY s")
    assert rows == [{"s": 1}, {"s": 2}]


def test_unwind_r(graph):
    rows = _rows(graph, f"{SEGMENT} WHERE size(r) = 2 UNWIND r AS x RETURN x.w AS w, type(x) AS t")
    assert rows == [{"w": 1, "t": "R"}, {"w": 2, "t": "R"}]


def test_collect_r(graph):
    rows = _rows(graph, f"{SEGMENT} RETURN collect(size(r)) AS sizes")
    assert sorted(rows[0]["sizes"]) == [1, 2]


def test_r_beside_a_path_variable(graph):
    rows = _rows(
        graph,
        "MATCH p=(a:A)-[r:R*1..2]->(b) WHERE size(r) = length(p) "
        "RETURN length(p) AS l, [x IN r | x.w] AS ws, [x IN relationships(p) | x.w] AS pws ORDER BY l",
    )
    assert rows == [{"l": 1, "ws": [1], "pws": [1]}, {"l": 2, "ws": [1, 2], "pws": [1, 2]}]


def test_path_variable_is_still_a_path(graph):
    rows = _rows(graph, "MATCH p=(a:A)-[r:R*2..2]->(b) RETURN p")
    [row] = rows
    assert [node["properties"]["name"] for node in row["p"]["nodes"]] == ["a", "b", "c"]
    assert [rel["properties"]["w"] for rel in row["p"]["relationships"]] == [1, 2]


def test_zero_length_segment_is_an_empty_list(graph):
    rows = _rows(graph, "MATCH (a:A)-[r:R*0..1]->(b) RETURN b.name AS b, r ORDER BY b")
    assert rows == [
        {"b": "a", "r": []},
        {"b": "b", "r": rows[1]["r"]},
    ]
    assert [rel["properties"]["w"] for rel in rows[1]["r"]] == [1]


def test_optional_match_miss_is_null(graph):
    rows = _rows(graph, "MATCH (c:B {name:'c'}) OPTIONAL MATCH (c)-[r:R*1..2]->(d) RETURN r IS NULL AS missing")
    assert rows == [{"missing": True}]


def test_undirected_segment_orders_relationships_along_the_walk(graph):
    rows = _rows(graph, "MATCH (c:B {name:'c'})-[r:R*2..2]-(a) RETURN [x IN r | x.w] AS ws")
    assert rows == [{"ws": [2, 1]}]


def test_unoptimised_plan_agrees(graph):
    query = f"{SEGMENT} WHERE all(x IN r WHERE x.w < 2) RETURN b.name AS b, size(r) AS s"
    assert _rows(graph, query) == _rows(graph, query, disable_optimizer=True) == [{"b": "b", "s": 1}]
