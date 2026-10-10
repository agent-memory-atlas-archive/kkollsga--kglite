"""A parameterised statement is plan-cached per exact parameter values.

The planner bakes the parameter values it reads (index pushdown, ``LIMIT
$n``, anchors) into the plan, so a cached plan may serve only the values it
was built from. Each shape below is run interleaved over several values, four
rounds, so every (shape, value) pair is planned, stored and then hit, and each
answer is compared with the unoptimised path (``disable_optimizer=True``,
which never touches the cache).
"""

from __future__ import annotations

import pandas as pd
import pytest

import kglite

N = 60


@pytest.fixture(scope="module")
def graph() -> kglite.KnowledgeGraph:
    g = kglite.KnowledgeGraph()
    g.add_nodes(
        pd.DataFrame(
            {
                "id": list(range(N)),
                "name": [f"p{i}" for i in range(N)],
                "age": [i % 7 for i in range(N)],
                "tag": [None if i % 5 == 0 else f"t{i % 3}" for i in range(N)],
            }
        ),
        "Person",
        "id",
        "name",
    )
    g.add_relationships(
        pd.DataFrame({"a": list(range(N)), "b": [(i * 7 + 1) % N for i in range(N)]}),
        "KNOWS",
        "Person",
        "a",
        "Person",
        "b",
    )
    return g


SHAPES = [
    ("inline_id", "MATCH (n:Person {id: $x}) RETURN n.name AS v", "x", [0, 7, 7, 59, 999, -1, None]),
    ("where_id", "MATCH (n:Person) WHERE n.id = $x RETURN n.name AS v", "x", [3, 4, 3, 1000, None]),
    ("where_name", "MATCH (n:Person) WHERE n.name = $x RETURN n.id AS v", "x", ["p3", "p4", "p3", "nope", ""]),
    ("range", "MATCH (n:Person) WHERE n.age >= $x RETURN count(n) AS v", "x", [0, 3, 6, 7, 3]),
    (
        "in_list",
        "MATCH (n:Person) WHERE n.id IN $x RETURN n.id AS v ORDER BY v",
        "x",
        [[1, 2], [3], [], [1, 2], [58, 59, 60]],
    ),
    (
        "starts_with",
        "MATCH (n:Person) WHERE n.name STARTS WITH $x RETURN count(n) AS v",
        "x",
        ["p1", "p5", "", "zz", "p1"],
    ),
    ("limit", "MATCH (n:Person) RETURN n.id AS v ORDER BY v LIMIT $x", "x", [1, 5, 0, 5, 12]),
    ("skip_limit", "MATCH (n:Person) RETURN n.id AS v ORDER BY v SKIP $x LIMIT 3", "x", [0, 4, 57, 4, 100]),
    ("two_hop", "MATCH (a:Person {id: $x})-[:KNOWS]->(b)-[:KNOWS]->(c) RETURN c.id AS v", "x", [0, 1, 0, 2, 9999]),
    ("null_tag", "MATCH (n:Person) WHERE n.tag = $x RETURN count(n) AS v", "x", ["t0", "t1", None, "t0", "t2"]),
    (
        "unwind",
        "UNWIND $x AS i MATCH (n:Person {id: i}) RETURN n.name AS v ORDER BY v",
        "x",
        [[1, 2, 3], [4], [1, 2, 3], []],
    ),
    ("float", "MATCH (n:Person) WHERE n.age > $x RETURN count(n) AS v", "x", [0.5, 3.0, 0.5, -0.0, 0.0, 5.99]),
    ("bool_expr", "RETURN $x AS v", "x", [True, False, 1, "1", 1.0]),
]


@pytest.mark.parametrize("name,query,key,values", SHAPES, ids=[s[0] for s in SHAPES])
def test_cached_answers_match_the_unoptimised_path(graph, name, query, key, values):
    for round_no in range(4):
        for value in values:
            got = graph.cypher(query, params={key: value}).to_list()
            expected = graph.cypher(query, params={key: value}, disable_optimizer=True).to_list()
            assert got == expected, (name, round_no, value)


def test_two_parameters_each_vary_independently(graph):
    q = "MATCH (n:Person) WHERE n.age = $a AND n.id >= $lo RETURN count(n) AS v"
    for _ in range(4):
        for a, lo in [(1, 0), (1, 30), (2, 0), (1, 0), (6, 59), (2, 30)]:
            got = graph.cypher(q, params={"a": a, "lo": lo}).to_list()
            expected = graph.cypher(q, params={"a": a, "lo": lo}, disable_optimizer=True).to_list()
            assert got == expected, (a, lo)


def test_a_write_invalidates_parameterised_plans():
    g = kglite.KnowledgeGraph()
    g.cypher("CREATE (:Item {id: 1, name: 'one'})")
    q = "MATCH (n:Item {id: $x}) RETURN n.name AS v"
    for _ in range(4):
        assert g.cypher(q, params={"x": 1}).to_list() == [{"v": "one"}]
    g.cypher("MATCH (n:Item {id: 1}) SET n.name = 'uno'")
    for _ in range(4):
        assert g.cypher(q, params={"x": 1}).to_list() == [{"v": "uno"}]
    g.cypher("CREATE (:Item {id: 2, name: 'two'})")
    for _ in range(4):
        assert g.cypher(q, params={"x": 2}).to_list() == [{"v": "two"}]


def test_a_missing_parameter_still_errors_after_the_bound_form_is_cached(graph):
    q = "MATCH (n:Person {id: $x}) RETURN n.name AS v"
    for _ in range(4):
        graph.cypher(q, params={"x": 3}).to_list()
    for _ in range(2):
        with pytest.raises(Exception, match="Missing parameter"):
            graph.cypher(q).to_list()
