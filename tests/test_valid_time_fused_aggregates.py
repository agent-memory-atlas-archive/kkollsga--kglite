"""Fused aggregates under ``FOR VALID_TIME AS OF``.

Each lifted fusion must answer exactly as the same statement with that
pass disabled (the guarded matcher route). The fixture is built to catch a
fused operator that skips a mask: a relationship valid at the instant whose
endpoint is not, a valid node with an invalid incident relationship, a
declared secondary label, a per-source-type relationship declaration,
parallel history relationships, and both conventions on the boundary day.
"""

from __future__ import annotations

import pytest

import kglite

PASS = "fuse_match_return_aggregate"

INSTANTS = [
    "2000-01-01",
    "2004-12-31",
    "2005-01-01",
    "2005-06-15",
    "2006-01-01",
    "2008-01-01",
    "2008-06-15",
    "2010-01-01",
    "2010-01-02",
    "2012-01-01",
    "2015-06-15",
    "2031-01-01",
]


def at(date: str, body: str) -> str:
    return f"FOR VALID_TIME AS OF date('{date}') {body}"


@pytest.fixture(scope="module")
def org():
    graph = kglite.KnowledgeGraph()
    graph.cypher(
        "CREATE (d1:Dept {id: 10, title: 'Ops', vf: date('2000-01-01'), vt: date('2010-01-01')}),"
        " (d2:Dept {id: 11, title: 'Ops'}),"
        " (d3:Dept {id: 12, title: 'Lab', vf: date('2012-01-01')}),"
        " (d4:Dept {id: 13, title: 'Lab', vf: date('2000-01-01'), vt: date('2005-01-01')}),"
        " (e1:Emp {id: 1, team: 'a', vf: date('2000-01-01')}),"
        " (e2:Emp {id: 2, team: 'a', vf: date('2000-01-01')}),"
        " (e3:Emp {id: 3, team: 'b', vf: date('2000-01-01')}),"
        " (e4:Emp {id: 4, team: 'b', vf: date('2000-01-01'), vt: date('2008-01-01')}),"
        " (e5:Emp {id: 5, team: 'a', vf: date('2000-01-01')}),"
        " (e6:Emp {id: 6, team: 'b', vf: date('2000-01-01'), p_from: date('2012-01-01'), p_to: date('2030-01-01')}),"
        " (e7:Emp {id: 7, team: 'a', vf: date('2000-01-01'), vt: date('2010-01-01')}),"
        " (e8:Emp {id: 8, team: 'b', vf: date('2000-01-01')}),"
        " (e1)-[:IN_DEPT {since: date('2000-01-01')}]->(d1),"
        " (e2)-[:IN_DEPT {since: date('2000-01-01'), until: date('2005-01-01')}]->(d1),"
        " (e2)-[:IN_DEPT {since: date('2005-01-01')}]->(d1),"
        " (e3)-[:IN_DEPT {since: date('2000-01-01')}]->(d1),"
        " (e3)-[:IN_DEPT {since: date('2000-01-01')}]->(d3),"
        " (e4)-[:IN_DEPT {since: date('2000-01-01')}]->(d2),"
        " (e5)-[:IN_DEPT {since: date('2000-01-01'), until: date('2006-01-01')}]->(d2),"
        " (e6)-[:IN_DEPT {since: date('2000-01-01')}]->(d2),"
        " (e7)-[:IN_DEPT {since: date('2000-01-01')}]->(d2),"
        " (e8)-[:IN_DEPT {since: date('2000-01-01')}]->(d4),"
        " (e1)-[:LINKED {f_from: date('2000-01-01'), f_to: date('2005-01-01')}]->(d1),"
        " (e2)-[:LINKED {f_from: date('2003-01-01')}]->(d1),"
        " (e3)-[:LINKED {f_from: date('2000-01-01'), f_to: date('2008-01-01'),"
        " from: date('2020-01-01')}]->(d2),"
        " (d1)-[:LINKED {from: date('2001-01-01'), to: date('2006-01-01'),"
        " f_from: date('2030-01-01')}]->(e1),"
        " (d2)-[:LINKED {from: date('2000-01-01'), to: date('2009-01-01')}]->(e2),"
        " (d2)-[:LINKED {from: date('2007-01-01')}]->(e5)"
    ).to_list()
    graph.cypher("MATCH (e:Emp {id: 6}) SET e:Lead").to_list()
    for declaration in (
        "{node: 'Emp', from: 'vf', to: 'vt', convention: 'closed'}",
        "{node: 'Lead', from: 'p_from', to: 'p_to', convention: 'closed'}",
        "{node: 'Dept', from: 'vf', to: 'vt', convention: 'half_open'}",
        "{relationship: 'IN_DEPT', from: 'since', to: 'until', convention: 'half_open'}",
        "{relationship: 'LINKED', source_type: 'Emp', from: 'f_from', to: 'f_to', convention: 'closed'}",
        "{relationship: 'LINKED', from: 'from', to: 'to', convention: 'half_open'}",
    ):
        graph.cypher(f"CALL db.temporal.declare({declaration})").to_list()
    return graph


def _norm(rows):
    return sorted(repr(sorted(row.items())) for row in rows)


# Each shape fuses when unguarded; the heads are the aggregate shapes the
# operator serves: property and node grouping, top-k, DISTINCT, lone count,
# untyped endpoints, undirected edges, property-constrained groups, the
# source-keyed declaration and the declared secondary label.
# A secondary-label endpoint keeps the pass off (multi-label gate), undated
# and guarded alike; the differential still holds it to the matcher's answer.
LEAD_SHAPE = "MATCH (l:Lead)-[:IN_DEPT]->(d:Dept) RETURN d.title AS t, count(l) AS n"

SHAPES = [
    "MATCH (e:Emp)-[:IN_DEPT]->(d:Dept) RETURN d.title AS t, count(e) AS n",
    "MATCH (e:Emp)-[:IN_DEPT]->(d:Dept) RETURN d.title AS t, count(e) AS n ORDER BY n DESC, t LIMIT 1",
    "MATCH (e:Emp)-[:IN_DEPT]->(d:Dept) RETURN d.title AS t, count(e) AS n ORDER BY n, t LIMIT 2",
    "MATCH (e:Emp)-[:IN_DEPT]->(d:Dept) RETURN d AS d, count(e) AS n",
    "MATCH (e:Emp)-[:IN_DEPT]->(d:Dept) RETURN d.id AS d, count(e) AS n ORDER BY n DESC, d LIMIT 3",
    "MATCH (d:Dept)<-[:IN_DEPT]-(e:Emp) RETURN d AS d, count(DISTINCT e) AS n",
    "MATCH (e:Emp)-[:IN_DEPT]->(d:Dept) RETURN e AS e, count(DISTINCT d) AS n",
    "MATCH (e:Emp)-[r:IN_DEPT]->(d:Dept) RETURN e.id AS e, count(r) AS n",
    "MATCH (e:Emp)-[:IN_DEPT]->(d) RETURN e.id AS e, count(d) AS n",
    "MATCH (e:Emp)-[:IN_DEPT]->(d:Dept {title: 'Ops'}) RETURN count(*) AS n",
    "MATCH (x)-[:IN_DEPT]->(d:Dept) RETURN d.title AS t, count(x) AS n",
    "MATCH (e:Emp)-[:IN_DEPT]-(d:Dept) RETURN e.id AS e, count(d) AS n",
    "MATCH (e:Emp {team: 'a'})-[:IN_DEPT]->(d:Dept) RETURN d.title AS t, count(e) AS n",
    "MATCH (e:Emp)-[:IN_DEPT]->(d:Dept {title: 'Ops'}) RETURN e.id AS e, count(d) AS n",
    LEAD_SHAPE,
    "MATCH (d:Dept)-[:LINKED]->(e:Emp) RETURN d.title AS t, count(e) AS n",
    "MATCH (e:Emp)-[:LINKED]->(d:Dept) RETURN d.title AS t, count(e) AS n",
    "MATCH (e:Emp)-[:LINKED]->(d:Dept) RETURN e.id AS e, count(d) AS n ORDER BY n DESC, e LIMIT 2",
]


def _tags(graph, query):
    return [row["operation"] for row in graph.cypher(f"EXPLAIN {query}")]


@pytest.mark.parametrize("shape", SHAPES)
def test_fused_answers_as_the_guarded_matcher(org, shape):
    for date in INSTANTS:
        query = at(date, shape)
        fused = org.cypher(query).to_list()
        plain = org.cypher(query, disabled_passes=[PASS]).to_list()
        assert _norm(fused) == _norm(plain), (date, shape)


@pytest.mark.parametrize("shape", [s for s in SHAPES if s != LEAD_SHAPE])
def test_the_aggregate_fuses_under_a_context(org, shape):
    ops = _tags(org, at("2006-01-01", shape))
    assert f"OptimizerPass {PASS}" in ops, (shape, ops)


def test_a_five_element_pattern_stays_unfused_under_a_context(org):
    query = at(
        "2006-01-01",
        "MATCH (a:Emp)-[:IN_DEPT]->(d:Dept)<-[:IN_DEPT]-(b:Emp) RETURN a AS a, count(b) AS n",
    )
    assert f"OptimizerPass {PASS}" not in _tags(org, query)
    undated = "MATCH (a:Emp)-[:IN_DEPT]->(d:Dept)<-[:IN_DEPT]-(b:Emp) RETURN a AS a, count(b) AS n"
    assert f"OptimizerPass {PASS}" in _tags(org, undated)


def test_the_five_element_shape_is_the_matcher_answer_under_a_context(org):
    shape = "MATCH (a:Emp)-[:IN_DEPT]->(d:Dept)<-[:IN_DEPT]-(b:Emp) RETURN a.id AS a, count(b) AS n"
    got = {r["a"]: r["n"] for r in org.cypher(at("2006-01-01", shape)).to_list()}
    # d1 holds e1, e2, e3 and d2 holds e4, e7; a relationship is not reused
    # within one path, so each employee pairs with the others only.
    assert got == {1: 2, 2: 2, 3: 2, 4: 1, 7: 1}


def counts(graph, date, shape, key="t"):
    return {row[key]: row["n"] for row in graph.cypher(at(date, shape)).to_list()}


PER_DEPT = "MATCH (e:Emp)-[:IN_DEPT]->(d:Dept) RETURN d.id AS d, count(e) AS n"


def test_goldens_per_department(org):
    # 2006-01-01: d1(valid) edges from e1, e2(B), e3 -> 3; d3 not yet valid so
    # e3's edge to it is hidden; d2: e4 (valid to 2008), e5's edge ended on
    # the day (half_open), e6 hidden (Lead from 2012), e7 -> 2; d4 closed 2005.
    assert counts(org, "2006-01-01", PER_DEPT, "d") == {10: 3, 11: 2}
    # 2005-01-01 (boundary): edge A ended (half-open), B began -> e2 once;
    # d4 ends that day half-open -> hidden.
    assert counts(org, "2005-01-01", PER_DEPT, "d") == {10: 3, 11: 3}
    # 2004-12-31: A valid, B not -> still one; d4 valid -> e8.
    assert counts(org, "2004-12-31", PER_DEPT, "d") == {10: 3, 11: 3, 13: 1}
    # 2010-01-01: d1 half-open ended -> hidden although its edges are valid;
    # e7 closed boundary day valid; e4 gone; e5 gone; e6 hidden.
    assert counts(org, "2010-01-01", PER_DEPT, "d") == {11: 1}
    # 2015-06-15: e6 is a Lead valid since 2012 -> visible; d3 valid, e3 edge.
    assert counts(org, "2015-06-15", PER_DEPT, "d") == {11: 1, 12: 1}


def test_goldens_secondary_label_and_keyed_declarations(org):
    lead = "MATCH (l:Lead)-[:IN_DEPT]->(d:Dept) RETURN d.title AS t, count(l) AS n"
    assert counts(org, "2006-01-01", lead) == {}
    assert counts(org, "2015-06-15", lead) == {"Ops": 1}
    # Emp->Dept LINKED is judged by f_from/f_to (closed); Dept->Emp by from/to.
    out = "MATCH (e:Emp)-[:LINKED]->(d:Dept) RETURN e.id AS e, count(d) AS n"
    assert counts(org, "2005-01-01", out, "e") == {1: 1, 2: 1, 3: 1}
    assert counts(org, "2005-01-02", out, "e") == {2: 1, 3: 1}
    back = "MATCH (d:Dept)-[:LINKED]->(e:Emp) RETURN d.id AS d, count(e) AS n"
    assert counts(org, "2005-01-01", back, "d") == {10: 1, 11: 1}
    assert counts(org, "2006-01-01", back, "d") == {11: 1}


def test_the_global_count_is_masked(org):
    total = "MATCH (e:Emp)-[:IN_DEPT]->(d:Dept) RETURN count(*) AS n"
    assert org.cypher(at("2006-01-01", total)).to_list() == [{"n": 5}]
    assert org.cypher(at("2010-01-01", total)).to_list() == [{"n": 1}]


# ---------------------------------------------------------------------------
# The WITH forms: fuse_match_with_aggregate and its top-k absorption.
# ---------------------------------------------------------------------------

WITH_PASS = "fuse_match_with_aggregate"
WITH_TOP_K_PASS = "fuse_match_with_aggregate_top_k"

WITH_SHAPES = [
    "MATCH (e:Emp)-[:IN_DEPT]->(d:Dept) WITH d, count(e) AS n RETURN d.id AS d, n",
    "MATCH (e:Emp)-[:IN_DEPT]->(d:Dept) WITH d, count(e) AS n RETURN d.title AS t, n",
    "MATCH (e:Emp)-[:IN_DEPT]->(d:Dept) WITH d, count(e) AS n WHERE n > 1 RETURN d.id AS d, n",
    "MATCH (e:Emp)-[:IN_DEPT]->(d:Dept) WITH d, count(*) AS n RETURN d.id AS d, n",
    "MATCH (d:Dept)<-[:IN_DEPT]-(e:Emp) WITH d, count(DISTINCT e) AS n RETURN d.id AS d, n",
    "MATCH (e:Emp)-[:IN_DEPT]->(d:Dept) WITH e, count(DISTINCT d) AS n RETURN e.id AS e, n",
    "MATCH (e:Emp)-[r:IN_DEPT]->(d:Dept) WITH e, count(r) AS n RETURN e.id AS e, n",
    "MATCH (e:Emp)-[:IN_DEPT]->(d) WITH e, count(d) AS n RETURN e.id AS e, n",
    "MATCH (x)-[:IN_DEPT]->(d:Dept) WITH d, count(x) AS n RETURN d.id AS d, n",
    "MATCH (e:Emp)-[:IN_DEPT]-(d:Dept) WITH e, count(d) AS n RETURN e.id AS e, n",
    "MATCH (e:Emp {team: 'a'})-[:IN_DEPT]->(d:Dept) WITH d, count(e) AS n RETURN d.id AS d, n",
    "MATCH (e:Emp)-[:IN_DEPT]->(d:Dept {title: 'Ops'}) WITH e, count(d) AS n RETURN e.id AS e, n",
    "MATCH (d:Dept)-[:LINKED]->(e:Emp) WITH d, count(e) AS n RETURN d.id AS d, n",
    "MATCH (e:Emp)-[:LINKED]->(d:Dept) WITH d, count(e) AS n RETURN d.id AS d, n",
    "MATCH (e:Emp)-[:LINKED]->(d:Dept) WITH e, count(d) AS n RETURN e.id AS e, n",
    "MATCH (e:Emp)-[:IN_DEPT]->(d:Dept) MATCH (d)-[r:LINKED]->(x:Emp) WITH d, count(r) AS n RETURN d.id AS d, n",
    "MATCH (e:Emp)-[:IN_DEPT]->(d:Dept) MATCH (e)-[r:LINKED]->(x) WITH e, count(r) AS n RETURN e.id AS e, n",
]

# The absorption takes one ORDER BY key, the count alias. Ties at the limit
# may break either way, so the cut shapes are compared on their counts; the
# uncut twin (LIMIT 100) is compared on full rows.
WITH_TOP_K_SHAPES = [
    "MATCH (e:Emp)-[:IN_DEPT]->(d:Dept) WITH d, count(e) AS n RETURN d.id AS d, n ORDER BY n DESC LIMIT 3",
    "MATCH (e:Emp)-[:IN_DEPT]->(d:Dept) WITH d, count(e) AS n RETURN d.id AS d, n ORDER BY n LIMIT 2",
    "MATCH (e:Emp)-[:IN_DEPT]->(d:Dept) WITH e, count(DISTINCT d) AS n RETURN e.id AS e, n ORDER BY n DESC LIMIT 2",
    "MATCH (e:Emp)-[:LINKED]->(d:Dept) WITH e, count(d) AS n RETURN e.id AS e, n ORDER BY n DESC LIMIT 2",
    "MATCH (e:Emp)-[:IN_DEPT]->(d:Dept) WITH d, count(e) AS n RETURN d.id AS d, n ORDER BY n DESC LIMIT 100",
]

# A property group key is outside the pass; the matcher answers it, and the
# differential still holds it to that answer.
WITH_UNFUSED = "MATCH (e:Emp)-[:IN_DEPT]->(d:Dept) WITH d.title AS t, count(e) AS n RETURN t, n"

WITH_ALL = WITH_SHAPES + WITH_TOP_K_SHAPES


@pytest.mark.parametrize("shape", [*WITH_SHAPES, WITH_UNFUSED, WITH_TOP_K_SHAPES[-1]])
def test_with_form_answers_as_the_guarded_matcher(org, shape):
    for date in INSTANTS:
        query = at(date, shape)
        fused = org.cypher(query).to_list()
        plain = org.cypher(query, disabled_passes=[WITH_PASS, WITH_TOP_K_PASS]).to_list()
        assert _norm(fused) == _norm(plain), (date, shape)


@pytest.mark.parametrize("shape", WITH_TOP_K_SHAPES)
def test_with_top_k_keeps_the_matcher_counts(org, shape):
    for date in INSTANTS:
        query = at(date, shape)
        fused = [row["n"] for row in org.cypher(query).to_list()]
        plain = [row["n"] for row in org.cypher(query, disabled_passes=[WITH_PASS, WITH_TOP_K_PASS]).to_list()]
        assert fused == plain, (date, shape)


@pytest.mark.parametrize("shape", WITH_ALL)
def test_with_form_fuses_under_a_context(org, shape):
    ops = _tags(org, at("2006-01-01", shape))
    assert f"OptimizerPass {WITH_PASS}" in ops, (shape, ops)
    # The fused clause is what ran: nothing falls back to the plain matcher.
    assert any(op.startswith("FusedMatchWithAggregate") for op in ops), (shape, ops)


@pytest.mark.parametrize("shape", WITH_TOP_K_SHAPES)
def test_with_top_k_is_absorbed_under_a_context(org, shape):
    assert f"OptimizerPass {WITH_TOP_K_PASS}" in _tags(org, at("2006-01-01", shape)), shape


def test_with_form_goldens_per_department(org):
    shape = "MATCH (e:Emp)-[:IN_DEPT]->(d:Dept) WITH d, count(e) AS n RETURN d.id AS d, n"
    # The hand counts of test_goldens_per_department, through the WITH form.
    assert counts(org, "2006-01-01", shape, "d") == {10: 3, 11: 2}
    assert counts(org, "2005-01-01", shape, "d") == {10: 3, 11: 3}
    assert counts(org, "2004-12-31", shape, "d") == {10: 3, 11: 3, 13: 1}
    assert counts(org, "2010-01-01", shape, "d") == {11: 1}
    assert counts(org, "2015-06-15", shape, "d") == {11: 1, 12: 1}


def test_with_form_goldens_top_k_and_two_match(org):
    top = "MATCH (e:Emp)-[:IN_DEPT]->(d:Dept) WITH d, count(e) AS n RETURN d.id AS d, n ORDER BY n DESC LIMIT 1"
    # 2004-12-31: d1 and d2 tie on 3; 2010-01-01 leaves one department.
    assert [r["n"] for r in org.cypher(at("2004-12-31", top)).to_list()] == [3]
    assert org.cypher(at("2010-01-01", top)).to_list() == [{"d": 11, "n": 1}]
    # Two-MATCH shape: each M1 row times the valid Dept->Emp LINKED edges of
    # the department. 2006-01-01: d1's link ended, d2 has 2 rows x 1 link.
    two = "MATCH (e:Emp)-[:IN_DEPT]->(d:Dept) MATCH (d)-[r:LINKED]->(x:Emp) WITH d, count(r) AS n RETURN d.id AS d, n"
    assert counts(org, "2006-01-01", two, "d") == {11: 2 * 1}
    assert counts(org, "2005-01-01", two, "d") == {10: 3, 11: 3}
    # 2010-01-01: d1 has ended; d2 keeps e7 (one M1 row) and its link to e5.
    assert counts(org, "2010-01-01", two, "d") == {11: 1}
    # 2000-06-01: d1's link to e1 has not begun; d2 has 3 rows x 1 link.
    assert counts(org, "2000-06-01", two, "d") == {11: 3}


def test_with_form_secondary_label_goldens(org):
    lead = "MATCH (l:Lead)-[:IN_DEPT]->(d:Dept) WITH d, count(l) AS n RETURN d.title AS t, n"
    assert counts(org, "2006-01-01", lead) == {}
    assert counts(org, "2015-06-15", lead) == {"Ops": 1}
