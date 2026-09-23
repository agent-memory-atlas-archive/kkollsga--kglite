"""Python binding contracts for embedding ownership, callbacks, and metadata."""

from __future__ import annotations

import pytest

import kglite


class _TrackingEmbedder:
    model_id = "test/model-v1"

    def __init__(self, outputs: list[list[float]] | None = None, dimension: int = 2) -> None:
        self.calls = 0
        self.outputs = outputs
        self.dimension = dimension

    def embed(self, texts: list[str]) -> list[list[float]]:
        self.calls += 1
        if self.outputs is not None:
            return self.outputs
        return [[1.0, 0.0] for _ in texts]


def _searchable_graph() -> kglite.KnowledgeGraph:
    graph = kglite.KnowledgeGraph()
    graph.cypher("CREATE (:Doc {id: 1, title: 'one', body: 'alpha'})")
    graph.set_embeddings("Doc", "body", {1: [1.0, 0.0]})
    return graph


def test_embed_texts_refuses_a_derived_durable_handle_before_model_work(tmp_path) -> None:
    path = tmp_path / "embedding-owner.kgl"
    owner = kglite.open(str(path), durable="full")
    owner.cypher("CREATE (:Doc {id: 1, title: 'one', body: 'alpha'})")
    view = owner.select("Doc")
    model = _TrackingEmbedder()
    view.set_embedder(model)

    with pytest.raises(ValueError, match="derived from a durable graph"):
        view.embed_texts("Doc", "body", show_progress=False)

    assert model.calls == 0
    assert view.embedding_info("Doc", "body") is None
    assert owner.embedding_info("Doc", "body") is None


@pytest.mark.parametrize(
    ("outputs", "message"),
    [
        ([], "returned 0 vectors for 1 texts"),
        ([[1.0, 0.0], [0.0, 1.0]], "returned 2 vectors for 1 texts"),
    ],
)
def test_search_text_rejects_model_output_cardinality(outputs, message) -> None:
    graph = _searchable_graph()
    graph.set_embedder(_TrackingEmbedder(outputs))

    with pytest.raises(ValueError, match=f"search_text: model.embed\\(\\) {message}"):
        graph.select("Doc").search_text("body", "query")


def test_search_text_rejects_model_output_width() -> None:
    graph = kglite.KnowledgeGraph()
    graph.cypher("CREATE (:Doc {id: 1, title: 'one', body: 'alpha'})")
    graph.set_embeddings("Doc", "body", {1: [1.0, 0.0, 0.0]})
    graph.set_embedder(_TrackingEmbedder([[1.0, 0.0, 0.0]], dimension=2))

    with pytest.raises(
        ValueError,
        match="search_text: model.embed\\(\\) returned vector width 3, expected registered model dimension 2",
    ):
        graph.select("Doc").search_text("body", "query")


def test_set_embedder_snapshots_dimension_and_model_identity() -> None:
    graph = kglite.KnowledgeGraph()
    graph.cypher("CREATE (:Doc {id: 1, title: 'one', body: 'alpha'})")
    model = _TrackingEmbedder()
    graph.set_embedder(model)

    # Registration metadata is immutable even though the callback object itself
    # remains shared and may keep ordinary mutable runtime state such as `calls`.
    model.dimension = 3
    model.model_id = "test/model-v2"
    report = graph.embed_texts("Doc", "body", show_progress=False)

    assert report["dimension"] == 2
    assert model.calls == 1
    assert graph.embedding_info("Doc", "body")["model"] == "test/model-v1"
