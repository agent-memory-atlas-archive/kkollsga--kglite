"""Scope of ORDER BY / WHERE attached to a projection.

A non-aggregating, non-DISTINCT WITH or RETURN keeps one output row per input
row, so its ORDER BY, SKIP/LIMIT and WHERE may still read variables the
projection dropped. A DISTINCT or aggregating projection merges rows, leaving
dropped variables without a value, so they stay refused. Variables never leak
past the clause group into later clauses.
"""

from __future__ import annotations

import pytest

import kglite


def _graph():
    graph = kglite.KnowledgeGraph()
    graph.cypher("CREATE (:A {name:'x', age:3}), (:A {name:'y', age:1}), (:A {name:'z', age:2})").to_list()
    return graph


def _col(graph, query, column="n"):
    return [row[column] for row in graph.cypher(query).to_list()]


def test_with_order_by_reads_a_dropped_variable():
    g = _graph()
    assert _col(g, "MATCH (a:A) WITH a.name AS n ORDER BY a.age RETURN n") == ["y", "z", "x"]
    assert _col(g, "MATCH (a:A) WITH a.name AS n ORDER BY a.age DESC RETURN n") == ["x", "z", "y"]


def test_with_order_by_dropped_variable_with_skip_and_limit():
    g = _graph()
    assert _col(g, "MATCH (a:A) WITH a.name AS n ORDER BY a.age SKIP 1 LIMIT 1 RETURN n") == ["z"]
    assert _col(g, "MATCH (a:A) WITH a.name AS n ORDER BY a.age LIMIT 2 RETURN n") == ["y", "z"]


def test_with_order_by_dropped_scalar_variable():
    g = _graph()
    q = "UNWIND [3, 1, 2] AS v WITH v * 10 AS n ORDER BY v RETURN n"
    assert _col(g, q) == [10, 20, 30]


def test_with_order_by_mixes_projected_alias_and_dropped_variable():
    g = _graph()
    q = "MATCH (a:A) WITH a.name AS n, a.age % 2 AS parity ORDER BY parity, a.age DESC RETURN n"
    assert _col(g, q) == ["z", "x", "y"]


def test_with_where_reads_a_dropped_variable():
    g = _graph()
    assert sorted(_col(g, "MATCH (a:A) WITH a.name AS n WHERE a.age > 1 RETURN n")) == ["x", "z"]


def test_with_order_by_and_where_together_keep_all_scope():
    g = _graph()
    q = "MATCH (a:A) WITH a.name AS n WHERE a.age > 1 ORDER BY a.age RETURN n"
    assert _col(g, q) == ["z", "x"]


def test_return_order_by_reads_a_dropped_variable():
    g = _graph()
    assert _col(g, "MATCH (a:A) RETURN a.name AS n ORDER BY a.age") == ["y", "z", "x"]


def test_dropped_variable_does_not_leak_past_the_clause_group():
    g = _graph()
    with pytest.raises(kglite.SchemaError, match="Undefined variable"):
        g.cypher("MATCH (a:A) WITH a.name AS n ORDER BY a.age RETURN a.age").to_list()
    with pytest.raises(kglite.SchemaError, match="Undefined variable"):
        g.cypher("MATCH (a:A) WITH a.name AS n WHERE a.age > 1 RETURN a").to_list()
    # A later MATCH binds afresh instead of reusing the dropped binding.
    q = "MATCH (a:A {name:'x'}) WITH a.name AS n ORDER BY a.age MATCH (a:A {name:'y'}) RETURN n, a.name AS m"
    assert g.cypher(q).to_list() == [{"n": "x", "m": "y"}]


@pytest.mark.parametrize(
    "query",
    (
        "MATCH (a:A) WITH DISTINCT a.name AS n ORDER BY a.age RETURN n",
        "MATCH (a:A) WITH a.name AS n, count(*) AS c ORDER BY a.age RETURN n",
        "MATCH (a:A) WITH DISTINCT a.name AS n WHERE a.age > 1 RETURN n",
        "MATCH (a:A) WITH a.name AS n, count(*) AS c WHERE a.age > 1 RETURN n",
        "MATCH (a:A) RETURN DISTINCT a.name AS n ORDER BY a.age",
        "MATCH (a:A) WITH a.name AS n ORDER BY count(a) RETURN n",
        "MATCH (a:A) WITH a.name AS n ORDER BY max(a.age) DESC RETURN n",
        "MATCH (a:A) RETURN a.name AS n ORDER BY count(*)",
    ),
)
def test_distinct_and_aggregating_projections_keep_dropped_variables_refused(query):
    with pytest.raises(kglite.SchemaError):
        _graph().cypher(query).to_list()


def test_with_order_by_dropped_variable_inside_write_query():
    g = _graph()
    g.cypher("MATCH (a:A) WITH a ORDER BY a.age LIMIT 1 SET a.first = true").to_list()
    q = "MATCH (a:A) WITH a.name AS n ORDER BY a.age CREATE (:Out {n: n}) RETURN n"
    assert _col(g, q) == ["y", "z", "x"]
