"""`describe()` shows relationship embedding stores beside node stores.

Before this, a graph whose vectors lived on relationships described itself as
having no embeddings at all: no store on the connection map, no `<semantic>`
hint, and a Cypher reference that never named `db.relationship_embeddings.*`. An
agent reading that description could not discover the feature.
"""

from __future__ import annotations

from pathlib import Path
import xml.etree.ElementTree as ET

import pytest

import kglite
from kglite import KnowledgeGraph

STORAGE_MODES = ["memory", "mapped", "disk"]


def _graph(node_store: bool, edge_store: bool, graph: KnowledgeGraph | None = None) -> KnowledgeGraph:
    """Node type and relationship type both named `SUPPORTS`; a node store has
    dim 3, a relationship store dim 2, so the two are never confused."""
    graph = graph if graph is not None else KnowledgeGraph()
    graph.cypher(
        "CREATE (:SUPPORTS {id: 1, title: 'a', body: 'node one'}), (:SUPPORTS {id: 2, title: 'b', body: 'node two'})"
    )
    graph.cypher(
        "MATCH (a:SUPPORTS {id: 1}), (b:SUPPORTS {id: 2}) "
        "CREATE (a)-[:SUPPORTS {body: 'edge text'}]->(b), (a)-[:SUPPORTS {body: 'more'}]->(b)"
    )
    if node_store:
        graph.set_embeddings("SUPPORTS", "body", {1: [1.0, 0.0, 0.0], 2: [0.0, 1.0, 0.0]})
    if edge_store:
        graph.cypher(
            "MATCH ()-[r:SUPPORTS {body: 'edge text'}]->() "
            "CALL db.relationship_embeddings.set({type:'SUPPORTS', text_column:'body', "
            "entries:[{relationship:r, vector:[0.6, 0.8]}]}) YIELD stored RETURN stored"
        )
    return graph


def _conn(xml: str) -> ET.Element:
    root = ET.fromstring(xml)
    (conn,) = [element for element in root.iter("conn") if element.get("type") == "SUPPORTS"]
    return conn


def _semantic(xml: str) -> str | None:
    element = ET.fromstring(xml).find(".//extensions/semantic")
    return None if element is None else element.get("hint")


def test_conn_line_carries_the_relationship_store() -> None:
    graph = _graph(node_store=True, edge_store=True)
    xml = graph.describe()

    assert _conn(xml).get("embeddings") == "body(dim=2,count=1)"
    # The same-name node store keeps its own element and dimension.
    (node_type,) = [element for element in ET.fromstring(xml).iter() if element.get("name") == "SUPPORTS"]
    assert [(e.get("text_col"), e.get("dim"), e.get("count")) for e in node_type.iter("embeddings")] == [
        ("body", "3", "2")
    ]
    assert _conn(graph.describe(connections=True)).get("embeddings") == "body(dim=2,count=1)"


def test_connection_detail_lists_the_store_as_a_child() -> None:
    detail = ET.fromstring(_graph(node_store=False, edge_store=True).describe(connections=["SUPPORTS"]))
    stores = [(e.get("text_col"), e.get("dim"), e.get("count")) for e in detail.iter("embeddings")]
    assert stores == [("body", "2", "1")]

    node_only = ET.fromstring(_graph(node_store=True, edge_store=False).describe(connections=["SUPPORTS"]))
    assert list(node_only.iter("embeddings")) == []


@pytest.mark.parametrize(
    ("node_store", "edge_store", "expect_node", "expect_relationship"),
    [
        (False, False, None, None),
        (True, False, True, False),
        (False, True, False, True),
        (True, True, True, True),
    ],
)
def test_semantic_hint_appears_for_either_entity(
    node_store: bool, edge_store: bool, expect_node: bool | None, expect_relationship: bool | None
) -> None:
    hint = _semantic(_graph(node_store, edge_store).describe())
    if expect_node is None:
        assert hint is None
        return
    assert hint is not None
    assert ("text_score(n, 'col'" in hint) is expect_node
    assert ("vector_score(r, 'col_emb'" in hint) is expect_relationship
    assert ("db.relationship_embeddings.query" in hint) is expect_relationship


def test_graphs_without_relationship_stores_have_no_embeddings_attribute() -> None:
    for node_store in (False, True):
        xml = _graph(node_store=node_store, edge_store=False).describe()
        assert "embeddings=" not in xml
        assert _conn(xml).get("embeddings") is None


def test_cypher_reference_names_the_relationship_procedures() -> None:
    reference = KnowledgeGraph().describe(cypher=True)
    (proc,) = [e for e in ET.fromstring(reference).iter("proc") if e.get("name") == "db.relationship_embeddings.*"]
    for name in ("set", "embed", "list", "remove", "drop", "query", "build_index", "refresh_index", "drop_index"):
        assert f"db.relationship_embeddings.{name}(" in proc.text
    functions = ET.fromstring(KnowledgeGraph().describe(cypher=["functions"]))
    (group,) = [e for e in functions.iter("group") if e.get("name") == "relationship_semantic"]
    assert "vector_score(r, 'col_emb'" in group.text


def _graph_in_mode(mode: str, tmp_path: Path) -> KnowledgeGraph:
    if mode == "disk":
        return kglite.open(str(tmp_path / "disk-graph"), storage="disk")
    return KnowledgeGraph(storage=mode)


def test_conn_embeddings_attribute_agrees_across_storage_modes(tmp_path: Path) -> None:
    lines = {}
    for mode in STORAGE_MODES:
        graph = _graph(node_store=True, edge_store=True, graph=_graph_in_mode(mode, tmp_path))
        conn = _conn(graph.describe())
        lines[mode] = dict(conn.attrib)
        assert _semantic(graph.describe()) == _semantic(_graph(True, True).describe()), mode
    assert lines["memory"]["embeddings"] == "body(dim=2,count=1)"
    assert lines["mapped"] == lines["memory"]
    assert lines["disk"] == lines["memory"]


# ── index presence (rendered surface), both entities ────────────────────────


def _indexed(graph: KnowledgeGraph, *, node_hnsw=False, edge_hnsw=False, node_bm25=False, edge_bm25=False):
    if node_hnsw:
        graph.build_vector_index("SUPPORTS", "body")
    if edge_hnsw:
        graph.cypher(
            "CALL db.relationship_embeddings.build_index({type:'SUPPORTS', text_column:'body'}) YIELD indexed RETURN "
            "indexed"
        )
    if node_bm25:
        graph.build_text_index("SUPPORTS", "body")
    if edge_bm25:
        graph.cypher(
            "CALL db.relationship_text_index.build({type:'SUPPORTS', text_column:'body'}) YIELD indexed RETURN indexed"
        )
    return graph


def _node_embeddings(xml: str) -> list[dict]:
    (node_type,) = [element for element in ET.fromstring(xml).iter() if element.get("name") == "SUPPORTS"]
    return [dict(e.attrib) for e in node_type.iter("embeddings")]


def test_hnsw_presence_is_shown_for_both_entities() -> None:
    graph = _indexed(_graph(True, True), node_hnsw=True, edge_hnsw=True)
    xml = graph.describe()
    assert _conn(xml).get("embeddings") == "body(dim=2,count=1,hnsw)"
    assert _node_embeddings(xml) == [{"text_col": "body", "dim": "3", "count": "2", "index": "hnsw"}]
    detail = ET.fromstring(graph.describe(connections=["SUPPORTS"]))
    assert [dict(e.attrib) for e in detail.iter("embeddings")] == [
        {"text_col": "body", "dim": "2", "count": "1", "index": "hnsw"}
    ]


def test_a_graph_with_only_relationship_indexes_marks_only_the_relationship() -> None:
    graph = _indexed(_graph(True, True), edge_hnsw=True, edge_bm25=True)
    xml = graph.describe()
    conn = _conn(xml)
    assert conn.get("embeddings") == "body(dim=2,count=1,hnsw)"
    assert conn.get("text_index") == "body"
    assert _node_embeddings(xml) == [{"text_col": "body", "dim": "3", "count": "2"}]
    (node_type,) = [element for element in ET.fromstring(xml).iter() if element.get("name") == "SUPPORTS"]
    assert list(node_type.iter("text_index")) == []


def test_bm25_presence_is_shown_for_both_entities() -> None:
    graph = _indexed(_graph(False, False), node_bm25=True, edge_bm25=True)
    xml = graph.describe()
    assert _conn(xml).get("text_index") == "body"
    (node_type,) = [element for element in ET.fromstring(xml).iter() if element.get("name") == "SUPPORTS"]
    assert [e.get("text_col") for e in node_type.iter("text_index")] == ["body"]
    detail = ET.fromstring(graph.describe(connections=["SUPPORTS"]))
    assert [e.get("text_col") for e in detail.iter("text_index")] == ["body"]


def test_without_indexes_the_description_is_unchanged_by_index_rendering() -> None:
    graph = _graph(True, True)
    for xml in (graph.describe(), graph.describe(connections=["SUPPORTS"]), graph.describe(types=["SUPPORTS"])):
        assert "hnsw" not in xml
        assert "text_index" not in xml
    assert _conn(graph.describe()).get("embeddings") == "body(dim=2,count=1)"


def test_relationship_semantic_is_a_direct_topic() -> None:
    topic = ET.fromstring(KnowledgeGraph().describe(cypher=["relationship_semantic"]))
    (element,) = [e for e in topic.iter("topic") if e.get("name") == "relationship_semantic"]
    summary = element.find("summary").text
    assert "Stores are per (relationship type, text column)" in summary
    assert "types:['A','B']" in element.find("usage").text
    functions = ET.fromstring(KnowledgeGraph().describe(cypher=["functions"]))
    (group,) = [e for e in functions.iter("group") if e.get("name") == "relationship_semantic"]
    assert group.text == summary
    hint = _semantic(_graph(False, True).describe())
    assert "stores are per relationship type and text column" in hint
    assert "describe(cypher=['relationship_semantic'])" in hint


def test_embedding_readout_is_described_for_both_entities() -> None:
    hint = _semantic(_graph(False, True).describe())
    assert "embedding(r, 'col_emb') returns its stored vector" in hint
    functions = ET.fromstring(KnowledgeGraph().describe(cypher=["functions"]))
    groups = {e.get("name"): e.text for e in functions.iter("group")}
    assert "embedding(r1, 'col_emb')" in groups["relationship_semantic"]
    assert "embedding(n, 'col_emb')" in groups["semantic"]
