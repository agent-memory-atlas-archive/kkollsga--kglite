"""`type()`, `startNode()`, `endNode()`, `keys()` and `properties()` on
relationship *values* — anything that is not a MATCH binding.

Absolute goldens: the differential corpus compares optimised against
unoptimised execution, and a scalar value-arm defect returns the same wrong
answer on both paths, so only expected values catch it.
"""

from __future__ import annotations

import pytest

from kglite import KnowledgeGraph

MODES = ["memory", "mapped", "disk"]

ACCESSORS = "RETURN type(rel) AS t, startNode(rel).id AS s, endNode(rel).id AS e, keys(rel) AS k, properties(rel) AS p"

EXPECTED = {
    "t": "CLAIMS",
    "s": 2,
    "e": 1,
    "k": ["rank", "type"],
    "p": {"rank": 7, "type": "CLAIMS"},
}

PRODUCERS = {
    "binding": "MATCH ()-[rel:CLAIMS]->()",
    "collect_index": "MATCH ()-[r:CLAIMS]->() WITH collect(r)[0] AS rel",
    "unwind": "MATCH ()-[r:CLAIMS]->() WITH collect(r) AS rs UNWIND rs AS rel",
    "call_subquery": "CALL { MATCH ()-[r:CLAIMS]->() RETURN collect(r)[0] AS rel } WITH rel",
    "path_relationship": "MATCH p = ()-[:CLAIMS]->() WITH relationships(p)[0] AS rel",
    "edge_embeddings_query": (
        "CALL db.relationship_embeddings.query({type:'CLAIMS', text_column:'rank', "
        "vector:[1.0,0.0], top_k:1, exact:true}) YIELD relationship WITH relationship AS rel"
    ),
}


def _graph(mode: str, tmp_path) -> KnowledgeGraph:
    if mode == "memory":
        graph = KnowledgeGraph()
    elif mode == "mapped":
        graph = KnowledgeGraph(storage="mapped")
    else:
        graph = KnowledgeGraph(storage="disk", path=str(tmp_path / "graph"))
    # The relationship points from id 2 to id 1, against creation order, so a
    # swapped start/end cannot pass.
    graph.cypher("CREATE (a:Doc {id: 1}), (b:Doc {id: 2}), (b)-[:CLAIMS {rank: 7}]->(a)")
    stored = graph.cypher(
        "MATCH ()-[r:CLAIMS]->() "
        "CALL db.relationship_embeddings.set({type:'CLAIMS', text_column:'rank', "
        "entries:[{relationship:r, vector:[1.0,0.0]}]}) YIELD stored RETURN stored"
    ).to_list()
    assert stored == [{"stored": 1}], "fixture must store the relationship vector"
    return graph


@pytest.mark.parametrize("mode", MODES)
@pytest.mark.parametrize("producer", sorted(PRODUCERS))
def test_accessors_on_relationship_values(mode, producer, tmp_path):
    graph = _graph(mode, tmp_path)
    rows = graph.cypher(f"{PRODUCERS[producer]} {ACCESSORS}").to_list()
    assert rows == [EXPECTED]


@pytest.mark.parametrize("mode", MODES)
def test_edge_embeddings_query_yield_names_its_endpoints(mode, tmp_path):
    graph = _graph(mode, tmp_path)
    rows = graph.cypher(
        "CALL db.relationship_embeddings.query({type:'CLAIMS', text_column:'rank', "
        "vector:[1.0,0.0], top_k:1, exact:true}) YIELD relationship "
        "RETURN type(relationship) AS t, startNode(relationship).id AS s, "
        "endNode(relationship).id AS e"
    ).to_list()
    assert rows == [{"t": "CLAIMS", "s": 2, "e": 1}]


def test_retired_binding_does_not_report_the_replacement(tmp_path):
    """A binding deleted and whose slot is reused in the same statement names
    no live relationship: its accessors are null, not the replacement's."""
    graph = _graph("memory", tmp_path)
    rows = graph.cypher(
        "MATCH (b:Doc {id: 2})-[r:CLAIMS]->(a:Doc {id: 1}) DELETE r "
        "CREATE (a)-[fresh:OTHER {rank: 1}]->(b) "
        "RETURN type(r) AS t, startNode(r).id AS s, endNode(r).id AS e, "
        "properties(r) AS p, type(fresh) AS ft, id(r) = id(fresh) AS reused"
    ).to_list()
    assert rows == [{"t": None, "s": None, "e": None, "p": None, "ft": "OTHER", "reused": True}]
