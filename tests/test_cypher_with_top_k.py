"""`WITH … ORDER BY … LIMIT k` followed by another clause keeps its top-K fusion.

The differential corpus (`with_top_k_*` entries) shows the fused plan answers
what the unfused plan answers. These pin what it cannot: that the fused
operator is reached (HNSW for a vector sort), the absolute answers, the WITH
scope, and that `disabled_passes` turns the pass off.
"""

from __future__ import annotations

import pandas as pd
import pytest

import kglite

VECTOR = "vector_score(d, 'summary_emb', [1.0,0.0])"
BM25 = "text_bm25(d, 'body', 'alpha')"
TAGS = "OPTIONAL MATCH (d)-[:HAS]->(t:Tag)"


@pytest.fixture
def graph() -> kglite.KnowledgeGraph:
    g = kglite.KnowledgeGraph()
    g.add_nodes(
        pd.DataFrame(
            {
                "id": list(range(6)),
                "age": [30, 25, 40, 35, 20, 50],
                "body": ["alpha beta", "alpha", "beta gamma", "alpha alpha beta", "gamma", "delta"],
                "summary": ["x"] * 6,
            }
        ),
        "Doc",
        "id",
        columns=["age", "body", "summary"],
    )
    g.add_nodes(pd.DataFrame({"id": [0, 1, 2]}), "Tag", "id")
    g.set_embeddings("Doc", "summary", {0: [1.0, 0.0], 1: [0.0, 1.0], 2: [-1.0, 0.0], 3: [0.7, 0.7], 4: [0.9, 0.1]})
    g.build_vector_index("Doc", "summary")
    g.build_text_index("Doc", "body")
    g.add_connections(pd.DataFrame({"d": [0, 0, 1, 3, 4, 4], "t": [0, 1, 1, 2, 0, 2]}), "HAS", "Doc", "d", "Tag", "t")
    return g


def _operations(graph, query: str) -> list[str]:
    return [row["operation"] for row in graph.cypher(f"EXPLAIN {query}").to_list()]


def _rows(graph, query: str, **kwargs) -> list[tuple]:
    return sorted(
        (tuple(row.values()) for row in graph.cypher(query, **kwargs).to_list()),
        key=repr,
    )


@pytest.mark.parametrize(
    "score,operation",
    [
        (VECTOR, "FusedVectorScoreTopK"),
        (BM25, "FusedTextBm25TopK"),
        ("d.age", "FusedOrderByTopK"),
    ],
)
@pytest.mark.parametrize("tail", [TAGS, "MATCH (d)-[:HAS]->(t:Tag)"])
def test_each_top_k_kind_fuses_with_a_clause_behind_it(graph, score, operation, tail) -> None:
    query = f"MATCH (d:Doc) WITH d, {score} AS s ORDER BY s DESC LIMIT 3 {tail} RETURN d.id AS id, s, t.id AS tag"
    operations = _operations(graph, query)
    assert any(op.startswith(operation) for op in operations), operations
    assert "OptimizerPass fuse_with_top_k" in operations


def test_vector_top_k_behind_a_clause_is_served_from_the_index(graph) -> None:
    query = f"MATCH (d:Doc) WITH d, {VECTOR} AS s ORDER BY s DESC LIMIT 3 {TAGS} RETURN d.id AS id, s, t.id AS tag"
    result = graph.cypher(query)
    assert result.diagnostics["retrieval"][0]["actual_mode"] == "hnsw"
    # Doc 5 has no vector: NULL ranks first under DESC.
    assert _rows(graph, query) == [
        (0, 1.0, 0),
        (0, 1.0, 1),
        (4, pytest.approx(0.9938837, rel=1e-5), 0),
        (4, pytest.approx(0.9938837, rel=1e-5), 2),
        (5, None, None),
    ]
    off = graph.cypher(query, disabled_passes=["fuse_with_top_k"])
    assert off.diagnostics["retrieval"] == []
    assert _rows(graph, query, disabled_passes=["fuse_with_top_k"]) == _rows(graph, query)


def test_bm25_and_property_top_k_answers(graph) -> None:
    bm25 = f"MATCH (d:Doc) WITH d, {BM25} AS s ORDER BY s DESC LIMIT 2 {TAGS} RETURN d.id AS id, t.id AS tag"
    assert _rows(graph, bm25) == [(1, 1), (3, 2)]
    prop = f"MATCH (d:Doc) WITH d, d.age AS a ORDER BY a DESC LIMIT 2 {TAGS} RETURN d.id AS id, a, t.id AS tag"
    assert _rows(graph, prop) == [(2, 40, None), (5, 50, None)]


@pytest.mark.parametrize("score", [VECTOR, BM25, "d.age"])
def test_a_non_projected_variable_does_not_leak_past_the_with(graph, score) -> None:
    # `d` is not carried by the WITH. A leaked binding would pin the second
    # MATCH's `d` to the top-ranked document instead of id 3.
    query = f"MATCH (d:Doc) WITH {score} AS s ORDER BY s DESC LIMIT 1 MATCH (d:Doc {{id: 3}}) RETURN d.id AS id"
    assert any(op.startswith("Fused") for op in _operations(graph, query))
    assert _rows(graph, query) == [(3,)]


def test_a_projected_variable_stays_bound(graph) -> None:
    query = "MATCH (d:Doc) WITH d, d.age AS a ORDER BY a DESC LIMIT 1 MATCH (d)-[:HAS]->(t:Tag) RETURN d.id AS id"
    assert _rows(graph, query) == []
    query = (
        "MATCH (d:Doc) WITH d AS top, d.age AS a ORDER BY a LIMIT 1 "
        "MATCH (top)-[:HAS]->(t:Tag) RETURN top.id AS id, t.id AS tag"
    )
    assert _rows(graph, query) == [(4, 0), (4, 2)]


@pytest.mark.parametrize(
    "query",
    [
        "MATCH (d:Doc) WITH DISTINCT d.age % 2 AS p ORDER BY p LIMIT 1 "
        "OPTIONAL MATCH (x:Doc {id: p}) RETURN p, x.age AS age",
        "MATCH (d:Doc) WITH d, d.age AS a ORDER BY a DESC SKIP 1 LIMIT 2 "
        "OPTIONAL MATCH (d)-[:HAS]->(t:Tag) RETURN d.id AS id",
        "MATCH (d:Doc) WITH * ORDER BY d.age LIMIT 2 OPTIONAL MATCH (d)-[:HAS]->(t:Tag) RETURN d.id AS id",
        "MATCH (d:Doc) WITH d, d.age AS a ORDER BY a + 1 LIMIT 2 OPTIONAL MATCH (d)-[:HAS]->(t:Tag) RETURN d.id AS id",
    ],
)
def test_bail_shapes_do_not_fuse(graph, query) -> None:
    assert "OptimizerPass fuse_with_top_k" not in _operations(graph, query)


def test_disabled_passes_turns_the_pass_off(graph) -> None:
    query = f"MATCH (d:Doc) WITH d, d.age AS a ORDER BY a DESC LIMIT 3 {TAGS} RETURN d.id AS id, t.id AS tag"
    assert "OptimizerPass fuse_with_top_k" in _operations(graph, query)
    plan = graph.cypher(f"EXPLAIN {query}", disabled_passes=["fuse_with_top_k"]).to_list()
    assert "OptimizerPass fuse_with_top_k" not in [row["operation"] for row in plan]
    assert _rows(graph, query, disabled_passes=["fuse_with_top_k"]) == _rows(graph, query)
