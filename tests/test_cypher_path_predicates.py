"""Absolute goldens for path variables read before the end of their clause.

A path variable (`p = …`) must be bound wherever the clause's own WHERE can
read it: the leading MATCH's fused WHERE, a WHERE the planner hoists into it
(`hoist_with_where`), and a non-leading OPTIONAL MATCH's scoped WHERE. Each of
those once ran before the path existed, so `length(p)` was null and every row
was dropped. The optimised and unoptimised plans share that executor code, so
the differential corpus cannot see it; these expected values can.

The graph is `(a:A)-[:R {w:1}]->(b:B)-[:R {w:2}]->(c:B)` plus an isolated
`(x:A)` and a separate `(x2:A)-[:S]->(y:B)`: one fixed-length path from an
`A`, two variable-length ones, and an `A` with no `R` edge for the
null-extended OPTIONAL MATCH rows.
"""

from __future__ import annotations

import pytest

import kglite

FIXED = "MATCH p=(a:A)-[:R]->(b)"
VARLEN = "MATCH p=(a:A)-[:R*1..2]->(b)"


@pytest.fixture(scope="module")
def graph():
    g = kglite.KnowledgeGraph()
    g.cypher(
        "CREATE (a:A {name:'a'})-[:R {w:1}]->(b:B {name:'b'})-[:R {w:2}]->(c:B {name:'c'}), "
        "(x:A {name:'x'}), (x2:A {name:'x2'})-[:S {w:9}]->(y:B {name:'y'})"
    )
    return g


def _count(graph, query, **kwargs):
    return graph.cypher(query, **kwargs).to_list()[0]["n"]


# (predicate, fixed-length count, variable-length count). The fixed path is
# a->b; the variable-length paths are a->b and a->b->c.
PATH_WHERE = [
    ("length(p) > 0", 1, 2),
    ("length(p) = 2", 0, 1),
    ("size(nodes(p)) > 1", 1, 2),
    ("size(relationships(p)) > 0", 1, 2),
    ("all(r IN relationships(p) WHERE r.w > 0)", 1, 2),
    ("any(r IN relationships(p) WHERE r.w = 2)", 0, 1),
    ("any(n IN nodes(p) WHERE n.name = 'a')", 1, 2),
    ("p IS NOT NULL", 1, 2),
    ("nodes(p)[1].name = 'b'", 1, 2),
    ("last(nodes(p)).name = 'c'", 0, 1),
    ("true AND length(p) > 0", 1, 2),
    ("b.name IS NOT NULL AND length(p) >= 1", 1, 2),
]


@pytest.mark.parametrize("predicate,fixed,varlen", PATH_WHERE, ids=[p for p, _, _ in PATH_WHERE])
def test_leading_match_where_reads_path(graph, predicate, fixed, varlen):
    assert _count(graph, f"{FIXED} WHERE {predicate} RETURN count(*) AS n") == fixed
    assert _count(graph, f"{VARLEN} WHERE {predicate} RETURN count(*) AS n") == varlen


# `WITH p` carries only the path, so the WITH form drops the case reading `b`.
WITH_WHERE = [case for case in PATH_WHERE if "b.name" not in case[0]]


@pytest.mark.parametrize("predicate,fixed,varlen", WITH_WHERE, ids=[p for p, _, _ in WITH_WHERE])
def test_with_where_reads_path(graph, predicate, fixed, varlen):
    """`hoist_with_where` moves some of these into the MATCH; all must agree."""
    for kwargs in ({}, {"disabled_passes": ["hoist_with_where"]}):
        assert _count(graph, f"{FIXED} WITH p WHERE {predicate} RETURN count(*) AS n", **kwargs) == fixed
        assert _count(graph, f"{VARLEN} WITH p WHERE {predicate} RETURN count(*) AS n", **kwargs) == varlen


def test_path_where_projects_the_filtered_rows(graph):
    rows = graph.cypher(
        f"{VARLEN} WHERE length(p) = 2 RETURN [n IN nodes(p) | n.name] AS names, [r IN relationships(p) | r.w] AS ws"
    ).to_list()
    assert rows == [{"names": ["a", "b", "c"], "ws": [1, 2]}]


def test_node_only_path_where(graph):
    assert _count(graph, "MATCH p=(a:A) WHERE length(p) = 0 RETURN count(*) AS n") == 3


def test_union_branch_path_where(graph):
    rows = graph.cypher(
        f"{FIXED} WHERE length(p) > 0 RETURN a.name AS n UNION MATCH (c:B {{name:'c'}}) RETURN c.name AS n"
    ).to_list()
    assert sorted(r["n"] for r in rows) == ["a", "c"]


def test_later_match_path_where(graph):
    """The WHERE of a non-leading MATCH was never fused; pinned alongside."""
    assert _count(graph, "MATCH (x:A) MATCH p=(x)-[:R]->(b) WHERE length(p) > 0 RETURN count(*) AS n") == 1


def test_path_where_under_limit(graph):
    assert _count(graph, f"{VARLEN} WHERE length(p) = 2 WITH p LIMIT 1 RETURN count(*) AS n") == 1


def test_path_where_with_distinct_target(graph):
    """The matcher-level DISTINCT licence must still see the bound path."""
    rows = graph.cypher(f"{VARLEN} WHERE length(p) = 2 RETURN DISTINCT b.name AS b").to_list()
    assert rows == [{"b": "c"}]


# --- OPTIONAL MATCH ---------------------------------------------------------


def test_leading_optional_match_binds_path(graph):
    rows = graph.cypher(
        "OPTIONAL MATCH p=(a:A {name:'a'})-[:R]->(b) RETURN length(p) AS l, [n IN nodes(p) | n.name] AS names"
    ).to_list()
    assert rows == [{"l": 1, "names": ["a", "b"]}]


def test_leading_optional_match_unmatched_path_is_null(graph):
    rows = graph.cypher("OPTIONAL MATCH p=(a:A {name:'x'})-[:R]->(b) RETURN p IS NULL AS missing").to_list()
    assert rows == [{"missing": True}]


def test_leading_optional_match_where_reads_path(graph):
    rows = graph.cypher("OPTIONAL MATCH p=(a:A)-[:R*1..2]->(b) WHERE length(p) = 2 RETURN b.name AS b").to_list()
    assert rows == [{"b": "c"}]


def test_later_optional_match_fixed_path(graph):
    rows = graph.cypher(
        "MATCH (x:A) OPTIONAL MATCH p=(x)-[:R]->(b) RETURN x.name AS x, p IS NOT NULL AS has, length(p) AS l ORDER BY x"
    ).to_list()
    assert rows == [
        {"x": "a", "has": True, "l": 1},
        {"x": "x", "has": False, "l": None},
        {"x": "x2", "has": False, "l": None},
    ]


def test_later_optional_match_var_length_path(graph):
    rows = graph.cypher(
        "MATCH (x:A) OPTIONAL MATCH p=(x)-[:R*1..2]->(b) "
        "RETURN x.name AS x, length(p) AS l, [r IN relationships(p) | r.w] AS ws, nodes(p) IS NULL AS missing "
        "ORDER BY x, l"
    ).to_list()
    assert [(r["x"], r["l"], r["missing"]) for r in rows] == [
        ("a", 1, False),
        ("a", 2, False),
        ("x", None, True),
        ("x2", None, True),
    ]
    assert [r["ws"] for r in rows[:2]] == [[1], [1, 2]]


def test_later_optional_match_where_reads_path(graph):
    rows = graph.cypher(
        "MATCH (x:A) OPTIONAL MATCH p=(x)-[:R*1..2]->(b) WHERE length(p) = 2 RETURN x.name AS x, b.name AS b ORDER BY x"
    ).to_list()
    assert rows == [{"x": "a", "b": "c"}, {"x": "x", "b": None}, {"x": "x2", "b": None}]


def test_later_optional_match_where_rejecting_every_path_null_extends(graph):
    rows = graph.cypher(
        "MATCH (x:A {name:'a'}) OPTIONAL MATCH p=(x)-[:R*1..2]->(b) WHERE length(p) > 5 "
        "RETURN x.name AS x, b.name AS b, p IS NULL AS missing"
    ).to_list()
    assert rows == [{"x": "a", "b": None, "missing": True}]


def test_later_optional_match_count_path(graph):
    rows = graph.cypher(
        "MATCH (x:A) OPTIONAL MATCH p=(x)-[:R*1..2]->(b) RETURN x.name AS x, count(p) AS n ORDER BY x"
    ).to_list()
    assert rows == [{"x": "a", "n": 2}, {"x": "x", "n": 0}, {"x": "x2", "n": 0}]


def test_later_optional_match_path_carried_through_with(graph):
    rows = graph.cypher(
        "MATCH (x:A) OPTIONAL MATCH p=(x)-[:R]->(b) WITH x, p RETURN x.name AS x, length(p) AS l ORDER BY x"
    ).to_list()
    assert rows == [{"x": "a", "l": 1}, {"x": "x", "l": None}, {"x": "x2", "l": None}]


# --- which binding a path is built from ----------------------------------------
#
# A row holds earlier clauses' paths and variable-length segments too; a path
# must be assembled from its own pattern's pieces, never "the first one".


def test_fixed_path_after_var_length_relationship(graph):
    rows = graph.cypher(
        "MATCH (a:A {name:'a'})-[r:R*1..1]->(b) MATCH p=(b)-[:R]->(c) RETURN [n IN nodes(p) | n.name] AS names"
    ).to_list()
    assert rows == [{"names": ["b", "c"]}]


def test_node_only_path_after_fixed_path(graph):
    rows = graph.cypher(
        "MATCH q=(a:A {name:'a'})-[:R]->(b) MATCH p=(b) RETURN length(p) AS l, [n IN nodes(p) | n.name] AS names"
    ).to_list()
    assert rows == [{"l": 0, "names": ["b"]}]


def test_path_mixing_fixed_and_var_length_hops(graph):
    rows = graph.cypher(
        "MATCH p=(a:A {name:'a'})-[:R]->(b)-[:R*1..1]->(c) "
        "RETURN length(p) AS l, [n IN nodes(p) | n.name] AS names, [r IN relationships(p) | r.w] AS ws"
    ).to_list()
    assert rows == [{"l": 2, "names": ["a", "b", "c"], "ws": [1, 2]}]


def test_path_mixing_var_length_then_fixed_hops(graph):
    rows = graph.cypher(
        "MATCH p=(a:A {name:'a'})-[s:R*1..1]->(b)-[:R]->(c) "
        "WHERE length(p) = 2 RETURN [n IN nodes(p) | n.name] AS names"
    ).to_list()
    assert rows == [{"names": ["a", "b", "c"]}]


def test_later_optional_match_shortest_path(graph):
    rows = graph.cypher(
        "MATCH (x:A) WHERE x.name IN ['a', 'x'] "
        "OPTIONAL MATCH p=shortestPath((x)-[:R*]->(c:B {name:'c'})) "
        "RETURN x.name AS x, length(p) AS l ORDER BY x"
    ).to_list()
    assert rows == [{"x": "a", "l": 2}, {"x": "x", "l": None}]


def test_later_optional_match_shortest_path_where(graph):
    rows = graph.cypher(
        "MATCH (x:A {name:'a'}) OPTIONAL MATCH p=shortestPath((x)-[:R*]->(c:B)) WHERE length(p) = 2 "
        "RETURN x.name AS x, c.name AS c"
    ).to_list()
    assert rows == [{"x": "a", "c": "c"}]
