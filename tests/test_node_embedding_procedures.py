"""`db.node_embeddings.*`, `db.node_text_index.*` and the `db.embeddings.*` /
`db.text_index.*` routers.

Every node procedure is checked against the Python method it mirrors, run on a
twin graph: the vectors a store holds, its index state, and the rows a query
returns. The routers are checked against the specific namespaces they route to.
"""

from __future__ import annotations

from pathlib import Path

import pytest

import kglite
from kglite import KnowledgeGraph

MODES = ["memory", "mapped", "disk"]
DOCS = [(1, "alpha"), (2, "beta beta"), (3, "gamma ray burst"), (4, "delta")]
VECTORS = {1: [1.0, 0.0], 2: [0.0, 1.0], 3: [0.6, 0.8], 4: [0.8, 0.6]}


def _graph(mode: str, tmp_path: Path, name: str = "g") -> KnowledgeGraph:
    graph = kglite.open(str(tmp_path / name), storage="disk") if mode == "disk" else KnowledgeGraph(storage=mode)
    for doc_id, text in DOCS:
        graph.cypher("CREATE (:Doc {id: $id, text: $text})", params={"id": doc_id, "text": text})
    graph.cypher("CREATE (:Note {id: 10, text: 'note one'}), (:Note {id: 11, text: 'note two'})")
    graph.cypher("MATCH (a:Doc {id: 1}), (b:Doc {id: 2}) CREATE (a)-[:CITES {text: 'cites'}]->(b)")
    return graph


class _Stub:
    """A deterministic two-dimensional model: [len(text), words]."""

    dimension = 2
    model_id = "stub/v1"

    def load(self) -> None:
        pass

    def unload(self) -> None:
        pass

    def embed(self, texts: list[str]) -> list[list[float]]:
        return [[float(len(text)), float(len(text.split()))] for text in texts]


def _set_docs(graph: KnowledgeGraph, namespace: str = "db.node_embeddings") -> list[dict]:
    return graph.cypher(
        "UNWIND $rows AS row MATCH (d:Doc {id: row.id}) "
        f"CALL {namespace}.set({{type: 'Doc', text_property: 'text', "
        "entries: [{node: d, vector: row.vector}]}) YIELD stored, dimension "
        "RETURN max(stored) AS stored, max(dimension) AS dimension",
        params={"rows": [{"id": key, "vector": vector} for key, vector in VECTORS.items()]},
    ).to_list()


@pytest.mark.parametrize("mode", MODES)
def test_set_writes_what_add_embeddings_writes(mode: str, tmp_path: Path) -> None:
    by_procedure = _graph(mode, tmp_path, "procedure")
    assert _set_docs(by_procedure) == [{"stored": 4, "dimension": 2}]
    by_method = _graph(mode, tmp_path, "method")
    by_method.add_embeddings("Doc", "text", VECTORS)
    assert by_procedure.embeddings("Doc", "text") == by_method.embeddings("Doc", "text")
    assert by_procedure.embedding_info("Doc", "text") == by_method.embedding_info("Doc", "text")


@pytest.mark.parametrize("mode", MODES)
def test_embed_writes_what_embed_texts_writes(mode: str, tmp_path: Path) -> None:
    by_procedure = _graph(mode, tmp_path, "procedure")
    by_procedure.set_embedder(_Stub())
    report = by_procedure.cypher(
        "MATCH (d:Doc) WITH collect(d) AS docs "
        "CALL db.node_embeddings.embed({type: 'Doc', text_property: 'text', nodes: docs}) "
        "YIELD embedded, skipped, dimension, model RETURN embedded, skipped, dimension, model"
    ).to_list()
    assert report == [{"embedded": 4, "skipped": 0, "dimension": 2, "model": "stub/v1"}]
    by_method = _graph(mode, tmp_path, "method")
    by_method.set_embedder(_Stub())
    by_method.embed_texts("Doc", "text", show_progress=False)
    assert by_procedure.embeddings("Doc", "text") == by_method.embeddings("Doc", "text")
    assert by_procedure.embedding_info("Doc", "text") == by_method.embedding_info("Doc", "text")


def test_index_lifecycle_matches_the_python_methods(tmp_path: Path) -> None:
    graph = _graph("memory", tmp_path)
    _set_docs(graph)
    built = graph.cypher(
        "CALL db.node_embeddings.build_index({type: 'Doc', text_property: 'text', m: 8}) "
        "YIELD indexed, metric, m RETURN indexed, metric, m"
    ).to_list()
    twin = _graph("memory", tmp_path, "twin")
    twin.add_embeddings("Doc", "text", VECTORS)
    method = twin.build_vector_index("Doc", "text", m=8)
    assert built == [{"indexed": method["indexed"], "metric": method["metric"], "m": method["m"]}]
    assert graph.has_vector_index("Doc", "text")
    listed = graph.cypher(
        "CALL db.node_embeddings.list({type: 'Doc'}) YIELD entity, count, index_state, delta "
        "RETURN entity, count, index_state, delta"
    ).to_list()
    assert listed == [{"entity": "node", "count": 4, "index_state": "online", "delta": 0}]
    assert graph.cypher(
        "CALL db.node_embeddings.refresh_index({type: 'Doc', text_property: 'text'}) YIELD refreshed RETURN refreshed"
    ).to_list() == [{"refreshed": twin.refresh_vector_index("Doc", "text")}]
    assert graph.cypher(
        "CALL db.node_embeddings.drop_index({type: 'Doc', text_property: 'text'}) YIELD dropped RETURN dropped"
    ).to_list() == [{"dropped": twin.drop_vector_index("Doc", "text")}]
    assert not graph.has_vector_index("Doc", "text")
    with pytest.raises(kglite.CypherExecutionError, match="no vector index"):
        graph.cypher("CALL db.node_embeddings.refresh_index({type: 'Doc', text_property: 'text'}) YIELD refreshed")


@pytest.mark.parametrize("mode", MODES)
def test_query_rows_equal_vector_search(mode: str, tmp_path: Path) -> None:
    graph = _graph(mode, tmp_path)
    _set_docs(graph)
    rows = graph.cypher(
        "CALL db.node_embeddings.query({type: 'Doc', text_property: 'text', vector: [1.0, 0.1], top_k: 3, exact: "
        "true}) "
        "YIELD node, score, search_method, type RETURN node.id AS id, score, search_method, type"
    ).to_list()
    expected = graph.select("Doc").vector_search("text", [1.0, 0.1], top_k=3, exact=True)
    assert [row["id"] for row in rows] == [hit["id"] for hit in expected]
    assert [row["score"] for row in rows] == pytest.approx([hit["score"] for hit in expected])
    assert {row["search_method"] for row in rows} == {"exact"}
    assert {row["type"] for row in rows} == {"Doc"}


def test_query_merges_types_and_refuses_mixed_metrics(tmp_path: Path) -> None:
    graph = _graph("memory", tmp_path)
    _set_docs(graph)
    graph.add_embeddings("Note", "text", {10: [0.9, 0.2], 11: [0.1, 1.0]})
    merged = graph.cypher(
        "CALL db.node_embeddings.query({text_property: 'text', vector: [1.0, 0.0], top_k: 3}) "
        "YIELD node, type RETURN node.id AS id, type"
    ).to_list()
    assert merged == [
        {"id": 1, "type": "Doc"},
        {"id": 10, "type": "Note"},
        {"id": 4, "type": "Doc"},
    ]
    graph.set_embeddings("Note", "text", {10: [0.9, 0.2]}, metric="euclidean")
    with pytest.raises(kglite.CypherExecutionError, match="score under different metrics"):
        graph.cypher(
            "CALL db.node_embeddings.query({types: ['Doc', 'Note'], text_property: 'text', vector: [1.0, 0.0]}) "
            "YIELD node RETURN node"
        )


def test_query_text_is_embedded_with_the_registered_model(tmp_path: Path) -> None:
    graph = _graph("memory", tmp_path)
    graph.set_embedder(_Stub())
    graph.embed_texts("Doc", "text", show_progress=False)
    by_text = graph.cypher(
        "CALL db.embeddings.query({type: 'Doc', text_property: 'text', text: 'gamma ray burst', top_k: 2}) "
        "YIELD node, score RETURN node.id AS id, score"
    ).to_list()
    by_vector = graph.cypher(
        "CALL db.node_embeddings.query({type: 'Doc', text_property: 'text', vector: $v, top_k: 2}) "
        "YIELD node, score RETURN node.id AS id, score",
        params={"v": _Stub().embed(["gamma ray burst"])[0]},
    ).to_list()
    assert by_text == by_vector


def test_remove_and_drop(tmp_path: Path) -> None:
    graph = _graph("memory", tmp_path)
    _set_docs(graph)
    assert graph.cypher(
        "MATCH (d:Doc) WHERE d.id IN [1, 2] WITH collect(d) AS ds "
        "CALL db.node_embeddings.remove({type: 'Doc', text_property: 'text', nodes: ds}) YIELD removed RETURN removed"
    ).to_list() == [{"removed": 2}]
    assert sorted(graph.embeddings("Doc", "text")) == [3, 4]
    assert graph.cypher(
        "CALL db.node_embeddings.drop({type: 'Doc', text_property: 'text'}) YIELD dropped RETURN dropped"
    ).to_list() == [{"dropped": True}]
    assert graph.embedding_info("Doc", "text") is None


def test_refusals_name_what_the_node_writer_names(tmp_path: Path) -> None:
    graph = _graph("memory", tmp_path)
    with pytest.raises(kglite.CypherExecutionError, match=r"Source column 'summary' not found on any 'Doc' node"):
        graph.cypher(
            "MATCH (d:Doc {id: 1}) CALL db.node_embeddings.set({type: 'Doc', text_property: 'summary', "
            "entries: [{node: d, vector: [1.0, 0.0]}]}) YIELD stored RETURN stored"
        )
    with pytest.raises(kglite.CypherExecutionError, match=r"'bogus'.*Accepted: type, text_property, entries, metric"):
        graph.cypher(
            "MATCH (d:Doc {id: 1}) CALL db.node_embeddings.set({type: 'Doc', text_property: 'text', "
            "entries: [{node: d, vector: [1.0, 0.0]}], bogus: 1}) YIELD stored RETURN stored"
        )
    with pytest.raises(kglite.CypherExecutionError, match="requires a registered embedder"):
        graph.cypher(
            "MATCH (d:Doc) WITH collect(d) AS ds CALL db.node_embeddings.embed({type: 'Doc', "
            "text_property: 'text', nodes: ds}) YIELD embedded RETURN embedded"
        )
    assert graph.embedding_info("Doc", "text") is None


def test_a_failed_statement_leaves_the_store_as_it_was(tmp_path: Path) -> None:
    graph = _graph("memory", tmp_path)
    _set_docs(graph)
    before = graph.embeddings("Doc", "text")
    with pytest.raises(kglite.CypherExecutionError):
        graph.cypher(
            "MATCH (d:Doc {id: 1}) CALL db.node_embeddings.set({type: 'Doc', text_property: 'text', "
            "entries: [{node: d, vector: [5.0, 5.0]}]}) YIELD stored "
            "WITH d CALL db.node_embeddings.set({type: 'Doc', text_property: 'text', "
            "entries: [{node: d, vector: [1.0, 2.0, 3.0]}]}) YIELD stored RETURN stored"
        )
    assert graph.embeddings("Doc", "text") == before


@pytest.mark.parametrize("mode", ["memory", "mapped"])
def test_node_text_index_matches_build_text_index(mode: str, tmp_path: Path) -> None:
    by_procedure = _graph(mode, tmp_path, "procedure")
    built = by_procedure.cypher(
        "CALL db.node_text_index.build({type: 'Doc', property: 'text'}) YIELD indexed, skipped, terms "
        "RETURN indexed, skipped, terms"
    ).to_list()
    by_method = _graph(mode, tmp_path, "method")
    method = by_method.build_text_index("Doc", "text")
    assert built == [{"indexed": method["indexed"], "skipped": method["skipped"], "terms": method["terms"]}]
    query = "MATCH (d:Doc) RETURN d.id AS id, text_bm25(d, 'text', 'beta ray') AS s ORDER BY id"
    assert by_procedure.cypher(query).to_list() == by_method.cypher(query).to_list()
    assert by_procedure.cypher(
        "CALL db.node_text_index.list({type: 'Doc'}) YIELD entity, property, documents RETURN entity, property, "
        "documents"
    ).to_list() == [{"entity": "node", "property": "text", "documents": 4}]
    assert by_procedure.cypher(
        "CALL db.node_text_index.drop({type: 'Doc', property: 'text'}) YIELD dropped RETURN dropped"
    ).to_list() == [{"dropped": True}]
    assert not by_procedure.has_text_index("Doc", "text")


def test_the_router_defaults_to_the_node_namespace(tmp_path: Path) -> None:
    routed = _graph("memory", tmp_path, "routed")
    assert _set_docs(routed, "db.embeddings") == [{"stored": 4, "dimension": 2}]
    specific = _graph("memory", tmp_path, "specific")
    _set_docs(specific)
    assert routed.embeddings("Doc", "text") == specific.embeddings("Doc", "text")
    assert routed.list_embeddings() == specific.list_embeddings()
    query = (
        "CALL {ns}.query({{type: 'Doc', text_property: 'text', vector: [1.0, 0.0], top_k: 2}}) "
        "YIELD node, score RETURN node.id AS id, score"
    )
    assert (
        routed.cypher(query.format(ns="db.embeddings")).to_list()
        == specific.cypher(query.format(ns="db.node_embeddings")).to_list()
    )


def test_the_router_routes_relationships_on_entity(tmp_path: Path) -> None:
    routed = _graph("memory", tmp_path, "routed")
    specific = _graph("memory", tmp_path, "specific")
    for graph, namespace, entity in (
        (routed, "db.embeddings", "entity: 'relationship', "),
        (specific, "db.relationship_embeddings", ""),
    ):
        graph.cypher(
            f"MATCH ()-[r:CITES]->() CALL {namespace}.set({{{entity}type: 'CITES', text_property: 'text', "
            "entries: [{relationship: r, vector: [1.0, 0.0]}]}) YIELD stored RETURN stored"
        )
        graph.cypher(
            f"CALL {namespace.replace('embeddings', 'text_index')}.build({{{entity}type: 'CITES', property: 'text'}}) "
            f"YIELD indexed RETURN indexed"
        )
    assert routed.relationship_embeddings("CITES", "text") == specific.relationship_embeddings("CITES", "text")
    assert routed.embedding_info("Doc", "text") is None
    listing = (
        "CALL {ns}.list({{{entity}type: 'CITES'}}) YIELD entity, type, property, documents RETURN entity, type, "
        "property, documents"
    )
    assert (
        routed.cypher(listing.format(ns="db.text_index", entity="entity: 'relationship', ")).to_list()
        == specific.cypher(listing.format(ns="db.relationship_text_index", entity="")).to_list()
    )
    rel_query = (
        "CALL {ns}.query({{{entity}type: 'CITES', text_property: 'text', vector: [1.0, 0.0]}}) "
        "YIELD relationship, score RETURN type(relationship) AS t, score"
    )
    assert routed.cypher(rel_query.format(ns="db.embeddings", entity="entity: 'relationship', ")).to_list() == (
        specific.cypher(rel_query.format(ns="db.relationship_embeddings", entity="")).to_list()
    )


def test_a_keyword_of_the_other_entity_is_refused_naming_it(tmp_path: Path) -> None:
    graph = _graph("memory", tmp_path)
    with pytest.raises(kglite.CypherSyntaxError, match=r"`relationships` belongs to entity:'relationship'"):
        graph.cypher(
            "MATCH ()-[r:CITES]->() CALL db.embeddings.remove({type: 'CITES', text_property: 'text', "
            "relationships: [r]}) YIELD removed RETURN removed"
        )
    with pytest.raises(kglite.CypherSyntaxError, match=r"`nodes` belongs to entity:'node'"):
        graph.cypher(
            "MATCH (d:Doc) CALL db.embeddings.remove({entity: 'relationship', type: 'CITES', "
            "text_property: 'text', nodes: [d]}) YIELD removed RETURN removed"
        )
    with pytest.raises(kglite.CypherSyntaxError, match=r"'entity' must be the string literal"):
        graph.cypher("CALL db.embeddings.list({entity: $e}) YIELD type RETURN type", params={"e": "node"})


@pytest.mark.parametrize(
    "name",
    [
        "db.edge_embeddings.list",
        "db.edge_embeddings.query",
        "db.edge_embeddings.set",
        "db.edge_text_index.list",
        "db.edge_text_index.build",
    ],
)
def test_the_unreleased_edge_names_are_unknown(name: str, tmp_path: Path) -> None:
    graph = _graph("memory", tmp_path)
    with pytest.raises(kglite.CypherExecutionError, match=f"Unknown procedure '{name}'"):
        graph.cypher(f"CALL {name}({{type: 'CITES'}}) YIELD dropped RETURN dropped")


def test_every_namespace_is_listed() -> None:
    names = {row["name"] for row in KnowledgeGraph().cypher("SHOW PROCEDURES").to_list()}
    for namespace, operations in (
        (
            "embeddings",
            ("set", "embed", "list", "remove", "drop", "build_index", "refresh_index", "drop_index", "query"),
        ),
        ("text_index", ("build", "refresh", "drop", "list")),
    ):
        for prefix in ("db.node_", "db.relationship_", "db."):
            for operation in operations:
                assert f"{prefix}{namespace}.{operation}" in names
