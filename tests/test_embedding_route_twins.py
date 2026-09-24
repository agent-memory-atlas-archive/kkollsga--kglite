"""The embedding readers and the store remover have a node twin, a
relationship twin and an ``entity=`` router — and none of them answers a
mistake with silence.

``remove_embeddings`` used to return ``None`` for a store that does not exist
(a relationship store on the node route, a typo), and ``embedding`` answered
``None`` whether the store or only the vector was missing. Relationship
stores had no Python search, readout or remover at all. The error messages
name the store the caller probably meant, and a remedy names a call in the
surface the caller used: a method from Python, a procedure from Cypher.
"""

from __future__ import annotations

import numpy as np
import pytest

import kglite
from kglite import KnowledgeGraph

KEYS = {"SUPPORTS": "uid"}
# (type, source, target, uid, text); b and c are a parallel SUPPORTS group 1 -> 20.
EDGES = [
    ("SUPPORTS", 1, 10, "a", "alpha"),
    ("SUPPORTS", 1, 20, "b", "beta"),
    ("SUPPORTS", 1, 20, "c", "gamma"),
    ("SUPPORTS", 2, 10, "d", "delta"),
    ("REFUTES", 2, 20, "e", "epsilon"),
]
REL_VECTORS = {(1, 10, "a"): [1.0, 0.0], (1, 20, "b"): [0.0, 1.0], (1, 20, "c"): [0.6, 0.8], (2, 10, "d"): [0.3, 0.1]}
NODE_VECTORS = {1: [1.0, 0.0], 2: [0.0, 1.0]}


class _Stub:
    dimension = 2
    model_id = "stub/twins"

    def embed(self, texts: list[str]) -> list[list[float]]:
        return [[float(len(text)), float(ord(text[0]) - 96)] for text in texts]


def _graph(*, embedder: bool = True) -> KnowledgeGraph:
    graph = KnowledgeGraph()
    graph.cypher(
        "CREATE (:Claimant {id: 1, note: 'first'}), (:Claimant {id: 2, note: 'second'}), "
        "(:Claim {id: 10}), (:Claim {id: 20})"
    )
    for rel_type, source, target, uid, text in EDGES:
        graph.cypher(
            f"MATCH (s:Claimant {{id: $s}}), (t:Claim {{id: $t}}) "
            f"CREATE (s)-[:{rel_type} {{uid: $uid, evidence: $e}}]->(t)",
            params={"s": source, "t": target, "uid": uid, "e": text},
        )
    graph.set_node_embeddings("Claimant", "note", NODE_VECTORS)
    graph.set_relationship_embeddings("SUPPORTS", "evidence", REL_VECTORS, relationship_keys=KEYS)
    graph.set_relationship_embeddings("REFUTES", "evidence", {(2, 20): [0.9, 0.1]})
    if embedder:
        graph.set_embedder(_Stub())
    return graph


# ── relationship search ──────────────────────────────────────────────────────


def _procedure(graph: KnowledgeGraph, vector, top_k: int, types=None) -> list[tuple]:
    selector = "" if types is None else "types: $types, "
    rows = graph.cypher(
        "CALL db.relationship_embeddings.query({" + selector + "text_column: 'evidence', vector: $v, "
        "top_k: $k}) YIELD relationship, score, type "
        "RETURN startNode(relationship).id AS s, endNode(relationship).id AS t, relationship.uid AS uid, "
        "type AS type, score",
        params={"v": vector, "k": top_k, "types": types},
    ).to_list()
    return [(r["s"], r["t"], r["uid"], r["type"], r["score"]) for r in rows]


def test_relationship_vector_search_is_the_query_procedures_ranking() -> None:
    graph = _graph()
    query = [0.8, 0.6]
    hits = graph.relationship_vector_search("evidence", query, top_k=4, relationship_keys=KEYS)
    assert [(h["source"], h["target"], h["key"], h["relationship_type"], h["score"]) for h in hits] == [
        (s, t, uid if rel == "SUPPORTS" else None, rel, score) for s, t, uid, rel, score in _procedure(graph, query, 4)
    ]
    assert set(hits[0]) == {"source", "target", "source_type", "target_type", "key", "relationship_type", "score"}
    assert hits[0]["source_type"] == "Claimant" and hits[0]["target_type"] == "Claim"
    one_type = graph.relationship_vector_search("evidence", query, top_k=10, types="REFUTES")
    assert [h["relationship_type"] for h in one_type] == ["REFUTES"]
    listed = graph.relationship_vector_search("evidence", query, top_k=10, types=["SUPPORTS"])
    assert [(h["source"], h["target"]) for h in listed] == [
        (s, t) for s, t, *_ in _procedure(graph, query, 10, ["SUPPORTS"])
    ]
    exact = graph.relationship_vector_search("evidence", query, top_k=4, exact=True)
    assert [h["score"] for h in exact] == [h["score"] for h in hits]


def test_the_router_and_the_text_twin_agree_with_the_vector_twin() -> None:
    graph = _graph()
    vector = _Stub().embed(["beta"])[0]
    direct = graph.relationship_vector_search("evidence", vector, top_k=3)
    assert graph.vector_search("evidence", vector, top_k=3, entity="relationship") == direct
    assert graph.relationship_search_text("evidence", "beta", top_k=3) == direct
    assert graph.search_text("evidence", "beta", top_k=3, entity="relationship") == direct
    node = graph.node_vector_search("note", [1.0, 0.0], top_k=2)
    assert graph.vector_search("note", [1.0, 0.0], top_k=2) == node
    assert graph.node_search_text("note", "first", top_k=2) == graph.search_text("note", "first", top_k=2)


def test_relationship_search_as_a_dataframe() -> None:
    pd = pytest.importorskip("pandas")
    frame = _graph().relationship_vector_search("evidence", [1.0, 0.0], top_k=2, to_df=True)
    assert isinstance(frame, pd.DataFrame)
    assert list(frame.columns) == [
        "source",
        "target",
        "source_type",
        "target_type",
        "key",
        "relationship_type",
        "score",
    ]


def test_relationship_search_refusals_name_the_store() -> None:
    graph = _graph()
    with pytest.raises(
        ValueError, match=r"No relationship embedding store for text column 'evidnce'\. Did you mean 'evidence'\?"
    ):
        graph.relationship_vector_search("evidnce", [1.0, 0.0])
    with pytest.raises(ValueError, match=r"'note' is a node embedding store \(on Claimant\)"):
        graph.relationship_vector_search("note", [1.0, 0.0])
    with pytest.raises(
        ValueError, match=r"No relationship embedding store 'SUPPORT\.evidence'\. Did you mean 'SUPPORTS'\?"
    ):
        graph.relationship_vector_search("evidence", [1.0, 0.0], types=["SUPPORT"])
    with pytest.raises(ValueError, match=r"types is empty"):
        graph.relationship_vector_search("evidence", [1.0, 0.0], types=[])
    # The node route names the relationship store of that column.
    with pytest.raises(ValueError, match=r"'evidence' is a relationship embedding store \(on REFUTES, SUPPORTS\)"):
        graph.vector_search("evidence", [1.0, 0.0])


# ── single-vector readout ────────────────────────────────────────────────────


def test_relationship_embedding_reads_one_vector_by_address() -> None:
    graph = _graph()
    rows = graph.relationship_embeddings("SUPPORTS", "evidence", relationship_keys=KEYS)
    by_address = {(r["source"], r["target"], r["key"]): r["vector"] for r in rows}
    assert graph.relationship_embedding("SUPPORTS", "evidence", (1, 10)) == by_address[(1, 10, "a")]
    assert (
        graph.relationship_embedding("SUPPORTS", "evidence", (1, 20, "c"), relationship_keys=KEYS)
        == by_address[(1, 20, "c")]
    )
    assert (
        graph.relationship_embedding("SUPPORTS", "evidence", ("Claimant", 2, "Claim", 10)) == by_address[(2, 10, "d")]
    )
    assert graph.embedding("SUPPORTS", "evidence", (1, 10), entity="relationship") == by_address[(1, 10, "a")]
    with pytest.raises(ValueError, match="ambiguous"):
        graph.relationship_embedding("SUPPORTS", "evidence", (1, 20))
    with pytest.raises(ValueError, match="no 'SUPPORTS' relationship connects those nodes"):
        graph.relationship_embedding("SUPPORTS", "evidence", (2, 20))
    with pytest.raises(
        ValueError, match=r"No relationship embedding store 'SUPPORTS\.evidnce'\. Did you mean 'evidence'\?"
    ):
        graph.relationship_embedding("SUPPORTS", "evidnce", (1, 10))
    with pytest.raises(TypeError, match=r"relationship_embedding\(\): address must be"):
        graph.relationship_embedding("SUPPORTS", "evidence", 1)


def test_node_embedding_refuses_a_store_that_does_not_exist() -> None:
    graph = _graph()
    assert graph.node_embedding("Claimant", "note", 1) == [1.0, 0.0]
    assert graph.embedding("Claimant", "note", 1) == [1.0, 0.0]
    assert graph.embedding("Claimant", "note", 99) is None
    with pytest.raises(
        ValueError, match=r"'SUPPORTS\.evidence' is a relationship embedding store — pass entity='relationship'"
    ):
        graph.embedding("SUPPORTS", "evidence", (1, 10))
    with pytest.raises(ValueError, match=r"Did you mean 'note'\? The text column is 'note'; 'note_emb' is"):
        graph.embedding("Claimant", "note_emb", 1)


def test_embedding_dim_twins() -> None:
    graph = _graph()
    assert graph.node_embedding_dim("Claimant", "note") == 2
    assert graph.embedding_dim("Claimant", "note") == 2
    assert graph.relationship_embedding_dim("SUPPORTS", "evidence") == 2
    assert graph.embedding_dim("SUPPORTS", "evidence", entity="relationship") == 2
    assert graph.embedding_dim("SUPPORTS", "evidence") is None
    assert graph.relationship_embedding_dim("SUPPORTS", "nope") is None


# ── removal ──────────────────────────────────────────────────────────────────


def _stores(graph: KnowledgeGraph) -> list[tuple[str, str, str]]:
    return sorted(
        (row["entity"], row.get("node_type") or row["relationship_type"], row["text_column"])
        for row in graph.list_embeddings()
    )


def test_remove_refuses_a_store_that_does_not_exist_and_changes_nothing() -> None:
    graph = _graph()
    before = _stores(graph)
    with pytest.raises(
        ValueError, match=r"'SUPPORTS\.evidence' is a relationship embedding store — pass entity='relationship'"
    ):
        graph.remove_embeddings("SUPPORTS", "evidence")
    with pytest.raises(ValueError, match=r"No node embedding store 'Claimant\.nte'\. Did you mean 'note'\?"):
        graph.remove_node_embeddings("Claimant", "nte")
    with pytest.raises(ValueError, match=r"No node embedding store 'Nobody\.x'"):
        graph.remove_embeddings("Nobody", "x")
    with pytest.raises(
        ValueError, match=r"No relationship embedding store 'SUPPORTS\.evidnce'\. Did you mean 'evidence'\?"
    ):
        graph.remove_relationship_embeddings("SUPPORTS", "evidnce")
    assert _stores(graph) == before


def test_remove_twins_and_router_remove_the_store_and_its_index() -> None:
    graph = _graph()
    graph.build_relationship_vector_index("SUPPORTS", "evidence")
    graph.remove_embeddings("SUPPORTS", "evidence", entity="relationship")
    assert ("relationship", "SUPPORTS", "evidence") not in _stores(graph)
    assert graph.has_relationship_vector_index("SUPPORTS", "evidence") is False
    graph.remove_relationship_embeddings("REFUTES", "evidence")
    graph.remove_embeddings("Claimant", "note")
    assert _stores(graph) == []


# ── the remedy names a call in the caller's surface ──────────────────────────


def test_refresh_without_an_index_names_the_method_from_python_and_the_procedure_from_cypher() -> None:
    graph = _graph()
    with pytest.raises(ValueError, match=r"Build one with build_relationship_vector_index\('SUPPORTS', 'evidence'\)\."):
        graph.refresh_relationship_vector_index("SUPPORTS", "evidence")
    with pytest.raises(
        kglite.CypherExecutionError,
        match=r"Build one with CALL db\.relationship_embeddings\.build_index\("
        r"\{type: 'SUPPORTS', text_column: 'evidence'\}\)",
    ):
        graph.cypher("CALL db.relationship_embeddings.refresh_index({type: 'SUPPORTS', text_column: 'evidence'})")
    with pytest.raises(ValueError, match=r"Build one with build_vector_index\('Claimant', 'note'\)\."):
        graph.refresh_node_vector_index("Claimant", "note")
    with pytest.raises(
        kglite.CypherExecutionError,
        match=r"Build one with CALL db\.node_embeddings\.build_index\(\{type: 'Claimant', text_column: 'note'\}\)",
    ):
        graph.cypher("CALL db.node_embeddings.refresh_index({type: 'Claimant', text_column: 'note'})")


def test_build_over_a_missing_store_names_the_store_and_the_writer() -> None:
    graph = _graph()
    with pytest.raises(ValueError, match=r"to index\. Did you mean 'evidence'\? The text column is 'evidence'"):
        graph.build_relationship_vector_index("SUPPORTS", "evidence_emb")
    with pytest.raises(ValueError, match=r"Write one first with embed_relationship_texts\('WROTE', 'x'\)"):
        KnowledgeGraph().build_relationship_vector_index("WROTE", "x")
    with pytest.raises(kglite.CypherExecutionError, match=r"Did you mean 'evidence'\?"):
        graph.cypher("CALL db.relationship_embeddings.build_index({type: 'SUPPORTS', text_column: 'evidnce'})")
    with pytest.raises(kglite.CypherExecutionError, match=r"Write one first with CALL db\.node_embeddings\.embed"):
        KnowledgeGraph().cypher("CALL db.node_embeddings.build_index({type: 'Doc', text_column: 'x'})")


def test_relationship_readers_suggest_the_column() -> None:
    graph = _graph()
    with pytest.raises(ValueError, match=r"Did you mean 'evidence'\?"):
        graph.relationship_embeddings("SUPPORTS", "evidnce")
    with pytest.raises(kglite.CypherExecutionError, match=r"Did you mean 'evidence'\?"):
        graph.cypher(
            "CALL db.relationship_embeddings.query({type: 'SUPPORTS', text_column: 'evidnce', vector: [1.0, 0.0]}) "
            "YIELD score RETURN score"
        )
    with pytest.raises(
        kglite.CypherExecutionError,
        match=r"no relationship embedding store for text_column 'note'\. 'note' is a node embedding store "
        r"\(on Claimant\) — use db\.node_embeddings\.\*",
    ):
        graph.cypher(
            "CALL db.relationship_embeddings.query({text_column: 'note', vector: [1.0, 0.0]}) YIELD score RETURN score"
        )


# ── signature parity, method names, a model only when one is needed ─────────


def test_mode_is_positional_on_both_embed_twins_and_metric_keyword_on_both() -> None:
    import inspect

    graph = _graph()
    node = inspect.signature(graph.embed_node_texts).parameters
    rel = inspect.signature(graph.embed_relationship_texts).parameters
    assert list(node)[1:] == ["text_column", "batch_size", "show_progress", "mode", "metric"]
    assert list(rel)[1:] == ["text_column", "batch_size", "show_progress", "mode", "metric"]
    assert node["mode"].kind is rel["mode"].kind is inspect.Parameter.POSITIONAL_OR_KEYWORD
    assert node["metric"].kind is rel["metric"].kind is inspect.Parameter.KEYWORD_ONLY
    assert graph.embed_relationship_texts("SUPPORTS", "evidence", 256, False, "all")["embedded"] == 4


def test_embed_node_texts_records_a_metric_and_refuses_a_different_one() -> None:
    graph = _graph()
    graph.embed_node_texts("Claimant", "note", show_progress=False, mode="all", metric="euclidean")
    assert graph.embedding_info("Claimant", "note")["metric"] == "euclidean"
    with pytest.raises(ValueError, match=r"Store metric is 'euclidean', but this call requested 'cosine'"):
        graph.embed_node_texts("Claimant", "note", show_progress=False, mode="changed", metric="cosine")
    graph.embed_texts("Claimant", "note", show_progress=False, mode="all", metric="dot_product")
    assert graph.embedding_info("Claimant", "note")["metric"] == "dot_product"
    with pytest.raises(ValueError, match=r"Unknown metric 'manhattan'"):
        graph.embed_node_texts("Claimant", "note", show_progress=False, metric="manhattan")


def test_a_writer_type_error_names_the_method_called() -> None:
    graph = _graph()
    with pytest.raises(TypeError, match=r"^add_relationship_embeddings\(\): embeddings must be a dict"):
        graph.add_relationship_embeddings("SUPPORTS", "evidence", np.zeros((2, 2), dtype=np.float32))
    with pytest.raises(TypeError, match=r"^set_relationship_embeddings\(\): embeddings must be a dict"):
        graph.set_relationship_embeddings("SUPPORTS", "evidence", np.zeros((2, 2), dtype=np.float32))


def test_a_pass_with_nothing_to_embed_needs_no_model() -> None:
    graph = _graph()
    graph.embed_node_texts("Claimant", "note", show_progress=False, mode="all")
    graph.embed_relationship_texts("SUPPORTS", "evidence", show_progress=False, mode="all")
    graph.set_embedder(None)
    node = graph.embed_node_texts("Claimant", "note", show_progress=False, mode="changed")
    assert (node["embedded"], node["skipped_existing"], node["dimension"]) == (0, 2, 2)
    rel = graph.embed_relationship_texts("SUPPORTS", "evidence", show_progress=False, mode="changed")
    assert (rel["embedded"], rel["skipped_existing"], rel["dimension"]) == (0, 4, 2)
    # Once something does need a vector, the skeleton names the fix.
    graph.cypher("MATCH (c:Claimant {id: 1}) SET c.note = 'edited'")
    with pytest.raises(RuntimeError, match="No embedding model registered"):
        graph.embed_node_texts("Claimant", "note", show_progress=False, mode="changed")
    graph.cypher("MATCH ()-[r:SUPPORTS {uid: 'a'}]->() SET r.evidence = 'edited'")
    with pytest.raises(RuntimeError, match="No embedding model registered"):
        graph.embed_relationship_texts("SUPPORTS", "evidence", show_progress=False, mode="changed")
    with pytest.raises(RuntimeError, match="No embedding model registered"):
        graph.embed_node_texts("Claimant", "note", show_progress=False, mode="all")


def test_drop_index_names_the_relationship_form() -> None:
    graph = _graph()
    with pytest.raises(
        kglite.CypherExecutionError, match=r"no relationship vector or text index is installed on 'SUPPORTS\.evidence'"
    ):
        graph.cypher("DROP INDEX relationship:SUPPORTS.evidence")
    graph.build_relationship_vector_index("SUPPORTS", "evidence")
    graph.cypher("DROP INDEX relationship:SUPPORTS.evidence")
    assert graph.has_relationship_vector_index("SUPPORTS", "evidence") is False


def test_an_incremental_relationship_pass_keeps_the_store_metric() -> None:
    """A metric-less pass reset a declared metric to cosine — re-embedding
    one changed relationship silently changed how the whole store scored."""
    graph = _graph()
    graph.embed_relationship_texts("SUPPORTS", "evidence", show_progress=False, mode="all", metric="euclidean")
    graph.cypher("MATCH ()-[r:SUPPORTS {uid: 'a'}]->() SET r.evidence = 'edited'")
    assert graph.embed_relationship_texts("SUPPORTS", "evidence", show_progress=False, mode="changed")["embedded"] == 1
    assert graph.embedding_info("SUPPORTS", "evidence", entity="relationship")["metric"] == "euclidean"
    graph.embed_relationship_texts("SUPPORTS", "evidence", show_progress=False, mode="all")
    assert graph.embedding_info("SUPPORTS", "evidence", entity="relationship")["metric"] == "euclidean"
    with pytest.raises(ValueError, match=r"Store metric is 'euclidean', but this call requested 'cosine'"):
        graph.embed_relationship_texts("SUPPORTS", "evidence", show_progress=False, mode="changed", metric="cosine")
    with pytest.raises(kglite.CypherExecutionError, match=r"Store metric is 'euclidean'"):
        graph.cypher(
            "MATCH ()-[r:SUPPORTS]->() WITH collect(r) AS rs CALL db.relationship_embeddings.embed({type: "
            "'SUPPORTS', text_column: 'evidence', relationships: rs, mode: 'missing', metric: 'dot_product'}) "
            "YIELD embedded RETURN embedded"
        )
    graph.embed_relationship_texts("SUPPORTS", "evidence", show_progress=False, mode="all", metric="cosine")
    assert graph.embedding_info("SUPPORTS", "evidence", entity="relationship")["metric"] == "cosine"
