"""Cypher-first relationship embedding acceptance coverage."""

from __future__ import annotations

import math

import pytest

import kglite
from kglite import KnowledgeGraph


class _Embedder:
    def __init__(self, model_id: str, dimension: int = 2, *, fail: BaseException | None = None):
        self.dimension = dimension
        self.model_id = model_id
        self.fail = fail
        self.loads = 0
        self.calls: list[list[str]] = []
        self.unloads = 0

    def load(self) -> None:
        self.loads += 1

    def embed(self, texts: list[str]) -> list[list[float]]:
        self.calls.append(list(texts))
        if self.fail is not None:
            raise self.fail
        return [self._vector(text) for text in texts]

    def unload(self) -> None:
        self.unloads += 1

    def _vector(self, text: str) -> list[float]:
        seed = float(sum(text.encode()) % 17 + 1)
        vector = [seed + offset for offset in range(self.dimension)]
        norm = math.sqrt(sum(value * value for value in vector))
        return [value / norm for value in vector]


def _graph() -> KnowledgeGraph:
    graph = KnowledgeGraph()
    graph.cypher(
        "CREATE (a:Doc {id: 1}), (b:Doc {id: 2}), (c:Doc {id: 3}), "
        "(a)-[:CLAIMS {text: 'alpha'}]->(b), "
        "(a)-[:CLAIMS {text: 'beta'}]->(b), "
        "(b)-[:CLAIMS]->(c)"
    )
    return graph


def _embed(graph: KnowledgeGraph, where: str = "true", **options: object) -> dict:
    fields = ["type: 'CLAIMS'", "text_property: 'text'", "relationships: relationships"]
    fields.extend(f"{key}: ${key}" for key in options)
    rows = graph.cypher(
        f"MATCH ()-[r:CLAIMS]->() WHERE {where} "
        "WITH collect(r) AS relationships "
        f"CALL db.edge_embeddings.embed({{{', '.join(fields)}}}) "
        "YIELD embedded, skipped, dimension, model "
        "RETURN embedded, skipped, dimension, model",
        params=options,
    ).to_list()
    assert len(rows) == 1
    return rows[0]


def test_empty_selection_is_noop_without_embedder() -> None:
    graph = _graph()
    assert _embed(graph, "false", mode="all") == {
        "embedded": 0,
        "skipped": 0,
        "dimension": 0,
        "model": None,
    }


def test_selected_all_preserves_unselected_and_does_not_relabel_model() -> None:
    graph = _graph()
    model_a = _Embedder("model/A")
    graph.set_embedder(model_a)
    assert _embed(graph, "r.text IS NOT NULL", mode="all")["embedded"] == 2

    before = graph.cypher(
        "MATCH ()-[r:CLAIMS]->() WHERE r.text = 'beta' RETURN vector_score(r, 'text_emb', $query) AS score",
        params={"query": model_a._vector("beta")},
    ).to_list()[0]["score"]
    model_b = _Embedder("model/B")
    graph.set_embedder(model_b)
    report = _embed(graph, "r.text = 'alpha'", mode="all")
    assert report["embedded"] == 1
    assert report["model"] is None
    after = graph.cypher(
        "MATCH ()-[r:CLAIMS]->() WHERE r.text = 'beta' RETURN vector_score(r, 'text_emb', $query) AS score",
        params={"query": model_a._vector("beta")},
    ).to_list()[0]["score"]
    assert after == pytest.approx(before)


def test_list_reports_relationship_store_metadata() -> None:
    graph = _graph()
    graph.set_embedder(_Embedder("model/A"))
    _embed(graph, "r.text = 'alpha'", mode="all")
    rows = graph.cypher(
        "CALL db.edge_embeddings.list({type:'CLAIMS', text_property:'text'}) "
        "YIELD entity, type, text_property, store, dimension, count, metric, model, index_state "
        "RETURN entity, type, text_property, store, dimension, count, metric, model, index_state"
    ).to_list()
    assert rows == [
        {
            "entity": "relationship",
            "type": "CLAIMS",
            "text_property": "text",
            "store": "text_emb",
            "dimension": 2,
            "count": 1,
            "metric": "cosine",
            "model": "model/A",
            "index_state": "none",
        }
    ]


def test_full_stored_coverage_allows_dimension_change_with_unembedded_relationship() -> None:
    graph = _graph()
    graph.set_embedder(_Embedder("model/A", 2))
    _embed(graph, "r.text = 'alpha'", mode="all")
    graph.set_embedder(_Embedder("model/B", 3))
    report = _embed(graph, "r.text = 'alpha'", mode="all")
    assert report["embedded"] == 1
    assert report["dimension"] == 3
    assert report["model"] == "model/B"


def test_missing_text_retained_incrementally_and_removed_by_all() -> None:
    graph = _graph()
    graph.cypher(
        "MATCH ()-[r:CLAIMS]->() WHERE r.text IS NULL WITH collect(r) AS relationships "
        "CALL db.edge_embeddings.set({type:'CLAIMS', text_property:'text', "
        "entries:[{relationship:relationships[0], vector:[1.0, 0.0]}]}) "
        "YIELD stored RETURN stored"
    )
    model = _Embedder("model/A")
    graph.set_embedder(model)
    changed = _embed(graph, "r.text IS NULL", mode="changed")
    assert changed["embedded"] == 0
    assert changed["skipped"] == 1
    assert graph.cypher(
        "MATCH ()-[r:CLAIMS]->() WHERE r.text IS NULL RETURN embedding_norm(r,'text_emb') AS n"
    ).to_list()[0]["n"] == pytest.approx(1.0)
    rebuilt = _embed(graph, "r.text IS NULL", mode="all")
    assert rebuilt["embedded"] == 0
    assert rebuilt["skipped"] == 1
    assert (
        graph.cypher("MATCH ()-[r:CLAIMS]->() WHERE r.text IS NULL RETURN embedding_norm(r,'text_emb') AS n").to_list()[
            0
        ]["n"]
        is None
    )


@pytest.mark.parametrize(
    "selection",
    ["[r, r]", "[{id: id(r), type: type(r)}]"],
)
def test_duplicate_and_fabricated_relationship_inputs_are_atomic(selection: str) -> None:
    graph = _graph()
    with pytest.raises(kglite.CypherExecutionError):
        graph.cypher(
            "MATCH ()-[r:CLAIMS]->() WITH r LIMIT 1 "
            f"CALL db.edge_embeddings.remove({{type:'CLAIMS', text_property:'text', relationships:{selection}}}) "
            "YIELD removed RETURN removed"
        )


def test_deleted_relationship_binding_is_stale_before_embedding_write() -> None:
    graph = _graph()
    with pytest.raises(kglite.CypherExecutionError, match="stale|relationship"):
        graph.cypher(
            "MATCH ()-[r:CLAIMS]->() WITH r LIMIT 1 DELETE r "
            "CALL db.edge_embeddings.remove({type:'CLAIMS', text_property:'text', relationships:[r]}) "
            "YIELD removed RETURN removed"
        )


def test_callback_failure_unloads_and_rolls_back_preceding_write() -> None:
    graph = _graph()
    model = _Embedder("model/A", fail=RuntimeError("model exploded"))
    graph.set_embedder(model)
    with pytest.raises(kglite.CypherExecutionError, match="model exploded"):
        graph.cypher(
            "MATCH ()-[r:CLAIMS]->() WHERE r.text = 'alpha' SET r.marker = 1 "
            "WITH collect(r) AS relationships "
            "CALL db.edge_embeddings.embed({type:'CLAIMS', text_property:'text', "
            "relationships:relationships, mode:'all'}) "
            "YIELD embedded RETURN embedded"
        )
    assert model.loads == 1
    assert model.unloads == 1
    assert (
        graph.cypher("MATCH ()-[r:CLAIMS]->() WHERE r.text = 'alpha' RETURN r.marker AS marker").to_list()[0]["marker"]
        is None
    )


def test_embed_reads_source_text_after_same_statement_set() -> None:
    graph = _graph()
    model = _Embedder("model/A")
    graph.set_embedder(model)
    rows = graph.cypher(
        "MATCH ()-[r:CLAIMS]->() WHERE r.text = 'alpha' "
        "SET r.text = 'updated' WITH collect(r) AS relationships "
        "CALL db.edge_embeddings.embed({type:'CLAIMS', text_property:'text', "
        "relationships:relationships, mode:'all'}) "
        "YIELD embedded RETURN embedded"
    ).to_list()
    assert rows == [{"embedded": 1}]
    assert model.calls == [["updated"]]


def test_mutable_graph_callback_reentry_is_refused() -> None:
    graph = _graph()

    class Reentrant(_Embedder):
        def embed(self, texts: list[str]) -> list[list[float]]:
            with pytest.raises(RuntimeError, match="accessed concurrently"):
                graph.cypher("MATCH (n:Doc) RETURN count(n) AS n")
            return super().embed(texts)

    graph.set_embedder(Reentrant("model/A"))
    assert _embed(graph, "r.text = 'alpha'", mode="all")["embedded"] == 1


def test_callback_can_read_committed_state_but_reentrant_write_is_rejected() -> None:
    graph = _graph()

    class Reentrant(_Embedder):
        session: object

        def embed(self, texts: list[str]) -> list[list[float]]:
            assert self.session.cypher("MATCH (n:Doc) RETURN count(n) AS n").to_list() == [{"n": 3}]
            with pytest.raises(kglite.KgError, match="cannot re-enter writes"):
                self.session.execute("CREATE (:Doc {id: 99})")
            return super().embed(texts)

    model = Reentrant("model/A")
    graph.set_embedder(model)
    session = graph.session()
    model.session = session
    rows = session.execute(
        "MATCH ()-[r:CLAIMS]->() WHERE r.text = 'alpha' "
        "WITH collect(r) AS relationships "
        "CALL db.edge_embeddings.embed({type:'CLAIMS', text_property:'text', "
        "relationships:relationships, mode:'all'}) "
        "YIELD embedded RETURN embedded"
    ).to_list()
    assert rows == [{"embedded": 1}]


@pytest.mark.parametrize(
    "query",
    [
        "CALL { MATCH ()-[r:CLAIMS]->() WHERE r.text = 'alpha' "
        "WITH collect(r) AS relationships "
        "CALL db.edge_embeddings.embed({type:'CLAIMS', text_property:'text', "
        "relationships:relationships, mode:'all'}) "
        "YIELD embedded RETURN embedded } RETURN embedded",
        "RETURN 0 AS embedded UNION ALL "
        "MATCH ()-[r:CLAIMS]->() WHERE r.text = 'alpha' "
        "WITH collect(r) AS relationships "
        "CALL db.edge_embeddings.embed({type:'CLAIMS', text_property:'text', "
        "relationships:relationships, mode:'all'}) "
        "YIELD embedded RETURN embedded",
    ],
)
def test_nested_mutating_embedding_call_obeys_existing_write_rejection(query: str) -> None:
    graph = _graph()
    model = _Embedder("model/A")
    graph.set_embedder(model)
    session = graph.session()
    with pytest.raises(kglite.CypherExecutionError, match="cannot run on the read path"):
        session.execute(query)
    assert model.calls == []


def test_read_subquery_can_return_relationships_to_top_level_embed() -> None:
    graph = _graph()
    model = _Embedder("model/A")
    graph.set_embedder(model)
    rows = graph.cypher(
        "CALL { MATCH ()-[r:CLAIMS]->() WHERE r.text = 'alpha' "
        "RETURN collect(r) AS relationships } "
        "CALL db.edge_embeddings.embed({type:'CLAIMS', text_property:'text', "
        "relationships:relationships, mode:'all'}) "
        "YIELD embedded RETURN embedded"
    ).to_list()
    assert rows == [{"embedded": 1}]
    assert model.calls == [["alpha"]]


def _store_state(graph: KnowledgeGraph) -> list[dict]:
    return graph.cypher(
        "CALL db.edge_embeddings.list({type:'CLAIMS', text_property:'text'}) "
        "YIELD count, dimension, metric, model, index_state, delta "
        "RETURN count, dimension, metric, model, index_state, delta"
    ).to_list()


def _vectors(graph: KnowledgeGraph) -> list[dict]:
    return graph.cypher(
        "MATCH ()-[r:CLAIMS]->() "
        "RETURN r.text AS text, embedding_norm(r,'text_emb') AS norm, "
        "vector_score(r,'text_emb',[1.0,0.0]) AS score ORDER BY text"
    ).to_list()


def _failing_tail(yielded: str) -> str:
    """A clause that fails *after* the embedding write above it.

    `c` still has an incoming CLAIMS, so the non-DETACH delete is refused —
    which is what makes these rollback tests rather than validation tests.
    """
    return f"WITH {yielded} AS kept MATCH (n:Doc {{id: 3}}) DELETE n RETURN kept"


def test_failed_statement_reverses_a_manual_relationship_vector_set() -> None:
    graph = _graph()
    graph.set_embedder(_Embedder("model/A"))
    _embed(graph, "true", mode="all")
    graph.cypher("CALL db.edge_embeddings.build_index({type:'CLAIMS', text_property:'text'})")
    before_state = _store_state(graph)
    before_vectors = _vectors(graph)
    assert before_state[0]["index_state"] == "online"

    with pytest.raises(kglite.CypherExecutionError):
        graph.cypher(
            "MATCH ()-[r:CLAIMS]->() WHERE r.text = 'alpha' "
            "CALL db.edge_embeddings.set({type:'CLAIMS', text_property:'text', "
            "entries:[{relationship:r, vector:[0.25, 0.75]}]}) YIELD stored " + _failing_tail("stored")
        )

    assert _store_state(graph) == before_state
    assert _vectors(graph) == before_vectors


def test_failed_statement_reverses_a_manual_relationship_vector_removal() -> None:
    graph = _graph()
    graph.set_embedder(_Embedder("model/A"))
    _embed(graph, "true", mode="all")
    graph.cypher("CALL db.edge_embeddings.build_index({type:'CLAIMS', text_property:'text'})")
    before_state = _store_state(graph)
    before_vectors = _vectors(graph)

    with pytest.raises(kglite.CypherExecutionError):
        graph.cypher(
            "MATCH ()-[r:CLAIMS]->() WHERE r.text = 'alpha' "
            "CALL db.edge_embeddings.remove({type:'CLAIMS', text_property:'text', "
            "relationships:[r]}) YIELD removed " + _failing_tail("removed")
        )

    assert _store_state(graph) == before_state
    assert _vectors(graph) == before_vectors


def test_failed_statement_leaves_no_store_the_set_created() -> None:
    graph = _graph()
    assert _store_state(graph) == []

    with pytest.raises(kglite.CypherExecutionError):
        graph.cypher(
            "MATCH ()-[r:CLAIMS]->() WHERE r.text = 'alpha' "
            "CALL db.edge_embeddings.set({type:'CLAIMS', text_property:'text', "
            "entries:[{relationship:r, vector:[1.0, 0.0]}]}) YIELD stored " + _failing_tail("stored")
        )

    assert _store_state(graph) == [], "a rolled-back set must not leave the store it created"
