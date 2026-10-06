"""A graph loaded from ``.kgl`` with a hole still forks under a held view.

A save keeps interior node holes and edge holes. Loading rebuilds petgraph's
free lists in slot order, so the writer overlay can predict the slots its
creates will take and a write under a ``freeze()`` forks instead of copying
the whole graph (``kglite._backend_is_forked``).
"""

import gc

import pytest

import kglite

SEED = "UNWIND range(0, 9) AS i CREATE (:Item {k: i})"
CHAIN = "MATCH (a:Item), (b:Item) WHERE b.k = a.k + 1 CREATE (a)-[:NEXT]->(b)"
HOLES = {
    "interior node": ["MATCH (n:Item) WHERE n.k IN [6, 2, 4] DETACH DELETE n"],
    "edge": ["MATCH (:Item {k: 3})-[r:NEXT]->() DELETE r", "MATCH (:Item {k: 7})-[r:NEXT]->() DELETE r"],
}
WRITES = [
    "UNWIND [100, 101, 102, 103] AS k CREATE (:Item {k: k})",
    "MATCH (n:Item {k: 100}) SET n.v = 1",
    "UNWIND [200, 201] AS k CREATE (:Item {k: k})",
]


def _state(g):
    nodes = [dict(r) for r in g.cypher("MATCH (n:Item) RETURN n.k AS k, n.v AS v ORDER BY k")]
    edges = [dict(r) for r in g.cypher("MATCH (a)-[:NEXT]->(b) RETURN a.k AS a, b.k AS b ORDER BY a")]
    return nodes, edges


def _saved_with(hole, tmp_path):
    g = kglite.KnowledgeGraph()
    g.cypher(SEED)
    g.cypher(CHAIN)
    for query in HOLES[hole]:
        g.cypher(query)
    path = str(tmp_path / "holes.kgl")
    g.save(path)
    return path


@pytest.mark.parametrize("hole", sorted(HOLES))
def test_a_reloaded_graph_with_a_hole_forks_under_a_held_view(tmp_path, hole):
    path = _saved_with(hole, tmp_path)
    control = kglite.load(path)
    g = kglite.load(path)
    view = g.freeze()
    before = _state(view)
    for query in WRITES:
        g.cypher(query)
        control.cypher(query)
        assert kglite._backend_is_forked(g) is True, f"{hole}: {query} copied the graph"
    assert _state(g) == _state(control)
    assert _state(view) == before
    del view
    gc.collect()
    g.cypher("CREATE (:Item {k: 300})")
    control.cypher("CREATE (:Item {k: 300})")
    assert kglite._backend_is_forked(g) is False
    assert _state(g) == _state(control)
