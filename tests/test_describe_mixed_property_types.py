"""`describe()` types a property by every value written to it, not the last one.

A relationship type loaded with string `context` values, then one `CREATE`
carrying `context: 42`, rendered `properties="context:Int64"` — one outlier
re-typed the whole column for an agent reading the connection map. A property
that holds two value types reports `mixed`, on the connection map, the
connection detail and the node detail alike; a column rewritten to one type
still reports that type.
"""

from __future__ import annotations

import re

import pandas as pd
import pytest

from kglite import KnowledgeGraph


def _loaded() -> KnowledgeGraph:
    graph = KnowledgeGraph()
    graph.add_nodes(
        pd.DataFrame({"id": range(10), "title": [f"d{i}" for i in range(10)], "ctx": ["s"] * 10}),
        "D",
        "id",
        "title",
    )
    graph.add_connections(
        pd.DataFrame({"s": range(9), "t": range(1, 10), "context": ["x"] * 9}),
        "R",
        "D",
        "s",
        "D",
        "t",
    )
    return graph


def _conn_props(graph: KnowledgeGraph) -> str:
    match = re.search(r'<conn type="R"[^>]*properties="([^"]*)"', graph.describe())
    assert match, graph.describe()
    return match.group(1)


def _prop_type(xml: str, name: str) -> str:
    match = re.search(rf'<prop name="{name}" type="([^"]*)"', xml)
    assert match, xml
    return match.group(1)


def test_one_outlier_relationship_makes_the_connection_property_mixed():
    graph = _loaded()
    assert _conn_props(graph) == "context:String"
    graph.cypher("MATCH (a:D {id: 0}), (b:D {id: 5}) CREATE (a)-[:R {context: 42}]->(b)")
    assert _conn_props(graph) == "context:mixed"
    assert _prop_type(graph.describe(connections=["R"]), "context") == "mixed"


def test_the_mixed_connection_type_survives_save_and_load(tmp_path):
    graph = _loaded()
    graph.cypher("MATCH (a:D {id: 0}), (b:D {id: 5}) CREATE (a)-[:R {context: 42}]->(b)")
    path = str(tmp_path / "g.kgl")
    graph.save(path)
    from kglite import load

    assert _conn_props(load(path)) == "context:mixed"


def test_a_same_type_write_keeps_the_connection_type():
    graph = _loaded()
    graph.cypher("MATCH (a:D {id: 0}), (b:D {id: 5}) CREATE (a)-[:R {context: 'y'}]->(b)")
    assert _conn_props(graph) == "context:String"
    assert _prop_type(graph.describe(connections=["R"]), "context") == "str"


@pytest.mark.parametrize(
    "statement",
    [
        "CREATE (:D {id: 100, title: 'x', ctx: 42})",
        "MATCH (n:D {id: 1}) SET n.ctx = 42",
    ],
    ids=["create", "set"],
)
def test_one_outlier_node_makes_the_node_property_mixed(statement):
    graph = _loaded()
    graph.cypher(statement)
    assert _prop_type(graph.describe(types=["D"]), "ctx") == "mixed"


def test_a_column_rewritten_to_one_type_reports_that_type():
    graph = _loaded()
    graph.cypher("MATCH (n:D) SET n.ctx = 42")
    assert _prop_type(graph.describe(types=["D"]), "ctx") == "Int64"
    graph.cypher("MATCH ()-[r:R]->() SET r.context = 42")
    assert _prop_type(graph.describe(connections=["R"]), "context") == "int"
