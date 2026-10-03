"""``LIMIT`` pushed into ``MATCH`` under ``FOR VALID_TIME AS OF``.

The pass stamps a row cap on the MATCH, so it is only correct when every cap
site counts rows the guard has already admitted. The fixture is built to make
a cap site count a hidden row: the first nodes in index order are hidden
versions, the first relationships a node holds are ended, the relationship
type's inverted index lists more hidden sources than the seed cap reads, and
the comma-pattern cartesian starts on hidden nodes. Every shape must answer as
the same statement with the pass disabled, at twelve instants and under ALL.
"""

from __future__ import annotations

import pytest

import kglite

PASS = "push_limit_into_match"

INSTANTS = [
    "2000-01-01",
    "2000-12-31",
    "2001-01-01",
    "2001-06-15",
    "2005-01-01",
    "2005-01-02",
    "2009-12-31",
    "2010-01-01",
    "2011-01-01",
    "2011-06-15",
    "2012-01-01",
    "2031-01-01",
]

CONTEXTS = [f"FOR VALID_TIME AS OF date('{d}') " for d in INSTANTS] + ["FOR VALID_TIME ALL "]


# The relationship-type inverted index that seeds an untyped first node (and
# its capped read) exists on the mapped and disk backends only.
@pytest.fixture(scope="module", params=["default", "mapped"])
def org(request):
    graph = kglite.KnowledgeGraph(storage=request.param)
    # Emp 1-6 are valid 2000..2005 and created first, so at any later instant
    # the first candidates in index order are hidden; Emp 7-12 begin in 2010.
    graph.cypher(
        "UNWIND range(1, 12) AS i CREATE (:Emp {id: i, team: CASE WHEN i % 2 = 0 THEN 'a' ELSE 'b' END,"
        " vf: CASE WHEN i <= 6 THEN date('2000-01-01') ELSE date('2010-01-01') END,"
        " vt: CASE WHEN i <= 6 THEN date('2005-01-01') ELSE null END})"
    ).to_list()
    graph.cypher(
        "CREATE (:Dept {id: 100, vf: date('2000-01-01')}), (:Dept {id: 101, vf: date('2000-01-01')}),"
        " (:Dept {id: 102, vf: date('2000-01-01'), vt: date('2003-01-01')})"
    ).to_list()
    # Each Emp's first relationship (the one listed first) is the ended one.
    graph.cypher(
        "MATCH (e:Emp), (a:Dept {id: 100}), (b:Dept {id: 101}), (c:Dept {id: 102}) "
        "WHERE e.id >= 7 "
        "CREATE (e)-[:IN_DEPT {since: date('2010-01-01'), until: date('2011-01-01')}]->(c),"
        " (e)-[:IN_DEPT {since: date('2010-01-01'), until: date('2011-01-01')}]->(a),"
        " (e)-[:IN_DEPT {since: date('2011-01-01')}]->(b)"
    ).to_list()
    graph.cypher(
        "MATCH (e:Emp), (a:Dept {id: 100}) WHERE e.id <= 6 "
        "CREATE (e)-[:IN_DEPT {since: date('2000-01-01'), until: date('2005-01-01')}]->(a)"
    ).to_list()
    # More hidden sources than the seed cap (1000) reads from the inverted
    # index, ahead of the visible ones.
    graph.cypher(
        "UNWIND range(1, 1100) AS i CREATE (:Intern {id: 1000 + i, vf: date('2000-01-01'), vt: date('2001-01-01')})"
    ).to_list()
    graph.cypher("UNWIND range(1, 6) AS i CREATE (:Staff {id: 2000 + i, vf: date('2002-01-01')})").to_list()
    graph.cypher("MATCH (n:Intern), (d:Dept {id: 100}) CREATE (n)-[:MENTORS]->(d)").to_list()
    graph.cypher("MATCH (n:Staff), (d:Dept {id: 100}) CREATE (n)-[:MENTORS]->(d)").to_list()
    for declaration in (
        "{node: 'Emp', from: 'vf', to: 'vt', convention: 'closed'}",
        "{node: 'Intern', from: 'vf', to: 'vt', convention: 'closed'}",
        "{node: 'Staff', from: 'vf', to: 'vt', convention: 'closed'}",
        "{node: 'Dept', from: 'vf', to: 'vt', convention: 'half_open'}",
        "{relationship: 'IN_DEPT', from: 'since', to: 'until', convention: 'half_open'}",
    ):
        graph.cypher(f"CALL db.temporal.declare({declaration})").to_list()
    return graph


# (shape, limit): every shape is one MATCH with a trailing literal LIMIT, which
# the pass rewrites when unguarded. Each probes a different cap site.
SHAPES = [
    # Node candidates truncated to the cap: the first ones are hidden.
    "MATCH (e:Emp) RETURN e.id AS id LIMIT 3",
    "MATCH (e:Emp) RETURN e.id AS id LIMIT 1",
    # WHERE before LIMIT: the cap must not stop on candidates the predicate drops.
    "MATCH (e:Emp) WHERE e.team = 'a' RETURN e.id AS id LIMIT 2",
    "MATCH (e:Emp) WHERE e.id % 3 = 0 RETURN e.id AS id LIMIT 2",
    # Fixed hops: the first relationships a node holds are ended.
    "MATCH (e:Emp)-[:IN_DEPT]->(d:Dept) RETURN e.id AS e, d.id AS d LIMIT 2",
    "MATCH (e:Emp)-[:IN_DEPT]->(d) RETURN e.id AS e, d.id AS d LIMIT 3",
    "MATCH (d:Dept)<-[:IN_DEPT]-(e:Emp) RETURN e.id AS e, d.id AS d LIMIT 3",
    "MATCH (e:Emp)-[r:IN_DEPT]->(d:Dept) WHERE d.id <> 100 RETURN e.id AS e, d.id AS d LIMIT 2",
    # Untyped first node: the relationship type's inverted index seeds it.
    "MATCH (a)-[:IN_DEPT]->(b) RETURN a.id AS a, b.id AS b LIMIT 3",
    # More hidden sources than the seed cap reads: the capped pass comes back
    # short and re-runs uncapped.
    "MATCH (a)-[:MENTORS]->(b) RETURN a.id AS a LIMIT 3",
    "MATCH (a)-[:MENTORS]->(b) WHERE a.id > 2003 RETURN a.id AS a LIMIT 2",
    # Variable-length segment, comma-pattern cartesian.
    "MATCH (e:Emp)-[:IN_DEPT*1..2]->(d:Dept) RETURN e.id AS e, d.id AS d LIMIT 2",
    "MATCH (e:Emp), (d:Dept) RETURN e.id AS e, d.id AS d LIMIT 4",
]


def _rows(result):
    return [tuple(sorted(row.items())) for row in result.to_list()]


def _unlimited(shape):
    head, _ = shape.rsplit(" LIMIT ", 1)
    return head


@pytest.mark.parametrize("shape", SHAPES)
def test_limit_answers_as_the_guarded_matcher(org, shape):
    for context in CONTEXTS:
        fused = _rows(org.cypher(context + shape))
        plain = _rows(org.cypher(context + shape, disabled_passes=[PASS]))
        assert fused == plain, (context, shape)
        # Independent of the pass: LIMIT n returns min(n, all) rows, each one
        # an admitted row of the unlimited statement.
        limit = int(shape.rsplit(" LIMIT ", 1)[1])
        everything = _rows(org.cypher(context + _unlimited(shape)))
        assert len(fused) == min(limit, len(everything)), (context, shape)
        assert set(fused) <= set(everything), (context, shape)


@pytest.mark.parametrize("shape", SHAPES)
def test_the_limit_is_pushed_under_a_context(org, shape):
    ops = [row["operation"] for row in org.cypher(f"EXPLAIN FOR VALID_TIME AS OF date('2012-01-01') {shape}")]
    assert f"OptimizerPass {PASS}" in ops, (shape, ops)
    off = [
        row["operation"]
        for row in org.cypher(
            f"EXPLAIN FOR VALID_TIME AS OF date('2012-01-01') {shape}",
            disabled_passes=[PASS],
        )
    ]
    assert f"OptimizerPass {PASS}" not in off


def ids(graph, context, shape, key="id"):
    return [row[key] for row in graph.cypher(context + shape).to_list()]


def test_goldens_hidden_first_candidates(org):
    nodes = "MATCH (e:Emp) RETURN e.id AS id LIMIT 3"
    # 2004: Emp 1-6 are the visible ones, and they come first.
    assert ids(org, "FOR VALID_TIME AS OF date('2004-06-01') ", nodes) == [1, 2, 3]
    # 2012: Emp 1-6 ended, so the first admitted candidates are 7, 8, 9.
    assert ids(org, "FOR VALID_TIME AS OF date('2012-01-01') ", nodes) == [7, 8, 9]
    # 2007: nothing is valid; the cap must not stop on hidden candidates.
    assert ids(org, "FOR VALID_TIME AS OF date('2007-01-01') ", nodes) == []
    # ALL sees every version.
    assert ids(org, "FOR VALID_TIME ALL ", nodes) == [1, 2, 3]


def test_goldens_where_before_limit(org):
    shape = "MATCH (e:Emp) WHERE e.team = 'a' RETURN e.id AS id LIMIT 2"
    assert ids(org, "FOR VALID_TIME AS OF date('2012-01-01') ", shape) == [8, 10]
    shape = "MATCH (e:Emp) WHERE e.id % 3 = 0 RETURN e.id AS id LIMIT 2"
    assert ids(org, "FOR VALID_TIME AS OF date('2012-01-01') ", shape) == [9, 12]


def test_goldens_capped_seed_retry(org):
    shape = "MATCH (a)-[:MENTORS]->(b) RETURN a.id AS a LIMIT 3"
    # The 1,100 interns end in 2001 and precede the staff in the index: the
    # capped seed pass reads only interns, finds none visible, and re-runs.
    got = ids(org, "FOR VALID_TIME AS OF date('2005-01-01') ", shape, "a")
    assert len(got) == 3 and all(2001 <= v <= 2006 for v in got), got
    got = ids(org, "FOR VALID_TIME AS OF date('2000-06-01') ", shape, "a")
    assert len(got) == 3 and all(1001 <= v <= 2100 for v in got), got
    # Nothing at all is visible before both begin and after both ended, bar the
    # open-ended staff.
    got = ids(org, "FOR VALID_TIME AS OF date('2031-01-01') ", shape, "a")
    assert len(got) == 3 and all(v > 2000 for v in got), got


def test_goldens_ended_relationships_are_not_counted(org):
    shape = "MATCH (e:Emp)-[:IN_DEPT]->(d:Dept) RETURN e.id AS e, d.id AS d LIMIT 2"
    rows = org.cypher("FOR VALID_TIME AS OF date('2012-01-01') " + shape).to_list()
    # Only the 2011-onward relationship to Dept 101 is visible at 2012.
    assert len(rows) == 2 and {r["d"] for r in rows} == {101}, rows
    rows = org.cypher("FOR VALID_TIME AS OF date('2010-06-01') " + shape).to_list()
    # Dept 102 ended in 2003, so only the Dept 100 relationship is visible.
    assert len(rows) == 2 and {r["d"] for r in rows} == {100}, rows
