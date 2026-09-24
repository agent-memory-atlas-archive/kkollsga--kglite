"""List-returning functions over a node or relationship carried as a value.

`labels(x)[0]` answered null whenever `x` was not a MATCH binding — a node
from `startNode(r)`, `collect`/`UNWIND`, `collect(...)[0]`, a map field, a path,
a procedure column or a subquery — while `head(labels(x))` was right; and on a
bound node every index but 0 (a secondary label) answered null too. These are
absolute goldens: the optimiser differential cannot see a defect both paths
share.
"""

from __future__ import annotations

import pytest

import kglite


@pytest.fixture(scope="module")
def graph() -> kglite.KnowledgeGraph:
    g = kglite.KnowledgeGraph()
    g.cypher("CREATE (a:Doc:Extra {id: 1, title: 'a', w: 5})-[:R {k: 'x', n: 1}]->(b:Doc {id: 2, title: 'b'})")
    g.set_embeddings("Doc", "title", {1: [1.0, 0.0], 2: [0.0, 1.0]})
    g.cypher(
        "MATCH ()-[r:R]->() CALL db.relationship_embeddings.set({type: 'R', text_property: 'k', "
        "entries: [{relationship: r, vector: [1.0, 0.0]}]}) YIELD stored RETURN stored"
    )
    return g


NODE_SOURCES = {
    "bound": "MATCH (x:Doc {id: 1}) ",
    "startNode": "MATCH (a:Doc {id: 1})-[r:R]->() WITH startNode(r) AS x ",
    "collect_unwind": "MATCH (a:Doc {id: 1}) WITH collect(a) AS xs UNWIND xs AS x ",
    "collect_index": "MATCH (a:Doc {id: 1}) WITH collect(a)[0] AS x ",
    "head_collect": "MATCH (a:Doc {id: 1}) WITH head(collect(a)) AS x ",
    "map_field": "MATCH (a:Doc {id: 1}) WITH {n: a} AS m WITH m.n AS x ",
    "path_node": "MATCH p = (:Doc {id: 1})-[:R]->() WITH nodes(p)[0] AS x ",
    "procedure": (
        "CALL db.node_embeddings.query({type: 'Doc', text_property: 'title', vector: [1.0, 0.0], "
        "top_k: 1}) YIELD node WITH node AS x "
    ),
    "subquery": "CALL { MATCH (a:Doc {id: 1}) RETURN a AS x } WITH x ",
}

NODE_EXPECTED = {
    "labels(x)[0]": "Doc",
    "labels(x)[1]": "Extra",
    "labels(x)[-1]": "Extra",
    "labels(x)[2]": None,
    "labels(x)[0..1]": ["Doc"],
    "'Extra' IN labels(x)": True,
    "head(labels(x))": "Doc",
    "size(labels(x))": 2,
    "'w' IN keys(x)": True,
    "properties(x)['w']": 5,
    "properties(x).w": 5,
    "[l IN labels(x) WHERE l <> 'Doc'][0]": "Extra",
}

REL_SOURCES = {
    "bound": "MATCH ()-[x:R]->() ",
    "collect_unwind": "MATCH ()-[r:R]->() WITH collect(r) AS rs UNWIND rs AS x ",
    "collect_index": "MATCH ()-[r:R]->() WITH collect(r)[0] AS x ",
    "map_field": "MATCH ()-[r:R]->() WITH {e: r} AS m WITH m.e AS x ",
    "path_relationship": "MATCH p = ()-[:R]->() WITH relationships(p)[0] AS x ",
    "procedure": (
        "CALL db.relationship_embeddings.query({type: 'R', text_property: 'k', vector: [1.0, 0.0], "
        "top_k: 1}) YIELD relationship WITH relationship AS x "
    ),
    "subquery": "CALL { MATCH ()-[r:R]->() RETURN r AS x } WITH x ",
}

REL_EXPECTED = {
    "type(x)": "R",
    "[type(x)][0]": "R",
    "'n' IN keys(x)": True,
    "properties(x)['n']": 1,
    "properties(x).k": "x",
    "labels(startNode(x))[0]": "Doc",
    "labels(startNode(x))[1]": "Extra",
    "labels(endNode(x))[0]": "Doc",
}


def _value(graph: kglite.KnowledgeGraph, query: str):
    rows = graph.cypher(query).to_list()
    assert len(rows) == 1, (query, rows)
    return rows[0]["v"]


@pytest.mark.parametrize("source", sorted(NODE_SOURCES))
@pytest.mark.parametrize("expression", sorted(NODE_EXPECTED))
def test_node_value_list_functions(graph, source, expression):
    query = NODE_SOURCES[source] + f"RETURN {expression} AS v"
    assert _value(graph, query) == NODE_EXPECTED[expression]


@pytest.mark.parametrize("source", sorted(REL_SOURCES))
@pytest.mark.parametrize("expression", sorted(REL_EXPECTED))
def test_relationship_value_list_functions(graph, source, expression):
    query = REL_SOURCES[source] + f"RETURN {expression} AS v"
    assert _value(graph, query) == REL_EXPECTED[expression]


@pytest.mark.parametrize(
    ("expression", "expected"),
    [
        ("nodes(p)[1].id", 2),
        ("labels(nodes(p)[0])[0]", "Doc"),
        ("labels(nodes(p)[-1])[0]", "Doc"),
        ("[n IN nodes(p) | labels(n)[0]][1]", "Doc"),
        ("relationships(p)[0].k", "x"),
        ("type(relationships(p)[0])", "R"),
        ("nodes(p)[0..1][0].id", 1),
    ],
)
def test_path_list_functions(graph, expression, expected):
    assert _value(graph, f"MATCH p = (:Doc {{id: 1}})-[:R]->() RETURN {expression} AS v") == expected


def test_the_graph_rag_loop_label_is_never_null(graph):
    """The reported shape: a label read off a relationship's start node feeds
    the next query's label."""
    rows = graph.cypher(
        "MATCH (a)-[r]->() WITH startNode(r) AS x RETURN labels(x)[0] AS l0, head(labels(x)) AS h"
    ).to_list()
    assert rows == [{"l0": "Doc", "h": "Doc"}]
