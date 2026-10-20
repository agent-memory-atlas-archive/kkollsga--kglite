"""Quantized scan copies of embedding stores.

A copy (``f16`` / ``int8`` / ``binary``) narrows the candidates of a brute-force
vector search; the winners are re-scored against the f32 vectors, so scores are
exact and only the candidate set is approximate. These tests pin the contract
through the public API: exactness of returned scores, the ``exact=True``
bypass, step-for-step maintenance under writes, persistence in both layouts,
the ``precision=`` keyword on the writers, the recall helper and the refusals.
"""

from __future__ import annotations

import math
import random

import pandas as pd
import pytest

import kglite

DIM = 32
N = 400


def _vectors(n=N, dim=DIM, seed=7):
    rng = random.Random(seed)
    return {i: [rng.gauss(0, 1) for _ in range(dim)] for i in range(n)}


def _graph(n=N, dim=DIM, metric="cosine", seed=7):
    g = kglite.KnowledgeGraph()
    g.add_nodes(
        pd.DataFrame({"id": list(range(n)), "title": [f"d{i}" for i in range(n)], "text": [f"t{i}" for i in range(n)]}),
        "Doc",
        "id",
        "title",
    )
    vectors = _vectors(n, dim, seed)
    g.set_embeddings("Doc", "text", vectors, metric=metric)
    return g, vectors


def _query(dim=DIM, seed=99):
    rng = random.Random(seed)
    return [rng.gauss(0, 1) for _ in range(dim)]


def _cosine(a, b):
    dot = sum(x * y for x, y in zip(a, b))
    return dot / (math.sqrt(sum(x * x for x in a)) * math.sqrt(sum(y * y for y in b)))


def _ids(hits):
    return [hit["id"] for hit in hits]


def _row(g):
    (row,) = [r for r in g.list_embeddings() if r["entity"] == "node"]
    return row


@pytest.mark.parametrize("precision", ["f16", "int8", "binary"])
def test_search_scores_are_exact_and_exact_true_bypasses_the_copy(precision):
    g, vectors = _graph()
    query = _query()
    exact = g.vector_search("text", query, top_k=5)
    report = g.quantize_embeddings("Doc", "text", precision)
    assert report["previous"] is None
    assert report["quantization"]["precision"] == precision
    assert report["count"] == N and report["dimension"] == DIM

    hits = g.vector_search("text", query, top_k=5)
    for hit in hits:
        # Every score is the exact f32 score of that node, whichever candidates were scanned.
        assert hit["score"] == pytest.approx(_cosine(query, vectors[hit["id"]]), abs=1e-5)
    assert g.vector_search("text", query, top_k=5, exact=True) == exact
    if precision == "f16":
        assert _ids(hits) == _ids(exact)


def test_list_embeddings_and_info_report_the_copy_and_drop_removes_it():
    g, _ = _graph()
    assert _row(g)["quantization"] is None
    g.quantize_embeddings("Doc", "text", "int8", oversample=6)
    info = _row(g)["quantization"]
    assert (info["precision"], info["oversample"], info["storage"]) == ("int8", 6, "heap")
    assert info["bytes"] == N * DIM + N * 4  # codes plus one f32 scale per vector
    assert g.embedding_info("Doc", "text")["quantization"] == info
    diagnostics = [d for d in g.embedding_diagnostics("Doc") if d["text_column"] == "text"]
    assert diagnostics[0]["quantization"] == info

    dropped = g.quantize_embeddings("Doc", "text", None)
    assert dropped["previous"] == "int8" and dropped["quantization"] is None
    assert _row(g)["quantization"] is None
    # dropping again is a no-op
    assert g.quantize_embeddings("Doc", "text", None)["previous"] is None


def test_the_copy_follows_writes_and_deletes():
    g, vectors = _graph()
    g.quantize_embeddings("Doc", "text", "f16")
    query = _query(seed=5)

    # An upsert that makes node 0 identical to the query ranks it first.
    g.add_embeddings("Doc", "text", {0: query})
    assert g.vector_search("text", query, top_k=1)[0]["id"] == 0

    # A new node's vector is searchable at once.
    g.cypher("CREATE (:Doc {id: 1000, title: 'new', text: 'new'})")
    g.add_embeddings("Doc", "text", {1000: [-x for x in query]})
    far = g.vector_search("text", [-x for x in query], top_k=1)[0]
    assert far["id"] == 1000

    # Deleting the best match removes it from the results; the copy stays in step.
    g.cypher("MATCH (d:Doc {id: 0}) DETACH DELETE d")
    after = g.vector_search("text", query, top_k=5)
    assert 0 not in _ids(after)
    assert _ids(after) == _ids(g.vector_search("text", query, top_k=5, exact=True))
    assert _row(g)["count"] == N  # 400 - 1 deleted + 1 created
    assert _row(g)["quantization"]["precision"] == "f16"
    assert len(vectors) == N


def test_a_rolled_back_delete_restores_the_copy():
    g, _ = _graph()
    g.quantize_embeddings("Doc", "text", "int8")
    query = _query(seed=3)
    before = g.vector_search("text", query, top_k=10)
    with pytest.raises(Exception):  # noqa: B017, PT011 - any failure rolls the statement back
        g.cypher("MATCH (d:Doc) WHERE d.id < 5 DETACH DELETE d WITH d RETURN 1/0")
    assert g.vector_search("text", query, top_k=10) == before


@pytest.mark.parametrize("layout", ["auto", "compact"])
def test_precision_round_trips_through_save_and_load(tmp_path, layout):
    g, _ = _graph(n=600, dim=16)
    g.quantize_embeddings("Doc", "text", "binary", oversample=12)
    query = _query(dim=16)
    expected = g.vector_search("text", query, top_k=8)
    path = str(tmp_path / "q.kgl")
    g.save(path, layout=layout)

    for storage in ("memory", "mapped"):
        loaded = kglite.load(path, embeddings=storage)
        info = _row(loaded)["quantization"]
        assert (info["precision"], info["oversample"]) == ("binary", 12)
        assert loaded.vector_search("text", query, top_k=8) == expected


def test_a_large_store_maps_its_copy_and_verifies_it(tmp_path):
    g, _ = _graph(n=1200, dim=1024, seed=3)  # 4.7 MB of f32: over the data-region floor
    g.quantize_embeddings("Doc", "text", "int8")
    path = str(tmp_path / "big.kgl")
    g.save(path)
    loaded = kglite.load(path, embeddings="mapped")
    row = _row(loaded)
    assert row["storage"] == "mapped" and row["quantization"]["storage"] == "mapped"
    assert loaded.verify_integrity()  # the copy's sections are covered
    query = _query(dim=1024)
    assert loaded.vector_search("text", query, top_k=5) == g.vector_search("text", query, top_k=5)

    # The first write promotes the copy along with the vectors.
    loaded.add_embeddings("Doc", "text", {0: query})
    assert _row(loaded)["quantization"]["storage"] == "heap"
    assert loaded.vector_search("text", query, top_k=1)[0]["id"] == 0


def test_precision_keyword_on_the_writers():
    g = kglite.KnowledgeGraph()
    g.add_nodes(pd.DataFrame({"id": [1, 2, 3], "title": ["a", "b", "c"], "note": ["x", "y", "z"]}), "P", "id", "title")
    result = g.set_embeddings("P", "note", {1: [1.0, 0.0], 2: [0.0, 1.0], 3: [1.0, 1.0]}, precision="f16")
    assert result["quantization"]["precision"] == "f16"
    assert g.embedding_info("P", "note")["quantization"]["precision"] == "f16"
    # Without the keyword a replaced store starts without a copy and the report has no key.
    plain = g.set_embeddings("P", "note", {1: [1.0, 0.0], 2: [0.0, 1.0]})
    assert "quantization" not in plain
    assert g.embedding_info("P", "note")["quantization"] is None

    class Model:
        dimension = 2
        model_id = "toy"

        def embed(self, texts):
            return [[float(len(t) + 1), 1.0] for t in texts]

    g.set_embedder(Model())
    embedded = g.embed_texts("P", "title", precision="int8")
    assert embedded["quantization"]["precision"] == "int8"
    # An incremental pass keeps a held copy in step without being asked again.
    g.add_nodes(pd.DataFrame({"id": [4], "title": ["dddd"]}), "P", "id", "title")
    g.embed_texts("P", "title", mode="missing")
    assert g.embedding_info("P", "title")["quantization"]["precision"] == "int8"
    assert g.embedding_info("P", "title")["count"] == 4


def test_precision_on_import_and_relationships(tmp_path):
    g, vectors = _graph(n=50, dim=8)
    path = str(tmp_path / "e.kgle")
    g.export_embeddings(path)
    fresh = kglite.KnowledgeGraph()
    fresh.add_nodes(
        pd.DataFrame({"id": list(range(50)), "title": [f"d{i}" for i in range(50)], "text": ["t"] * 50}),
        "Doc",
        "id",
        "title",
    )
    stats = fresh.import_embeddings(path, precision="int8")
    assert stats["quantized_stores"] == 1
    assert fresh.embedding_info("Doc", "text")["quantization"]["precision"] == "int8"

    rg = kglite.KnowledgeGraph()
    rg.add_nodes(pd.DataFrame({"id": [1, 2, 3], "title": ["a", "b", "c"]}), "N", "id", "title")
    rg.add_relationships(
        pd.DataFrame({"s": [1, 2, 3], "t": [2, 3, 1], "note": ["p", "q", "r"]}), "R", "N", "s", "N", "t"
    )
    rows = {(1, 2): [1.0, 0.0], (2, 3): [0.0, 1.0], (3, 1): [1.0, 1.0]}
    result = rg.set_embeddings("R", "note", rows, entity="relationship", precision="f16")
    assert result["quantization"]["precision"] == "f16"
    (rel_row,) = [r for r in rg.list_embeddings() if r["entity"] == "relationship"]
    assert rel_row["quantization"]["precision"] == "f16"
    assert len(vectors) == 50


def test_a_relationship_store_searches_through_its_copy():
    rg = kglite.KnowledgeGraph()
    n = 120
    rg.add_nodes(pd.DataFrame({"id": list(range(n)), "title": [f"n{i}" for i in range(n)]}), "N", "id", "title")
    rg.add_relationships(
        pd.DataFrame({"s": list(range(n)), "t": [(i + 1) % n for i in range(n)], "note": ["x"] * n}),
        "R",
        "N",
        "s",
        "N",
        "t",
    )
    rng = random.Random(1)
    rows = {(i, (i + 1) % n): [rng.gauss(0, 1) for _ in range(DIM)] for i in range(n)}
    rg.set_embeddings("R", "note", rows, entity="relationship")
    query = _query(seed=8)
    exact = rg.vector_search("note", query, top_k=5, entity="relationship")
    rg.quantize_embeddings("R", "note", "f16", entity="relationship")
    quantized = rg.vector_search("note", query, top_k=5, entity="relationship")
    assert [h["source"] for h in quantized] == [h["source"] for h in exact]
    forced = rg.vector_search("note", query, top_k=5, exact=True, entity="relationship")
    assert forced == exact
    recall = rg.quantization_recall("R", "note", sample=20, entity="relationship")
    assert recall["precision"] == "f16" and recall["recall"] >= 0.95


def test_recall_helper_reports_both_stages_and_is_seeded():
    g, _ = _graph(n=800, dim=64)
    with pytest.raises(kglite.ArgumentError, match="no quantized copy"):
        g.quantization_recall("Doc", "text")
    g.quantize_embeddings("Doc", "text", "int8")
    first = g.quantization_recall("Doc", "text", sample=50, top_k=10, seed=4)
    assert first["precision"] == "int8" and first["sampled"] == 50 and first["top_k"] == 10
    assert 0.5 <= first["recall"] <= 1.0
    assert first["recall_rescored"] >= first["recall"] - 1e-9
    assert first == g.quantization_recall("Doc", "text", sample=50, top_k=10, seed=4)
    deeper = g.quantization_recall("Doc", "text", sample=50, top_k=10, seed=4, oversample=20)
    assert deeper["oversample"] == 20 and deeper["recall_rescored"] >= first["recall_rescored"] - 1e-9
    # Measuring with another oversample does not change the store.
    assert _row(g)["quantization"]["oversample"] == 4


def test_refusals_name_the_problem_and_change_nothing():
    g, _ = _graph(n=20, dim=4)
    with pytest.raises(kglite.ArgumentError, match="unknown quantization precision"):
        g.quantize_embeddings("Doc", "text", "f8")
    with pytest.raises(kglite.ArgumentError, match="No node embedding store"):
        g.quantize_embeddings("Doc", "missing", "f16")
    with pytest.raises(kglite.ArgumentError, match="needs a precision"):
        g.quantize_embeddings("Doc", "text", None, oversample=3)
    with pytest.raises(kglite.ArgumentError, match="at least 1"):
        g.quantize_embeddings("Doc", "text", "f16", oversample=0)
    with pytest.raises(kglite.ArgumentError, match="entity"):
        g.quantize_embeddings("Doc", "text", "f16", entity="edge")
    assert _row(g)["quantization"] is None

    p, _ = _graph(n=20, dim=4, metric="poincare")
    with pytest.raises(kglite.ArgumentError, match="poincare"):
        p.quantize_embeddings("Doc", "text", "f16")

    # A refused precision= leaves the target store untouched, before any write.
    q = kglite.KnowledgeGraph()
    q.add_nodes(pd.DataFrame({"id": [1], "title": ["a"], "t": ["x"]}), "P", "id", "title")
    with pytest.raises(kglite.ArgumentError, match="poincare"):
        q.set_embeddings("P", "t", {1: [0.1, 0.2]}, metric="poincare", precision="f16")
    assert q.list_embeddings() == []
    with pytest.raises(kglite.ArgumentError, match="unknown quantization precision"):
        q.set_embeddings("P", "t", {1: [0.1, 0.2]}, precision="nope")
    assert q.list_embeddings() == []


def test_poincare_queries_ignore_the_copy():
    g, _ = _graph()
    query = [x * 0.01 for x in _query()]
    exact = g.vector_search("text", query, top_k=5, metric="poincare")
    g.quantize_embeddings("Doc", "text", "int8")
    assert g.vector_search("text", query, top_k=5, metric="poincare") == exact


def test_the_copy_really_narrows_the_candidates():
    # Non-vacuity: with a one-vector-per-result pool of 1-bit codes, some queries
    # must miss an exact winner, or the scan never went through the copy.
    g, _ = _graph()
    queries = [_query(seed=s) for s in range(40)]
    exact = [_ids(g.vector_search("text", q, top_k=5, exact=True)) for q in queries]
    g.quantize_embeddings("Doc", "text", "binary", oversample=1)
    narrowed = [_ids(g.vector_search("text", q, top_k=5)) for q in queries]
    assert narrowed != exact
    # ...and exact=True still reproduces the exact answers.
    assert [_ids(g.vector_search("text", q, top_k=5, exact=True)) for q in queries] == exact
    # Raising the pool to the whole store restores them (the copy is then skipped).
    g.quantize_embeddings("Doc", "text", "binary", oversample=1000)
    assert [_ids(g.vector_search("text", q, top_k=5)) for q in queries] == exact
