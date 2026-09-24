"""The embedding inventory covers relationship stores as well as node stores.

`list_embeddings()`, `embedding_info()` and `embedding_diagnostics()` used to
see node stores only, so a graph whose vectors lived on relationships reported
an empty inventory. Every row now carries `entity`, relationship rows name
their type under `relationship_type` (never `node_type`), and a node type and a
relationship type sharing a name stay distinguishable.
"""

from __future__ import annotations

from pathlib import Path

import pytest

import kglite
from kglite import KnowledgeGraph

NODE_LIST_KEYS = {"node_type", "text_column", "store_name", "dimension", "count", "metric"}
NODE_DIAGNOSTIC_KEYS = {
    "node_type",
    "text_column",
    "embedding_key",
    "nodes_with_property",
    "nodes_embedded",
    "status",
    "dimension",
    "metric",
    "length_stats",
}


def _same_name_graph(graph: KnowledgeGraph | None = None) -> KnowledgeGraph:
    """A node type and a relationship type both called `SUPPORTS`, each with a
    `body_emb` store of a different dimension, so a lookup that resolved the
    wrong entity is visible in the result."""
    graph = graph if graph is not None else KnowledgeGraph()
    graph.cypher(
        "CREATE (:SUPPORTS {id: 1, title: 'n1', body: 'node text one'}), "
        "(:SUPPORTS {id: 2, title: 'n2', body: 'node text two'})"
    )
    graph.cypher(
        "MATCH (a:SUPPORTS {id: 1}), (b:SUPPORTS {id: 2}) "
        "CREATE (a)-[:SUPPORTS {body: 'edge text', note: 'aside'}]->(b)"
    )
    graph.set_embeddings("SUPPORTS", "body", {1: [1.0, 0.0, 0.0], 2: [0.0, 1.0, 0.0]})
    graph.cypher(
        "MATCH ()-[r:SUPPORTS]->() "
        "CALL db.relationship_embeddings.set({type:'SUPPORTS', text_column:'body', "
        "entries:[{relationship:r, vector:[0.6, 0.8]}], metric:'dot_product'}) "
        "YIELD stored RETURN stored"
    )
    return graph


def _graph_in_mode(mode: str, tmp_path: Path) -> KnowledgeGraph:
    if mode == "disk":
        return kglite.open(str(tmp_path / "disk-graph"), storage="disk")
    return KnowledgeGraph(storage=mode)


def test_list_embeddings_reports_both_entities() -> None:
    rows = _same_name_graph().list_embeddings()

    assert rows == [
        {
            "entity": "node",
            "node_type": "SUPPORTS",
            "text_column": "body",
            "store_name": "body_emb",
            "dimension": 3,
            "count": 2,
            "metric": "cosine",
        },
        {
            "entity": "relationship",
            "relationship_type": "SUPPORTS",
            "text_column": "body",
            "store_name": "body_emb",
            "dimension": 2,
            "count": 1,
            "metric": "dot_product",
        },
    ]


def test_node_only_rows_gain_only_the_entity_key() -> None:
    graph = KnowledgeGraph()
    graph.cypher("CREATE (:Doc {id: 1, title: 'a', summary: 'some words here'})")
    graph.set_embeddings("Doc", "summary", {1: [1.0, 0.0]})

    (listed,) = graph.list_embeddings()
    assert set(listed) == NODE_LIST_KEYS | {"entity"}
    assert listed["entity"] == "node"
    for row in graph.embedding_diagnostics():
        assert set(row) == NODE_DIAGNOSTIC_KEYS | {"entity"}
        assert row["entity"] == "node"
    info = graph.embedding_info("Doc", "summary")
    assert info is not None and info["node_type"] == "Doc" and "relationship_type" not in info


def test_embedding_info_resolves_the_requested_entity_on_a_same_name_pair() -> None:
    graph = _same_name_graph()

    node = graph.embedding_info("SUPPORTS", "body")
    assert node == {
        "node_type": "SUPPORTS",
        "text_column": "body",
        "dimension": 3,
        "count": 2,
        "model": None,
        "metric": "cosine",
        "hashed": 0,
    }
    assert graph.embedding_info("SUPPORTS", "body", entity="node") == node
    assert graph.embedding_info("SUPPORTS", "body", entity="relationship") == {
        "relationship_type": "SUPPORTS",
        "text_column": "body",
        "dimension": 2,
        "count": 1,
        "model": None,
        "metric": "dot_product",
        "hashed": 0,
    }
    assert graph.embedding_info("SUPPORTS", "note", entity="relationship") is None
    assert graph.embedding_info("MISSING", "body", entity="relationship") is None


def test_embedding_info_entity_is_keyword_only_and_validated() -> None:
    graph = _same_name_graph()
    with pytest.raises(TypeError):
        graph.embedding_info("SUPPORTS", "body", "relationship")  # type: ignore[misc]
    with pytest.raises(kglite.ArgumentError, match="entity"):
        graph.embedding_info("SUPPORTS", "body", entity="edge")


@pytest.mark.parametrize("mode", ["memory", "mapped", "disk"])
def test_diagnostics_report_relationship_stores_in_every_storage_mode(mode: str, tmp_path: Path) -> None:
    graph = _same_name_graph(_graph_in_mode(mode, tmp_path))
    rows = graph.embedding_diagnostics()

    node_rows = [row for row in rows if row["entity"] == "node"]
    relationship_rows = [row for row in rows if row["entity"] == "relationship"]
    assert rows == node_rows + relationship_rows, "node rows come first"
    assert all("relationship_type" not in row for row in node_rows)
    assert {row["text_column"]: row["status"] for row in node_rows} == {"body": "embedded", "title": "embeddable"}

    by_column = {row["text_column"]: row for row in relationship_rows}
    assert set(by_column) == {"body", "note"}
    body = by_column["body"]
    assert "node_type" not in body
    assert body["relationship_type"] == "SUPPORTS"
    assert body["embedding_key"] == "body_emb"
    assert body["relationships_with_property"] == 1
    assert body["relationships_embedded"] == 1
    assert body["status"] == "embedded"
    assert body["dimension"] == 2
    assert body["metric"] == "dot_product"
    assert body["length_stats"] == {
        "mean_length": float(len("edge text")),
        "max_length": len("edge text"),
        "distinct_count": 1,
        "distinct_ratio": 1.0,
    }
    note = by_column["note"]
    assert note["status"] == "embeddable"
    assert note["relationships_embedded"] == 0
    assert note["dimension"] is None and note["metric"] is None


def test_relationship_store_without_its_property_is_an_orphan() -> None:
    graph = _same_name_graph()
    graph.cypher("MATCH ()-[r:SUPPORTS]->() REMOVE r.body")

    (body,) = [
        row for row in graph.embedding_diagnostics() if row["entity"] == "relationship" and row["text_column"] == "body"
    ]
    assert body["status"] == "store_orphan"
    assert body["relationships_with_property"] == 0
    assert body["relationships_embedded"] == 1


def test_diagnostics_scope_filters_by_entity() -> None:
    graph = _same_name_graph()
    graph.cypher("CREATE (:Doc {id: 9})-[:LINKS {caption: 'a caption'}]->(:Doc {id: 10})")

    # A relationship type with no store is scanned by default, exactly as a node
    # type is: its string properties surface as "embeddable" candidates.
    default = graph.embedding_diagnostics()
    assert ("relationship", "LINKS", "caption", "embeddable") in [
        (row["entity"], row.get("relationship_type"), row["text_column"], row["status"]) for row in default
    ]
    scoped = graph.embedding_diagnostics(relationship_type="LINKS")
    assert [(row["entity"], row["relationship_type"], row["text_column"], row["status"]) for row in scoped] == [
        ("relationship", "LINKS", "caption", "embeddable")
    ]
    assert {row["entity"] for row in graph.embedding_diagnostics(node_type="SUPPORTS")} == {"node"}
    both = graph.embedding_diagnostics(node_type="Doc", relationship_type="SUPPORTS")
    assert {(row["entity"], row.get("node_type") or row.get("relationship_type")) for row in both} == {
        ("relationship", "SUPPORTS")
    }, "Doc has no string property, so it contributes no row"

    with pytest.raises(ValueError, match="Relationship type 'NOPE'"):
        graph.embedding_diagnostics(relationship_type="NOPE")
    with pytest.raises(ValueError, match="Node type 'NOPE'"):
        graph.embedding_diagnostics(node_type="NOPE")


def test_unembedded_relationship_properties_are_candidates_by_default() -> None:
    """ "What could I embed?" must answer for relationships too: a graph whose
    relationships carry string properties but no store reports them."""
    graph = KnowledgeGraph()
    graph.cypher("CREATE (:Doc {id: 1})-[:CITES {quote: 'a quoted passage', weight: 2}]->(:Doc {id: 2})")

    rows = [row for row in graph.embedding_diagnostics() if row["entity"] == "relationship"]
    assert [(row["relationship_type"], row["text_column"], row["status"]) for row in rows] == [
        ("CITES", "quote", "embeddable")
    ]
    (quote,) = rows
    assert quote["relationships_with_property"] == 1
    assert quote["relationships_embedded"] == 0
    assert quote["embedding_key"] == "quote_emb"
