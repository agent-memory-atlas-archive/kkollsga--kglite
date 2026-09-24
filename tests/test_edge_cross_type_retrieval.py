"""Cross-type relationship retrieval: several relationship stores ranked as one.

`db.edge_embeddings.query` takes `types: [...]`, or — with neither `type` nor
`types` — every store for `text_property`, and merges the stores' top-k into
one ranking. The fused `vector_score(r, …) ORDER BY … DESC LIMIT k` does the
same for a type alternation `[r:A|B]` and an untyped `[r]` when every type in
play carries the store. Every answer here is checked against a brute-force
oracle computed in Python, and the fused answers against the unfused pipeline.
"""

from __future__ import annotations

import math

import pytest

import kglite
from kglite import KnowledgeGraph

PASS = "fuse_vector_score_order_limit"
QUERY = [0.3, 1.0]

# (type, k, angle) — distinct angles, so no two cosines tie for QUERY.
EDGES = [
    ("A", 1, 0.10),
    ("B", 2, 0.35),
    ("D", 3, 0.60),
    ("A", 4, 0.85),
    ("B", 5, 1.10),
    ("D", 6, 1.35),
    ("A", 7, 1.60),
    ("B", 8, 2.20),
    ("D", 9, 2.90),
]


def _graph(
    mode: str = "memory", *, metrics: dict[str, str] | None = None, indexed: tuple[str, ...] = ()
) -> KnowledgeGraph:
    graph = KnowledgeGraph(storage=mode)
    graph.cypher("CREATE (:Hub {id: 0})")
    for rel_type, k, angle in EDGES:
        metric = (metrics or {}).get(rel_type)
        graph.cypher(
            f"MATCH (h:Hub) CREATE (h)-[r:{rel_type} {{k: $k}}]->(:Doc {{id: $k}}) "
            f"WITH r CALL db.edge_embeddings.set({{type: '{rel_type}', text_property: 'text', "
            "entries: [{relationship: r, vector: $v}], metric: $metric}) YIELD stored RETURN stored",
            params={"k": k, "v": [math.cos(angle), math.sin(angle)], "metric": metric},
        )
    for rel_type in indexed:
        graph.cypher(
            f"CALL db.edge_embeddings.build_index({{type: '{rel_type}', text_property: 'text'}}) "
            "YIELD indexed RETURN indexed"
        )
    return graph


def _oracle(types: set[str], vector: list[float], k: int) -> list[tuple[str, int]]:
    norm = math.hypot(*vector)
    scored = [
        (rel_type, key, (math.cos(angle) * vector[0] + math.sin(angle) * vector[1]) / norm)
        for rel_type, key, angle in EDGES
        if rel_type in types
    ]
    scored.sort(key=lambda row: -row[2])
    return [(rel_type, key) for rel_type, key, _ in scored[:k]]


def _call(graph: KnowledgeGraph, selector: str, k: int = 4, extra: str = "") -> list[dict]:
    fields = [f for f in (selector, "text_property: 'text'", "vector: $v", f"top_k: {k}", extra) if f]
    return graph.cypher(
        f"CALL db.edge_embeddings.query({{{', '.join(fields)}}}) "
        "YIELD relationship, score, search_method, type "
        "RETURN relationship.k AS k, type(relationship) AS rel_type, type, score, search_method",
        params={"v": QUERY},
    ).to_list()


# ── the procedure ─────────────────────────────────────────────────────────────


@pytest.mark.parametrize("mode", ["memory", "mapped"])
def test_types_list_merges_the_named_stores(mode: str) -> None:
    graph = _graph(mode)
    rows = _call(graph, "types: ['D', 'A']", k=4)
    assert [(row["type"], row["k"]) for row in rows] == _oracle({"A", "D"}, QUERY, 4)
    assert all(row["type"] == row["rel_type"] for row in rows), "the column agrees with the value"
    assert all(row["search_method"] == "exact" for row in rows)
    assert [row["score"] for row in rows] == sorted((row["score"] for row in rows), reverse=True)


@pytest.mark.parametrize("mode", ["memory", "mapped"])
def test_omitting_type_ranks_every_store_for_the_property(mode: str) -> None:
    graph = _graph(mode)
    rows = _call(graph, "", k=5)
    assert [(row["type"], row["k"]) for row in rows] == _oracle({"A", "B", "D"}, QUERY, 5)


def test_a_single_type_still_works_and_yields_its_type() -> None:
    graph = _graph()
    rows = _call(graph, "type: 'B'", k=2)
    assert [(row["type"], row["k"]) for row in rows] == _oracle({"B"}, QUERY, 2)


def test_search_method_is_per_row_and_follows_each_stores_index() -> None:
    graph = _graph(indexed=("A",))
    rows = _call(graph, "types: ['A', 'B']", k=6)
    assert {(row["type"], row["search_method"]) for row in rows} == {("A", "hnsw"), ("B", "exact")}
    assert [(row["type"], row["k"]) for row in rows] == _oracle({"A", "B"}, QUERY, 6)


def test_ties_across_stores_order_by_type_then_slot() -> None:
    graph = KnowledgeGraph()
    graph.cypher("CREATE (:Hub {id: 0})")
    for rel_type, k in [("Z", 1), ("M", 2), ("Z", 3), ("M", 4)]:
        graph.cypher(
            f"MATCH (h:Hub) CREATE (h)-[r:{rel_type} {{k: $k}}]->(:Doc) "
            f"WITH r CALL db.edge_embeddings.set({{type: '{rel_type}', text_property: 'text', "
            "entries: [{relationship: r, vector: [1.0, 0.0]}]}) YIELD stored RETURN stored",
            params={"k": k},
        )
    rows = _call(graph, "types: ['Z', 'M']", k=4)
    assert [(row["type"], row["k"]) for row in rows] == [("M", 2), ("M", 4), ("Z", 1), ("Z", 3)]


def test_text_spelling_ranks_across_types() -> None:
    class Embedder:
        dimension = 2
        model_id = "fake/cross-type"

        def load(self) -> None:
            pass

        def unload(self) -> None:
            pass

        def embed(self, texts: list[str]) -> list[list[float]]:
            return [list(QUERY) for _ in texts]

    graph = _graph()
    graph.set_embedder(Embedder())
    rows = graph.cypher(
        "CALL db.edge_embeddings.query({types: ['A', 'B', 'D'], text_property: 'text', text: 'anything', top_k: 3}) "
        "YIELD relationship, type RETURN relationship.k AS k, type"
    ).to_list()
    assert [(row["type"], row["k"]) for row in rows] == _oracle({"A", "B", "D"}, QUERY, 3)


@pytest.mark.parametrize(
    ("selector", "message"),
    [
        ("type: 'A', types: ['B']", "'type' and 'types' are mutually exclusive"),
        ("types: []", "'types' is empty"),
        ("types: 'A'", "'types' must be a list of relationship types"),
        ("types: ['A', 'MISSING']", "No relationship embedding store 'MISSING.text'"),
    ],
)
def test_bad_type_selections_are_refused(selector: str, message: str) -> None:
    graph = _graph()
    with pytest.raises(kglite.CypherExecutionError, match=message):
        _call(graph, selector)


def test_a_property_with_no_store_is_refused() -> None:
    graph = _graph()
    with pytest.raises(kglite.CypherExecutionError, match="no relationship embedding store for text_property 'nope'"):
        graph.cypher(
            "CALL db.edge_embeddings.query({text_property: 'nope', vector: [1.0, 0.0]}) YIELD score RETURN score"
        )


def test_stores_under_different_metrics_refuse_the_merge_naming_both() -> None:
    graph = _graph(metrics={"B": "euclidean"})
    with pytest.raises(kglite.CypherExecutionError) as info:
        _call(graph, "types: ['A', 'B']")
    message = str(info.value)
    assert "'A.text' (metric 'cosine')" in message
    assert "'B.text' (metric 'euclidean')" in message
    # Omitting type reaches the same refusal; one explicit metric resolves it.
    with pytest.raises(kglite.CypherExecutionError, match="different metrics"):
        _call(graph, "")
    rows = _call(graph, "types: ['A', 'B']", k=3, extra="metric: 'cosine'")
    assert [(row["type"], row["k"]) for row in rows] == _oracle({"A", "B"}, QUERY, 3)


# ── the fused MATCH route ─────────────────────────────────────────────────────


def _fused(graph: KnowledgeGraph, pattern: str, k: int) -> object:
    query = (
        f"MATCH {pattern} RETURN type(r) AS t, r.k AS k, vector_score(r, 'text_emb', {QUERY}) AS s "
        f"ORDER BY s DESC LIMIT {k}"
    )
    fused = graph.cypher(query)
    unfused = graph.cypher(query, disabled_passes=[PASS])
    assert fused.to_list() == unfused.to_list(), pattern
    return fused


ALL_THREE = "relationship:A.text_emb,relationship:B.text_emb,relationship:D.text_emb"


@pytest.mark.parametrize("mode", ["memory", "mapped"])
@pytest.mark.parametrize("indexed", [(), ("A", "B", "D")], ids=["exact", "hnsw"])
@pytest.mark.parametrize("pattern", ["()-[r:A|B|D]->()", "()-[r]->()", "(:Hub)-[r:B|D|A]->(:Doc)"])
def test_alternation_and_untyped_match_the_oracle(mode: str, indexed: tuple[str, ...], pattern: str) -> None:
    graph = _graph(mode, indexed=indexed)
    fused = _fused(graph, pattern, 4)
    assert [(row["t"], row["k"]) for row in fused.to_list()] == _oracle({"A", "B", "D"}, QUERY, 4)
    records = fused.diagnostics["retrieval"]
    assert any(
        record["store"] == ALL_THREE and record["actual_mode"] == ("hnsw" if indexed else "exact") for record in records
    ), records


def test_text_score_over_an_alternation_takes_the_same_route() -> None:
    graph = _graph(indexed=("A", "B"))
    query = "MATCH ()-[r:A|B]->() RETURN r.k AS k, text_score(r, 'text', $q) AS s ORDER BY s DESC LIMIT 3"
    fused = graph.cypher(query, params={"q": QUERY})
    unfused = graph.cypher(query, params={"q": QUERY}, disabled_passes=[PASS])
    assert fused.to_list() == unfused.to_list()
    assert [row["k"] for row in fused.to_list()] == [key for _, key in _oracle({"A", "B"}, QUERY, 3)]
    assert any(
        record["store"] == "relationship:A.text_emb,relationship:B.text_emb" and record["actual_mode"] == "hnsw"
        for record in fused.diagnostics["retrieval"]
    )


def test_a_type_in_play_without_the_store_raises_the_scalar_error() -> None:
    graph = _graph()
    graph.cypher("MATCH (h:Hub), (d:Doc {id: 1}) CREATE (h)-[:PLAIN]->(d)")
    for pattern in ["()-[r]->()", "()-[r:A|PLAIN]->()"]:
        for passes in (None, [PASS]):
            with pytest.raises(kglite.CypherExecutionError, match="found for relationship type 'PLAIN'"):
                graph.cypher(
                    f"MATCH {pattern} RETURN vector_score(r, 'text_emb', {QUERY}) AS s ORDER BY s DESC LIMIT 3",
                    disabled_passes=passes,
                )


def test_profile_reports_the_merged_route() -> None:
    graph = _graph(indexed=("A", "B", "D"))
    profiled = graph.cypher(
        f"PROFILE MATCH ()-[r:A|B|D]->() RETURN r.k AS k, vector_score(r, 'text_emb', {QUERY}) AS s "
        "ORDER BY s DESC LIMIT 3"
    )
    assert [row["k"] for row in profiled.to_list()] == [key for _, key in _oracle({"A", "B", "D"}, QUERY, 3)]
    assert any(record["store"] == ALL_THREE for record in profiled.diagnostics["retrieval"])
