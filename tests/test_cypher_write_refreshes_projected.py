"""A write through a projected node or relationship value is visible to the rest of the statement.

`UNWIND collect(n) AS x SET x.p = v RETURN x.p` used to answer the value from
before the SET: the projected `Value::Node` / `Value::Relationship` is a
snapshot, and the write reached the stored element only. The same held for
REMOVE, label SETs and a following SET that reads the value it just wrote.
"""

import pytest

import kglite


@pytest.fixture
def kg():
    g = kglite.KnowledgeGraph()
    g.cypher("CREATE (:L {name: 'old'})-[:R {w: 1}]->(:L {name: 'other'})")
    return g


def test_set_property_on_unwound_node_reads_new_value(kg):
    rows = kg.cypher(
        "MATCH (a:L {name: 'old'}) WITH collect(a) AS ns UNWIND ns AS n SET n.name = 'new' RETURN n.name AS v"
    ).to_dicts()
    assert rows == [{"v": "new"}]
    assert kg.cypher("MATCH (a:L {name: 'new'}) RETURN count(a) AS c").to_dicts() == [{"c": 1}]


def test_set_new_property_on_unwound_node_appears_in_node_and_properties(kg):
    row = kg.cypher(
        "MATCH (a:L {name: 'old'}) UNWIND [a] AS n SET n.k = 7 RETURN n.k AS k, properties(n).k AS pk, n.name AS nm"
    ).to_dicts()
    assert row == [{"k": 7, "pk": 7, "nm": "old"}]


def test_value_captured_before_set_keeps_the_old_value(kg):
    row = kg.cypher(
        "MATCH (a:L {name: 'old'}) WITH collect(a) AS ns, collect(a.name) AS before "
        "UNWIND ns AS n SET n.name = 'new' RETURN n.name AS v, before"
    ).to_dicts()
    assert row == [{"v": "new", "before": ["old"]}]


def test_remove_property_on_unwound_node_reads_null(kg):
    row = kg.cypher("MATCH (a:L {name: 'old'}) UNWIND [a] AS n SET n.k = 1 REMOVE n.k RETURN n.k AS k").to_dicts()
    assert row == [{"k": None}]


def test_set_label_on_unwound_node_shows_in_labels(kg):
    row = kg.cypher("MATCH (a:L {name: 'old'}) UNWIND [a] AS n SET n:Extra RETURN labels(n) AS l").to_dicts()
    assert row == [{"l": ["L", "Extra"]}]


def test_second_set_reads_the_first(kg):
    row = kg.cypher(
        "MATCH (a:L {name: 'old'}) UNWIND [a] AS n SET n.name = 'a1' SET n.name = n.name + 'b' RETURN n.name AS v"
    ).to_dicts()
    assert row == [{"v": "a1b"}]


def test_set_property_on_unwound_relationship_reads_new_value(kg):
    row = kg.cypher(
        "MATCH ()-[r:R]->() WITH collect(r) AS rs UNWIND rs AS x SET x.w = 2 RETURN x.w AS w, properties(x).w AS pw"
    ).to_dicts()
    assert row == [{"w": 2, "pw": 2}]
