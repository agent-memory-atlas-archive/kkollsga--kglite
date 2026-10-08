"""MERGE on a declared-unique key: one row, no creation on a hit, a node on a miss.

The early exit these paths take is proven structurally in the engine's
`merge_unique_probe_tests`; these goldens pin the answers it must not change.
"""

import pytest

import kglite


def _rows(graph, query, **kwargs):
    return graph.cypher(query, **kwargs).to_list()


@pytest.fixture
def plain_graph():
    graph = kglite.KnowledgeGraph()
    _rows(graph, "CREATE (:U {k: 'a', v: 1}), (:U {k: 'a', v: 2}), (:U {k: 'b', v: 1})")
    return graph


@pytest.fixture
def unique_graph():
    graph = kglite.KnowledgeGraph()
    _rows(graph, "CREATE CONSTRAINT FOR (n:U) REQUIRE n.k IS UNIQUE")
    _rows(graph, "CREATE (:U {k: 'a', v: 1}), (:U {k: 'b', v: 1}), (:U {k: 7, v: 3})")
    return graph


def test_merge_without_uniqueness_returns_every_match(plain_graph):
    assert _rows(plain_graph, "MERGE (n:U {k: 'a'}) RETURN n.v AS v ORDER BY v") == [{"v": 1}, {"v": 2}]
    assert _rows(plain_graph, "MATCH (n:U) RETURN count(n) AS c") == [{"c": 3}]


def test_merge_on_unique_key_hit_returns_one_row_and_creates_nothing(unique_graph):
    rows = _rows(unique_graph, "MERGE (n:U {k: 'a'}) ON MATCH SET n.seen = true RETURN n.v AS v, n.seen AS seen")
    assert rows == [{"v": 1, "seen": True}]
    assert _rows(unique_graph, "MATCH (n:U) RETURN count(n) AS c") == [{"c": 3}]
    rows = _rows(unique_graph, "MERGE (n:U {k: 7}) RETURN n.v AS v")
    assert rows == [{"v": 3}]
    assert _rows(unique_graph, "MATCH (n:U) RETURN count(n) AS c") == [{"c": 3}]


def test_merge_on_unique_key_miss_creates_exactly_one(unique_graph):
    rows = _rows(unique_graph, "MERGE (n:U {k: 'new'}) ON CREATE SET n.v = 9 RETURN n.v AS v")
    assert rows == [{"v": 9}]
    assert _rows(unique_graph, "MATCH (n:U) RETURN count(n) AS c") == [{"c": 4}]
    assert _rows(unique_graph, "MERGE (n:U {k: 'new'}) RETURN count(n) AS c") == [{"c": 1}]
    assert _rows(unique_graph, "MATCH (n:U) RETURN count(n) AS c") == [{"c": 4}]


def test_merge_on_unique_key_with_a_differing_extra_property_is_refused(unique_graph):
    # The pattern names the occupied tuple but not the occupant's `v`: no node
    # matches, MERGE tries to create, and the constraint refuses the second 'a'.
    with pytest.raises(Exception, match="(?i)unique|constraint"):
        _rows(unique_graph, "MERGE (n:U {k: 'a', v: 2}) RETURN n")
    assert _rows(unique_graph, "MATCH (n:U) RETURN count(n) AS c") == [{"c": 3}]


def test_merge_on_composite_unique_key(unique_graph):
    _rows(unique_graph, "CREATE CONSTRAINT FOR (n:C) REQUIRE (n.a, n.b) IS UNIQUE")
    _rows(unique_graph, "CREATE (:C {a: 'x', b: 1}), (:C {a: 'x', b: 2})")
    assert _rows(unique_graph, "MERGE (n:C {a: 'x', b: 2}) RETURN n.b AS b") == [{"b": 2}]
    assert _rows(unique_graph, "MERGE (n:C {a: 'x'}) RETURN count(n) AS c") == [{"c": 2}]
    assert _rows(unique_graph, "MATCH (n:C) RETURN count(n) AS c") == [{"c": 2}]


def test_unwind_upsert_on_unique_key(unique_graph):
    rows = _rows(
        unique_graph,
        "UNWIND ['a', 'z', 'a', 'z'] AS key MERGE (n:U {k: key}) RETURN key, n.v AS v",
    )
    assert rows == [{"key": "a", "v": 1}, {"key": "z", "v": None}, {"key": "a", "v": 1}, {"key": "z", "v": None}]
    assert _rows(unique_graph, "MATCH (n:U) RETURN count(n) AS c") == [{"c": 4}]
