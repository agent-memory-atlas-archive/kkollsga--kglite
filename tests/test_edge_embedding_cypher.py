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
        f"CALL db.relationship_embeddings.embed({{{', '.join(fields)}}}) "
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
        "CALL db.relationship_embeddings.list({type:'CLAIMS', text_property:'text'}) "
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
        "CALL db.relationship_embeddings.set({type:'CLAIMS', text_property:'text', "
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


def _vectors_on_alpha_and_beta(graph: KnowledgeGraph) -> None:
    for text, vector in [("alpha", [1.0, 0.0]), ("beta", [0.0, 1.0])]:
        graph.cypher(
            "MATCH ()-[r:CLAIMS {text: $text}]->() "
            "CALL db.relationship_embeddings.set({type:'CLAIMS', text_property:'text', "
            "entries:[{relationship:r, vector:$vector}]}) YIELD stored RETURN stored",
            params={"text": text, "vector": vector},
        )


@pytest.mark.parametrize(
    ("selection", "params", "message"),
    [
        ("[r, r]", {}, "appears more than once"),
        ("[{id: id(r), type: type(r)}]", {}, "expected a relationship value"),
        # A relationship that left the engine as a result row and came back as
        # a parameter is a map, not a bound relationship — the round trip
        # strips the statement identity the procedures require.
        ("$rels", "round_tripped", "expected a relationship value"),
    ],
)
def test_duplicate_and_fabricated_relationship_inputs_are_atomic(
    selection: str, params: dict | str, message: str
) -> None:
    graph = _graph()
    _vectors_on_alpha_and_beta(graph)
    if params == "round_tripped":
        params = {"rels": [graph.cypher("MATCH ()-[r:CLAIMS]->() RETURN r LIMIT 1").to_list()[0]["r"]]}
    with pytest.raises(kglite.CypherExecutionError, match=message):
        graph.cypher(
            "MATCH ()-[r:CLAIMS]->() WITH r LIMIT 1 "
            f"CALL db.relationship_embeddings.remove({{type:'CLAIMS', text_property:'text', "
            f"relationships:{selection}}}) "
            "YIELD removed RETURN removed",
            params=params,
        )
    assert graph.cypher(
        "CALL db.relationship_embeddings.list({type:'CLAIMS', text_property:'text'}) YIELD count RETURN count"
    ).to_list() == [{"count": 2}], "a refused selection removes nothing"


def test_text_score_with_a_literal_vector_matches_vector_score_on_a_relationship() -> None:
    graph = _graph()
    _vectors_on_alpha_and_beta(graph)
    rows = graph.cypher(
        "MATCH ()-[r:CLAIMS]->() WHERE r.text IS NOT NULL "
        "RETURN r.text AS text, text_score(r,'text',[0.6,0.8]) AS text_score, "
        "vector_score(r,'text_emb',[0.6,0.8]) AS vector_score ORDER BY text"
    ).to_list()
    assert rows == [
        {"text": "alpha", "text_score": pytest.approx(0.6), "vector_score": pytest.approx(0.6)},
        {"text": "beta", "text_score": pytest.approx(0.8), "vector_score": pytest.approx(0.8)},
    ]


def test_deleted_relationship_binding_is_stale_before_embedding_write() -> None:
    graph = _graph()
    with pytest.raises(kglite.CypherExecutionError, match="stale|relationship"):
        graph.cypher(
            "MATCH ()-[r:CLAIMS]->() WITH r LIMIT 1 DELETE r "
            "CALL db.relationship_embeddings.remove({type:'CLAIMS', text_property:'text', relationships:[r]}) "
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
            "CALL db.relationship_embeddings.embed({type:'CLAIMS', text_property:'text', "
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
        "CALL db.relationship_embeddings.embed({type:'CLAIMS', text_property:'text', "
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
        "CALL db.relationship_embeddings.embed({type:'CLAIMS', text_property:'text', "
        "relationships:relationships, mode:'all'}) "
        "YIELD embedded RETURN embedded"
    ).to_list()
    assert rows == [{"embedded": 1}]


@pytest.mark.parametrize(
    "query",
    [
        "CALL { MATCH ()-[r:CLAIMS]->() WHERE r.text = 'alpha' "
        "WITH collect(r) AS relationships "
        "CALL db.relationship_embeddings.embed({type:'CLAIMS', text_property:'text', "
        "relationships:relationships, mode:'all'}) "
        "YIELD embedded RETURN embedded } RETURN embedded",
        "RETURN 0 AS embedded UNION ALL "
        "MATCH ()-[r:CLAIMS]->() WHERE r.text = 'alpha' "
        "WITH collect(r) AS relationships "
        "CALL db.relationship_embeddings.embed({type:'CLAIMS', text_property:'text', "
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
        "CALL db.relationship_embeddings.embed({type:'CLAIMS', text_property:'text', "
        "relationships:relationships, mode:'all'}) "
        "YIELD embedded RETURN embedded"
    ).to_list()
    assert rows == [{"embedded": 1}]
    assert model.calls == [["alpha"]]


def _store_state(graph: KnowledgeGraph) -> list[dict]:
    return graph.cypher(
        "CALL db.relationship_embeddings.list({type:'CLAIMS', text_property:'text'}) "
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
    graph.cypher("CALL db.relationship_embeddings.build_index({type:'CLAIMS', text_property:'text'})")
    before_state = _store_state(graph)
    before_vectors = _vectors(graph)
    assert before_state[0]["index_state"] == "online"

    with pytest.raises(kglite.CypherExecutionError):
        graph.cypher(
            "MATCH ()-[r:CLAIMS]->() WHERE r.text = 'alpha' "
            "CALL db.relationship_embeddings.set({type:'CLAIMS', text_property:'text', "
            "entries:[{relationship:r, vector:[0.25, 0.75]}]}) YIELD stored " + _failing_tail("stored")
        )

    assert _store_state(graph) == before_state
    assert _vectors(graph) == before_vectors


def test_failed_statement_reverses_a_manual_relationship_vector_removal() -> None:
    graph = _graph()
    graph.set_embedder(_Embedder("model/A"))
    _embed(graph, "true", mode="all")
    graph.cypher("CALL db.relationship_embeddings.build_index({type:'CLAIMS', text_property:'text'})")
    before_state = _store_state(graph)
    before_vectors = _vectors(graph)

    with pytest.raises(kglite.CypherExecutionError):
        graph.cypher(
            "MATCH ()-[r:CLAIMS]->() WHERE r.text = 'alpha' "
            "CALL db.relationship_embeddings.remove({type:'CLAIMS', text_property:'text', "
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
            "CALL db.relationship_embeddings.set({type:'CLAIMS', text_property:'text', "
            "entries:[{relationship:r, vector:[1.0, 0.0]}]}) YIELD stored " + _failing_tail("stored")
        )

    assert _store_state(graph) == [], "a rolled-back set must not leave the store it created"


def test_path_relationship_equals_the_bound_relationship_in_a_write_statement() -> None:
    """The same edge reached two ways is one value, inside a write as outside it.

    The relationship value's equality compared a transient per-statement
    identity that only a bound variable carried, so ``r = relationships(p)[0]``
    and ``r IN relationships(p)`` answered ``false`` whenever the statement also
    wrote -- and answered ``true`` in the read-only form of the same query.
    """
    graph = _graph()
    read = graph.cypher(
        "MATCH p = (a:Doc)-[r:CLAIMS]->(b:Doc) WHERE r.text = 'alpha' "
        "RETURN r = relationships(p)[0] AS eq, r IN relationships(p) AS member"
    ).to_list()
    assert read == [{"eq": True, "member": True}]

    written = graph.cypher(
        "MATCH p = (a:Doc)-[r:CLAIMS]->(b:Doc) WHERE r.text = 'alpha' SET a.touched = 1 "
        "WITH p, r RETURN r = relationships(p)[0] AS eq, r IN relationships(p) AS member"
    ).to_list()
    assert written == [{"eq": True, "member": True}]


def test_distinct_folds_bound_and_path_relationships_into_one_group() -> None:
    """``DISTINCT`` counted the same edge twice inside a write statement."""
    graph = _graph()
    rows = graph.cypher(
        "MATCH p = (a:Doc)-[r:CLAIMS]->(b:Doc) WHERE r.text = 'alpha' SET a.touched = 1 "
        "WITH p, r UNWIND [r, relationships(p)[0]] AS x "
        "RETURN size(collect(DISTINCT x)) AS distinct_count"
    ).to_list()
    assert rows == [{"distinct_count": 1}]


def test_membership_against_path_relationships_selects_the_edge_for_delete() -> None:
    """``WHERE r IN rels DELETE r`` deleted nothing while equality saw identity."""
    graph = _graph()
    graph.cypher(
        "MATCH p = (a:Doc)-[r:CLAIMS]->(b:Doc) WHERE r.text = 'alpha' SET a.touched = 1 "
        "WITH collect(relationships(p)[0]) AS rels "
        "MATCH ()-[r:CLAIMS]->() WHERE r IN rels DELETE r RETURN size(rels) AS listed"
    )
    remaining = graph.cypher("MATCH ()-[r:CLAIMS]->() RETURN count(*) AS n").to_list()
    assert remaining == [{"n": 2}]


def test_path_relationships_are_accepted_by_set_and_remove() -> None:
    """Path-derived relationships were refused as not bound by this statement."""
    graph = _graph()
    stored = graph.cypher(
        "MATCH p = (a:Doc)-[r:CLAIMS]->(b:Doc) WHERE r.text = 'alpha' "
        "WITH p, relationships(p)[0] AS pr "
        "CALL db.relationship_embeddings.set({type: 'CLAIMS', text_property: 'text', "
        "entries: [{relationship: pr, vector: [1.0, 0.0]}]}) YIELD stored RETURN stored"
    ).to_list()
    assert stored == [{"stored": 1}]

    removed = graph.cypher(
        "MATCH p = (a:Doc)-[r:CLAIMS]->(b:Doc) WHERE r.text = 'alpha' WITH p "
        "CALL db.relationship_embeddings.remove({type: 'CLAIMS', text_property: 'text', "
        "relationships: [relationships(p)[0]]}) YIELD removed RETURN removed"
    ).to_list()
    assert removed == [{"removed": 1}]


def test_variable_length_path_relationships_are_accepted_by_embed() -> None:
    """A variable-length path's relationships exist only as materialised values.

    There is no bound relationship variable to fall back on, so the refusal made
    the whole shape unreachable. Both two-hop paths end at the text-less
    ``(b)-[:CLAIMS]->(c)``, which is skipped, so each row embeds exactly one.
    """
    graph = _graph()
    graph.set_embedder(_Embedder("model/A"))
    rows = graph.cypher(
        "MATCH p = (a:Doc)-[:CLAIMS*2..2]->(c:Doc) WITH relationships(p) AS rels "
        "CALL db.relationship_embeddings.embed({type: 'CLAIMS', text_property: 'text', "
        "relationships: rels}) YIELD embedded RETURN embedded"
    ).to_list()
    assert [row["embedded"] for row in rows] == [1, 1]


def test_path_relationship_deleted_earlier_in_the_statement_is_refused() -> None:
    """A retired slot must not be written through the path that named it."""
    graph = _graph()
    with pytest.raises(Exception, match="db.relationship_embeddings.set"):
        graph.cypher(
            "MATCH p = (a:Doc)-[r:CLAIMS]->(b:Doc) WHERE r.text = 'alpha' DELETE r WITH p "
            "CALL db.relationship_embeddings.set({type: 'CLAIMS', text_property: 'text', "
            "entries: [{relationship: relationships(p)[0], vector: [1.0, 0.0]}]}) "
            "YIELD stored RETURN stored"
        )
    assert _store_state(graph) == []


def test_path_relationships_score_inside_a_write_statement() -> None:
    """``vector_score`` on a projected relationship demands this statement's token."""
    graph = _graph()
    graph.cypher(
        "MATCH ()-[r:CLAIMS]->() WHERE r.text = 'alpha' "
        "CALL db.relationship_embeddings.set({type: 'CLAIMS', text_property: 'text', "
        "entries: [{relationship: r, vector: [1.0, 0.0]}]}) YIELD stored RETURN stored"
    )
    rows = graph.cypher(
        "MATCH p = (a:Doc)-[r:CLAIMS]->(b:Doc) WHERE r.text = 'alpha' SET a.touched = 1 "
        "WITH relationships(p) AS rels UNWIND rels AS rel "
        "RETURN vector_score(rel, 'text_emb', [1.0, 0.0]) AS score, "
        "embedding_norm(rel, 'text_emb') AS norm"
    ).to_list()
    assert rows == [{"score": 1.0, "norm": 1.0}]


def test_default_mode_stamps_the_model_on_a_store_it_creates() -> None:
    """A fresh store records the model that filled it, in every mode.

    The default ``missing`` mode wrote ``model: null`` on a store it had just
    created whole, and the mismatch guard reads the *prior* stamp -- so a later
    same-width model was free to mix its vectors into the same store.
    """
    graph = _graph()
    graph.set_embedder(_Embedder("model/A"))
    report = _embed(graph, "r.text IS NOT NULL")
    assert report["embedded"] == 2
    assert report["model"] == "model/A"
    assert _store_state(graph)[0]["model"] == "model/A"

    graph.set_embedder(_Embedder("model/B"))
    for mode in ("changed", "missing"):
        with pytest.raises(kglite.CypherExecutionError, match="mode='all'"):
            _embed(graph, "r.text IS NOT NULL", mode=mode)
    assert _store_state(graph)[0]["model"] == "model/A"


def test_manual_set_of_an_identical_vector_takes_ownership_of_the_cell() -> None:
    """A manual vector owns its cell even when it equals the generated one.

    The batch was filtered by vector equality, so a byte-identical write never
    cleared the generated text hash and ``mode='changed'`` went on skipping a
    relationship the manual write had taken over.
    """
    graph = _graph()
    model = _Embedder("model/A")
    graph.set_embedder(model)
    assert _embed(graph, "r.text = 'alpha'", mode="all")["embedded"] == 1

    graph.cypher(
        "MATCH ()-[r:CLAIMS]->() WHERE r.text = 'alpha' "
        "CALL db.relationship_embeddings.set({type:'CLAIMS', text_property:'text', "
        "entries:[{relationship:r, vector:$vector}]}) YIELD stored RETURN stored",
        params={"vector": model._vector("alpha")},
    )
    assert _store_state(graph)[0]["model"] is None

    assert _embed(graph, "r.text = 'alpha'", mode="changed")["embedded"] == 1


def test_failed_statement_reverses_a_relationship_delete_with_its_vector_index() -> None:
    """A rolled-back DELETE leaves the HNSW index where it found it.

    The prune invalidates the index and the undo's restore invalidates it
    again, so the failed statement silently dropped an index it never touched.
    """
    graph = _graph()
    graph.set_embedder(_Embedder("model/A"))
    _embed(graph, "true", mode="all")
    graph.cypher("CALL db.relationship_embeddings.build_index({type:'CLAIMS', text_property:'text'})")
    before_state = _store_state(graph)
    before_vectors = _vectors(graph)
    assert before_state[0]["index_state"] == "online"

    with pytest.raises(kglite.CypherExecutionError):
        graph.cypher("MATCH ()-[r:CLAIMS]->() WHERE r.text = 'alpha' DELETE r " + _failing_tail("1"))

    assert _store_state(graph) == before_state
    assert _vectors(graph) == before_vectors


#: Every ``db.relationship_embeddings.*`` procedure with the extra required arguments
#: its call needs beyond ``type``/``text_property``.
_PROCEDURE_ARGUMENTS = {
    "set": "entries: []",
    "remove": "relationships: []",
    "embed": "relationships: []",
    "query": "vector: [1.0, 0.0]",
    "drop": None,
    "build_index": None,
    "refresh_index": None,
    "drop_index": None,
    "list": None,
}


@pytest.mark.parametrize("procedure", sorted(_PROCEDURE_ARGUMENTS))
def test_every_edge_embedding_procedure_refuses_an_unknown_parameter(procedure: str) -> None:
    """Only ``list`` rejected unknown keys; the other eight ignored them.

    A silently-ignored key leaves the default in place and reports success --
    ``{metric_: 'euclidean'}`` built a cosine index and answered ``indexed``.
    """
    graph = _graph()
    fields = ["type: 'CLAIMS'", "text_property: 'text'"]
    extra = _PROCEDURE_ARGUMENTS[procedure]
    if extra is not None:
        fields.append(extra)
    fields.append("bogus: 1")
    with pytest.raises(Exception, match="unknown parameter 'bogus'"):
        graph.cypher(f"CALL db.relationship_embeddings.{procedure}({{{', '.join(fields)}}})")


# ── db.relationship_embeddings.query({text: …}) ─────────────────────────────────────
# Preparation rewrites `text` into `vector: $__ts_N` and embeds it once with the
# registered embedder, exactly as it embeds a `text_score` query. These are the
# golden checks for the text spelling; the differential corpus registers no
# embedder, so it can exercise only the vector spelling.

_QUERY_ROWS = (
    "YIELD relationship, score, search_method "
    "RETURN relationship.text AS text, score, search_method ORDER BY score DESC, text"
)


def _embedded_graph() -> tuple[KnowledgeGraph, _Embedder]:
    graph = _graph()
    model = _Embedder("model/A")
    graph.set_embedder(model)
    assert _embed(graph)["embedded"] == 2
    model.calls.clear()
    return graph, model


@pytest.mark.parametrize("indexed", [False, True], ids=["exact", "after_build_index"])
def test_query_text_equals_the_vector_query_with_the_embedders_vector(indexed: bool) -> None:
    graph, model = _embedded_graph()
    if indexed:
        graph.cypher("CALL db.relationship_embeddings.build_index({type:'CLAIMS', text_property:'text'})")
    exact = "true" if not indexed else "false"
    by_text = graph.cypher(
        f"CALL db.relationship_embeddings.query({{type:'CLAIMS', text_property:'text', text:$q, exact:{exact}}}) "
        + _QUERY_ROWS,
        params={"q": "alpha"},
    ).to_list()
    assert model.calls == [["alpha"]], "the query text is embedded exactly once"
    by_vector = graph.cypher(
        f"CALL db.relationship_embeddings.query({{type:'CLAIMS', text_property:'text', vector:$v, exact:{exact}}}) "
        + _QUERY_ROWS,
        params={"v": model._vector("alpha")},
    ).to_list()
    assert by_text == by_vector
    assert [row["text"] for row in by_text] == ["alpha", "beta"]
    assert by_text[0]["score"] == pytest.approx(1.0)
    assert {row["search_method"] for row in by_text} == {"hnsw" if indexed else "exact"}


def test_query_text_literal_through_a_session_write_is_embedded_once() -> None:
    graph, model = _embedded_graph()
    session = graph.session()
    rows = session.execute(
        "CALL db.relationship_embeddings.query({type:'CLAIMS', text_property:'text', text:'beta', top_k:1}) "
        "YIELD relationship SET relationship.hit = true RETURN relationship.text AS text"
    ).to_list()
    assert rows == [{"text": "beta"}]
    assert model.calls == [["beta"]]
    assert session.execute("MATCH ()-[r:CLAIMS {hit: true}]->() RETURN r.text AS text").to_list() == [{"text": "beta"}]


@pytest.mark.parametrize(
    ("options", "message"),
    [
        ("text:'alpha', vector:[1.0, 0.0]", "'text' and 'vector' are mutually exclusive"),
        ("text:[1.0, 0.0]", "'text' must be a string literal or a \\$parameter"),
        ("text:$q", "parameter \\$q for 'text' must be a string"),
    ],
)
def test_query_text_refuses_a_second_query_or_a_non_text_value(options: str, message: str) -> None:
    graph, model = _embedded_graph()
    with pytest.raises(Exception, match=message):
        graph.cypher(
            f"CALL db.relationship_embeddings.query({{type:'CLAIMS', text_property:'text', {options}}}) "
            "YIELD relationship RETURN relationship",
            params={"q": [1.0, 0.0]},
        )
    assert model.calls == []


def test_query_text_from_a_row_is_refused() -> None:
    graph, model = _embedded_graph()
    with pytest.raises(Exception, match="cannot depend on a row"):
        graph.cypher(
            "WITH 'alpha' AS t "
            "CALL db.relationship_embeddings.query({type:'CLAIMS', text_property:'text', text:t}) "
            "YIELD relationship RETURN relationship"
        )
    assert model.calls == []


def test_query_text_without_an_embedder_names_the_procedure() -> None:
    graph = _graph()
    _vectors_on_alpha_and_beta(graph)
    message = r"db\.relationship_embeddings\.query\(\{text: \.\.\.\}\) requires a registered embedding model"
    with pytest.raises(Exception, match=message):
        graph.cypher(
            "CALL db.relationship_embeddings.query({type:'CLAIMS', text_property:'text', text:'alpha'}) "
            "YIELD relationship RETURN relationship"
        )


def test_query_unknown_parameter_refusal_lists_text() -> None:
    graph = _graph()
    with pytest.raises(Exception, match=r"Accepted: type, types, text_property, vector, text, top_k"):
        graph.cypher(
            "CALL db.relationship_embeddings.query({type:'CLAIMS', text_property:'text', vector:[1.0, 0.0], bogus:1})"
        )


# ── Errors a user can act on ────────────────────────────────────────────────
# A blank-slate user test found `set`/`embed` creating a store for a property no
# relationship carries, and vector-write errors naming an internal slot number.


def _no_claims_store(graph: KnowledgeGraph) -> None:
    assert _store_state(graph) == []
    assert [row for row in graph.list_embeddings() if row["entity"] == "relationship"] == []
    assert "embeddings=" not in graph.describe(connections=["CLAIMS"])


@pytest.mark.parametrize("procedure", ["set", "embed"])
def test_a_text_property_no_relationship_carries_is_refused_before_a_store_exists(
    procedure: str,
) -> None:
    graph = _graph()
    graph.set_embedder(_Embedder("model/A"))
    if procedure == "set":
        call = (
            "MATCH ()-[r:CLAIMS]->() WITH r LIMIT 1 CALL db.relationship_embeddings.set("
            "{type:'CLAIMS', text_property:'txet', entries:[{relationship:r, vector:[1.0, 0.0]}]}) "
            "YIELD stored RETURN stored"
        )
    else:
        call = (
            "MATCH ()-[r:CLAIMS]->() WITH collect(r) AS rs CALL db.relationship_embeddings.embed("
            "{type:'CLAIMS', text_property:'txet', relationships: rs}) YIELD embedded RETURN embedded"
        )
    with pytest.raises(kglite.CypherExecutionError) as error:
        graph.cypher(call)
    message = str(error.value)
    assert f"CALL db.relationship_embeddings.{procedure}" in message
    assert "Text property 'txet' not found on any 'CLAIMS' relationship" in message
    assert "'CLAIMS' relationships carry: text" in message
    graph.cypher("MATCH ()-[r:CLAIMS]->() RETURN count(r)")  # the graph still answers
    assert [row for row in graph.list_embeddings() if row.get("text_column") == "txet"] == []
    assert "txet" not in graph.describe(connections=["CLAIMS"])


def test_a_property_only_some_relationships_carry_is_still_accepted() -> None:
    graph = _graph()
    graph.set_embedder(_Embedder("model/A"))
    # The third CLAIMS relationship carries no `text`; two do.
    assert _embed(graph)["embedded"] == 2


def _claims_set(graph: KnowledgeGraph, entries: str, params: dict | None = None) -> None:
    graph.cypher(
        "MATCH (:Doc {id: 1})-[r:CLAIMS]->(:Doc {id: 2}) WITH collect(r) AS rs "
        f"CALL db.relationship_embeddings.set({{type:'CLAIMS', text_property:'text', entries:{entries}}}) "
        "YIELD stored RETURN stored",
        params=params or {},
    )


@pytest.mark.parametrize(
    ("entries", "params", "expected"),
    [
        (
            "[{relationship: rs[0], vector: [1.0, 0.0]}, {relationship: rs[1], vector: [1.0]}]",
            None,
            "Embedding for relationship (Doc id=1)-[:CLAIMS]->(Doc id=2) (entries[1]) has dimension 1, expected 2",
        ),
        (
            "[{relationship: rs[0], vector: $v}]",
            {"v": [float("nan"), 0.0]},
            "Invalid embedding for relationship (Doc id=1)-[:CLAIMS]->(Doc id=2) (entries[0]): "
            "vector coordinate 0 must be finite",
        ),
        (
            "[{relationship: rs[0], vector: [1.0, 0.0]}, {relationship: rs[0], vector: [0.0, 1.0]}]",
            None,
            "relationship (Doc id=1)-[:CLAIMS]->(Doc id=2) appears more than once (entries[0] and entries[1])",
        ),
    ],
    ids=["dimension", "non-finite", "repeated"],
)
def test_vector_write_errors_name_the_relationship_not_a_slot(entries: str, params: dict | None, expected: str) -> None:
    graph = _graph()
    with pytest.raises(kglite.CypherExecutionError) as error:
        _claims_set(graph, entries, params)
    message = str(error.value)
    assert expected in message
    assert "slot" not in message
    _no_claims_store(graph)


def test_embedder_output_errors_name_the_relationship_not_a_slot() -> None:
    class _WrongDimension(_Embedder):
        def embed(self, texts: list[str]) -> list[list[float]]:
            return [[1.0, 0.0, 0.0] for _ in texts]

    graph = _graph()
    graph.set_embedder(_WrongDimension("model/A"))
    with pytest.raises(kglite.CypherExecutionError) as error:
        _embed(graph, "r.text = 'alpha'")
    message = str(error.value)
    assert "(Doc id=1)-[:CLAIMS]->(Doc id=2)" in message
    assert "slot" not in message


def test_a_relationship_deleted_earlier_is_named_by_position_not_slot() -> None:
    graph = _graph()
    with pytest.raises(kglite.CypherExecutionError) as error:
        graph.cypher(
            "MATCH ()-[r:CLAIMS]->() WITH r LIMIT 1 DELETE r "
            "CALL db.relationship_embeddings.remove({type:'CLAIMS', text_property:'text', relationships:[r]}) "
            "YIELD removed RETURN removed"
        )
    message = str(error.value)
    assert "relationships[0]" in message and "deleted" in message
    assert "slot" not in message
