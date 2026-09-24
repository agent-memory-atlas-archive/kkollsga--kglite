"""`describe()` shows relationship embedding stores beside node stores.

Before this, a graph whose vectors lived on relationships described itself as
having no embeddings at all: no store on the connection map, no `<semantic>`
hint, and a Cypher reference that never named `db.edge_embeddings.*`. An
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
            "CALL db.edge_embeddings.set({type:'SUPPORTS', text_property:'body', "
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
    assert ("db.edge_embeddings.query" in hint) is expect_relationship


def test_graphs_without_relationship_stores_have_no_embeddings_attribute() -> None:
    for node_store in (False, True):
        xml = _graph(node_store=node_store, edge_store=False).describe()
        assert "embeddings=" not in xml
        assert _conn(xml).get("embeddings") is None


def test_cypher_reference_names_the_relationship_procedures() -> None:
    reference = KnowledgeGraph().describe(cypher=True)
    (proc,) = [e for e in ET.fromstring(reference).iter("proc") if e.get("name") == "db.edge_embeddings.*"]
    for name in ("set", "embed", "list", "remove", "drop", "query", "build_index", "refresh_index", "drop_index"):
        assert f"db.edge_embeddings.{name}(" in proc.text
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
