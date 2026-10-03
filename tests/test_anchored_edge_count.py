"""``({id: V})-[:T]->(c)`` relationship counts, fused and unfused.

An untyped ``{id: V}`` anchor names every node carrying that id, whatever its
type; the count is the sum over all of them. The fused count must resolve the
anchor at execution (never bake a node into the plan), under a valid-time
context as well, and answer exactly as the same statement with the pass off.
"""

from __future__ import annotations

import datetime
import subprocess
import sys

import pytest

import kglite

PASS = "fuse_anchored_edge_count"
TYPES = ["Aa", "Bb", "Cc", "Dd", "Ee", "Ff"]

# Rows: (id, type) pairs share id 7 across every type; each type's id-7 node
# has a different number of outgoing and incoming :LINK edges.
BUILD = """
import sys, kglite
g = kglite.KnowledgeGraph()
types = sys.argv[1].split(",")
for i, t in enumerate(types):
    g.cypher(f"CREATE (:{t} {{id: 7, title: '{t}'}})").to_list()
for i, t in enumerate(types):
    for k in range(i):
        g.cypher(f"MATCH (a:{t} {{id: 7}}) CREATE (a)-[:LINK]->(:Leaf {{id: {100 * i + k}}})").to_list()
        g.cypher(f"MATCH (a:{t} {{id: 7}}) CREATE (:Src {{id: {1000 + 100 * i + k}}})-[:LINK]->(a)").to_list()
q = sys.argv[2]
print(g.cypher(q).to_list()[0]["n"])
"""


def _run(types: list[str], query: str) -> int:
    out = subprocess.run(
        [sys.executable, "-c", BUILD, ",".join(types), query],
        capture_output=True,
        text=True,
        check=True,
        encoding="utf-8",
    )
    return int(out.stdout.strip().splitlines()[-1])


@pytest.mark.parametrize(
    "query,expected",
    [
        ("MATCH ({id: 7})-[:LINK]->(c) RETURN count(c) AS n", sum(range(6))),
        ("MATCH (s)-[:LINK]->({id: 7}) RETURN count(*) AS n", sum(range(6))),
    ],
)
def test_the_count_sums_every_node_with_the_id_in_every_process(query, expected):
    """Hash-map order differs per process; the answer may not."""
    answers = set()
    for rotation in range(6):
        order = TYPES[rotation:] + TYPES[:rotation]
        answers.add(_run(order, query))
    assert answers == {expected}


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
def shared():
    """Id 7 on three types; Emp 7 carries two versions (one per era) and its
    relationships are valid on different spans."""
    graph = kglite.KnowledgeGraph()
    graph.cypher(
        "CREATE (e1:Emp {id: 7, v: 1, vf: date('2000-01-01'), vt: date('2008-01-01')}),"
        " (e2:Emp {id: 7, v: 2, vf: date('2008-01-01')}),"
        " (d:Dept {id: 7, vf: date('2000-01-01'), vt: date('2012-01-01')}),"
        " (c:Cost {id: 7}),"
        " (p1:Proj {id: 1, vf: date('2000-01-01')}),"
        " (p2:Proj {id: 2, vf: date('2006-01-01'), vt: date('2010-01-01')}),"
        " (p3:Proj {id: 3, vf: date('2009-01-01')}),"
        " (e1)-[:WORKS {s: date('2000-01-01'), u: date('2007-01-01')}]->(p1),"
        " (e1)-[:WORKS {s: date('2000-01-01')}]->(p2),"
        " (e2)-[:WORKS {s: date('2008-01-01')}]->(p3),"
        " (e2)-[:WORKS {s: date('2008-01-01')}]->(p1),"
        " (d)-[:WORKS {s: date('2000-01-01')}]->(p2),"
        " (d)-[:HOSTS {s: date('2000-01-01')}]->(p3),"
        " (c)-[:WORKS]->(p1),"
        " (p3)-[:WORKS {s: date('2009-01-01')}]->(e2),"
        " (p1)-[:WORKS {s: date('2000-01-01')}]->(d)"
    ).to_list()
    for declaration in (
        "{node: 'Emp', from: 'vf', to: 'vt', convention: 'half_open'}",
        "{node: 'Dept', from: 'vf', to: 'vt', convention: 'half_open'}",
        "{node: 'Proj', from: 'vf', to: 'vt', convention: 'half_open'}",
        "{relationship: 'WORKS', from: 's', to: 'u', convention: 'half_open'}",
        "{relationship: 'HOSTS', from: 's', to: 'u', convention: 'half_open'}",
    ):
        graph.cypher(f"CALL db.temporal.declare({declaration})").to_list()
    return graph


SHAPES = [
    "MATCH ({id: 7})-[:WORKS]->(c) RETURN count(c) AS n",
    "MATCH ({id: 7})-[:WORKS]->(c) RETURN count(*) AS n",
    "MATCH ({id: 7})-[:WORKS|HOSTS]->(c) RETURN count(c) AS n",
    "MATCH ({id: 7})-[r]->(c) RETURN count(c) AS n",
    "MATCH ({id: 7})<-[:WORKS]-(c) RETURN count(c) AS n",
    "MATCH (s)-[:WORKS]->({id: 7}) RETURN count(s) AS n",
    "MATCH (s)<-[:WORKS]-({id: 7}) RETURN count(*) AS n",
    "MATCH (s)-[:WORKS|HOSTS]->({id: 7}) RETURN count(s) AS n",
    "MATCH ({id: 99})-[:WORKS]->(c) RETURN count(c) AS n",
    "MATCH (s)-[:WORKS]->({id: 1}) RETURN count(s) AS n",
    "MATCH ({id: 3})<-[:HOSTS]-(c) RETURN count(c) AS n",
]


def _n(rows):
    return [row["n"] for row in rows]


@pytest.mark.parametrize("shape", SHAPES)
def test_fused_answers_as_the_matcher_unguarded(shared, shape):
    assert _n(shared.cypher(shape).to_list()) == _n(shared.cypher(shape, disabled_passes=[PASS]).to_list())
    assert _n(shared.cypher("FOR VALID_TIME ALL " + shape).to_list()) == _n(
        shared.cypher("FOR VALID_TIME ALL " + shape, disabled_passes=[PASS]).to_list()
    )


@pytest.mark.parametrize("shape", SHAPES)
def test_fused_answers_as_the_matcher_at_every_instant(shared, shape):
    for date in INSTANTS:
        query = at(date, shape)
        fused = _n(shared.cypher(query).to_list())
        plain = _n(shared.cypher(query, disabled_passes=[PASS]).to_list())
        assert fused == plain, (date, shape)


def _plan(graph, query):
    return [row["operation"] for row in graph.cypher("EXPLAIN " + query)]


@pytest.mark.parametrize("shape", SHAPES)
def test_the_count_fuses_without_a_filter(shared, shape):
    plan = _plan(shared, "FOR VALID_TIME ALL " + shape)
    assert f"OptimizerPass {PASS}" in plan, (shape, plan)


@pytest.mark.parametrize(
    "shape",
    [
        "MATCH (s:Emp)-[:WORKS]->({id: 1}) RETURN count(*) AS n",
        "MATCH (s)-[:WORKS]->({id: 1}) RETURN s.id AS s, count(*) AS n",
        "MATCH (s)-[:WORKS]->({id: 1}) WITH s, count(*) AS n RETURN s.id AS s, n",
    ],
)
def test_an_id_anchor_off_the_group_is_left_to_the_matcher(shared, shape):
    """The aggregate operators scan the group end; an anchor on the far end
    would cost a scan of every node and count same-id duplicates, so the
    matcher's id seed answers instead."""
    plan = _plan(shared, "FOR VALID_TIME ALL " + shape)
    assert not [op for op in plan if "FusedMatch" in op and "Aggregate" in op], plan
    assert _n(shared.cypher("FOR VALID_TIME ALL " + shape).to_list()) == _n(
        shared.cypher("FOR VALID_TIME ALL " + shape, disabled_passes=["fuse_match_return_aggregate"]).to_list()
    )


def test_the_expected_counts_at_a_few_instants(shared):
    """Hand-derived goldens: the answer follows the instant, one cached plan."""
    query = "MATCH ({id: 7})-[:WORKS]->(c) RETURN count(c) AS n"
    expected = {}
    for date in ("2001-01-01", "2006-06-01", "2009-06-01", "2011-01-01", "2020-01-01"):
        expected[date] = _n(shared.cypher(at(date, query), disabled_passes=[PASS]).to_list())
    for _ in range(2):
        for date, want in expected.items():
            assert _n(shared.cypher(at(date, query)).to_list()) == want, date
    assert len({tuple(v) for v in expected.values()}) > 1, expected


def test_an_instant_parameter_is_never_baked_into_the_plan(shared):
    query = "FOR VALID_TIME AS OF $t MATCH ({id: 7})-[:WORKS]->(c) RETURN count(c) AS n"
    seen = set()
    for date in INSTANTS * 2:
        got = _n(shared.cypher(query, params={"t": datetime.date.fromisoformat(date)}).to_list())
        want = _n(shared.cypher(at(date, query.split("$t ")[1]), disabled_passes=[PASS]).to_list())
        assert got == want, date
        seen.add(tuple(got))
    assert len(seen) > 1
