"""A write clause's value expressions charge the statement's ``max_work_units``
budget exactly as a read's projection does — cumulatively across the
statement's rows — so ``SET`` / ``CREATE`` / ``MERGE`` / ``FOREACH`` cannot
spend work a read of the same expression would be refused. The 10,000,000
backstop stays the ceiling when no budget is set.

Red proof: each write evaluator ran on a fresh, unlimited budget, so every
refused write below ran to completion (only the backstop applied).
"""

from __future__ import annotations

import pytest

import kglite

BUDGET = r"exceeding the max_work_units budget of 5000"
BUDGET_CONSUMED = r"consumed \d+ collection items.*budget of 5000"


def _graph() -> kglite.KnowledgeGraph:
    g = kglite.KnowledgeGraph()
    g.cypher("CREATE (:N {id: 1})").to_list()
    return g


def _count(g, label: str) -> int:
    return g.cypher(f"MATCH (n:{label}) RETURN count(n) AS c").to_list()[0]["c"]


@pytest.mark.parametrize(
    "write",
    [
        "MATCH (n:N) SET n.x = size(range(1, 10000))",
        "MATCH (n:N) SET n += {x: size(range(1, 10000))}",
        "CREATE (:M {x: size(range(1, 10000))})",
        "MATCH (n:N) CREATE (n)-[:R {x: size(range(1, 10000))}]->(:M)",
        "MERGE (m:M {id: 2}) ON CREATE SET m.x = size(range(1, 10000))",
        "MATCH (n:N) MERGE (m:M {id: 3}) ON MATCH SET m.x = 0 ON CREATE SET m.x = size(range(1, 10000))",
    ],
    ids=["set", "set-map", "create", "create-rel", "merge-on-create", "merge-after-match"],
)
def test_a_write_expression_over_the_budget_is_refused(write) -> None:
    g = _graph()
    # The read of the same expression is refused, so the write is too.
    with pytest.raises(kglite.CypherExecutionError, match=BUDGET):
        g.cypher("MATCH (n:N) RETURN size(range(1, 10000)) AS x", max_work_units=5000).to_list()
    with pytest.raises(kglite.CypherExecutionError, match=BUDGET):
        g.cypher(write, max_work_units=5000).to_list()
    assert _count(g, "M") == 0
    assert g.cypher("MATCH (n:N) RETURN n.x AS x").to_list() == [{"x": None}]
    # Within the budget it runs.
    g.cypher(write, max_work_units=50_000).to_list()


def test_the_charge_is_cumulative_across_rows_as_in_a_read() -> None:
    g = _graph()
    read = "UNWIND range(1, 200) AS i RETURN size(range(1, i)) AS x"
    write = "UNWIND range(1, 200) AS i CREATE (:M {x: size(range(1, i))})"
    for query in (read, write):
        with pytest.raises(kglite.CypherExecutionError, match=r"consumed \d+ collection items.*budget of 5000"):
            g.cypher(query, max_work_units=5000).to_list()
    assert _count(g, "M") == 0
    g.cypher(write, max_work_units=30_000).to_list()
    assert _count(g, "M") == 200


def test_a_foreach_list_charges_the_budget() -> None:
    g = _graph()
    with pytest.raises(kglite.CypherExecutionError, match=BUDGET):
        g.cypher(
            "MATCH (n:N) FOREACH (k IN range(1, 3) | SET n.x = size(range(1, 10000)))",
            max_work_units=5000,
        ).to_list()


def test_ordinary_write_expressions_charge_nothing() -> None:
    """The UNWIND's own ``range()`` consumes one collection item per row; the
    ``CREATE``'s arithmetic and string expressions consume none, so a budget
    of exactly the row count passes and one less is refused at the
    ``range()``. The default (no budget) path is the backstop alone."""
    g = kglite.KnowledgeGraph()
    rows = 200_000
    write = f"UNWIND range(1, {rows}) AS i CREATE (:B {{i: i, s: toString(i), d: i * 2}})"
    with pytest.raises(kglite.CypherExecutionError, match=r"range\(\)"):
        g.cypher(write, max_work_units=rows - 1).to_list()
    assert _count(g, "B") == 0
    g.cypher(write, max_work_units=rows).to_list()
    assert _count(g, "B") == rows
    g.cypher(write.replace(":B", ":C")).to_list()
    assert _count(g, "C") == rows


def test_the_transaction_and_session_paths_charge_the_budget() -> None:
    g = _graph()
    with g.begin() as tx:
        with pytest.raises(kglite.CypherExecutionError, match=BUDGET):
            tx.cypher("MATCH (n:N) SET n.x = size(range(1, 10000))", max_work_units=5000)
    session = g.session()
    with pytest.raises(kglite.CypherExecutionError, match=BUDGET):
        session.execute("CREATE (:M {x: size(range(1, 10000))})", max_work_units=5000)
    assert session.cypher("MATCH (m:M) RETURN count(m) AS c").to_list() == [{"c": 0}]
    session.execute("CREATE (:M {x: size(range(1, 10000))})", max_work_units=50_000)
    assert session.cypher("MATCH (m:M) RETURN count(m) AS c").to_list() == [{"c": 1}]


@pytest.mark.parametrize(
    "write",
    [
        "MATCH (n:N) SET n.x = size(range(1, 600))",
        "MATCH (n:N) SET n.x = size(range(1, 600)) + 1, n.y = size(range(1, 600))",
        "UNWIND range(1, 10) AS i CREATE (:M {i: i, x: size(range(1, 600))})",
        "UNWIND range(1, 10) AS i MERGE (m:M {id: i}) ON CREATE SET m.x = size(range(1, 600))",
        "MATCH (n:N) CREATE (n)-[:R {x: size(range(1, 600))}]->(:M)",
    ],
    ids=["set", "set-two-items", "unwind-create", "merge-on-create", "create-rel"],
)
def test_a_row_invariant_expression_is_charged_once_per_clause_as_in_a_read(write) -> None:
    """A read folds an expression that is the same on every row once per
    clause, so ``RETURN size(range(1, 600))`` over 10 rows charges 600; a write
    clause folds it the same way, instead of charging it on every row."""
    g = kglite.KnowledgeGraph()
    g.cypher("UNWIND range(1, 10) AS i CREATE (:N {id: i})").to_list()
    read = "MATCH (n:N) RETURN size(range(1, 600)) AS x"
    assert len(g.cypher(read, max_work_units=5000).to_list()) == 10
    g.cypher(write, max_work_units=5000).to_list()
    # A row-dependent expression is still charged per row, in both.
    with pytest.raises(kglite.CypherExecutionError, match=BUDGET_CONSUMED):
        g.cypher("MATCH (n:N) RETURN size(range(1, 600 + n.id)) AS x", max_work_units=5000).to_list()
    with pytest.raises(kglite.CypherExecutionError, match=BUDGET_CONSUMED):
        g.cypher("MATCH (n:N) SET n.z = size(range(1, 600 + n.id))", max_work_units=5000).to_list()
