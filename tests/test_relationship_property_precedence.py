"""`r.<key>` on a relationship: the stored property first, the envelope second.

A relationship that stores `type` or `id` (knwl's `KnwlEdge.type`, knwler's
`relation.id`) used to read them back only in some places. A MATCH variable
read `type` from the envelope in RETURN while a pushed-down WHERE read the
property, so a row passed `WHERE r.type = 'x'` and then returned
`r.type = 'x'` as false. A relationship value (`collect`, `UNWIND`,
`relationships(p)`, `YIELD relationship`) read `id`, `type`, `start` and `end`
from the envelope only. A relationship without the property falls back to
the envelope, so a query over such a graph returns exactly what it did before.
Run in every storage mode.
"""

import pandas as pd
import pytest

from kglite import KnowledgeGraph


@pytest.fixture(params=["default", "mapped", "disk"])
def graph(request, tmp_path):
    options = {"storage": request.param}
    if request.param == "disk":
        options["path"] = str(tmp_path / "graph")
    g = KnowledgeGraph(**options)
    g.add_nodes(pd.DataFrame({"id": ["a", "b", "c", "d"]}), "E", "id", "id")
    # Edge a->b stores every envelope-named key; edge c->d stores none of them.
    g.add_connections(
        pd.DataFrame(
            {
                "s": ["a"],
                "t": ["b"],
                "id": ["user-7"],
                "type": ["user-type"],
                "start": ["s"],
                "end": ["e"],
                "text": ["alpha"],
            }
        ),
        "REL",
        "E",
        "s",
        "E",
        "t",
    )
    g.add_connections(pd.DataFrame({"s": ["c"], "t": ["d"], "w": [1], "text": ["beta"]}), "REL", "E", "s", "E", "t")
    return g


def rows(graph, query, **params):
    return graph.cypher(query, params=params or None).to_list()


def test_the_python_relationship_dict_shape_is_unchanged(graph):
    (row,) = rows(graph, "MATCH (:E {id:'a'})-[r:REL]->() RETURN r")
    assert set(row["r"]) == {"id", "start", "end", "type", "properties"}
    assert row["r"]["type"] == "REL"
    assert isinstance(row["r"]["id"], int)
    assert row["r"]["properties"]["type"] == "user-type"
    assert row["r"]["properties"]["id"] == "user-7"


def test_where_and_return_agree_on_a_stored_type(graph):
    assert rows(
        graph,
        "MATCH ()-[r:REL]->() WHERE r.type = 'user-type' RETURN r.type AS t, r.type = 'user-type' AS eq",
    ) == [{"t": "user-type", "eq": True}]
    assert rows(graph, "MATCH (a:E {id:'a'})-[r:REL]->(b) WHERE r.type = 'REL' RETURN b.id AS b") == []


@pytest.mark.parametrize(
    "query",
    [
        "MATCH ()-[r:REL]->() WITH r.type AS t RETURN t ORDER BY t",
        "MATCH ()-[r:REL]->() RETURN r.type AS t ORDER BY r.type",
        "MATCH ()-[r:REL]->() WITH collect(r) AS rs UNWIND rs AS x RETURN x.type AS t ORDER BY t",
        "MATCH p = ()-[:REL]->() UNWIND relationships(p) AS x RETURN x.type AS t ORDER BY t",
    ],
    ids=["with", "order-by", "unwind-value", "path-value"],
)
def test_stored_type_or_the_relationship_type_in_every_clause(graph, query):
    assert rows(graph, query) == [{"t": "REL"}, {"t": "user-type"}]


@pytest.mark.parametrize(
    "prefix",
    [
        "MATCH (:E {id:'a'})-[x:REL]->() ",
        "MATCH (:E {id:'a'})-[r:REL]->() WITH collect(r) AS rs UNWIND rs AS x ",
        "MATCH p = (:E {id:'a'})-[:REL]->() WITH relationships(p)[0] AS x ",
    ],
    ids=["binding", "unwind", "path"],
)
def test_every_stored_key_wins(graph, prefix):
    assert rows(graph, prefix + "RETURN x.id AS id, x.type AS type, x.start AS s, x.`end` AS e") == [
        {"id": "user-7", "type": "user-type", "s": "s", "e": "e"}
    ]


@pytest.mark.parametrize(
    "prefix",
    [
        "MATCH (:E {id:'c'})-[x:REL]->() ",
        "MATCH (:E {id:'c'})-[r:REL]->() WITH collect(r)[0] AS x ",
    ],
    ids=["binding", "value"],
)
def test_without_the_property_the_envelope_answers(graph, prefix):
    assert rows(
        graph,
        prefix + "RETURN x.type = type(x) AS t, x.id = id(x) AS i, "
        "x.start = startNode(x).id AS s, x.end_id = endNode(x).id AS e, x.start AS start",
    ) == [{"t": True, "i": True, "s": True, "e": True, "start": "c"}]


def test_set_type_is_read_back(graph):
    graph.cypher("MATCH (:E {id:'c'})-[r:REL]->() SET r.type = 'set-type'")
    assert rows(graph, "MATCH (:E {id:'c'})-[r:REL]->() RETURN r.type AS t, type(r) AS tt") == [
        {"t": "set-type", "tt": "REL"}
    ]


class _Embedder:
    dimension = 2
    model_id = "test/precedence"

    def load(self) -> None:
        pass

    def unload(self) -> None:
        pass

    def embed(self, texts: list[str]) -> list[list[float]]:
        return [[1.0, 0.0] if text == "alpha" else [0.0, 1.0] for text in texts]


def test_knwler_relation_id_round_trips_through_yield_relationship():
    # knwler stores its own relation id on the relationship; a retrieval that
    # yields the relationship must hand that id back, not the storage slot.
    graph = KnowledgeGraph()
    graph.cypher(
        "CREATE (a:Entity {id:'e1'}), (b:Entity {id:'e2'}), (c:Entity {id:'e3'}), "
        "(a)-[:RELATION {id:'r6', type:'works_at', description:'alpha'}]->(b), "
        "(b)-[:RELATION {id:'r7', type:'leads_to', description:'beta'}]->(c)"
    )
    graph.set_embedder(_Embedder())
    graph.cypher(
        "MATCH ()-[r:RELATION]->() WITH collect(r) AS rs "
        "CALL db.edge_embeddings.embed({type:'RELATION', text_property:'description', relationships: rs}) "
        "YIELD embedded RETURN embedded"
    )
    got = rows(
        graph,
        "CALL db.edge_embeddings.query({type:'RELATION', text_property:'description', text:'alpha', top_k:1}) "
        "YIELD relationship, score "
        "RETURN relationship.id AS id, relationship.type AS type, type(relationship) AS rel_type",
    )
    assert got == [{"id": "r6", "type": "works_at", "rel_type": "RELATION"}]
