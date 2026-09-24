"""A subquery pattern starts at a node the row already holds, however it holds it.

`COUNT { (s)--() }` and `EXISTS { (s)-->() }` were anchored when `s` was a
MATCH binding but scanned the whole graph per row when `s` arrived as a
*value* — `WITH startNode(r) AS s`, `UNWIND collect(n) AS s` — and a
correlated `CALL { WITH s MATCH (s)--(:P) }` started at the `:P` label scan
instead of the imported `s`. The answers were right; the cost grew with the
graph. The subquery-expression route is pinned through `max_work_units`: the
anchored plan does a handful of work per row, the scan does the whole chain,
so a budget between the two is red on the scanning plan. (The label scan a
reversed `CALL` body started from charges no work units, so that route is
pinned by the planner's `start_anchor_tests`.) The answers are pinned as
absolute values.
"""

from __future__ import annotations

import pytest

import kglite

N = 3_000
BUDGET = 1_500  # well above the anchored plans' work, well below one chain scan


@pytest.fixture(scope="module")
def chain() -> kglite.KnowledgeGraph:
    graph = kglite.KnowledgeGraph()
    graph.cypher("UNWIND range(0, $n - 1) AS i CREATE (:P {id: i})", params={"n": N})
    graph.cypher(
        "UNWIND range(0, $n - 2) AS i MATCH (a:P {id: i}), (b:P {id: i + 1}) CREATE (a)-[:R]->(b)",
        params={"n": N},
    )
    return graph


SEEDS = "UNWIND range(0, 4) AS i MATCH (s:P {id: i}) "
# Node 0 has one neighbour, nodes 1..4 two: 1 + 4 * 2 = 9.
ANCHORED = [
    pytest.param(SEEDS + "RETURN sum(COUNT { (s)--() }) AS n", 9, id="count-match-binding"),
    pytest.param(
        SEEDS + "WITH collect(s) AS ns UNWIND ns AS s RETURN sum(COUNT { (s)--() }) AS n", 9, id="count-unwind-value"
    ),
    pytest.param(
        "UNWIND range(0, 4) AS i MATCH (:P {id: i})-[r:R]->() WITH startNode(r) AS s "
        "RETURN sum(COUNT { (s)--() }) AS n",
        9,
        id="count-startnode-value",
    ),
    pytest.param(
        SEEDS + "WITH collect(s) AS ns UNWIND ns AS s RETURN sum(COUNT { (s)<--(:P) }) AS n",
        4,
        id="count-value-labelled-far-end",
    ),
    pytest.param(
        SEEDS + "WITH collect(s) AS ns UNWIND ns AS s RETURN sum(CASE WHEN EXISTS { (s)<--() } THEN 1 ELSE 0 END) AS n",
        4,
        id="exists-unwind-value",
    ),
    pytest.param(
        SEEDS + "WITH collect(s) AS ns UNWIND ns AS s WITH s WHERE EXISTS { (s)-[:R]->(t) WHERE t.id > 2 } "
        "RETURN count(s) AS n",
        3,
        id="exists-value-with-where",
    ),
    pytest.param(
        SEEDS + "CALL { WITH s MATCH (s)--(:P) RETURN count(*) AS d } RETURN sum(d) AS n",
        9,
        id="call-import-labelled-far-end",
    ),
    pytest.param(
        SEEDS + "WITH collect(s) AS ns UNWIND ns AS s CALL { WITH s MATCH (s)--(:P) RETURN count(*) AS d } "
        "RETURN sum(d) AS n",
        9,
        id="call-import-value-labelled-far-end",
    ),
    pytest.param(
        SEEDS + "WITH collect(s) AS ns UNWIND ns AS s MATCH (s)--(:P) RETURN count(*) AS n",
        9,
        id="match-value-labelled-far-end",
    ),
]


@pytest.mark.parametrize(("query", "expected"), ANCHORED)
def test_subquery_anchors_on_the_row_node(chain, query, expected) -> None:
    assert chain.cypher(query).to_list() == [{"n": expected}]
    assert chain.cypher(query, disable_optimizer=True).to_list() == [{"n": expected}]
    assert chain.cypher(query, max_work_units=BUDGET).to_list() == [{"n": expected}]


def test_budget_is_red_on_a_scanning_plan(chain) -> None:
    # The control: an unanchored subquery over the same chain does exceed it,
    # so the budget above can tell the two plans apart.
    with pytest.raises(kglite.CypherExecutionError, match="max_work_units"):
        chain.cypher(SEEDS + "RETURN sum(COUNT { (:P)-->(:P) }) AS n", max_work_units=BUDGET).to_list()


def test_a_null_value_matches_nothing(chain) -> None:
    rows = chain.cypher(
        "UNWIND [null] AS s RETURN COUNT { (s)--() } AS c, EXISTS { (s)--() } AS e", max_work_units=BUDGET * 10
    ).to_list()
    assert rows == [{"c": 0, "e": False}]


def test_a_deleted_node_value_matches_nothing() -> None:
    graph = kglite.KnowledgeGraph()
    graph.cypher("CREATE (:P {id: 1})-[:R]->(:P {id: 2})")
    rows = graph.cypher(
        "MATCH (s:P {id: 1}) WITH collect(s) AS ns MATCH (d:P {id: 1}) DETACH DELETE d "
        "WITH ns UNWIND ns AS s RETURN COUNT { (s)--() } AS c"
    ).to_list()
    assert rows == [{"c": 0}]
