"""Unembedded rows and `ORDER BY vector_score(...) DESC LIMIT k`.

openCypher sorts `null` above every value, so a node or relationship with no
vector — `vector_score` is `null` for it — takes the first rows of a DESC
top-k, and the planner answers by row scan (`fallback_reason: 'row_coverage'`).
The documented recipe, `WHERE vector_score(...) IS NOT NULL`, drops those rows
and keeps the store route. CYPHER.md, the semantic-search guide and the
`describe()` semantic topics all state this; these tests keep them true.
"""

from __future__ import annotations

from kglite import KnowledgeGraph

VECTORS = {1: [1.0, 0.0], 2: [0.8, 0.6], 3: [0.0, 1.0]}
QUERY = [1.0, 0.0]


def _nodes() -> KnowledgeGraph:
    graph = KnowledgeGraph()
    graph.cypher("UNWIND range(1, 5) AS i CREATE (:D {id: i, title: 'd' + toString(i), t: 'x'})")
    graph.set_embeddings("D", "t", VECTORS)  # ids 4 and 5 have no vector
    graph.build_vector_index("D", "t")
    return graph


def _relationships() -> KnowledgeGraph:
    graph = KnowledgeGraph()
    graph.cypher("CREATE (h:Hub {id: 0})")
    graph.cypher(
        "UNWIND range(1, 5) AS k MATCH (h:Hub) CREATE (h)-[:C {k: k, t: 'text ' + toString(k)}]->(:Doc {id: k})"
    )
    for k, vector in VECTORS.items():
        graph.cypher(
            "MATCH ()-[r:C {k: $k}]->() CALL db.relationship_embeddings.set({type: 'C', text_property: 't', "
            "entries: [{relationship: r, vector: $v}]}) YIELD stored RETURN stored",
            params={"k": k, "v": vector},
        )
    graph.cypher(
        "CALL db.relationship_embeddings.build_index({type: 'C', text_property: 't'}) YIELD indexed RETURN indexed"
    )
    return graph


NODE_TOP = "MATCH (n:D) {where}RETURN n.id AS id, vector_score(n, 't_emb', $q) AS s ORDER BY s DESC LIMIT 3"
REL_TOP = "MATCH ()-[r:C]->() {where}RETURN r.k AS id, vector_score(r, 't_emb', $q) AS s ORDER BY s DESC LIMIT 3"


def _run(graph: KnowledgeGraph, template: str, where: str):
    result = graph.cypher(template.format(where=where), params={"q": QUERY})
    return [(row["id"], row["s"] is None) for row in result.to_list()], result.diagnostics["retrieval"]


def test_unembedded_nodes_come_first_without_the_filter() -> None:
    rows, retrieval = _run(_nodes(), NODE_TOP, "")
    assert sorted(rows[:2]) == [(4, True), (5, True)]
    assert retrieval[0]["fallback_reason"] == "row_coverage"


def test_the_filter_drops_unembedded_nodes_and_keeps_the_store_route() -> None:
    rows, retrieval = _run(_nodes(), NODE_TOP, "WHERE vector_score(n, 't_emb', $q) IS NOT NULL ")
    assert rows == [(1, False), (2, False), (3, False)]
    assert retrieval[0]["store"] == "D.t_emb"
    assert retrieval[0]["fallback_reason"] is None
    assert retrieval[0]["actual_mode"] == "hnsw"


def test_unembedded_relationships_come_first_without_the_filter() -> None:
    rows, retrieval = _run(_relationships(), REL_TOP, "")
    assert sorted(rows[:2]) == [(4, True), (5, True)]
    assert retrieval[0]["fallback_reason"] == "row_coverage"


def test_the_filter_drops_unembedded_relationships_and_keeps_the_store_route() -> None:
    rows, retrieval = _run(_relationships(), REL_TOP, "WHERE vector_score(r, 't_emb', $q) IS NOT NULL ")
    assert rows == [(1, False), (2, False), (3, False)]
    assert retrieval[0]["store"] == "relationship:C.t_emb"
    assert retrieval[0]["fallback_reason"] is None
    assert retrieval[0]["actual_mode"] == "hnsw"


def test_the_procedure_never_returns_an_unembedded_relationship() -> None:
    rows = _relationships().cypher(
        "CALL db.relationship_embeddings.query({type: 'C', text_property: 't', vector: $q, top_k: 5}) "
        "YIELD relationship RETURN relationship.k AS k",
        params={"q": QUERY},
    )
    assert sorted(row["k"] for row in rows.to_list()) == [1, 2, 3]
