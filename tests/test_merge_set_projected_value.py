"""MERGE ... ON MATCH / ON CREATE SET on a projected node reads back as written.

``UNWIND collect(n) AS x`` binds ``x`` to a node snapshot. The SET writes the
stored node; the rows handed on must show the written value, as after a plain
SET.
"""

import pytest

from kglite import KnowledgeGraph


@pytest.fixture
def graph():
    g = KnowledgeGraph()
    g.cypher("CREATE (:X {id: 1, p: 0}), (:X {id: 2, p: 0})")
    return g


PRE = "MATCH (n:X) WITH collect(n) AS ns UNWIND ns AS x MERGE (y:Y {id: x.id}) "
POST = " RETURN x.id AS id, x.p AS p ORDER BY id"


def test_on_create_set_is_visible_to_return(graph):
    got = graph.cypher(PRE + "ON CREATE SET x.p = 5" + POST).to_list()
    assert got == [{"id": 1, "p": 5}, {"id": 2, "p": 5}]


def test_on_match_set_is_visible_to_return(graph):
    graph.cypher(PRE + "ON CREATE SET x.p = 5" + POST)
    got = graph.cypher(PRE + "ON MATCH SET x.p = 7" + POST).to_list()
    assert got == [{"id": 1, "p": 7}, {"id": 2, "p": 7}]
    stored = graph.cypher("MATCH (n:X) RETURN n.id AS id, n.p AS p ORDER BY id").to_list()
    assert stored == got
