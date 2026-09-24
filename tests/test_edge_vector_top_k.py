"""`vector_score(r, …) ORDER BY s DESC LIMIT k` over relationships.

The fused relationship route (the store entry, or the rows route after it
declines) must return exactly what the unfused pipeline returns — rows, order,
endpoint projection — and report the same retrieval evidence whichever route
answered (PROFILE disables the entry, so it is the rows-route control).
"""

from __future__ import annotations

import pytest

from kglite import KnowledgeGraph

PASS = "fuse_vector_score_order_limit"

# k=1 and k=4 share a vector: a tie. The hub's relationships are inserted in
# an order unlike their vectors', so matcher order and score order differ.
VECTORS = {1: [1.0, 0.2], 2: [0.1, 1.0], 3: [0.9, 0.5], 4: [1.0, 0.2], 5: [-1.0, 0.1], 6: [0.6, 0.6]}


def _graph(*, indexed: bool = False, sparse: bool = False) -> KnowledgeGraph:
    graph = KnowledgeGraph()
    graph.cypher(
        "CREATE (h:Hub {id: 0}), (a:Doc {id: 1}), (b:Doc {id: 2}), (c:Doc {id: 3}), "
        "(d:Doc {id: 4}), (e:Doc {id: 5}), (f:Doc {id: 6}), "
        "(h)-[:C {k: 3, text: 't'}]->(c), (h)-[:C {k: 1, text: 't'}]->(a), (h)-[:C {k: 5, text: 't'}]->(e), "
        "(h)-[:C {k: 2, text: 't'}]->(b), (h)-[:C {k: 6, text: 't'}]->(f), (h)-[:C {k: 4, text: 't'}]->(d)"
    )
    for k, vector in VECTORS.items():
        if sparse and k == 6:
            continue
        graph.cypher(
            "MATCH ()-[r:C {k: $k}]->() CALL db.relationship_embeddings.set({type: 'C', text_property: 'text', "
            "entries: [{relationship: r, vector: $v}]}) YIELD stored RETURN stored",
            params={"k": k, "v": vector},
        )
    if indexed:
        graph.cypher(
            "CALL db.relationship_embeddings.build_index({type: 'C', text_property: 'text'}) YIELD indexed RETURN "
            "indexed"
        )
    return graph


def _same_routes(graph: KnowledgeGraph, query: str) -> list[dict]:
    result = graph.cypher(query)
    profiled = graph.cypher("PROFILE " + query)
    unfused = graph.cypher(query, disabled_passes=[PASS])
    assert result.to_list() == profiled.to_list() == unfused.to_list()
    assert result.diagnostics["retrieval"] == profiled.diagnostics["retrieval"]
    return result


SCAN = (
    "MATCH (h:Hub)-[r:C]->(d:Doc) RETURN h.id AS h, d.id AS d, r.k AS k, "
    "vector_score(r, 'text_emb', {q}) AS s ORDER BY s DESC LIMIT {k}"
)


@pytest.mark.parametrize("indexed", [False, True], ids=["exact", "hnsw"])
def test_scan_equals_unfused_with_endpoints(indexed) -> None:
    graph = _graph(indexed=indexed)
    result = _same_routes(graph, SCAN.format(q="[0.0, 1.0]", k=3))
    assert [(row["h"], row["d"], row["k"]) for row in result.to_list()] == [(0, 2, 2), (0, 6, 6), (0, 3, 3)]
    info = result.diagnostics["retrieval"]
    assert len(info) == 1
    assert info[0]["store"] == "relationship:C.text_emb"
    assert info[0]["actual_mode"] == ("hnsw" if indexed else "exact")


@pytest.mark.parametrize("k", [1, 2, 6])
def test_ties_keep_the_matcher_order(k) -> None:
    graph = _graph()
    _same_routes(graph, SCAN.format(q="[1.0, 0.0]", k=k))


def test_where_asc_and_nulls_keep_the_unfused_answer() -> None:
    graph = _graph(indexed=True)
    _same_routes(
        graph,
        "MATCH (h:Hub)-[r:C]->(d:Doc) WHERE d.id > 2 RETURN d.id AS d, "
        "vector_score(r, 'text_emb', [0.0, 1.0]) AS s ORDER BY s DESC LIMIT 2",
    )
    _same_routes(graph, SCAN.format(q="[0.0, 1.0]", k=2).replace("DESC", "ASC"))
    sparse = _graph(sparse=True)
    rows = _same_routes(sparse, SCAN.format(q="[0.0, 1.0]", k=2)).to_list()
    assert rows[0]["k"] == 6 and rows[0]["s"] is None, "an unembedded relationship scores NULL, first"


def test_disabled_pass_is_the_control() -> None:
    graph = _graph()
    query = SCAN.format(q="[0.0, 1.0]", k=3)
    explained = str(graph.cypher("EXPLAIN " + query).to_list())
    assert "FusedVectorScoreTopK" in explained
    control = str(graph.cypher("EXPLAIN " + query, disabled_passes=[PASS]).to_list())
    assert "FusedVectorScoreTopK" not in control
