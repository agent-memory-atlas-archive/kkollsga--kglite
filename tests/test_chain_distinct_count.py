"""``count(DISTINCT x)`` over a linear chain, answered by reachability sweeps.

The matcher enumerates every path of the chain and deduplicates afterwards;
the fused clause (``FusedChainDistinctCount``, planned by
``fuse_chain_path_count``) sweeps the chain forward, then backward from the far
end, and counts the nodes (or relationships) that lie on a complete path.

Every golden here comes from a brute-force path enumerator written over the
fixture's own edge lists, never from the engine. The fixtures are built so a
forward-only answer, a node-test-free answer and a relationship count without
id deduplication would each give a different number somewhere.
"""

from __future__ import annotations

import pytest

import kglite
from tests.test_chain_path_count import (
    AT_SITE,
    CONTEXTS,
    DEPTS,
    EMPS,
    IN_REGION,
    INSTANTS,
    LEAD,
    REGIONS,
    WORKS_IN,
    _create,
    _valid,
)

PASS = "fuse_chain_path_count"
CLAUSE = "FusedChainDistinctCount"


def _ops(graph, query, **kwargs):
    return [row["operation"] for row in graph.cypher(f"EXPLAIN {query}", **kwargs)]


def _fused(graph, query):
    return any(op.startswith(CLAUSE) for op in _ops(graph, query))


def _n(graph, query, **kwargs):
    return graph.cypher(query, **kwargs).to_list()


# ── a brute-force path enumerator over explicit edge lists ───────────────


def enumerate_paths(starts, hops):
    """Every path as (nodes, edge ids). A hop is (edges, direction, keep).

    ``edges`` is a list of (edge_id, source, target); ``direction`` is "out",
    "in" or "both"; ``keep`` tests the node a hop reaches. An undirected hop
    meets a self-loop once. Edge identity is the id, so parallel edges differ.
    """
    paths = [([s], []) for s in starts]
    for edges, direction, keep in hops:
        step = []
        for nodes, ids in paths:
            here = nodes[-1]
            for eid, src, dst in edges:
                reached = []
                if direction in ("out", "both") and src == here:
                    reached.append(dst)
                if direction in ("in", "both") and dst == here and not (direction == "both" and src == dst):
                    reached.append(src)
                for peer in reached:
                    if keep(peer):
                        step.append((nodes + [peer], ids + [eid]))
        paths = step
    return paths


def distinct_per_position(paths, hop_count):
    nodes = [len({p[0][j] for p in paths}) for j in range(hop_count + 1)]
    rels = [len({p[1][i] for p in paths}) for i in range(hop_count)]
    return nodes, rels


# ── a plain graph with branching, dead ends, parallels and a self-loop ───

TEAMS = {"t1": "n", "t2": "s", "t3": "n", "t4": "n"}
DEPT_KIND = {"d1": "big", "d2": "small", "d3": "big", "d4": "big"}
PROJECTS = ["p1", "p2", "p3", "p4", "p5"]
TASKS = {"w1": False, "w2": True, "w3": True, "w4": False, "w5": False, "w6": True}  # priority label
LED_BY = [("t1", "d1"), ("t1", "d1"), ("t2", "d1"), ("t2", "d2"), ("t3", "d3"), ("t4", "d4")]
FUNDED_BY = [  # (project, dept, share)
    ("p1", "d1", 50),
    ("p2", "d1", 100),
    ("p2", "d2", 50),
    ("p3", "d2", 25),
    ("p4", "d3", 50),
    ("p5", "d9", 50),
]
IN_PROJECT = [("w1", "p1"), ("w2", "p1"), ("w2", "p2"), ("w3", "p2"), ("w4", "p3"), ("w5", "p4"), ("w6", "p5")]
PARTNER = [("d1", "d1"), ("d1", "d2"), ("d2", "d3"), ("d3", "d3"), ("d4", "d1")]
# d4 funds nothing and d9 is a dept nobody leads: dead ends on either side.
EXTRA_DEPTS = {"d9": "big"}


def _plain(graph):
    kinds = {**DEPT_KIND, **EXTRA_DEPTS}
    parts = [f"({t}:Team {{id: '{t}', region: '{r}'}})" for t, r in TEAMS.items()]
    parts += [f"({d}:Dept {{id: '{d}', kind: '{k}'}})" for d, k in kinds.items()]
    parts += [f"({p}:Project {{id: '{p}'}})" for p in PROJECTS]
    parts += [f"({w}:Task {{id: '{w}'}})" for w in TASKS]
    graph.cypher("CREATE " + ", ".join(parts)).to_list()
    for rel, rows, src, dst in (
        ("LED_BY", LED_BY, "Team", "Dept"),
        ("FUNDED_BY", FUNDED_BY, "Project", "Dept"),
        ("IN_PROJECT", IN_PROJECT, "Task", "Project"),
        ("PARTNER", PARTNER, "Dept", "Dept"),
    ):
        for row in rows:
            share = f" {{share: {row[2]}}}" if len(row) == 3 else ""
            graph.cypher(
                f"MATCH (a:{src} {{id: '{row[0]}'}}), (b:{dst} {{id: '{row[1]}'}}) CREATE (a)-[:{rel}{share}]->(b)"
            ).to_list()
    graph.cypher("MATCH (w:Task) WHERE w.id IN ['w2', 'w3', 'w6'] SET w:Priority").to_list()
    graph.cypher("MATCH (d:Dept {id: 'd2'}) SET d:Sponsor").to_list()
    return graph


@pytest.fixture(params=["default", "mapped", "disk"])
def plain(request, tmp_path):
    if request.param == "disk":
        graph = kglite.KnowledgeGraph(storage="disk", path=str(tmp_path / "p.kgl"))
    else:
        graph = kglite.KnowledgeGraph(storage=request.param)
    return _plain(graph)


def _edges(rows):
    return [(i, a, b) for i, (a, b, *_rest) in enumerate(rows)]


def _funded(share=None):
    return [(i, p, d) for i, (p, d, s) in enumerate(FUNDED_BY) if share is None or s == share]


ALL_KINDS = {**DEPT_KIND, **EXTRA_DEPTS}
TEAM_IDS = list(TEAMS)
HEAD = "(f:Team)-[r1:LED_BY]->(c:Dept)<-[r2:FUNDED_BY]-(l:Project)<-[r3:IN_PROJECT]-(w:Task)"


def _chain_hops(dept_ok=lambda d: True, share=None, task_ok=lambda w: True):
    return [
        (_edges(LED_BY), "out", dept_ok),
        (_funded(share), "in", lambda p: p in PROJECTS),
        (_edges(IN_PROJECT), "in", task_ok),
    ]


def _check_all_positions(graph, head, hops, starts=TEAM_IDS, names=("f", "c", "l", "w"), rels=("r1", "r2", "r3")):
    paths = enumerate_paths(starts, hops)
    node_goldens, rel_goldens = distinct_per_position(paths, len(hops))
    for name, golden in zip(names, node_goldens):
        q = f"MATCH {head} RETURN count(DISTINCT {name}) AS n"
        assert _fused(graph, q), q
        assert _n(graph, q) == [{"n": golden}], q
        assert _n(graph, q, disabled_passes=[PASS]) == [{"n": golden}], q
    for name, golden in zip(rels, rel_goldens):
        q = f"MATCH {head} RETURN count(DISTINCT {name}) AS n"
        assert _fused(graph, q), q
        assert _n(graph, q) == [{"n": golden}], q
        assert _n(graph, q, disabled_passes=[PASS]) == [{"n": golden}], q
    return node_goldens, rel_goldens


def test_every_position_and_relationship_matches_the_enumerator(plain):
    nodes, rels = _check_all_positions(plain, HEAD, _chain_hops())
    # The fixture separates what a forward-only sweep reports from the answer:
    # d4 and p5 are reached forward but lead nowhere, so the intermediate
    # counts must be strictly smaller than a forward reachability.
    assert nodes == [3, 3, 4, 5], nodes
    assert rels == [5, 5, 6], rels  # w6 reaches p5, which funds only an unled dept


def test_a_forward_only_answer_would_be_wrong_here():
    # Forward reachability from the teams: d1 d2 d3 d4, projects p1..p4 (p5 funds d9, no team leads it),
    # tasks w1..w5. Of those d4 funds nothing, so it is on no complete path.
    forward_depts = {d for _t, d in LED_BY}
    complete_depts = {d for _t, d in LED_BY if any(dd == d for _p, dd, _s in FUNDED_BY)}
    assert forward_depts - complete_depts == {"d4"}


def test_reversed_spelling_gives_the_same_counts(plain):
    head = "(w:Task)-[r3:IN_PROJECT]->(l:Project)-[r2:FUNDED_BY]->(c:Dept)<-[r1:LED_BY]-(f:Team)"
    paths = enumerate_paths(TEAM_IDS, _chain_hops())
    node_goldens, rel_goldens = distinct_per_position(paths, 3)
    for name, golden in zip(("f", "c", "l", "w"), node_goldens):
        assert _n(plain, f"MATCH {head} RETURN count(DISTINCT {name}) AS n") == [{"n": golden}], name
    for name, golden in zip(("r1", "r2", "r3"), rel_goldens):
        assert _n(plain, f"MATCH {head} RETURN count(DISTINCT {name}) AS n") == [{"n": golden}], name


def test_node_properties_and_edge_properties_filter_each_hop(plain):
    head = (
        "(f:Team)-[r1:LED_BY]->(c:Dept {kind: 'big'})<-[r2:FUNDED_BY {share: 50}]-(l:Project)<-[r3:IN_PROJECT]-(w:Task)"
    )
    hops = _chain_hops(dept_ok=lambda d: ALL_KINDS[d] == "big", share=50)
    nodes, rels = _check_all_positions(plain, head, hops)
    # big depts d1, d3 (d4 funds nothing); share 50: p1->d1 and p4->d3 -> w1, w2, w5.
    assert nodes == [3, 2, 2, 3] and rels[0] == 4, (nodes, rels)


def test_multiple_labels_filter_the_reached_node(plain):
    head = "(f:Team)-[r1:LED_BY]->(c:Dept)<-[r2:FUNDED_BY]-(l:Project)<-[r3:IN_PROJECT]-(w:Task:Priority)"
    hops = _chain_hops(task_ok=lambda w: TASKS[w])
    _check_all_positions(plain, head, hops)
    head = "(f:Team)-[r1:LED_BY]->(c:Sponsor)<-[r2:FUNDED_BY]-(l:Project)<-[r3:IN_PROJECT]-(w:Task)"
    hops = _chain_hops(dept_ok=lambda d: d == "d2")
    nodes, _ = _check_all_positions(plain, head, hops)
    assert nodes[1] == 1


def test_a_label_alternation_peer(plain):
    head = "(f:Team)-[:LED_BY]->(c:Dept)<-[:FUNDED_BY]-(l:Project)<-[:IN_PROJECT]-(w:Task|Team)"
    paths = enumerate_paths(TEAM_IDS, _chain_hops(task_ok=lambda w: w in TASKS))
    for name, j in (("f", 0), ("w", 3)):
        assert _n(plain, f"MATCH {head} RETURN count(DISTINCT {name}) AS n") == [{"n": len({p[0][j] for p in paths})}]


def test_an_undirected_self_loop_hop_counts_the_loop_once(plain):
    head = "(f:Team)-[:LED_BY]->(c:Dept)-[x:PARTNER]-(p:Dept)<-[:FUNDED_BY]-(l:Project)"
    hops = [
        (_edges(LED_BY), "out", lambda d: True),
        (_edges(PARTNER), "both", lambda d: True),
        (_funded(), "in", lambda p: p in PROJECTS),
    ]
    paths = enumerate_paths(TEAM_IDS, hops)
    nodes, rels = distinct_per_position(paths, 3)
    for name, golden in (("f", nodes[0]), ("c", nodes[1]), ("p", nodes[2]), ("l", nodes[3]), ("x", rels[1])):
        q = f"MATCH {head} RETURN count(DISTINCT {name}) AS n"
        assert _fused(plain, q), q
        assert _n(plain, q) == [{"n": golden}], q
    # d1 and d3 carry self-loops. A relationship met from both its ends, and a
    # self-loop met twice, would each inflate the relationship count.
    assert rels[1] == 5


def test_an_empty_frontier_counts_zero(plain):
    for name in ("f", "c", "l", "w", "r1", "r3"):
        q = f"MATCH {HEAD.replace('f:Team', 'f:Nothing')} RETURN count(DISTINCT {name}) AS n"
        assert _n(plain, q) == [{"n": 0}], name
    q = f"MATCH {ANON_HEAD.replace('w:Task', 'w:Nothing')} RETURN count(DISTINCT f) AS n"
    assert _fused(plain, q)
    assert _n(plain, q) == [{"n": 0}]


def test_the_count_alias_and_a_two_hop_chain(plain):
    head = "(f:Team)-[:LED_BY]->(c:Dept)<-[:FUNDED_BY]-(l:Project)"
    paths = enumerate_paths(TEAM_IDS, _chain_hops()[:2])
    nodes, _ = distinct_per_position(paths, 2)
    q = f"MATCH {head} RETURN count(DISTINCT l) AS projects"
    assert _fused(plain, q)
    assert _n(plain, q) == [{"projects": nodes[2]}]


def test_the_count_reads_the_graph_it_runs_on_not_the_plan(plain):
    q = f"MATCH {HEAD} RETURN count(DISTINCT w) AS n"
    assert _n(plain, q) == [{"n": 5}]
    plain.cypher("MATCH (w:Task {id: 'w6'}), (p:Project {id: 'p4'}) CREATE (w)-[:IN_PROJECT]->(p)").to_list()
    assert _n(plain, q) == [{"n": 6}]


def test_parameters_resolve(plain):
    q = (
        "MATCH (f:Team {id: $fid})-[:LED_BY]->(c:Dept)<-[:FUNDED_BY]-(l:Project)<-[:IN_PROJECT]-(w:Task) "
        "RETURN count(DISTINCT w) AS n"
    )
    paths = enumerate_paths(["t2"], _chain_hops())
    assert _n(plain, q, params={"fid": "t2"}) == [{"n": len({p[0][3] for p in paths})}]


# ── shapes that stay on the matcher ──────────────────────────────────────

ANON_HEAD = "(f:Team)-[:LED_BY]->(c:Dept)<-[:FUNDED_BY]-(l:Project)<-[:IN_PROJECT]-(w:Task)"
BAILS = [
    # a residual predicate
    "MATCH {head} WHERE c.kind <> 'small' RETURN count(DISTINCT w) AS n",
    # a property of the counted variable
    "MATCH {head} RETURN count(DISTINCT w.id) AS n",
    # another aggregate beside it
    "MATCH {head} RETURN count(DISTINCT w) AS n, count(*) AS m",
    # a grouping key
    "MATCH {head} RETURN f.id AS f, count(DISTINCT w) AS n",
    # DISTINCT rows
    "MATCH {head} RETURN DISTINCT w.id AS i",
    # HAVING-shaped post-filter
    "MATCH {head} WITH count(DISTINCT w) AS n WHERE n > 1 RETURN n",
    # OPTIONAL MATCH
    "MATCH (f:Team) OPTIONAL MATCH (f)-[:LED_BY]->(c:Dept)<-[:FUNDED_BY]-(l:Project)<-[:IN_PROJECT]-(w:Task) "
    "RETURN count(DISTINCT w) AS n",
    # a comma pattern
    "MATCH (f:Team)-[:LED_BY]->(c:Dept)<-[:FUNDED_BY]-(l:Project), (l)<-[:IN_PROJECT]-(w:Task) "
    "RETURN count(DISTINCT w) AS n",
    # a variable-length hop
    "MATCH (f:Team)-[:LED_BY]->(c:Dept)<-[:FUNDED_BY*1..2]-(l:Project) RETURN count(DISTINCT l) AS n",
    # a path assignment
    "MATCH p = (f:Team)-[:LED_BY]->(c:Dept)<-[:FUNDED_BY]-(l:Project) RETURN count(DISTINCT l) AS n",
    # a repeated variable (a cycle)
    "MATCH (f:Team)-[:LED_BY]->(c:Dept)<-[:FUNDED_BY]-(l:Project)<-[:IN_PROJECT]-(w:Task)-[:IN_PROJECT]->(l) "
    "RETURN count(DISTINCT w) AS n",
    # hop types that overlap
    "MATCH (a:Dept)-[:PARTNER]->(b:Dept)-[:PARTNER]->(c:Dept)-[:PARTNER]->(d:Dept) RETURN count(DISTINCT d) AS n",
    "MATCH (a:Dept)-[:PARTNER]->(b:Dept)-[:PARTNER]->(c:Dept) RETURN count(DISTINCT c) AS n",
    # an untyped hop
    "MATCH (f:Team)-[:LED_BY]->(c:Dept)<-[]-(l:Project) RETURN count(DISTINCT l) AS n",
    # a single hop is the hop fusions' shape
    "MATCH (f:Team)-[:LED_BY]->(c:Dept) RETURN count(DISTINCT c) AS n",
]


@pytest.mark.parametrize("shape", BAILS)
def test_the_unfusable_shapes_stay_on_the_matcher(plain, shape):
    q = shape.format(head=ANON_HEAD)
    assert not _fused(plain, q), q
    assert _n(plain, q) == _n(plain, q, disabled_passes=[PASS]), q


def test_the_aggregate_only_hint_is_accepted_but_another_aggregate_variable_is_not(plain):
    q = f"MATCH {HEAD} RETURN count(DISTINCT w) AS n"
    assert "OptimizerPass push_distinct_into_match" in _ops(plain, q)
    assert _fused(plain, q)
    # min(w.id) beside it is a second aggregate over w: the pair stays unfused.
    q2 = f"MATCH {HEAD} RETURN count(DISTINCT w) AS n, min(w.id) AS m"
    assert not _fused(plain, q2)


def test_the_disabled_hint_pass_still_fuses(plain):
    q = f"MATCH {HEAD} RETURN count(DISTINCT w) AS n"
    assert _n(plain, q, disabled_passes=["push_distinct_into_match"]) == _n(plain, q)


# ── valid time: an independent model of the temporal organisation ────────


def _model_paths(t, hops):
    """Complete paths Emp -> Dept -> Site [-> Region] at instant `t` (None = ALL)."""

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

    def rel_ok(lo, hi, closed):
        return t is None or _valid(lo, hi, t, closed)

    paths = []
    for wi, (a, b, lo, hi) in enumerate(WORKS_IN):
        if not (emp_ok(a) and dept_ok(b) and rel_ok(lo, hi, False)):
            continue
        for si, (b2, c, lo2, hi2) in enumerate(AT_SITE):
            if b2 != b or not rel_ok(lo2, hi2, True):
                continue
            if hops == 2:
                paths.append(((a, b, c), (wi, si)))
                continue
            for ri, (c2, d, lo3, hi3) in enumerate(IN_REGION):
                if c2 == c and region_ok(d) and rel_ok(lo3, hi3, False):
                    paths.append(((a, b, c, d), (wi, si, ri)))
    return paths


@pytest.fixture(scope="module", params=["default", "mapped", "disk"])
def org(request, tmp_path_factory):
    if request.param == "disk":
        graph = kglite.KnowledgeGraph(storage="disk", path=str(tmp_path_factory.mktemp("cdc") / "g.kgl"))
    else:
        graph = kglite.KnowledgeGraph(storage=request.param)
    _create(graph)
    return graph


ORG3 = "(e:Emp)-[r1:WORKS_IN]->(d:Dept)-[r2:AT_SITE]->(s:Site)-[r3:IN_REGION]->(r:Region)"
ORG2 = "(e:Emp)-[r1:WORKS_IN]->(d:Dept)-[r2:AT_SITE]->(s:Site)"


@pytest.mark.parametrize("context", CONTEXTS)
def test_every_position_matches_the_model_under_every_context(org, context):
    t = INSTANTS[CONTEXTS.index(context)] if "AS OF" in context else None
    for hops, head, names, rels in (
        (3, ORG3, ("e", "d", "s", "r"), ("r1", "r2", "r3")),
        (2, ORG2, ("e", "d", "s"), ("r1", "r2")),
    ):
        paths = _model_paths(t, hops)
        for j, name in enumerate(names):
            q = f"{context}MATCH {head} RETURN count(DISTINCT {name}) AS n"
            assert _fused(org, q), q
            assert _n(org, q) == [{"n": len({p[0][j] for p in paths})}], q
        for i, name in enumerate(rels):
            q = f"{context}MATCH {head} RETURN count(DISTINCT {name}) AS n"
            assert _n(org, q) == [{"n": len({p[1][i] for p in paths})}], q


@pytest.mark.parametrize("context", ["", "FOR VALID_TIME ALL ", CONTEXTS[3]])
def test_fused_answers_as_the_matcher_with_no_model(org, context):
    for head, names in ((ORG3, ("e", "d", "s", "r", "r1", "r2", "r3")), (ORG2, ("e", "d", "s", "r1", "r2"))):
        for name in names:
            q = f"{context}MATCH {head} RETURN count(DISTINCT {name}) AS n"
            assert _n(org, q) == _n(org, q, disabled_passes=[PASS]), q


def test_the_model_goldens_are_not_all_equal(org):
    # The sweep above must be able to fail: positions and instants disagree.
    seen = {(j, t): len({p[0][j] for p in _model_paths(t, 3)}) for j in range(4) for t in INSTANTS}
    assert len(set(seen.values())) >= 4
    assert len({len({p[1][1] for p in _model_paths(t, 3)}) for t in INSTANTS}) >= 3


def test_a_secondary_label_hides_the_start_until_it_is_valid(org):
    lead = "MATCH (e:Lead)-[:WORKS_IN]->(d:Dept)-[:AT_SITE]->(s:Site) RETURN count(DISTINCT d) AS n"
    assert _n(org, f"FOR VALID_TIME AS OF date('2005-06-01') {lead}") == [{"n": 0}]
    assert _n(org, f"FOR VALID_TIME AS OF date('2007-06-15') {lead}") == [{"n": 1}]
    assert _n(org, f"FOR VALID_TIME AS OF date('2012-06-01') {lead}") == [{"n": 0}]


# ── the planner-reversed spelling and the matcher's own shapes ───────────


@pytest.fixture(params=["default", "mapped", "disk"])
def fan(request, tmp_path):
    if request.param == "disk":
        graph = kglite.KnowledgeGraph(storage="disk", path=str(tmp_path / "f.kgl"))
    else:
        graph = kglite.KnowledgeGraph(storage=request.param)
    counts = {"A": 16, "M": 2, "B": 4, "C": 4, "P": 20, "Q": 1, "W": 3}
    for label, n in counts.items():
        graph.cypher(f"UNWIND range(1, {n}) AS i CREATE (:{label} {{id: i}})").to_list()
    for src, rel, dst in (("A", "R", "M"), ("M", "S", "B"), ("B", "T", "C"), ("P", "U", "Q"), ("Q", "V", "W")):
        graph.cypher(f"MATCH (a:{src}), (b:{dst}) CREATE (a)-[:{rel}]->(b)").to_list()
    return graph


@pytest.mark.parametrize(
    "query,golden",
    [
        ("MATCH (a:A)-[:R]->(:M)-[:S]->(:B) RETURN count(DISTINCT a) AS n", 16),
        ("MATCH (:A)-[:R]->(m:M)-[:S]->(:B)-[:T]->(:C) RETURN count(DISTINCT m) AS n", 2),
        ("MATCH (:A)-[:R]->(:M)-[:S]->(b:B)-[:T]->(:C) RETURN count(DISTINCT b) AS n", 4),
        ("MATCH (:A)-[:R]->(:M)-[:S]->(:B)-[:T]->(c:C) RETURN count(DISTINCT c) AS n", 4),
        ("MATCH (:P)-[:U]->(:Q)-[:V]->(w:W) RETURN count(DISTINCT w) AS n", 3),
        ("MATCH (p:P)-[:U]->(:Q)-[:V]->(:W) RETURN count(DISTINCT p) AS n", 20),
        ("MATCH (:P)-[:U]->(q:Q)-[:V]->(:W) RETURN count(DISTINCT q) AS n", 1),
        ("MATCH (:A)-[r:R]->(:M)-[:S]->(:B) RETURN count(DISTINCT r) AS n", 32),
        ("MATCH (:A)-[:R]->(:M)-[s:S]->(:B) RETURN count(DISTINCT s) AS n", 8),
    ],
)
def test_the_distinct_collapse_shapes_fuse_and_answer(fan, query, golden):
    assert _fused(fan, query), query
    assert _n(fan, query) == [{"n": golden}]
    assert _n(fan, query, disabled_passes=[PASS]) == [{"n": golden}]


def test_a_budget_stops_the_sweeps():
    graph = kglite.KnowledgeGraph()
    graph.cypher("UNWIND range(1, 3000) AS i CREATE (:A {id: i})-[:R]->(:B {id: i})-[:S]->(:C {id: i})").to_list()
    q = "MATCH (a:A)-[:R]->(b:B)-[:S]->(c:C) RETURN count(DISTINCT c) AS n"
    assert _fused(graph, q)
    assert _n(graph, q) == [{"n": 3000}]
    with pytest.raises(Exception, match="work"):
        graph.cypher(q, max_work_units=100).to_list()
