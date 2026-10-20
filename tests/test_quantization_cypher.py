"""Cypher node retrieval through a quantized scan copy.

``vector_score`` top-k and the embedding query procedures narrow candidates by
the copy and re-score them against f32, so scores are exact and the route is
reported (``actual_mode`` / ``search_method``). These tests pin the golden
equivalence with the exact route, the ``exact`` bypass, the index-first rule,
the Poincare exclusion and the valid-time mask.
"""

from __future__ import annotations

import datetime as dt
import random

import pandas as pd
import pytest

import kglite

DIM = 32
N = 400
TOP = "MATCH (d:Doc) RETURN d.id AS id, vector_score(d, 'text_emb', $q{opts}) AS s ORDER BY s DESC LIMIT 5"


def _vectors(seed=7):
    rng = random.Random(seed)
    return {i: [rng.gauss(0, 1) for _ in range(DIM)] for i in range(N)}


def _query(seed=99):
    rng = random.Random(seed)
    return [rng.gauss(0, 1) for _ in range(DIM)]


def _graph(metric="cosine", temporal=False):
    g = kglite.KnowledgeGraph()
    frame = {"id": list(range(N)), "title": [f"d{i}" for i in range(N)], "text": [f"t{i}" for i in range(N)]}
    if temporal:
        frame["vf"] = [dt.date(2000, 1, 1)] * N
        # Odd ids stop being valid before the instant the tests ask about.
        frame["vt"] = [dt.date(2001, 1, 1) if i % 2 else dt.date(2020, 1, 1) for i in range(N)]
    g.add_nodes(pd.DataFrame(frame), "Doc", "id", "title")
    if temporal:
        g.set_temporal("Doc", "vf", "vt")
    g.set_embeddings("Doc", "text", _vectors(), metric=metric)
    return g


def _run(g, query=TOP, opts="", **extra):
    return g.cypher(query.format(opts=opts), params={"q": _query(), **extra})


def _rows(result):
    return [(r["id"], r["s"]) for r in result.to_list()]


def _modes(result):
    return [r["actual_mode"] for r in (result.diagnostics or {}).get("retrieval", [])]


@pytest.mark.parametrize("precision,oversample", [("f16", None), ("int8", 8)])
def test_whole_type_top_k_matches_exact_with_exact_scores(precision, oversample):
    g = _graph()
    exact = _run(g, opts=", {exact: true}")
    assert _modes(exact) == ["exact"]
    g.quantize_embeddings("Doc", "text", precision, oversample)
    fast = _run(g)
    assert _modes(fast) == ["quantized"]
    assert _rows(fast) == _rows(exact)  # ids, order and the f32 scores, bit for bit


def test_exact_option_bypasses_the_copy():
    g = _graph()
    g.quantize_embeddings("Doc", "text", "f16")
    forced = _run(g, opts=", {exact: true}")
    assert _modes(forced) == ["exact"]
    assert _run(g).diagnostics["retrieval"][0]["actual_mode"] == "quantized"


def test_an_index_serves_before_the_copy():
    g = _graph()
    g.build_node_vector_index("Doc", "text")
    g.quantize_embeddings("Doc", "text", "f16")
    assert _modes(_run(g)) == ["hnsw"]


def test_a_metric_the_copy_cannot_rank_stays_exact():
    g = _graph()
    g.quantize_embeddings("Doc", "text", "f16")
    result = _run(g, opts=", 'poincare'")
    assert _modes(result) == ["exact"]


def test_unquantized_store_reports_no_quantized_route():
    assert _modes(_run(_graph())) == ["exact"]


def test_dropping_the_copy_returns_to_the_exact_route():
    g = _graph()
    g.quantize_embeddings("Doc", "text", "f16")
    g.quantize_embeddings("Doc", "text", None)
    assert _modes(_run(g)) == ["exact"]


def test_node_query_procedure_reports_the_quantized_route():
    g = _graph()
    call = (
        "CALL db.node_embeddings.query({text_column: 'text', vector: $q, top_k: 5%s}) "
        "YIELD node, score, search_method RETURN node.id AS id, score AS s, search_method AS m"
    )
    exact = g.cypher(call % ", exact: true", params={"q": _query()}).to_list()
    g.quantize_embeddings("Doc", "text", "f16")
    fast = g.cypher(call % "", params={"q": _query()}).to_list()
    assert {r["m"] for r in fast} == {"quantized"}
    assert {r["m"] for r in exact} == {"exact"}
    assert [(r["id"], r["s"]) for r in fast] == [(r["id"], r["s"]) for r in exact]


def test_a_where_that_narrows_the_rows_is_scored_exactly():
    g = _graph()
    g.quantize_embeddings("Doc", "text", "f16")
    query = (
        "MATCH (d:Doc) WHERE d.id % 3 = 0 "
        "RETURN d.id AS id, vector_score(d, 'text_emb', $q) AS s ORDER BY s DESC LIMIT 5"
    )
    narrowed = _run(g, query)
    assert "quantized" not in _modes(narrowed)
    assert all(row[0] % 3 == 0 for row in _rows(narrowed))


AS_OF = "FOR VALID_TIME AS OF $t "


@pytest.mark.parametrize("precision,oversample", [("f16", None), ("int8", 8)])
def test_valid_time_mask_ranks_valid_vectors_through_the_copy(precision, oversample):
    g = _graph(temporal=True)
    t = dt.date(2010, 1, 1)
    exact = _run(g, AS_OF + TOP, opts=", {exact: true}", t=t)
    assert all(row[0] % 2 == 0 for row in _rows(exact))
    g.quantize_embeddings("Doc", "text", precision, oversample)
    fast = _run(g, AS_OF + TOP, t=t)
    assert _modes(fast) == ["quantized_mask"]
    assert fast.diagnostics["temporal"]["retrieval"] == "quantized_mask"
    assert _rows(fast) == _rows(exact)
    forced = _run(g, AS_OF + TOP, opts=", {exact: true}", t=t)
    assert _modes(forced) == ["exact"]
    assert forced.diagnostics["temporal"]["retrieval"] == "exact_mask"


def test_valid_time_query_procedure_uses_the_copy():
    g = _graph(temporal=True)
    call = (
        "CALL db.node_embeddings.query({text_column: 'text', vector: $q, top_k: 5%s}) "
        "YIELD node, score, search_method RETURN node.id AS id, score AS s, search_method AS m"
    )
    t = dt.date(2010, 1, 1)
    exact = g.cypher(AS_OF + call % ", exact: true", params={"q": _query(), "t": t}).to_list()
    g.quantize_embeddings("Doc", "text", "f16")
    fast = g.cypher(AS_OF + call % "", params={"q": _query(), "t": t}).to_list()
    assert {r["m"] for r in fast} == {"quantized_mask"}
    assert [(r["id"], r["s"]) for r in fast] == [(r["id"], r["s"]) for r in exact]
    assert all(r["id"] % 2 == 0 for r in fast)
