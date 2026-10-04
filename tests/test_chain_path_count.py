"""``fuse_chain_path_count``: a linear chain's ``count(*)`` as a degree-product DP.

The matcher materialises every path of the chain; the fused clause counts them
hop by hop over the nodes the chain reaches. Each shape must answer as the
same statement with the pass disabled (the matcher), on every storage mode,
and under ``FOR VALID_TIME AS OF`` at twelve instants and under ALL. The
temporal fixture hides endpoints whose relationships are valid, ends
relationships on valid endpoints, carries parallel history relationships, an
undeclared middle layer, and a declared secondary label; an independent
brute-force model of the fixture gives the goldens.
"""

from __future__ import annotations

import pytest

import kglite

PASS = "fuse_chain_path_count"

INSTANTS = [
    "2000-01-01",
    "2003-01-01",
    "2004-12-31",
    "2005-01-01",
    "2006-01-01",
    "2007-06-15",
    "2008-01-01",
    "2009-01-01",
    "2010-01-01",
    "2012-01-01",
    "2013-01-01",
    "2031-01-01",
]
CONTEXTS = [f"FOR VALID_TIME AS OF date('{d}') " for d in INSTANTS] + ["FOR VALID_TIME ALL "]

# name: (valid from, valid to) with None open-ended; closed nodes, half-open
# departments and regions, as declared below.
EMPS = {
    1: ("2000-01-01", None),
    2: ("2000-01-01", "2008-01-01"),
    3: ("2003-01-01", None),
    4: ("2000-01-01", None),
    5: ("2009-01-01", None),
    6: ("2000-01-01", "2004-12-31"),
}
LEAD = {4: ("2006-01-01", "2012-01-01")}
DEPTS = {1: ("2000-01-01", None), 2: ("2000-01-01", "2010-01-01"), 3: ("2005-01-01", None)}
REGIONS = {1: ("2000-01-01", None), 2: ("2007-01-01", "2013-01-01")}
SITES = (1, 2, 3)
# (source, target, from, to); WORKS_IN half-open, AT_SITE closed, IN_REGION half-open.
WORKS_IN = [
    (1, 1, "2000-01-01", None),
    (1, 2, "2000-01-01", "2005-01-01"),
    (1, 1, "2002-01-01", "2004-01-01"),
    (2, 1, "2001-01-01", None),
    (2, 2, "2000-01-01", None),
    (3, 2, "2003-01-01", None),
    (3, 3, "2005-01-01", None),
    (4, 3, "2000-01-01", "2009-01-01"),
    (5, 1, "2009-01-01", None),
    (5, 3, "2009-01-01", "2012-01-01"),
    (6, 1, "2000-01-01", None),
]
AT_SITE = [
    (1, 1, "2000-01-01", None),
    (1, 2, "2004-06-01", "2011-12-31"),
    (2, 2, "2000-01-01", None),
    (3, 3, "2005-01-01", None),
    (3, 1, "2008-01-01", None),
]
IN_REGION = [
    (1, 1, "2000-01-01", None),
    (2, 2, "2000-01-01", None),
    (2, 1, "2009-01-01", None),
    (3, 2, "2006-01-01", "2010-01-01"),
    (3, 1, "2000-01-01", None),
]


def _d(value):
    return "null" if value is None else f"date('{value}')"


def _create(graph):
    parts = []
    for i, (vf, vt) in EMPS.items():
        lead = LEAD.get(i, (None, None))
        parts.append(f"(e{i}:Emp {{id: {i}, vf: {_d(vf)}, vt: {_d(vt)}, p_from: {_d(lead[0])}, p_to: {_d(lead[1])}}})")
    for i, (vf, vt) in DEPTS.items():
        parts.append(f"(d{i}:Dept {{id: {i}, title: '{'Ops' if i != 3 else 'Lab'}', vf: {_d(vf)}, vt: {_d(vt)}}})")
    for i in SITES:
        parts.append(f"(s{i}:Site {{id: {i}}})")
    for i, (vf, vt) in REGIONS.items():
        parts.append(f"(r{i}:Region {{id: {i}, vf: {_d(vf)}, vt: {_d(vt)}}})")
    graph.cypher("CREATE " + ", ".join(parts)).to_list()
    for rel, rows, src, dst in (
        ("WORKS_IN", WORKS_IN, "e", "d"),
        ("AT_SITE", AT_SITE, "d", "s"),
        ("IN_REGION", IN_REGION, "s", "r"),
    ):
        for a, b, lo, hi in rows:
            graph.cypher(
                f"MATCH (a:{_label(src)} {{id: {a}}}), (b:{_label(dst)} {{id: {b}}}) "
                f"CREATE (a)-[:{rel} {{lo: {_d(lo)}, hi: {_d(hi)}}}]->(b)"
            ).to_list()
    graph.cypher("MATCH (e:Emp) WHERE e.id = 4 SET e:Lead").to_list()
    for declaration in (
        "{node: 'Emp', from: 'vf', to: 'vt', convention: 'closed'}",
        "{node: 'Lead', from: 'p_from', to: 'p_to', convention: 'closed'}",
        "{node: 'Dept', from: 'vf', to: 'vt', convention: 'half_open'}",
        "{node: 'Region', from: 'vf', to: 'vt', convention: 'half_open'}",
        "{relationship: 'WORKS_IN', from: 'lo', to: 'hi', convention: 'half_open'}",
        "{relationship: 'AT_SITE', from: 'lo', to: 'hi', convention: 'closed'}",
        "{relationship: 'IN_REGION', from: 'lo', to: 'hi', convention: 'half_open'}",
    ):
        graph.cypher(f"CALL db.temporal.declare({declaration})").to_list()


def _label(prefix):
    return {"e": "Emp", "d": "Dept", "s": "Site", "r": "Region"}[prefix]


@pytest.fixture(scope="module", params=["default", "mapped", "disk"])
def org(request, tmp_path_factory):
    if request.param == "disk":
        graph = kglite.KnowledgeGraph(storage="disk", path=str(tmp_path_factory.mktemp("chain") / "g.kgl"))
    else:
        graph = kglite.KnowledgeGraph(storage=request.param)
    _create(graph)
    return graph


# ── an independent model of the fixture ─────────────────────────────────


def _valid(lo, hi, t, closed):
    if lo is not None and t < lo:
        return False
    if hi is None:
        return True
    return t <= hi if closed else t < hi


def _model_count(t, hops, start_ok=lambda i: True, mid_ok=lambda i: True):
    """Paths Emp -> Dept -> Site [-> Region] at instant `t` (None = ALL)."""

    def emp_ok(i):
        if t is None:
            return True
        ok = _valid(*EMPS[i], t, True)
        if i in LEAD:
            ok = ok and _valid(*LEAD[i], t, True)
        return ok

    def dept_ok(i):
        return t is None or _valid(*DEPTS[i], t, False)

    def region_ok(i):
        return t is None or _valid(*REGIONS[i], t, False)

    def rel_ok(row, closed):
        return t is None or _valid(row[2], row[3], t, closed)

    total = 0
    for a, b, lo, hi in WORKS_IN:
        if not (emp_ok(a) and dept_ok(b) and rel_ok((a, b, lo, hi), False) and start_ok(a) and mid_ok(b)):
            continue
        for b2, c, lo2, hi2 in AT_SITE:
            if b2 != b or not rel_ok((b2, c, lo2, hi2), True):
                continue
            if hops == 2:
                total += 1
                continue
            for c2, d, lo3, hi3 in IN_REGION:
                if c2 == c and region_ok(d) and rel_ok((c2, d, lo3, hi3), False):
                    total += 1
    return total


THREE = "MATCH (e:Emp)-[:WORKS_IN]->(d:Dept)-[:AT_SITE]->(s:Site)-[:IN_REGION]->(r:Region) RETURN count(*) AS n"
TWO = "MATCH (e:Emp)-[:WORKS_IN]->(d:Dept)-[:AT_SITE]->(s:Site) RETURN count(*) AS n"

# Shapes that fuse under a context: both hop counts, both directions of
# traversal, untyped nodes, a middle property, the secondary-labelled start,
# count of a bound variable, an undirected hop, a relationship variable and an
# alternation whose branches stay disjoint from the other hops.
FUSED = [
    THREE,
    TWO,
    "MATCH (r:Region)<-[:IN_REGION]-(s:Site)<-[:AT_SITE]-(d:Dept)<-[:WORKS_IN]-(e:Emp) RETURN count(*) AS n",
    "MATCH (s:Site)<-[:AT_SITE]-(d:Dept)<-[:WORKS_IN]-(e:Emp) RETURN count(*) AS n",
    "MATCH (e)-[:WORKS_IN]->(d)-[:AT_SITE]->(s)-[:IN_REGION]->(r) RETURN count(*) AS n",
    "MATCH (e:Emp)-[:WORKS_IN]->(d:Dept {title: 'Ops'})-[:AT_SITE]->(s:Site) RETURN count(*) AS n",
    "MATCH (e:Emp)-[:WORKS_IN]->(d:Dept {title: 'Lab'})-[:AT_SITE]->(s:Site)-[:IN_REGION]->(r:Region) "
    "RETURN count(*) AS n",
    "MATCH (e:Lead)-[:WORKS_IN]->(d:Dept)-[:AT_SITE]->(s:Site)-[:IN_REGION]->(r:Region) RETURN count(*) AS n",
    "MATCH (e:Emp {id: 3})-[:WORKS_IN]->(d:Dept)-[:AT_SITE]->(s:Site)-[:IN_REGION]->(r:Region) RETURN count(*) AS n",
    "MATCH (e:Emp)-[:WORKS_IN]->(d:Dept)-[:AT_SITE]->(s:Site)-[:IN_REGION]->(r:Region {id: 1}) RETURN count(*) AS n",
    "MATCH (e:Emp)-[:WORKS_IN]->(d:Dept)-[:AT_SITE]->(s:Site)-[:IN_REGION]->(r:Region) RETURN count(r) AS n",
    "MATCH (e:Emp)-[:WORKS_IN]->(d:Dept)-[:AT_SITE]->(s:Site)-[:IN_REGION]->(r:Region) RETURN count(*) AS total",
    "MATCH (e:Emp)-[x:WORKS_IN]->(d:Dept)-[:AT_SITE]->(s:Site) RETURN count(x) AS n",
    "MATCH (e:Emp)-[:WORKS_IN]-(d:Dept)-[:AT_SITE]->(s:Site) RETURN count(*) AS n",
    "MATCH (e:Emp)-[:WORKS_IN]->(d:Dept)-[:AT_SITE]-(s:Site)-[:IN_REGION]->(r:Region) RETURN count(*) AS n",
    "MATCH (e:Emp)-[:WORKS_IN|ASSIGNED_TO]->(d:Dept)-[:AT_SITE]->(s:Site) RETURN count(*) AS n",
    "MATCH (e:Emp)-[:WORKS_IN]->(d:Dept)-[:AT_SITE]->(s:Site)-[:IN_REGION]->(r:Nothing) RETURN count(*) AS n",
]

# Answered by the matcher either way: DISTINCT over a property (the bare
# `count(DISTINCT v)` is `FusedChainDistinctCount`), a group key beside a second aggregate, a cycle, a repeated
# type, an untyped hop, a variable-length hop, a residual WHERE, OPTIONAL MATCH,
# comma patterns.
UNFUSED = [
    "MATCH (e:Emp)-[:WORKS_IN]->(d:Dept)-[:AT_SITE]->(s:Site)-[:IN_REGION]->(r:Region) RETURN count(DISTINCT r.id)",
    "MATCH (e:Emp)-[:WORKS_IN]->(d:Dept)-[:AT_SITE]->(s:Site)-[:IN_REGION]->(r:Region) "
    "RETURN d.id AS d, count(*) AS n, min(e.id) AS m",
    "MATCH (e:Emp)-[:WORKS_IN]->(d:Dept)-[:AT_SITE]->(s:Site)<-[:AT_SITE]-(d2:Dept) RETURN count(*) AS n",
    "MATCH (e:Emp)-[:WORKS_IN]->(d:Dept)-[]->(s:Site)-[:IN_REGION]->(r:Region) RETURN count(*) AS n",
    "MATCH (e:Emp)-[:WORKS_IN]->(d:Dept)-[:AT_SITE*1..2]->(s:Site) RETURN count(*) AS n",
    "MATCH (e:Emp)-[:WORKS_IN]->(d:Dept)-[:AT_SITE]->(s:Site) WHERE e.id > 2 RETURN count(*) AS n",
    "MATCH (e:Emp) OPTIONAL MATCH (e)-[:WORKS_IN]->(d:Dept)-[:AT_SITE]->(s:Site) RETURN count(s) AS n",
    "MATCH (e:Emp)-[:WORKS_IN]->(d:Dept)-[:AT_SITE]->(s:Site), (s)-[:IN_REGION]->(r:Region) RETURN count(*) AS n",
    "MATCH (e:Emp)-[:WORKS_IN]->(d:Dept)-[:AT_SITE]->(s:Site)-[:IN_REGION]->(r:Region)<-[:IN_REGION]-(s2:Site) "
    "RETURN count(*) AS n",
]


def _ops(graph, query):
    return [row["operation"] for row in graph.cypher(f"EXPLAIN {query}")]


def _n(graph, query, **kwargs):
    return graph.cypher(query, **kwargs).to_list()


@pytest.mark.parametrize("shape", FUSED + UNFUSED)
def test_fused_answers_as_the_matcher_under_every_context(org, shape):
    for context in CONTEXTS + [""]:
        fused = _n(org, context + shape)
        plain = _n(org, context + shape, disabled_passes=[PASS])
        assert fused == plain, (context, shape)


@pytest.mark.parametrize("shape", FUSED[:12])
def test_the_chain_fuses_under_a_context(org, shape):
    ops = _ops(org, CONTEXTS[4] + shape)
    assert f"OptimizerPass {PASS}" in ops, (shape, ops)
    assert any(op.startswith("FusedChainPathCount") for op in ops), ops


def test_all_fuses_the_three_hop_chain_but_leaves_two_hops_to_the_aggregate_fusion(org):
    assert f"OptimizerPass {PASS}" in _ops(org, "FOR VALID_TIME ALL " + THREE)
    assert f"OptimizerPass {PASS}" not in _ops(org, "FOR VALID_TIME ALL " + TWO)


@pytest.mark.parametrize("shape", UNFUSED)
def test_the_unfusable_shapes_stay_on_the_matcher(org, shape):
    assert f"OptimizerPass {PASS}" not in _ops(org, CONTEXTS[4] + shape), shape


def test_a_two_hop_chain_stays_unfused_without_a_context():
    graph = kglite.KnowledgeGraph()
    graph.cypher("CREATE (:A {id: 1})-[:R]->(:B {id: 1})-[:S]->(:C {id: 1})").to_list()
    q = "MATCH (a:A)-[:R]->(b:B)-[:S]->(c:C) RETURN count(*) AS n"
    assert f"OptimizerPass {PASS}" not in _ops(graph, q)
    assert graph.cypher(q).to_list() == [{"n": 1}]


@pytest.mark.parametrize("hops,shape", [(3, THREE), (2, TWO)])
def test_goldens_match_the_model_at_every_instant(org, hops, shape):
    for date in INSTANTS:
        got = _n(org, f"FOR VALID_TIME AS OF date('{date}') {shape}")[0]["n"]
        assert got == _model_count(date, hops), (date, hops)
    assert _n(org, f"FOR VALID_TIME ALL {shape}")[0]["n"] == _model_count(None, hops)


def test_model_goldens_are_not_all_equal(org):
    # The sweep above must be able to fail: the instants disagree.
    counts = {_model_count(d, 3) for d in INSTANTS}
    assert len(counts) >= 5, counts
    assert _model_count(None, 3) == 30


def test_hand_goldens_two_instants(org):
    # 2006-01-01: e1, e2, e3 and e4 (a Lead from 2006) are visible; the pairs
    # e1d1, e2d1, e2d2, e3d2, e3d3, e4d3 reach 2, 2, 1, 1, 1, 1 sites; only s1
    # and s3 reach the one visible region (r1), through e1d1, e2d1, e3d3, e4d3.
    assert _n(org, f"FOR VALID_TIME AS OF date('2006-01-01') {TWO}") == [{"n": _model_count("2006-01-01", 2)}]
    assert _model_count("2006-01-01", 2) == 8
    assert _model_count("2006-01-01", 3) == 4
    assert _n(org, f"FOR VALID_TIME AS OF date('2006-01-01') {THREE}") == [{"n": 4}]
    # 2031-01-01: only e1d1, e5d1 and e3d3 survive (the other departments and
    # Lead windows have ended); region r1 is the one visible region, reached
    # from s1 (e1d1, e5d1, and e3d3 over its late site) and from s3 (e3d3).
    assert _model_count("2031-01-01", 3) == 4
    assert _n(org, f"FOR VALID_TIME AS OF date('2031-01-01') {THREE}") == [{"n": 4}]


def test_a_secondary_label_hides_the_start_until_it_is_valid(org):
    lead = "MATCH (e:Lead)-[:WORKS_IN]->(d:Dept)-[:AT_SITE]->(s:Site) RETURN count(*) AS n"
    assert _n(org, f"FOR VALID_TIME AS OF date('2005-06-01') {lead}") == [{"n": 0}]
    assert _n(org, f"FOR VALID_TIME AS OF date('2007-06-15') {lead}")[0]["n"] > 0
    assert _n(org, f"FOR VALID_TIME AS OF date('2012-06-01') {lead}") == [{"n": 0}]


# ── the unguarded path: goldens, trails, anchors, overflow ──────────────


def _plain(storage_graph):
    storage_graph.cypher(
        "CREATE (f1:Team {id: 1}), (f2:Team {id: 2}), (c1:Dept {id: 1}), (c2:Dept {id: 2}),"
        " (l1:Project {id: 1}), (l2:Project {id: 2}), (l3:Project {id: 3}),"
        " (w1:Task {id: 1}), (w2:Task {id: 2}), (w3:Task {id: 3}), (w4:Task {id: 4}),"
        " (f1)-[:LED_BY]->(c1), (f1)-[:LED_BY]->(c1), (f2)-[:LED_BY]->(c1),"
        " (f2)-[:LED_BY]->(c2),"
        " (l1)-[:FUNDED_BY {share: 50}]->(c1), (l2)-[:FUNDED_BY {share: 100}]->(c1),"
        " (l3)-[:FUNDED_BY {share: 50}]->(c2),"
        " (w1)-[:IN_PROJECT]->(l1), (w2)-[:IN_PROJECT]->(l1), (w3)-[:IN_PROJECT]->(l2),"
        " (w4)-[:IN_PROJECT]->(l3),"
        " (c1)-[:PARTNER]->(c1), (c1)-[:PARTNER]->(c2)"
    ).to_list()
    return storage_graph


@pytest.fixture(params=["default", "mapped", "disk"])
def plain(request, tmp_path):
    if request.param == "disk":
        graph = kglite.KnowledgeGraph(storage="disk", path=str(tmp_path / "p.kgl"))
    else:
        graph = kglite.KnowledgeGraph(storage=request.param)
    return _plain(graph)


CHAIN = "(f:Team)-[:LED_BY]->(c:Dept)<-[:FUNDED_BY]-(l:Project)<-[:IN_PROJECT]-(w:Task)"


def test_hand_golden_three_hop(plain):
    # f1 reaches c1 twice (parallel), f2 reaches c1 and c2. c1 has projects
    # l1 (tasks w1, w2) and l2 (w3): 3 tasks; c2 has l3 (w4): 1 task.
    # f1: 2 * 3 = 6; f2: 1 * 3 + 1 * 1 = 4.
    q = f"MATCH {CHAIN} RETURN count(*) AS n"
    assert f"OptimizerPass {PASS}" in _ops(plain, q)
    assert plain.cypher(q).to_list() == [{"n": 10}]
    assert plain.cypher(q, disabled_passes=[PASS]).to_list() == [{"n": 10}]


def test_the_undirected_hop_counts_a_self_loop_once(plain):
    # c1 -[:PARTNER]-> c1 is one relationship: from c1 it is one match, not two.
    q = "MATCH (f:Team)-[:LED_BY]->(c:Dept)-[:PARTNER]-(p:Dept)<-[:FUNDED_BY]-(l:Project) RETURN count(*) AS n"
    assert f"OptimizerPass {PASS}" in _ops(plain, q)
    fused = plain.cypher(q).to_list()
    assert fused == plain.cypher(q, disabled_passes=[PASS]).to_list()
    # c1 -> p in {c1 (loop, once), c2}; p = c2 has c2<-PARTNER-c1 only.
    # f paths into c1: 3; into c2: 1 (via c2's PARTNER from c1 only when c = c1).
    # c = c1: p = c1 -> projects of c1: 2 -> 3 * 1 * 2 = 6; p = c2 -> l3 -> 3 * 1 = 3;
    # c = c2: p = c1 (incoming PARTNER) -> 2 projects -> 1 * 2 = 2.
    assert fused == [{"n": 11}]


def test_the_edge_property_filter_applies(plain):
    q = (
        "MATCH (f:Team)-[:LED_BY]->(c:Dept)<-[:FUNDED_BY {share: 50}]-(l:Project)"
        "<-[:IN_PROJECT]-(w:Task) RETURN count(*) AS n"
    )
    assert f"OptimizerPass {PASS}" in _ops(plain, q)
    # share 50: l1 (c1) and l3 (c2): f1: 2 * 2 = 4, f2: 1 * 2 + 1 * 1 = 3.
    assert plain.cypher(q).to_list() == [{"n": 7}]


def test_a_repeated_type_keeps_the_matchers_trail_rule(plain):
    # PARTNER twice: the matcher forbids reusing the c1 self-loop, the DP could not.
    q = "MATCH (a:Dept)-[:PARTNER]->(b:Dept)-[:PARTNER]->(c:Dept)-[:PARTNER]->(d:Dept) RETURN count(*) AS n"
    assert f"OptimizerPass {PASS}" not in _ops(plain, q)
    assert plain.cypher(q).to_list() == plain.cypher(q, disabled_passes=[PASS]).to_list()


def test_the_cycle_keeps_its_identity_constraint(plain):
    plain.cypher("MATCH (l:Project {id: 1}), (f:Team {id: 1}) CREATE (l)-[:COVERS]->(f)").to_list()
    q = "MATCH (f:Team)-[:LED_BY]->(c:Dept)<-[:FUNDED_BY]-(l:Project)-[:COVERS]->(f) RETURN count(*) AS n"
    assert f"OptimizerPass {PASS}" not in _ops(plain, q)
    assert plain.cypher(q).to_list() == [{"n": 2}]


def test_the_count_reads_the_graph_it_runs_on_not_the_plan(plain):
    q = f"MATCH {CHAIN} RETURN count(*) AS n"
    assert plain.cypher(q).to_list() == [{"n": 10}]
    plain.cypher("MATCH (w:Task {id: 4}) CREATE (w)-[:IN_PROJECT]->(:Project {id: 9})").to_list()
    plain.cypher("MATCH (l:Project {id: 9}), (c:Dept {id: 2}) CREATE (l)-[:FUNDED_BY]->(c)").to_list()
    assert plain.cypher(q).to_list() == [{"n": 11}]


def test_parameters_in_a_chain_resolve(plain):
    q = (
        "MATCH (f:Team {id: $fid})-[:LED_BY]->(c:Dept)<-[:FUNDED_BY]-(l:Project)"
        "<-[:IN_PROJECT]-(w:Task) RETURN count(*) AS n"
    )
    assert f"OptimizerPass {PASS}" in _ops(plain, q.replace("$fid", "1"))
    assert plain.cypher(q, params={"fid": 1}).to_list() == [{"n": 6}]
    assert plain.cypher(q, params={"fid": 2}).to_list() == [{"n": 4}]
    with pytest.raises(Exception, match="fid"):
        plain.cypher(q).to_list()


def test_an_anchored_chain_costs_its_frontier_not_the_graph():
    graph = kglite.KnowledgeGraph()
    graph.cypher("CREATE (:Team {id: 1})-[:LED_BY]->(:Dept {id: 1})<-[:FUNDED_BY]-(:Project {id: 0})").to_list()
    graph.cypher("UNWIND range(1, 5000) AS i CREATE (:Project {id: i})-[:FUNDED_BY]->(:Dept {id: 1000 + i})").to_list()
    graph.cypher(
        "UNWIND range(1, 5000) AS i MATCH (c:Dept {id: 1000 + i}) CREATE (:Team {id: 10 + i})-[:LED_BY]->(c)"
    ).to_list()
    graph.cypher(
        "MATCH (l:Project {id: 0}) UNWIND range(1, 5) AS i CREATE (:Task {id: i})-[:IN_PROJECT]->(l)"
    ).to_list()
    graph.cypher(
        "UNWIND range(1, 5000) AS i MATCH (l:Project {id: i}) CREATE (:Task {id: 100 + i})-[:IN_PROJECT]->(l)"
    ).to_list()
    tail = "-[:LED_BY]->(c:Dept)<-[:FUNDED_BY]-(l:Project)<-[:IN_PROJECT]-"
    at_start = f"MATCH (f:Team {{id: 1}}){tail}(w:Task) RETURN count(*) AS n"
    at_end = f"MATCH (f:Team){tail}(w:Task {{id: 3}}) RETURN count(*) AS n"
    unanchored = f"MATCH (f:Team){tail}(w:Task) RETURN count(*) AS n"
    for query, expected, budget in ((at_start, 5, 100), (at_end, 1, 100), (unanchored, 5005, None)):
        assert f"OptimizerPass {PASS}" in _ops(graph, query)
        kwargs = {} if budget is None else {"max_work_units": budget}
        assert graph.cypher(query, **kwargs).to_list() == [{"n": expected}], query
        assert graph.cypher(query, disabled_passes=[PASS]).to_list() == [{"n": expected}], query
    # Starting from the wrong end of the anchored chain walks the large layer:
    # the same budget refuses it, so the two cases above are not vacuous.
    off = ["optimize_pattern_start_node"]
    assert f"OptimizerPass {PASS}" in _ops_disabled(graph, at_end, off)
    with pytest.raises(Exception, match="work"):
        graph.cypher(at_end, max_work_units=100, disabled_passes=off).to_list()


def _ops_disabled(graph, query, disabled):
    return [row["operation"] for row in graph.cypher(f"EXPLAIN {query}", disabled_passes=disabled)]


@pytest.mark.parametrize("hops,expected", [(6, 10**18)])
def test_a_count_past_the_matcher_row_budget_is_still_exact(hops, expected):
    graph = _fan_out(hops)
    q = _fan_query(hops)
    assert graph.cypher(q).to_list() == [{"n": expected}]


def test_a_count_past_the_integer_range_is_an_error():
    graph = _fan_out(7)
    with pytest.raises(Exception, match="exceeds"):
        graph.cypher(_fan_query(7)).to_list()


def _fan_out(hops):
    graph = kglite.KnowledgeGraph()
    graph.cypher("CREATE " + ", ".join(f"(:L{i} {{id: 1}})" for i in range(hops + 1))).to_list()
    for i in range(hops):
        graph.cypher(f"MATCH (a:L{i}), (b:L{i + 1}) UNWIND range(1, 1000) AS k CREATE (a)-[:T{i}]->(b)").to_list()
    return graph


def _fan_query(hops):
    pattern = "(n0:L0)" + "".join(f"-[:T{i}]->(n{i + 1}:L{i + 1})" for i in range(hops))
    return f"MATCH {pattern} RETURN count(*) AS n"


# ── two hops over overlapping types: the matcher's relationship-uniqueness rule ──

# (source, target, from, to), half-open. Self-loops on p1, p3 and p5, a parallel
# pair p2 -> p3, a reciprocal pair p1 <-> p2, and p5 closed in 2008.
KNOWS = [
    (1, 2, "2000-01-01", None),
    (2, 3, "2000-01-01", None),
    (2, 3, "2004-01-01", "2009-01-01"),
    (3, 3, "2000-01-01", None),
    (1, 1, "2000-01-01", "2010-01-01"),
    (4, 2, "2001-01-01", None),
    (2, 1, "2000-01-01", None),
    (3, 5, "2000-01-01", None),
    (5, 5, "2000-01-01", None),
]
FOLLOWS = [(1, 3, "2000-01-01", None), (3, 3, "2002-01-01", None), (5, 1, "2000-01-01", "2007-01-01")]
PEOPLE = {
    1: ("2000-01-01", None),
    2: ("2000-01-01", None),
    3: ("2000-01-01", None),
    4: ("2000-01-01", None),
    5: ("2000-01-01", "2008-01-01"),
}


@pytest.fixture(scope="module", params=["default", "mapped", "disk"])
def knows(request, tmp_path_factory):
    if request.param == "disk":
        graph = kglite.KnowledgeGraph(storage="disk", path=str(tmp_path_factory.mktemp("knows") / "g.kgl"))
    else:
        graph = kglite.KnowledgeGraph(storage=request.param)
    graph.cypher(
        "CREATE "
        + ", ".join(
            f"(:Person {{id: {i}, team: '{'x' if i % 2 else 'y'}', vf: {_d(vf)}, vt: {_d(vt)}}})"
            for i, (vf, vt) in PEOPLE.items()
        )
    ).to_list()
    for rel, rows in (("KNOWS", KNOWS), ("FOLLOWS", FOLLOWS)):
        for a, b, lo, hi in rows:
            graph.cypher(
                f"MATCH (a:Person {{id: {a}}}), (b:Person {{id: {b}}}) "
                f"CREATE (a)-[:{rel} {{lo: {_d(lo)}, hi: {_d(hi)}, w: {a + b}}}]->(b)"
            ).to_list()
    graph.cypher("CALL db.temporal.declare({node: 'Person', from: 'vf', to: 'vt', convention: 'closed'})").to_list()
    for rel in ("KNOWS", "FOLLOWS"):
        graph.cypher(
            f"CALL db.temporal.declare({{relationship: '{rel}', from: 'lo', to: 'hi', convention: 'half_open'}})"
        ).to_list()
    return graph


SAME_TYPE = [
    "MATCH (a)-[:KNOWS]->(b)-[:KNOWS]->(c) RETURN count(*) AS n",
    "MATCH (a)-[:KNOWS]->(b)<-[:KNOWS]-(c) RETURN count(*) AS n",
    "MATCH (a)<-[:KNOWS]-(b)-[:KNOWS]->(c) RETURN count(*) AS n",
    "MATCH (a)<-[:KNOWS]-(b)<-[:KNOWS]-(c) RETURN count(*) AS n",
    "MATCH (a:Person)-[:KNOWS]->(b:Person)-[:KNOWS]->(c:Person) RETURN count(*) AS n",
    "MATCH (a:Person {team: 'x'})-[:KNOWS]->(b)-[:KNOWS]->(c:Person {team: 'y'}) RETURN count(*) AS n",
    "MATCH (a)-[]->(b)-[]->(c) RETURN count(*) AS n",
    "MATCH (a)-[]->(b)<-[]-(c) RETURN count(*) AS n",
    "MATCH (a)-[:KNOWS|FOLLOWS]->(b)-[:KNOWS]->(c) RETURN count(*) AS n",
    "MATCH (a)-[:KNOWS|FOLLOWS]->(b)<-[:FOLLOWS|KNOWS]-(c) RETURN count(*) AS n",
    "MATCH (a)-[:KNOWS]->(b)-[:FOLLOWS]->(c) RETURN count(*) AS n",
    "MATCH (a)-[:KNOWS {w: 6}]->(b)-[:KNOWS]->(c) RETURN count(*) AS n",
    "MATCH (a)-[r:KNOWS]->(b)<-[:KNOWS]-(c) RETURN count(r) AS n",
    "MATCH (a:Person {id: 2})-[:KNOWS]->(b)-[:KNOWS]->(c) RETURN count(*) AS n",
    "MATCH (a)-[:KNOWS]->(b)<-[:KNOWS]-(c:Person {id: 3}) RETURN count(*) AS n",
]
# Undirected hops and three same-type hops have no correction: matcher either way.
SAME_TYPE_MATCHER = [
    "MATCH (a)-[:KNOWS]-(b)-[:KNOWS]->(c) RETURN count(*) AS n",
    "MATCH (a)-[:KNOWS]-(b)-[:KNOWS]-(c) RETURN count(*) AS n",
    "MATCH (a)-[:KNOWS]->(b)-[:KNOWS]->(c)-[:KNOWS]->(d) RETURN count(*) AS n",
]


@pytest.mark.parametrize("shape", SAME_TYPE + SAME_TYPE_MATCHER)
def test_two_overlapping_hops_answer_as_the_matcher(knows, shape):
    for context in CONTEXTS + [""]:
        fused = _n(knows, context + shape)
        plain = _n(knows, context + shape, disabled_passes=[PASS])
        assert fused == plain, (context, shape)


@pytest.mark.parametrize("shape", SAME_TYPE)
def test_two_overlapping_hops_fuse_under_a_context(knows, shape):
    assert f"OptimizerPass {PASS}" in _ops(knows, CONTEXTS[4] + shape), shape
    assert f"OptimizerPass {PASS}" not in _ops(knows, "FOR VALID_TIME ALL " + shape), shape


@pytest.mark.parametrize("shape", SAME_TYPE_MATCHER)
def test_uncorrectable_overlap_stays_on_the_matcher(knows, shape):
    assert f"OptimizerPass {PASS}" not in _ops(knows, CONTEXTS[4] + shape), shape


def test_the_trail_rule_goldens(knows):
    at = "FOR VALID_TIME AS OF date('2005-06-01') "
    forward = _n(knows, at + SAME_TYPE[0])
    opposite = _n(knows, at + SAME_TYPE[1])
    # All nine KNOWS rows hold at 2005-06-01. Each first edge leads to its
    # target's out-degree, less itself when it is a self-loop:
    # e1 3, e2 2, e3 2, e4 (p3 loop) 1, e5 (p1 loop) 1, e6 3, e7 2, e8 1, e9 (p5 loop) 0.
    assert forward == [{"n": 15}]
    # In-degrees p1 2, p2 2, p3 3, p5 2, p4 0: sum of in * (in - 1) = 2 + 2 + 6 + 2.
    assert opposite == [{"n": 12}]
    # The matcher agrees; the uncorrected degree products would be 18 and 21.
    assert _n(knows, at + SAME_TYPE[0], disabled_passes=[PASS]) == forward
    assert _n(knows, at + SAME_TYPE[1], disabled_passes=[PASS]) == opposite
