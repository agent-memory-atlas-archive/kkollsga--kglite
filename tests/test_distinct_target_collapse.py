"""The matcher's intermediate-hop collapse under a DISTINCT target.

`push_distinct_into_match` lets the matcher keep one partial match per node at
an anonymous intermediate hop. That is only sound while the DISTINCT target is
bound *after* that hop: a target bound earlier (the start node, or an earlier
intermediate) differs between the partials being merged, so merging them
deletes target values. Every expectation here is an absolute value, on every
storage mode, plain and under valid time.
"""

from __future__ import annotations

import pytest

import kglite

# 16 :A -R-> 2 :M -S-> 4 :B -T-> 4 :C, and 20 :P -U-> 1 :Q -V-> 3 :W. The
# second chain is the one the start-node pass reverses (its last node is more
# than 5x more selective than its first), which puts a target written last
# at the start of the executed pattern.
CASES = [
    # target is the start node
    ("MATCH (a:A)-[:R]->(:M)-[:S]->(:B) RETURN count(DISTINCT a) AS n", [{"n": 16}]),
    ("MATCH (a:A)-[:R]->(:M)-[:S]->(:B)-[:T]->(:C) RETURN count(DISTINCT a) AS n", [{"n": 16}]),
    ("MATCH (a:A)-[:R]->(:M)-[:S]->(:B) RETURN DISTINCT a.id AS i ORDER BY i", [{"i": i} for i in range(1, 17)]),
    # target is an intermediate with an anonymous node before and after it
    ("MATCH (:A)-[:R]->(m:M)-[:S]->(:B)-[:T]->(:C) RETURN count(DISTINCT m) AS n", [{"n": 2}]),
    ("MATCH (:A)-[:R]->(m:M)-[:S]->(:B)-[:T]->(:C) RETURN DISTINCT m.id AS i ORDER BY i", [{"i": 100}, {"i": 101}]),
    ("MATCH (:A)-[:R]->(m:M)-[:S]->(:B) RETURN count(DISTINCT m) AS n", [{"n": 2}]),
    # target after every anonymous node: the collapse is still legal
    ("MATCH (:A)-[:R]->(:M)-[:S]->(b:B)-[:T]->(:C) RETURN count(DISTINCT b) AS n", [{"n": 4}]),
    ("MATCH (:A)-[:R]->(:M)-[:S]->(b:B) RETURN count(DISTINCT b) AS n", [{"n": 4}]),
    # written last, reversed by the start-node pass into target-first
    ("MATCH (:P)-[:U]->(:Q)-[:V]->(w:W) RETURN count(DISTINCT w) AS n", [{"n": 3}]),
    ("MATCH (:P)-[:U]->(:Q)-[:V]->(w:W) RETURN DISTINCT w.id AS i ORDER BY i", [{"i": 1}, {"i": 2}, {"i": 3}]),
    # written first, target at the start
    ("MATCH (p:P)-[:U]->(:Q)-[:V]->(:W) RETURN count(DISTINCT p) AS n", [{"n": 20}]),
    # the matcher still enumerates every path when nothing is collapsed
    ("MATCH (:A)-[:R]->(:M)-[:S]->(:B) RETURN count(*) AS n", [{"n": 128}]),
]


@pytest.fixture(scope="module", params=["default", "mapped", "disk"])
def plain(request, tmp_path_factory):
    if request.param == "disk":
        graph = kglite.KnowledgeGraph(storage="disk", path=str(tmp_path_factory.mktemp("dtc") / "g.kgl"))
    else:
        graph = kglite.KnowledgeGraph(storage=request.param)
    graph.cypher(
        "CREATE "
        + ", ".join(
            [f"(:A {{id: {i}}})" for i in range(1, 17)]
            + [f"(:M {{id: {i}}})" for i in (100, 101)]
            + [f"(:B {{id: {i}}})" for i in (201, 202, 203, 204)]
            + [f"(:C {{id: {i}}})" for i in (300, 301, 302, 303)]
            + [f"(:P {{id: {i}}})" for i in range(1, 21)]
            + ["(:Q {id: 1})"]
            + [f"(:W {{id: {i}}})" for i in (1, 2, 3)]
        )
    ).to_list()
    for src, rel, dst in (("A", "R", "M"), ("M", "S", "B"), ("B", "T", "C"), ("P", "U", "Q"), ("Q", "V", "W")):
        graph.cypher(f"MATCH (a:{src}), (b:{dst}) CREATE (a)-[:{rel}]->(b)").to_list()
    return graph


@pytest.mark.parametrize("query,expected", CASES, ids=[str(i) for i in range(len(CASES))])
def test_golden(plain, query, expected):
    assert plain.cypher(query).to_list() == expected


@pytest.mark.parametrize("query,expected", CASES, ids=[str(i) for i in range(len(CASES))])
def test_golden_without_the_pushed_hint(plain, query, expected):
    rows = plain.cypher(query, disabled_passes=["push_distinct_into_match"]).to_list()
    assert rows == expected


@pytest.mark.parametrize("query,expected", CASES[:2] + CASES[3:5], ids=["a3", "a4", "m4", "m4rows"])
def test_a_named_intermediate_agrees(plain, query, expected):
    named = query.replace("(:M)", "(x:M)").replace("(:B)", "(y:B)")
    assert plain.cypher(named).to_list() == expected


# ── valid time ──────────────────────────────────────────────────────────
# A: 8 nodes, ids 1-2 closed at 2010; M: 100 open, 101 closed at 2015.
TEMPORAL = [
    ("MATCH (a:A)-[:R]->(:M)-[:S]->(:B) RETURN count(DISTINCT a) AS n", {"2020": 6, "2012": 6, "ALL": 8}),
    ("MATCH (a:A)-[:R]->(:M)-[:S]->(:B)-[:T]->(:C) RETURN count(DISTINCT a) AS n", {"2020": 6, "2012": 6, "ALL": 8}),
    ("MATCH (:A)-[:R]->(m:M)-[:S]->(:B)-[:T]->(:C) RETURN count(DISTINCT m) AS n", {"2020": 1, "2012": 2, "ALL": 2}),
    ("MATCH (a:A)-[:R]->(:M)-[:S]->(:B) RETURN DISTINCT a.id AS i", {"2020": 6, "2012": 6, "ALL": 8}),
]


@pytest.fixture(scope="module", params=["default", "mapped", "disk"])
def timed(request, tmp_path_factory):
    if request.param == "disk":
        graph = kglite.KnowledgeGraph(storage="disk", path=str(tmp_path_factory.mktemp("dtct") / "g.kgl"))
    else:
        graph = kglite.KnowledgeGraph(storage=request.param)

    def end(i, cutoff):
        return f"date('{cutoff}')" if cutoff else "null"

    parts = []
    for i in range(1, 9):
        parts.append(f"(:A {{id: {i}, vf: date('2000-01-01'), vt: {end(i, '2010-01-01') if i <= 2 else 'null'}}})")
    parts.append("(:M {id: 100, vf: date('2000-01-01'), vt: null})")
    parts.append("(:M {id: 101, vf: date('2000-01-01'), vt: date('2015-01-01')})")
    for i in (201, 202, 203):
        parts.append(f"(:B {{id: {i}, vf: date('2000-01-01'), vt: null}})")
    for i in (300, 301, 302, 303):
        parts.append(f"(:C {{id: {i}, vf: date('2000-01-01'), vt: null}})")
    graph.cypher("CREATE " + ", ".join(parts)).to_list()
    for src, rel, dst in (("A", "R", "M"), ("M", "S", "B"), ("B", "T", "C")):
        graph.cypher(f"MATCH (a:{src}), (b:{dst}) CREATE (a)-[:{rel}]->(b)").to_list()
    for label in ("A", "M", "B", "C"):
        graph.cypher(
            f"CALL db.temporal.declare({{node: '{label}', from: 'vf', to: 'vt', convention: 'closed'}})"
        ).to_list()
    return graph


@pytest.mark.parametrize("instant", ["2020", "2012", "ALL"])
@pytest.mark.parametrize("query,expected", TEMPORAL, ids=["a2", "a3", "m3", "rows"])
def test_golden_under_valid_time(timed, query, expected, instant):
    prefix = "FOR VALID_TIME ALL " if instant == "ALL" else f"FOR VALID_TIME AS OF date('{instant}-06-01') "
    rows = timed.cypher(prefix + query).to_list()
    want = expected[instant]
    got = rows[0]["n"] if len(rows) == 1 and "n" in rows[0] else len(rows)
    assert got == want
    plain = timed.cypher(prefix + query, disabled_passes=["push_distinct_into_match"]).to_list()
    assert plain == rows
