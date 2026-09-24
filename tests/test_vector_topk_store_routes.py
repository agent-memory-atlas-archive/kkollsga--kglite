"""Which route answers `ORDER BY vector_score(...) DESC LIMIT k`, and what it returns.

Every shape here used to reach the right answer by scoring every row: a
`WHERE vector_score(...) IS NOT NULL` filter scored each row once to filter
and once to rank while the diagnostics reported `hnsw`; `WITH r, score ...
RETURN startNode(r).id` never fused; an undirected relationship pattern and a
node type with unembedded members fell back to `row_coverage`. The routes are
pinned through EXPLAIN (the absorbed filter leaves no `Where` step) and the
retrieval diagnostics; the answers are pinned against an exact oracle computed
here and against the unoptimised pipeline, so a defect the two plans share
cannot hide behind the differential corpus.
"""

from __future__ import annotations

import numpy as np
import pytest

from kglite import KnowledgeGraph

N = 40
DIM = 8
UNEMBEDDED = (3, 11, 12, 30)  # scattered through the type's order
TIED = (5, 27)  # identical vectors: equal scores rank in pattern order


def _vectors() -> np.ndarray:
    vectors = np.random.default_rng(11).standard_normal((N, DIM)).astype(np.float32)
    vectors[TIED[1]] = vectors[TIED[0]]
    return vectors


VECTORS = _vectors()


def _cosine(query: np.ndarray, ids) -> dict[int, float]:
    q = query / np.linalg.norm(query)
    return {i: float(VECTORS[i] @ q / np.linalg.norm(VECTORS[i])) for i in ids}


def _node_graph(*, embedded, indexed: bool) -> KnowledgeGraph:
    graph = KnowledgeGraph()
    graph.cypher("UNWIND range(0, $n - 1) AS i CREATE (:D {id: i, t: 'x'})", params={"n": N})
    graph.set_embeddings("D", "t", {i: VECTORS[i].tolist() for i in embedded})
    if indexed:
        graph.build_vector_index("D", "t")
    return graph


def _rel_graph(*, embedded, indexed: bool) -> KnowledgeGraph:
    graph = KnowledgeGraph()
    graph.cypher("UNWIND range(0, $n) AS i CREATE (:P {id: i})", params={"n": N})
    graph.cypher(
        "UNWIND range(0, $n - 1) AS i MATCH (a:P {id: i}), (b:P {id: i + 1}) CREATE (a)-[:C {k: i, t: 'x'}]->(b)",
        params={"n": N},
    )
    graph.set_relationship_embeddings("C", "t", {(i, i + 1): VECTORS[i].tolist() for i in embedded})
    if indexed:
        graph.cypher(
            "CALL db.relationship_embeddings.build_index({type: 'C', text_property: 't'}) YIELD indexed RETURN indexed"
        )
    return graph


def _expected(query: np.ndarray, embedded, limit: int, *, nulls: bool) -> list[tuple[int, float | None]]:
    """The openCypher answer: NULL scores first under DESC (in pattern order,
    which is id order here), then scores descending, ties in pattern order."""
    scores = _cosine(query, embedded)
    ranked = [(i, scores[i]) for i in sorted(scores, key=lambda i: (-scores[i], i))]
    head = [(i, None) for i in range(N) if i not in scores] if nulls else []
    return (head + ranked)[:limit]


def _answer(result) -> list[tuple[int, float | None]]:
    return [(row["id"], row["s"]) for row in result.to_list()]


def _assert_matches(actual, expected) -> None:
    assert [i for i, _ in actual] == [i for i, _ in expected]
    for (_, got), (_, want) in zip(actual, expected):
        assert (got is None) == (want is None)
        if want is not None:
            assert got == pytest.approx(want, abs=1e-5)


def _run(graph: KnowledgeGraph, query: str, q: np.ndarray):
    params = {"q": q.tolist()}
    result = graph.cypher(query, params=params)
    unoptimised = graph.cypher(query, params=params, disable_optimizer=True)
    assert result.to_list() == unoptimised.to_list()
    steps = [step["operation"] for step in graph.cypher("EXPLAIN " + query, params=params).to_list()]
    return result, result.diagnostics.get("retrieval") or [], steps


NODE = "MATCH (n:D) {where}RETURN n.id AS id, vector_score(n, 't_emb', $q{opts}) AS s ORDER BY s DESC LIMIT {k}"
NOT_NULL = "WHERE vector_score(n, 't_emb', $q{opts}) IS NOT NULL "
ALL = range(N)
PARTIAL = [i for i in range(N) if i not in UNEMBEDDED]


@pytest.mark.parametrize("indexed", [True, False], ids=["hnsw", "exact"])
@pytest.mark.parametrize("embedded", [ALL, PARTIAL], ids=["whole", "partial"])
def test_node_not_null_filter_is_served_by_the_store(indexed, embedded) -> None:
    graph = _node_graph(embedded=embedded, indexed=indexed)
    q = VECTORS[TIED[0]]
    result, retrieval, steps = _run(graph, NODE.format(where=NOT_NULL.format(opts=""), opts="", k=6), q)
    assert "Where" not in steps
    assert retrieval[0]["actual_mode"] == ("hnsw" if indexed else "exact")
    assert retrieval[0]["fallback_reason"] == (None if indexed else "no_index")
    _assert_matches(_answer(result), _expected(q, embedded, 6, nulls=False))


def test_node_not_null_filter_with_forced_exact() -> None:
    graph = _node_graph(embedded=PARTIAL, indexed=True)
    q = VECTORS[TIED[0]]
    opts = ", {exact: true}"
    result, retrieval, steps = _run(graph, NODE.format(where=NOT_NULL.format(opts=opts), opts=opts, k=6), q)
    assert "Where" not in steps
    assert (retrieval[0]["actual_mode"], retrieval[0]["fallback_reason"]) == ("exact", "forced_exact")
    _assert_matches(_answer(result), _expected(q, PARTIAL, 6, nulls=False))


@pytest.mark.parametrize("indexed", [True, False], ids=["hnsw", "exact"])
@pytest.mark.parametrize("limit", [2, 4, 7], ids=["nulls-only", "nulls-exactly", "nulls-then-scores"])
def test_partial_node_coverage_ranks_nulls_first_from_the_store(indexed, limit) -> None:
    graph = _node_graph(embedded=PARTIAL, indexed=indexed)
    q = VECTORS[TIED[0]]
    result, retrieval, _ = _run(graph, NODE.format(where="", opts="", k=limit), q)
    expected = _expected(q, PARTIAL, limit, nulls=True)
    _assert_matches(_answer(result), expected)
    if limit <= len(UNEMBEDDED):
        route = ("exact", "row_coverage")  # no score was needed at all
    else:
        route = ("hnsw", None) if indexed else ("exact", "no_index")
    assert (retrieval[0]["actual_mode"], retrieval[0]["fallback_reason"]) == route
    assert retrieval[0]["store"] == "D.t_emb"


def test_store_order_unlike_type_order_keeps_the_row_route() -> None:
    graph = KnowledgeGraph()
    graph.cypher("UNWIND range(0, $n - 1) AS i CREATE (:D {id: i, t: 'x'})", params={"n": N})
    graph.set_embeddings("D", "t", {i: VECTORS[i].tolist() for i in reversed(PARTIAL)})
    graph.build_vector_index("D", "t")
    q = VECTORS[TIED[0]]
    result, retrieval, _ = _run(graph, NODE.format(where="", opts="", k=7), q)
    _assert_matches(_answer(result), _expected(q, PARTIAL, 7, nulls=True))
    assert retrieval[0]["fallback_reason"] == "row_coverage"
    result, retrieval, _ = _run(graph, NODE.format(where=NOT_NULL.format(opts=""), opts="", k=7), q)
    _assert_matches(_answer(result), _expected(q, PARTIAL, 7, nulls=False))
    assert retrieval[0]["actual_mode"] == "hnsw"


def test_not_null_filter_ranked_without_projecting_the_score() -> None:
    graph = _node_graph(embedded=PARTIAL, indexed=True)
    q = VECTORS[TIED[0]]
    query = (
        "MATCH (n:D) WHERE vector_score(n, 't_emb', $q) IS NOT NULL "
        "RETURN n.id AS id ORDER BY vector_score(n, 't_emb', $q) DESC LIMIT 5"
    )
    result, retrieval, steps = _run(graph, query, q)
    assert "Where" not in steps and not any(step.startswith("FusedNodeScanTopK") for step in steps)
    assert retrieval[0]["actual_mode"] == "hnsw"
    assert [row["id"] for row in result.to_list()] == [i for i, _ in _expected(q, PARTIAL, 5, nulls=False)]


def test_not_null_filter_as_last_conjunct_keeps_the_rest_of_the_where() -> None:
    graph = _node_graph(embedded=PARTIAL, indexed=True)
    q = VECTORS[TIED[0]]
    query = NODE.format(where="WHERE n.id % 2 = 0 AND vector_score(n, 't_emb', $q) IS NOT NULL ", opts="", k=5)
    result, retrieval, steps = _run(graph, query, q)
    assert "Where" in steps
    assert retrieval[0]["actual_mode"] == "hnsw"
    _assert_matches(_answer(result), _expected(q, [i for i in PARTIAL if i % 2 == 0], 5, nulls=False))


def test_a_different_call_in_the_filter_is_not_absorbed() -> None:
    graph = _node_graph(embedded=PARTIAL, indexed=True)
    q = VECTORS[TIED[0]]
    query = (
        "MATCH (n:D) WHERE vector_score(n, 't_emb', $q, 'dot_product') IS NOT NULL "
        "RETURN n.id AS id, vector_score(n, 't_emb', $q) AS s ORDER BY s DESC LIMIT 5"
    )
    result, _, steps = _run(graph, query, q)
    assert "Where" in steps
    _assert_matches(_answer(result), _expected(q, PARTIAL, 5, nulls=False))


def test_node_with_top_k_then_projection() -> None:
    graph = _node_graph(embedded=ALL, indexed=True)
    q = VECTORS[TIED[0]]
    query = "MATCH (n:D) WITH n, vector_score(n, 't_emb', $q) AS s ORDER BY s DESC LIMIT 4 RETURN n.id AS id, s"
    result, retrieval, _ = _run(graph, query, q)
    assert retrieval[0]["actual_mode"] == "hnsw"
    _assert_matches(_answer(result), _expected(q, ALL, 4, nulls=False))


REL = "MATCH ()-[r:C]->() {where}RETURN r.k AS id, vector_score(r, 't_emb', $q{opts}) AS s ORDER BY s DESC LIMIT {k}"
REL_NOT_NULL = "WHERE vector_score(r, 't_emb', $q{opts}) IS NOT NULL "


@pytest.mark.parametrize("indexed", [True, False], ids=["hnsw", "exact"])
@pytest.mark.parametrize("embedded", [ALL, PARTIAL], ids=["whole", "partial"])
def test_relationship_not_null_filter_is_served_by_the_store(indexed, embedded) -> None:
    graph = _rel_graph(embedded=embedded, indexed=indexed)
    q = VECTORS[TIED[0]]
    result, retrieval, steps = _run(graph, REL.format(where=REL_NOT_NULL.format(opts=""), opts="", k=6), q)
    assert "Where" not in steps
    assert retrieval[0]["actual_mode"] == ("hnsw" if indexed else "exact")
    assert retrieval[0]["fallback_reason"] == (None if indexed else "no_index")
    _assert_matches(_answer(result), _expected(q, embedded, 6, nulls=False))


def test_relationship_not_null_filter_on_the_rows_route() -> None:
    graph = _rel_graph(embedded=PARTIAL, indexed=True)
    q = VECTORS[TIED[0]]
    query = REL.format(where=REL_NOT_NULL.format(opts=""), opts="", k=6).replace(
        "MATCH ()-[r:C]->()", "MATCH (a:P)-[r:C]->(b:P) WHERE a.id < 35 WITH a, r, b"
    )
    result, retrieval, _ = _run(graph, query, q)
    assert (retrieval[0]["actual_mode"], retrieval[0]["fallback_reason"]) == ("hnsw", None)
    _assert_matches(_answer(result), _expected(q, [i for i in PARTIAL if i < 35], 6, nulls=False))


def test_relationship_partial_coverage_without_the_filter_reports_row_coverage() -> None:
    graph = _rel_graph(embedded=PARTIAL, indexed=True)
    q = VECTORS[TIED[0]]
    result, retrieval, _ = _run(graph, REL.format(where="", opts="", k=6), q)
    assert retrieval[0]["fallback_reason"] == "row_coverage"
    rows = _answer(result)
    assert sorted(i for i, s in rows if s is None) == list(UNEMBEDDED)


def test_relationship_with_top_k_then_endpoint_projection() -> None:
    graph = _rel_graph(embedded=ALL, indexed=True)
    q = VECTORS[TIED[0]]
    query = (
        "MATCH ()-[r:C]->() WITH r, vector_score(r, 't_emb', $q) AS s ORDER BY s DESC LIMIT 5 "
        "RETURN startNode(r).id AS id, endNode(r).id AS b, s"
    )
    result, retrieval, _ = _run(graph, query, q)
    assert (retrieval[0]["actual_mode"], retrieval[0]["fallback_reason"]) == ("hnsw", None)
    expected = _expected(q, ALL, 5, nulls=False)
    _assert_matches(_answer(result), expected)
    assert [row["b"] for row in result.to_list()] == [i + 1 for i, _ in expected]


@pytest.mark.parametrize("indexed", [True, False], ids=["hnsw", "exact"])
def test_undirected_relationship_top_k_counts_relationships_not_rows(indexed) -> None:
    graph = _rel_graph(embedded=ALL, indexed=indexed)
    q = VECTORS[TIED[0]]
    query = (
        "MATCH (a:P)-[r:C]-(b:P) RETURN r.k AS id, a.id AS a, vector_score(r, 't_emb', $q) AS s ORDER BY s DESC LIMIT 6"
    )
    result, retrieval, _ = _run(graph, query, q)
    assert (retrieval[0]["actual_mode"], retrieval[0]["fallback_reason"]) == (
        ("hnsw", None) if indexed else ("exact", "no_index")
    )
    best = _expected(q, ALL, 3, nulls=False)
    # Each relationship is matched once per orientation, both rows scored alike.
    _assert_matches(_answer(result), [pair for pair in best for _ in range(2)])
    assert sorted(row["a"] for row in result.to_list()[:2]) == [best[0][0], best[0][0] + 1]
