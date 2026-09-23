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


def _axis_docs() -> kglite.KnowledgeGraph:
    """Two `Doc` nodes on opposite axes, so cosine against `[1, 0]` is 1 and 0."""
    graph = kglite.KnowledgeGraph()
    graph.add_nodes(
        pd.DataFrame({"id": [0, 1], "title": ["a", "b"], "summary": ["alpha", "beta"]}),
        "Doc",
        "id",
        "title",
    )
    graph.set_embeddings("Doc", "summary", {0: [1.0, 0.0], 1: [0.0, 1.0]})
    return graph


def test_node_values_score_like_direct_bindings() -> None:
    """A node reaching the scalars as a *value* scores, it does not fail.

    `collect(n)` + `UNWIND`, `head(...)`, a `CALL {}` column and `nodes(p)` all
    hand the scalar a materialised node rather than a pattern binding. That was
    refused with "first argument must be a node or relationship variable" (and,
    before that, answered NULL) -- losing the score of a node the caller holds.
    Every number below is cosine against the stored unit vectors.
    """
    graph = _axis_docs()
    rows = graph.cypher(
        "MATCH (d:Doc) WITH collect(d) AS held UNWIND held AS m "
        "RETURN m.title AS title, vector_score(m, 'summary_emb', [1.0, 0.0]) AS score, "
        "embedding_norm(m, 'summary_emb') AS norm, "
        "text_score(m, 'summary', [1.0, 0.0]) AS rewritten ORDER BY title"
    ).to_list()
    assert rows == [
        {"title": "a", "score": 1.0, "norm": 1.0, "rewritten": 1.0},
        {"title": "b", "score": 0.0, "norm": 1.0, "rewritten": 0.0},
    ]

    inline = graph.cypher(
        "MATCH (d:Doc) WHERE d.title = 'a' WITH collect(d) AS held "
        "RETURN vector_score(head(held), 'summary_emb', [1.0, 0.0]) AS score, "
        "embedding_norm(held[0], 'summary_emb') AS norm"
    ).to_list()
    assert inline == [{"score": 1.0, "norm": 1.0}]

    subquery = graph.cypher(
        "CALL { MATCH (d:Doc) WHERE d.title = 'b' RETURN d AS m } "
        "RETURN vector_score(m, 'summary_emb', [0.0, 1.0]) AS score"
    ).to_list()
    assert subquery == [{"score": 1.0}]


def test_path_node_values_score_like_direct_bindings() -> None:
    graph = _axis_docs()
    graph.cypher("MATCH (a:Doc), (b:Doc) WHERE a.id = 0 AND b.id = 1 CREATE (a)-[:LINKS]->(b)")
    rows = graph.cypher(
        "MATCH p = (:Doc)-[:LINKS]->(:Doc) UNWIND nodes(p) AS m "
        "RETURN m.title AS title, vector_score(m, 'summary_emb', [1.0, 0.0]) AS score ORDER BY title"
    ).to_list()
    assert rows == [{"title": "a", "score": 1.0}, {"title": "b", "score": 0.0}]


def test_a_node_value_whose_slot_died_scores_null() -> None:
    """A value is a snapshot; a snapshot of a deleted node has no score."""
    graph = _axis_docs()
    rows = graph.cypher(
        "MATCH (d:Doc) WHERE d.title = 'a' DELETE d WITH collect(d) AS held UNWIND held AS m "
        "RETURN vector_score(m, 'summary_emb', [1.0, 0.0]) AS score, "
        "embedding_norm(m, 'summary_emb') AS norm"
    ).to_list()
    assert rows == [{"score": None, "norm": None}]
