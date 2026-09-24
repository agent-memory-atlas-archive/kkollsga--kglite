"""Inline-map values in MATCH take the expression grammar CREATE's maps take.

`MATCH (d:D {id: row[0]})` used to fail with "Pattern parse error: Expected
property key or '}'", although `CREATE (:D {id: row[0]})` accepted the same
value: a MATCH pattern goes through a secondary pattern lexer that reads only a
scalar literal, `$param`, `var` or `var.prop` as a value. Run in every storage
mode.
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
    g.add_nodes(pd.DataFrame({"id": ["a", "b", "c"], "n": [2, 3, 4]}), "D", "id", "id")
    g.add_connections(pd.DataFrame({"s": ["a"], "t": ["b"], "w": [5]}), "R", "D", "s", "D", "t")
    return g


def rows(graph, query, **params):
    return graph.cypher(query, params=params or None).to_list()


@pytest.mark.parametrize(
    "query",
    [
        "UNWIND [['a', 1]] AS row MATCH (d:D {id: row[0]}) RETURN d.id AS id",
        "UNWIND [{k: 'a'}] AS row MATCH (d:D {id: row['k']}) RETURN d.id AS id",
        "WITH ['x', 'a'] AS list MATCH (d:D {id: list[1]}) RETURN d.id AS id",
        "WITH {k: ['a']} AS map MATCH (d:D {id: map.k[0]}) RETURN d.id AS id",
        "UNWIND ['A'] AS name MATCH (d:D {id: toLower(name)}) RETURN d.id AS id",
        "UNWIND [1] AS x MATCH (d:D {n: x + 1}) RETURN d.id AS id",
        "MATCH (d:D {id: toLower('A')}) RETURN d.id AS id",
        "MATCH (d:D {n: 1 + 1}) RETURN d.id AS id",
        "MATCH (d:D {id: $list[1]}) RETURN d.id AS id",
    ],
    ids=[
        "index",
        "key",
        "list",
        "map-member-index",
        "function",
        "arithmetic",
        "const-function",
        "const-arith",
        "param-index",
    ],
)
def test_expression_values_match(graph, query):
    assert rows(graph, query, list=["x", "a"]) == [{"id": "a"}]


@pytest.mark.parametrize(
    "query",
    [
        "UNWIND [['a']] AS row MATCH (d:D {id: row[0]}) RETURN count(*) AS c",
        "MATCH (d:D {n: 1 + 1}) RETURN count(*) AS c",
        "UNWIND [['a'], ['b']] AS row MATCH (d:D {id: row[0]}) WITH d ORDER BY d.id LIMIT 1 RETURN count(*) AS c",
    ],
)
def test_aggregating_shapes_see_the_value(graph, query):
    assert rows(graph, query) == [{"c": 1}]


def test_the_default_value_forms_are_unchanged(graph):
    assert rows(graph, "MATCH (d:D {id: 'a'}) RETURN d.id AS id") == [{"id": "a"}]
    assert rows(graph, "MATCH (d:D {id: $p}) RETURN d.id AS id", p="a") == [{"id": "a"}]
    assert rows(graph, "WITH 'a' AS v MATCH (d:D {id: v}) RETURN d.id AS id") == [{"id": "a"}]
    assert rows(graph, "MATCH (d:D {id: null}) RETURN d.id AS id") == []


def test_optional_match_relationship_maps_exists_and_merge(graph):
    assert rows(
        graph,
        "UNWIND [['zz']] AS row OPTIONAL MATCH (d:D {id: row[0]}) RETURN row[0] AS k, d.id AS id",
    ) == [{"k": "zz", "id": None}]
    assert rows(graph, "UNWIND [[5]] AS row MATCH (:D)-[r:R {w: row[0]}]->(b) RETURN b.id AS id") == [{"id": "b"}]
    assert rows(
        graph,
        "UNWIND [['b'], ['zz']] AS row RETURN row[0] AS k, EXISTS { (d:D {id: row[0]}) } AS e",
    ) == [{"k": "b", "e": True}, {"k": "zz", "e": False}]
    graph.cypher("UNWIND [['a']] AS row MERGE (d:D {id: row[0]})")
    assert rows(graph, "MATCH (d:D) RETURN count(*) AS c") == [{"c": 3}]


def test_an_expression_that_fails_is_an_error_not_a_silent_miss(graph):
    with pytest.raises(Exception, match="division by zero"):
        rows(graph, "UNWIND [0] AS z MATCH (d:D {n: 1 / z}) RETURN d.id AS id")


def test_an_unparseable_value_names_the_construct_not_a_missing_key(graph):
    with pytest.raises(Exception) as error:
        rows(graph, "UNWIND [1] AS row MATCH (d:D {id: row[}) RETURN d")
    message = str(error.value)
    assert "Unexpected token in expression" in message
    assert "Expected property key" not in message
