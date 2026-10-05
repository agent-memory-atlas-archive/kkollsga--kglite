"""`vector_score(r, …) ORDER BY s DESC LIMIT k` over relationships.

The fused relationship route (the store entry, or the rows route after it
declines) must return what the unfused pipeline returns — rows, order,
endpoint projection — and report the same retrieval evidence whichever route
answered (PROFILE disables the entry, so it is the rows-route control).

Tie order is unspecified: on equal scores the entry ranks by relationship
index, the unfused query by matcher order. Where scores tie, the routes must
agree on the scores and on every row above the tie, and may pick different
members of the tied group.
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
            "MATCH ()-[r:C {k: $k}]->() CALL db.relationship_embeddings.set({type: 'C', text_column: 'text', "
            "entries: [{relationship: r, vector: $v}]}) YIELD stored RETURN stored",
            params={"k": k, "v": vector},
        )
    if indexed:
        graph.cypher(
            "CALL db.relationship_embeddings.build_index({type: 'C', text_column: 'text'}) YIELD indexed RETURN indexed"
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


TIED_SCAN = (
    "MATCH (h:Hub)-[r:C]->(d:Doc) RETURN id(r) AS rid, d.id AS d, r.k AS k, "
    "vector_score(r, 'text_emb', [1.0, 0.0]) AS s ORDER BY s DESC LIMIT {k}"
)


def _tied_graph() -> KnowledgeGraph:
    """k=1 and k=4 tie for the top. k=1 is the lower relationship index but
    hangs off the second hub, so the matcher yields k=4 first."""
    graph = KnowledgeGraph()
    graph.cypher(
        "CREATE (h1:Hub {id: 0}), (h2:Hub {id: 9}), (a:Doc {id: 1}), (b:Doc {id: 2}), (c:Doc {id: 3}), "
        "(h2)-[:C {k: 1, text: 't'}]->(a), (h1)-[:C {k: 4, text: 't'}]->(b), (h1)-[:C {k: 2, text: 't'}]->(c)"
    )
    for k in (1, 4, 2):
        graph.cypher(
            "MATCH ()-[r:C {k: $k}]->() CALL db.relationship_embeddings.set({type: 'C', text_column: 'text', "
            "entries: [{relationship: r, vector: $v}]}) YIELD stored RETURN stored",
            params={"k": k, "v": VECTORS[k]},
        )
    return graph


def _same_up_to_ties(rows: list[dict], control: list[dict], everything: list[dict]) -> None:
    """`rows` equal `control` but for which members of a tied score they hold."""
    assert [row["s"] for row in rows] == [row["s"] for row in control]
    for score in {row["s"] for row in rows}:
        tied = [row for row in everything if row["s"] == score]
        chosen = [row for row in rows if row["s"] == score]
        assert all(row in tied for row in chosen), (score, chosen, tied)
        if len(chosen) == len(tied):
            in_control = [row for row in control if row["s"] == score]
            assert sorted(chosen, key=lambda r: r["rid"]) == sorted(in_control, key=lambda r: r["rid"])


@pytest.mark.parametrize("k", [1, 2, 3])
def test_ties_rank_by_relationship_index(k) -> None:
    """The store entry serves a tie instead of declining to the full scan:
    equal scores rank by relationship index. The unfused query ranks them in
    matcher order, so the two may name different tied rows."""
    graph = _tied_graph()
    query = TIED_SCAN.format(k=k)
    unfused = graph.cypher(query, disabled_passes=[PASS]).to_list()
    everything = graph.cypher(TIED_SCAN.format(k=100), disabled_passes=[PASS]).to_list()
    assert [row["k"] for row in everything[:2]] == [4, 1], "the fixture must put matcher order against index order"
    fused = graph.cypher(query)
    rows = fused.to_list()
    for control in (graph.cypher("PROFILE " + query).to_list(), unfused):
        _same_up_to_ties(rows, control, everything)
    assert fused.diagnostics["retrieval"][0]["actual_mode"] == "exact"
    assert [row["k"] for row in rows] == [1, 4, 2][:k]


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
