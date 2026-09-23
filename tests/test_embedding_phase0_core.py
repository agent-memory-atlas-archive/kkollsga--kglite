"""Phase-0 regressions for vector provenance and finite-number invariants."""

import hashlib
import math

import pandas as pd
import pytest

import kglite


class _Model:
    def __init__(self, model_id: str | None, *, nonfinite: float | None = None) -> None:
        self.dimension = 4
        if model_id is not None:
            self.model_id = model_id
        self.nonfinite = nonfinite
        self.calls: list[list[str]] = []

    def embed(self, texts: list[str]) -> list[list[float]]:
        self.calls.append(list(texts))
        vectors = [[float(b) for b in hashlib.sha256(text.encode()).digest()[: self.dimension]] for text in texts]
        if self.nonfinite is not None and vectors:
            vectors[0][1] = self.nonfinite
        return vectors


def _docs() -> kglite.KnowledgeGraph:
    graph = kglite.KnowledgeGraph()
    graph.add_nodes(
        pd.DataFrame(
            {
                "id": [0, 1, 2],
                "title": ["a", "b", "c"],
                "summary": ["alpha", "beta", "gamma"],
            }
        ),
        "Doc",
        "id",
        "title",
    )
    return graph


@pytest.mark.parametrize("mode", ["missing", "changed"])
def test_incremental_generation_rejects_different_known_model_atomically(mode: str) -> None:
    graph = _docs()
    graph.set_embedder(_Model("model/a"))
    graph.embed_texts("Doc", "summary", show_progress=False)
    before = graph.embeddings("Doc", "summary")

    graph.set_embedder(_Model("model/b"))
    with pytest.raises(ValueError, match="mode='all'"):
        graph.embed_texts("Doc", "summary", mode=mode, show_progress=False)

    assert graph.embeddings("Doc", "summary") == before
    assert graph.embedding_info("Doc", "summary")["model"] == "model/a"


def test_incremental_generation_rejects_unknown_model_over_known_store() -> None:
    graph = _docs()
    graph.set_embedder(_Model("model/a"))
    graph.embed_texts("Doc", "summary", show_progress=False)
    before = graph.embeddings("Doc", "summary")

    graph.set_embedder(_Model(None))
    with pytest.raises(ValueError, match="mode='all'"):
        graph.embed_texts("Doc", "summary", mode="changed", show_progress=False)

    assert graph.embeddings("Doc", "summary") == before
    assert graph.embedding_info("Doc", "summary")["model"] == "model/a"


def test_same_known_model_can_incrementally_refresh_changed_text() -> None:
    graph = _docs()
    model = _Model("model/a")
    graph.set_embedder(model)
    graph.embed_texts("Doc", "summary", show_progress=False)
    graph.cypher("MATCH (n:Doc {id: 1}) SET n.summary = 'rewritten'")

    report = graph.embed_texts("Doc", "summary", mode="changed", show_progress=False)

    assert report["embedded"] == 1
    assert graph.embedding_info("Doc", "summary")["model"] == "model/a"


def test_named_incremental_generation_does_not_claim_unknown_existing_vectors() -> None:
    graph = _docs()
    graph.set_embeddings("Doc", "summary", {0: [1.0, 0.0, 0.0, 0.0]})
    graph.set_embedder(_Model("model/b"))

    report = graph.embed_texts("Doc", "summary", mode="changed", show_progress=False)

    assert report["embedded"] == 3
    assert graph.embedding_info("Doc", "summary")["model"] is None


def test_manual_upsert_clears_only_written_hash_and_aggregate_model() -> None:
    graph = _docs()
    model = _Model("model/a")
    graph.set_embedder(model)
    graph.embed_texts("Doc", "summary", show_progress=False)

    graph.add_embeddings("Doc", "summary", {1: [9.0, 8.0, 7.0, 6.0]})
    mixed = graph.embedding_info("Doc", "summary")
    assert mixed["model"] is None
    assert mixed["hashed"] == 2

    report = graph.embed_texts("Doc", "summary", mode="changed", show_progress=False)
    assert report["embedded"] == 1
    assert graph.embedding_info("Doc", "summary")["model"] is None

    graph.embed_texts("Doc", "summary", mode="all", show_progress=False)
    rebuilt = graph.embedding_info("Doc", "summary")
    assert rebuilt["model"] == "model/a"
    assert rebuilt["hashed"] == 3


def test_unknown_id_only_manual_add_preserves_provenance() -> None:
    graph = _docs()
    graph.set_embedder(_Model("model/a"))
    graph.embed_texts("Doc", "summary", show_progress=False)

    report = graph.add_embeddings("Doc", "summary", {999: [9.0, 8.0, 7.0, 6.0]})

    assert report["skipped"] == 1
    info = graph.embedding_info("Doc", "summary")
    assert info["model"] == "model/a"
    assert info["hashed"] == 3


@pytest.mark.parametrize("bad", [math.nan, math.inf, -math.inf])
def test_manual_vector_ingest_rejects_nonfinite_atomically(bad: float) -> None:
    graph = _docs()
    graph.set_embeddings("Doc", "summary", {0: [1.0, 0.0, 0.0, 0.0]})
    before = graph.embeddings("Doc", "summary")

    with pytest.raises(ValueError, match="finite"):
        graph.add_embeddings("Doc", "summary", {1: [0.0, bad, 0.0, 1.0]})

    assert graph.embeddings("Doc", "summary") == before


@pytest.mark.parametrize("bad", [math.nan, math.inf, -math.inf])
def test_generated_vector_rejects_nonfinite_atomically(bad: float) -> None:
    graph = _docs()
    graph.set_embedder(_Model("bad/model", nonfinite=bad))

    with pytest.raises(ValueError, match="finite"):
        graph.embed_texts("Doc", "summary", show_progress=False)

    assert graph.embedding_info("Doc", "summary") is None


@pytest.mark.parametrize("bad", [math.nan, math.inf, -math.inf])
def test_python_vector_query_rejects_nonfinite(bad: float) -> None:
    graph = _docs()
    graph.set_embeddings("Doc", "summary", {0: [1.0, 0.0, 0.0, 0.0]})

    with pytest.raises(ValueError, match="finite"):
        graph.vector_search("summary", [1.0, bad, 0.0, 0.0], exact=True)


@pytest.mark.parametrize("bad", [math.nan, math.inf, -math.inf])
def test_cypher_vector_query_rejects_nonfinite(bad: float) -> None:
    graph = _docs()
    graph.set_embeddings("Doc", "summary", {0: [1.0, 0.0, 0.0, 0.0]})

    with pytest.raises(kglite.CypherExecutionError, match="finite"):
        graph.cypher(
            "MATCH (n:Doc) RETURN vector_score(n, 'summary_emb', $query) AS score",
            params={"query": [1.0, bad, 0.0, 0.0]},
        ).to_list()
