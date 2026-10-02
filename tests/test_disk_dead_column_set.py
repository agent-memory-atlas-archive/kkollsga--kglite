"""A SET survives when the column it targets was only ever held by a deleted row.

A disk save drops the rows no live node points at, which leaves a column that
only a dead row ever wrote with no values. A later save that cannot write the
type's column regions (a SET that turns a column into a mixed-kind one) flattens
the type into a sidecar without that column; the next load then held an empty
column and silently dropped every SET on it, including after another save.
"""

from __future__ import annotations

import pytest

import kglite

VALUES = [7, "x", 2.5, True, [1, 2]]
# Statements that force the sidecar (flatten) save of the type.
FLATTENERS = [
    "MATCH (n:Person {id: 3}) SET n.tag = [1, 2]",
    "MATCH (n:Person {id: 3}) SET n.title = 5",
    "MATCH (n:Person {id: 3}) SET n.tag = 'a'",
]


def _rows(graph, query):
    return graph.cypher(query).to_list()


def _build(path, dead_value, flatten):
    g = kglite.KnowledgeGraph(storage="disk", path=path)
    g.cypher("CREATE (n:Person {id: 1, title: 'a'})")
    g.cypher("MATCH (n:Person {id: 1}) SET n.extra = $v", params={"v": dead_value})
    g.cypher("MATCH (n:Person {id: 1}) DETACH DELETE n")
    g.cypher("CREATE (n:Person {id: 2, title: 'b'})")
    g.cypher("CREATE (n:Person {id: 3, title: 'c'})")
    g.save(path)
    g.cypher(flatten)
    g.save(path)
    return g


@pytest.mark.parametrize("flatten", FLATTENERS)
@pytest.mark.parametrize("dead_value", VALUES, ids=repr)
@pytest.mark.parametrize("new_value", VALUES, ids=repr)
def test_set_on_dead_row_only_column_survives_save_and_reopen(tmp_path, flatten, dead_value, new_value):
    path = str(tmp_path / "g")
    g = _build(path, dead_value, flatten)
    del g
    g = kglite.load(path)
    q = "MATCH (n:Person {id: 2}) SET n.extra = $v RETURN n.extra AS v"
    got = g.cypher(q, params={"v": new_value}).to_list()
    assert got == [{"v": new_value}]
    assert _rows(g, "MATCH (n:Person {id: 2}) RETURN n.extra AS v") == [{"v": new_value}]
    assert _rows(g, "MATCH (n:Person {id: 3}) RETURN n.extra AS v") == [{"v": None}]
    g.save(path)
    del g
    g = kglite.load(path)
    assert _rows(g, "MATCH (n:Person {id: 2}) RETURN n.extra AS v") == [{"v": new_value}]
    # A node created after the reopen lands on its own row, not an earlier one.
    g.cypher("CREATE (n:Person {id: 4, title: 'd', extra: 'late'})")
    g.save(path)
    del g
    g = kglite.load(path)
    rows = _rows(g, "MATCH (n:Person) RETURN n.id AS i, n.extra AS e ORDER BY i")
    assert rows == [{"i": 2, "e": new_value}, {"i": 3, "e": None}, {"i": 4, "e": "late"}]
