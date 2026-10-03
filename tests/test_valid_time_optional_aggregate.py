"""``OPTIONAL MATCH ... count()`` fused under ``FOR VALID_TIME AS OF``.

The fused operator counts each driving row's matches through the guarded
per-node counter, which tests the bound node, every relationship and the peer.
Each shape must answer as the same statement with the pass disabled (the
guarded matcher), at twelve instants and under ALL. The fixture is the fused
aggregate suite's: a relationship valid at the instant whose peer is not, a
valid peer behind an ended relationship, a declared secondary label,
per-source-type relationship declarations, and rows with no match at all.
"""

from __future__ import annotations

import pytest

from tests.test_valid_time_fused_aggregates import INSTANTS, at, org  # noqa: F401

PASS = "fuse_optional_match_aggregate"
CONTEXTS = [at(d, "") for d in INSTANTS] + ["FOR VALID_TIME ALL "]

# Shapes the pass fuses: RETURN and WITH forms, both directions, a typed and an
# untyped peer, a declared peer type, a peer property, a relationship variable,
# count(*), derived counts, the per-source-type declaration and the comma
# driving MATCH that pre-binds both ends.
FUSED = [
    "MATCH (d:Dept) OPTIONAL MATCH (d)<-[:IN_DEPT]-(e:Emp) RETURN d.id AS d, count(e) AS n",
    "MATCH (d:Dept) OPTIONAL MATCH (d)<-[:IN_DEPT]-(e) RETURN d.id AS d, count(e) AS n",
    "MATCH (d:Dept) OPTIONAL MATCH (d)<-[:IN_DEPT]-(e:Emp) RETURN d.id AS d, count(*) AS n",
    "MATCH (d:Dept) OPTIONAL MATCH (d)<-[:IN_DEPT]-(e:Emp) WITH d, count(e) AS n RETURN d.id AS d, n",
    "MATCH (d:Dept) OPTIONAL MATCH (d)<-[:IN_DEPT]-(e:Emp) RETURN d.title AS t, count(e) AS n",
    "MATCH (d:Dept) OPTIONAL MATCH (d)<-[:IN_DEPT]-(e:Emp) RETURN d.id AS d, count(e) + 1 AS n",
    "MATCH (e:Emp) OPTIONAL MATCH (e)-[:IN_DEPT]->(d:Dept) RETURN e.id AS e, count(d) AS n",
    "MATCH (e:Emp) OPTIONAL MATCH (e)-[:IN_DEPT]->(d) RETURN e.id AS e, count(d) AS n",
    "MATCH (e:Emp) OPTIONAL MATCH (e)-[r:IN_DEPT]->(d:Dept) RETURN e.id AS e, count(r) AS n",
    "MATCH (e:Emp) OPTIONAL MATCH (e)-[:IN_DEPT]-(d:Dept) RETURN e.id AS e, count(d) AS n",
    "MATCH (e:Emp) OPTIONAL MATCH (e)-[:IN_DEPT]->(d:Dept {title: 'Ops'}) RETURN e.id AS e, count(d) AS n",
    "MATCH (e:Emp {team: 'a'}) OPTIONAL MATCH (e)-[:IN_DEPT]->(d:Dept) RETURN e.id AS e, count(d) AS n",
    "MATCH (e:Emp) OPTIONAL MATCH (e)-[:LINKED]->(d:Dept) RETURN e.id AS e, count(d) AS n",
    "MATCH (d:Dept) OPTIONAL MATCH (d)-[:LINKED]->(e:Emp) RETURN d.id AS d, count(e) AS n",
    "MATCH (d:Dept) OPTIONAL MATCH (d)-[:LINKED]->(e) RETURN d.id AS d, count(e) AS n",
    "MATCH (e:Emp) OPTIONAL MATCH (e)-[:LINKED]->(d:Dept) WITH e, count(d) AS n RETURN e.id AS e, n",
    "MATCH (e:Emp), (d:Dept) OPTIONAL MATCH (e)-[:IN_DEPT]->(d) RETURN e.id AS e, d.id AS d, count(*) AS n",
    # More than one OPTIONAL MATCH: the last one is the fused clause, counting
    # over the rows the first one expanded.
    "MATCH (e:Emp) OPTIONAL MATCH (e)-[:IN_DEPT]->(d:Dept) OPTIONAL MATCH (e)-[:LINKED]->(x:Dept) "
    "RETURN e.id AS e, count(x) AS n",
    "MATCH (d:Dept) OPTIONAL MATCH (d)<-[:IN_DEPT]-(e:Emp) OPTIONAL MATCH (d)-[:LINKED]->(x:Emp) "
    "RETURN d.id AS d, count(x) AS n",
]

# Outside the pass or off its counter, and answered by the matcher either way:
# a declared secondary label on the peer, a variable-length edge, a clause-owned
# WHERE, two patterns in one OPTIONAL MATCH, a count of the first of two
# OPTIONAL variables.
OTHER = [
    "MATCH (d:Dept) OPTIONAL MATCH (d)<-[:IN_DEPT]-(l:Lead) RETURN d.id AS d, count(l) AS n",
    "MATCH (e:Emp) OPTIONAL MATCH (e)-[:IN_DEPT*1..2]->(d:Dept) RETURN e.id AS e, count(d) AS n",
    "MATCH (e:Emp) OPTIONAL MATCH (e)-[:IN_DEPT]->(d:Dept) WHERE d.title = 'Ops' RETURN e.id AS e, count(d) AS n",
    "MATCH (e:Emp) OPTIONAL MATCH (e)-[:IN_DEPT]->(d:Dept), (e)-[:LINKED]->(x:Dept) RETURN e.id AS e, count(d) AS n",
    "MATCH (e:Emp) OPTIONAL MATCH (e)-[:IN_DEPT]->(d:Dept) OPTIONAL MATCH (e)-[:LINKED]->(x:Dept) "
    "RETURN e.id AS e, count(d) AS n",
]


def _norm(rows):
    return sorted(repr(sorted(row.items())) for row in rows)


def _tags(graph, query):
    return [row["operation"] for row in graph.cypher(f"EXPLAIN {query}")]


@pytest.mark.parametrize("shape", FUSED + OTHER)
def test_fused_answers_as_the_guarded_matcher(org, shape):  # noqa: F811
    for context in CONTEXTS:
        fused = org.cypher(context + shape).to_list()
        plain = org.cypher(context + shape, disabled_passes=[PASS]).to_list()
        assert _norm(fused) == _norm(plain), (context, shape)


@pytest.mark.parametrize("shape", FUSED)
def test_the_count_fuses_under_a_context(org, shape):  # noqa: F811
    ops = _tags(org, at("2006-01-01", shape))
    assert f"OptimizerPass {PASS}" in ops, (shape, ops)
    assert any(op.startswith("FusedOptionalMatchAggregate") for op in ops), (shape, ops)
    plain = [row["operation"] for row in org.cypher(f"EXPLAIN {at('2006-01-01', shape)}", disabled_passes=[PASS])]
    assert not any(op.startswith("FusedOptionalMatchAggregate") for op in plain)


def counts(graph, date, shape, key="d"):
    return {row[key]: row["n"] for row in graph.cypher(at(date, shape)).to_list()}


PER_DEPT = "MATCH (d:Dept) OPTIONAL MATCH (d)<-[:IN_DEPT]-(e:Emp) RETURN d.id AS d, count(e) AS n"
PER_EMP = "MATCH (e:Emp) OPTIONAL MATCH (e)-[:IN_DEPT]->(d:Dept) RETURN e.id AS e, count(d) AS n"


def test_goldens_zero_match_rows_yield_zero(org):  # noqa: F811
    # 2006-01-01: Dept 10 holds e1, e2 (second edge), e3; Dept 11 holds e4 and
    # e7; Dept 12 is not valid yet and Dept 13 closed, so neither drives a row.
    assert counts(org, "2006-01-01", PER_DEPT) == {10: 3, 11: 2}
    # 2010-01-01: Dept 10 ended (half-open); Dept 11 keeps e7 on its closed day.
    assert counts(org, "2010-01-01", PER_DEPT) == {11: 1}
    # 2012-01-01: Dept 12 begins and e3's edge to it is visible; Dept 11 holds e6.
    assert counts(org, "2012-01-01", PER_DEPT) == {11: 1, 12: 1}
    # An employee with no visible department still drives a row, with 0: e5's
    # edge ended on 2006-01-01 (half-open) and e8's department closed in 2005.
    per_emp = counts(org, "2006-01-01", PER_EMP, "e")
    assert per_emp == {1: 1, 2: 1, 3: 1, 4: 1, 5: 0, 7: 1, 8: 0}
    # 2010-01-01: Dept 10 has ended, so only e7 (closed on its last day) counts.
    assert counts(org, "2010-01-01", PER_EMP, "e") == {1: 0, 2: 0, 3: 0, 5: 0, 7: 1, 8: 0}
    assert counts(org, "2000-06-01", PER_EMP, "e")[8] == 1


def test_a_hidden_peer_is_not_counted(org):  # noqa: F811
    # e3's relationship to Dept 12 is valid from 2000, but the department only
    # begins in 2012: it is the peer, not the relationship, that hides it.
    assert counts(org, "2011-06-15", PER_EMP, "e")[3] == 0
    assert counts(org, "2012-01-01", PER_EMP, "e")[3] == 1
    # Employee 6 is a Lead only from 2012; before that it is not a driving row.
    assert 6 not in counts(org, "2006-01-01", PER_EMP, "e")
    assert counts(org, "2015-06-15", PER_EMP, "e")[6] == 1


def test_star_counts_the_padding_row(org):  # noqa: F811
    star = "MATCH (e:Emp) OPTIONAL MATCH (e)-[:LINKED]->(d:Dept) RETURN e.id AS e, count(*) AS n"
    per_var = "MATCH (e:Emp) OPTIONAL MATCH (e)-[:LINKED]->(d:Dept) RETURN e.id AS e, count(d) AS n"
    for date in INSTANTS:
        assert all(v >= 1 for v in counts(org, date, star, "e").values())
        assert all(v >= 0 for v in counts(org, date, per_var, "e").values())
    # 2005-01-01: e1 (closed) and e2, e3 link to a Dept; the rest pad.
    assert counts(org, "2005-01-01", per_var, "e")[1] == 1
    assert counts(org, "2005-01-02", per_var, "e")[1] == 0
    assert counts(org, "2005-01-02", star, "e")[1] == 1


def test_all_counts_every_version(org):  # noqa: F811
    rows = org.cypher("FOR VALID_TIME ALL " + PER_DEPT).to_list()
    assert {r["d"]: r["n"] for r in rows} == {10: 4, 11: 4, 12: 1, 13: 1}
