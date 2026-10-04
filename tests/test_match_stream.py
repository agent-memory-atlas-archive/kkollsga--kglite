"""A leading MATCH streamed into an aggregate answers as the materialized route.

The streaming pipeline now takes its rows from the first MATCH's matcher a
slice of start nodes at a time instead of from a materialized row set. Every
shape here runs with ``streaming=False`` and ``streaming=True`` and must answer
identically (values compared through ``repr``, which prints a float at full
precision, so a reassociated sum fails) — undated, at twelve instants and under
ALL. The fixture is the fused-aggregate suite's organisation graph.
"""

from __future__ import annotations

import pandas as pd
import pytest

import kglite
from tests.test_valid_time_fused_aggregates import INSTANTS, at, org  # noqa: F401

CONTEXTS = ["", *[at(d, "") for d in INSTANTS], "FOR VALID_TIME ALL "]

CHAIN = "MATCH (a:Emp)-[:IN_DEPT]->(d:Dept)<-[:IN_DEPT]-(b:Emp)"

SHAPES = [
    f"{CHAIN} RETURN sum(a.id * 0.1 + b.id * 0.07) AS s",
    f"{CHAIN} RETURN avg(b.id * 0.3) AS m, min(a.id) AS lo, max(b.id) AS hi, count(*) AS n",
    f"{CHAIN} RETURN d.id AS d, sum(a.id * 0.1 + b.id * 0.07) AS s, avg(b.id * 0.3) AS m, count(*) AS n",
    f"{CHAIN} RETURN d.title AS t, sum(b.id * 0.1) AS s, count(DISTINCT b.id) AS people",
    f"{CHAIN} RETURN count(DISTINCT a) AS n, sum(b.id) AS s",
    f"{CHAIN} WHERE a.id < b.id RETURN d.id AS d, sum(b.id * 0.1) AS s",
    f"{CHAIN} RETURN a.id AS a, sum(b.id * 0.1) AS s ORDER BY s DESC LIMIT 3",
    f"{CHAIN} WITH d.id AS d, sum(b.id * 0.1) AS s WHERE s > 0.5 RETURN d, s",
    "MATCH (a:Emp)-[:IN_DEPT]->(d:Dept)<-[:LINKED]-(e:Emp)-[:IN_DEPT]->(d2:Dept) "
    "RETURN d.id AS d, d2.id AS d2, count(*) AS n, sum(e.id * 0.1) AS s",
    "MATCH p = (a:Emp)-[:IN_DEPT]->(d:Dept)<-[:IN_DEPT]-(b:Emp) "
    "RETURN d.id AS d, sum(length(p)) AS hops, count(*) AS n",
    "MATCH (a:Emp {id: -5})-[:IN_DEPT]->(d:Dept)<-[:IN_DEPT]-(b:Emp) RETURN sum(b.id * 0.1) AS s, count(*) AS n",
    "MATCH (a:Emp {id: -5})-[:IN_DEPT]->(d:Dept)<-[:IN_DEPT]-(b:Emp) RETURN d.id AS d, count(*) AS n",
    "MATCH (e:Emp) RETURN e.team AS team, sum(e.id * 0.1) AS s, avg(e.id * 0.3) AS m",
    "MATCH (a:Emp)-[r:IN_DEPT]->(d:Dept) RETURN d.id AS d, count(r) AS n, sum(a.id * 0.1) AS s",
    f"{CHAIN} RETURN d.id AS d, count(a) AS na, count(b) AS nb, count(*) AS n",
    "MATCH p = (a:Emp)-[:IN_DEPT]->(d:Dept)<-[:IN_DEPT]-(b:Emp) RETURN d.id AS d, count(p) AS paths, count(b) AS nb",
]


def _rows(rows):
    return [repr(sorted(row.items())) for row in rows]


@pytest.mark.parametrize("shape", SHAPES)
def test_streamed_answers_as_materialized(org, shape):  # noqa: F811
    for context in CONTEXTS:
        query = context + shape
        off = org.cypher(query, streaming=False).to_list()
        on = org.cypher(query, streaming=True).to_list()
        # Group order is first-seen order on both routes, so rows are
        # compared in order — except where the query has no ORDER BY the
        # engine promises none; sorting guards only against that, the
        # values themselves stay bit-for-bit.
        assert sorted(_rows(on)) == sorted(_rows(off)), (context, shape)
        if "ORDER BY" in shape:
            assert _rows(on) == _rows(off), (context, shape)


def test_float_sums_associate_in_row_order():
    """Many nodes sharing a grouping value used to fold per-node partial sums."""
    graph = kglite.KnowledgeGraph()
    graph.add_nodes(
        pd.DataFrame({"id": range(60), "band": [i % 3 for i in range(60)]}),
        "Team",
        "id",
        "id",
    )
    rows = [(t, 1000 + t * 7 + j) for t in range(60) for j in range(5)]
    graph.add_nodes(
        pd.DataFrame(
            {
                "id": [r[1] for r in rows],
                "w": [(r[1] * 0.37) / 3.0 + 1e-9 * r[1] for r in rows],
            }
        ),
        "Task",
        "id",
        "id",
    )
    graph.add_relationships(
        pd.DataFrame({"team": [r[0] for r in rows], "task": [r[1] for r in rows]}),
        "OWNS",
        "Team",
        "team",
        "Task",
        "task",
    )
    shape = "MATCH (t:Team)-[:OWNS]->(k:Task) RETURN t.band AS band, sum(k.w) AS s, avg(k.w) AS a"
    off = graph.cypher(shape, streaming=False).to_list()
    on = graph.cypher(shape, streaming=True).to_list()
    assert _rows(on) == _rows(off)
    # Not vacuous: the sums are not representable exactly in any order, so a
    # reassociated fold would land on different digits for at least one band.
    assert len({row["s"] for row in off}) == 3


def test_an_explicit_budget_errors_identically_on_both_routes(org):  # noqa: F811
    query = at("2006-01-01", f"{CHAIN} RETURN d.id AS d, sum(b.id * 0.1) AS s")
    messages = []
    for streaming in (False, True):
        with pytest.raises(kglite.CypherExecutionError, match="max_work_units") as excinfo:
            org.cypher(query, max_work_units=1, streaming=streaming)
        messages.append(str(excinfo.value))
    assert messages[0] == messages[1]
    assert len(org.cypher(query, max_work_units=1000, streaming=True).to_list()) == 2


def test_a_deadline_aborts_a_streamed_aggregate():
    graph = kglite.KnowledgeGraph()
    graph.add_nodes(pd.DataFrame({"id": range(40)}), "A", "id", "id")
    graph.add_nodes(pd.DataFrame({"id": [0]}), "H", "id", "id")
    graph.add_nodes(pd.DataFrame({"id": range(4000)}), "C", "id", "id")
    graph.add_relationships(
        pd.DataFrame({"s": range(40), "t": [0] * 40}),
        "R1",
        "A",
        "s",
        "H",
        "t",
    )
    graph.add_relationships(
        pd.DataFrame({"s": [0] * 4000, "t": range(4000)}),
        "R2",
        "H",
        "s",
        "C",
        "t",
    )
    query = "MATCH (a:A)-[:R1]->(h:H)-[:R2]->(c:C) RETURN h.id AS h, sum(c.id * 0.5) AS s"
    assert graph.cypher(query, streaming=True).to_list() == graph.cypher(query, streaming=False).to_list()
    with pytest.raises(kglite.CypherTimeoutError):
        graph.cypher(query, streaming=True, timeout_ms=1)
