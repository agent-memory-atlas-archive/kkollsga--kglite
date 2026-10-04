"""``RETURN <key>, count(DISTINCT x)`` over a linear chain, answered by sweeps.

The grouped form of ``FusedChainDistinctCount``: the chain's forward and
backward sweeps leave the nodes that lie on a complete path, and each valid node
of the grouping position gets the set of ``x`` reachable from it through valid
nodes; groups then merge by key *value* and count the union of their sets.

Every golden comes from the brute-force path enumerator of
``test_chain_distinct_count`` over the fixture's own edge lists. The fixtures
share key values across nodes (a forward-only valid set, a missing union across
nodes of one key, or a per-node count would each differ somewhere) and keep
dead ends the sweeps must drop.
"""

from __future__ import annotations

import pytest

import kglite
from tests.test_chain_distinct_count import (
    ALL_KINDS,
    ANON_HEAD,
    CLAUSE,
    HEAD,
    LED_BY,
    PARTNER,
    PASS,
    PROJECTS,
    TASKS,
    TEAM_IDS,
    TEAMS,
    _chain_hops,
    _edges,
    _funded,
    _fused,
    _model_paths,
    _n,
    _ops,
    enumerate_paths,
)
from tests.test_chain_distinct_count import (
    org as _org_fixture,
)
from tests.test_chain_distinct_count import (
    plain as _plain_fixture,
)
from tests.test_chain_path_count import CONTEXTS, INSTANTS

# The fixtures of the ungrouped tests, under their own names.
plain = _plain_fixture
org = _org_fixture

# ── goldens ──────────────────────────────────────────────────────────────


def grouped_golden(paths, group_pos, key_fn, target):
    """{key: distinct targets} over `paths`; `target` is ("n", j) or ("r", i)."""
    kind, j = target
    groups: dict = {}
    for nodes, ids in paths:
        value = nodes[j] if kind == "n" else ids[j]
        groups.setdefault(key_fn(nodes[group_pos]), set()).add(value)
    return {key: len(values) for key, values in groups.items()}


def run_grouped(graph, query, **kwargs):
    rows = _n(graph, query, **kwargs)
    return {row["k"]: row["n"] for row in rows}, rows


def check_grouped(graph, head, key_expr, group_pos, key_fn, target_var, target, hops, starts, prefix=""):
    paths = enumerate_paths(starts, hops)
    golden = grouped_golden(paths, group_pos, key_fn, target)
    q = f"{prefix}MATCH {head} RETURN {key_expr} AS k, count(DISTINCT {target_var}) AS n"
    assert _fused(graph, q), q
    got, rows = run_grouped(graph, q)
    assert len(rows) == len(got), f"duplicate key rows in {q}"
    assert got == golden, q
    plain_got, _ = run_grouped(graph, q, disabled_passes=[PASS])
    assert plain_got == golden, q
    return golden


# ── a plain graph: every (group position, target) pair ───────────────────

KEYS = {
    0: ("f.region", lambda t: TEAMS[t]),
    1: ("c.kind", lambda d: ALL_KINDS[d]),
    2: ("l.kind", lambda p: None),  # Project has no `kind`: one NULL group
    3: ("w.id", lambda w: w),
}
NODE_VARS = ("f", "c", "l", "w")
REL_VARS = ("r1", "r2", "r3")
TARGETS = [(var, ("n", j)) for j, var in enumerate(NODE_VARS)] + [(var, ("r", i)) for i, var in enumerate(REL_VARS)]


@pytest.mark.parametrize("group_pos", range(4))
def test_every_group_and_target_position_matches_the_enumerator(plain, group_pos):
    key_expr, key_fn = KEYS[group_pos]
    for var, target in TARGETS:
        check_grouped(plain, HEAD, key_expr, group_pos, key_fn, var, target, _chain_hops(), TEAM_IDS)


def test_the_goldens_separate_the_failure_modes():
    paths = enumerate_paths(TEAM_IDS, _chain_hops())
    # The NULL key groups every project: the departments they fund overlap
    # (p2 and p3 both fund d2), so the union differs from the sum over the
    # group's nodes and from its largest single node.
    golden = grouped_golden(paths, 2, KEYS[2][1], ("n", 1))
    per_node = grouped_golden(paths, 2, lambda p: p, ("n", 1))
    assert golden == {None: 3}
    assert sum(per_node.values()) > golden[None] > max(per_node.values())
    # d4 is reached forward and funds nothing: it is in no group.
    assert "d4" not in grouped_golden(paths, 1, lambda d: d, ("n", 3))
    assert grouped_golden(paths, 2, KEYS[2][1], ("n", 3)) == {None: 5}
    assert grouped_golden(paths, 1, KEYS[1][1], ("n", 3)) == {"big": 4, "small": 3}


def test_reversed_spelling_gives_the_same_groups(plain):
    head = "(w:Task)-[r3:IN_PROJECT]->(l:Project)-[r2:FUNDED_BY]->(c:Dept)<-[r1:LED_BY]-(f:Team)"
    paths = enumerate_paths(TEAM_IDS, _chain_hops())
    for group_pos, (key_expr, key_fn) in KEYS.items():
        for var, (kind, j) in TARGETS:
            golden = grouped_golden(paths, group_pos, key_fn, (kind, j))
            q = f"MATCH {head} RETURN {key_expr} AS k, count(DISTINCT {var}) AS n"
            assert _fused(plain, q), q
            assert run_grouped(plain, q)[0] == golden, q


def test_a_node_key_and_a_key_that_is_the_target_itself(plain):
    paths = enumerate_paths(TEAM_IDS, _chain_hops())
    # grouping by the counted node: one distinct node per key value
    q = f"MATCH {HEAD} RETURN c.kind AS k, count(DISTINCT c) AS n"
    assert _fused(plain, q)
    assert run_grouped(plain, q)[0] == grouped_golden(paths, 1, KEYS[1][1], ("n", 1))
    # the bare node as key: one row per valid node
    q = f"MATCH {HEAD} RETURN c AS k, count(DISTINCT w) AS n"
    assert _fused(plain, q)
    fused = {row["k"]["properties"]["id"]: row["n"] for row in _n(plain, q)}
    unfused = {row["k"]["properties"]["id"]: row["n"] for row in _n(plain, q, disabled_passes=[PASS])}
    assert fused == unfused == grouped_golden(paths, 1, lambda d: d, ("n", 3))


def test_empty_groups_emit_no_row(plain):
    q = f"MATCH {HEAD} RETURN c.id AS k, count(DISTINCT w) AS n"
    got, rows = run_grouped(plain, q)
    # d4 funds nothing and d9 is led by no team: no complete path through either.
    assert set(got) == {"d1", "d2", "d3"} and len(rows) == 3
    q = f"MATCH {HEAD.replace('f:Team', 'f:Nothing')} RETURN c.kind AS k, count(DISTINCT w) AS n"
    assert _fused(plain, q)
    assert _n(plain, q) == []


def test_column_order_follows_the_return_list(plain):
    paths = enumerate_paths(TEAM_IDS, _chain_hops())
    golden = grouped_golden(paths, 1, KEYS[1][1], ("n", 3))
    q = f"MATCH {HEAD} RETURN count(DISTINCT w) AS n, c.kind AS k"
    assert _fused(plain, q)
    result = plain.cypher(q)
    assert list(result.columns) == ["n", "k"]
    assert {row["k"]: row["n"] for row in result.to_list()} == golden
    q = f"MATCH {HEAD} RETURN c.kind, count(DISTINCT w)"
    assert list(plain.cypher(q).columns) == ["c.kind", "count(DISTINCT w)"]


def test_order_by_skip_and_limit_over_the_returned_columns(plain):
    paths = enumerate_paths(TEAM_IDS, _chain_hops())
    golden = grouped_golden(paths, 2, lambda p: p, ("n", 3))
    ranked = sorted(golden.items(), key=lambda kv: (-kv[1], kv[0]))
    for tail, expect in (
        ("ORDER BY n DESC, k", ranked),
        ("ORDER BY n DESC, k LIMIT 2", ranked[:2]),
        ("ORDER BY n DESC, k SKIP 1 LIMIT 2", ranked[1:3]),
        ("ORDER BY k DESC LIMIT 3", sorted(golden.items(), reverse=True)[:3]),
    ):
        q = f"MATCH {HEAD} RETURN l.id AS k, count(DISTINCT w) AS n {tail}"
        assert _fused(plain, q), q
        want = [{"k": k, "n": n} for k, n in expect]
        assert _n(plain, q) == want, q
        assert _n(plain, q, disabled_passes=[PASS]) == want, q
    q = f"MATCH {HEAD} RETURN c.kind, count(DISTINCT w) ORDER BY c.kind DESC LIMIT 1"
    assert _fused(plain, q)
    assert _n(plain, q) == [{"c.kind": "small", "count(DISTINCT w)": 3}]
    q = f"MATCH {HEAD} RETURN c.kind AS k, count(DISTINCT w) AS n LIMIT 1"
    assert _fused(plain, q)
    assert len(_n(plain, q)) == 1


def test_multiple_labels_filter_the_reached_nodes(plain):
    head = "(f:Team)-[r1:LED_BY]->(c:Dept)<-[r2:FUNDED_BY]-(l:Project)<-[r3:IN_PROJECT]-(w:Task:Priority)"
    hops = _chain_hops(task_ok=lambda w: TASKS[w])
    for group_pos, (key_expr, key_fn) in KEYS.items():
        for var, target in TARGETS:
            check_grouped(plain, head, key_expr, group_pos, key_fn, var, target, hops, TEAM_IDS)
    head = "(f:Team)-[r1:LED_BY]->(c:Sponsor)<-[r2:FUNDED_BY]-(l:Project)<-[r3:IN_PROJECT]-(w:Task)"
    hops = _chain_hops(dept_ok=lambda d: d == "d2")
    golden = check_grouped(plain, head, "c.kind", 1, KEYS[1][1], "w", ("n", 3), hops, TEAM_IDS)
    assert golden == {"small": 3}


def test_node_and_edge_property_filters_apply_before_grouping(plain):
    head = (
        "(f:Team)-[r1:LED_BY]->(c:Dept {kind: 'big'})<-[r2:FUNDED_BY {share: 50}]-(l:Project)<-[r3:IN_PROJECT]-(w:Task)"
    )
    hops = _chain_hops(dept_ok=lambda d: ALL_KINDS[d] == "big", share=50)
    for group_pos, (key_expr, key_fn) in KEYS.items():
        for var, target in TARGETS:
            check_grouped(plain, head, key_expr, group_pos, key_fn, var, target, hops, TEAM_IDS)


def test_an_undirected_self_loop_hop(plain):
    head = "(f:Team)-[a:LED_BY]->(c:Dept)-[x:PARTNER]-(p:Dept)<-[b:FUNDED_BY]-(l:Project)"
    hops = [
        (_edges(LED_BY), "out", lambda d: True),
        (_edges(PARTNER), "both", lambda d: True),
        (_funded(), "in", lambda p: p in PROJECTS),
    ]
    keys = [
        ("f.region", lambda t: TEAMS[t]),
        ("c.kind", lambda d: ALL_KINDS[d]),
        ("p.kind", lambda d: ALL_KINDS[d]),
        ("l.id", lambda p: p),
    ]
    targets = [("f", ("n", 0)), ("c", ("n", 1)), ("p", ("n", 2)), ("l", ("n", 3))]
    targets += [("a", ("r", 0)), ("x", ("r", 1)), ("b", ("r", 2))]
    for group_pos, (key_expr, key_fn) in enumerate(keys):
        for var, target in targets:
            check_grouped(plain, head, key_expr, group_pos, key_fn, var, target, hops, TEAM_IDS)


def test_a_two_hop_chain_and_a_group_at_either_end(plain):
    head = "(f:Team)-[r1:LED_BY]->(c:Dept)<-[r2:FUNDED_BY]-(l:Project)"
    hops = _chain_hops()[:2]
    for group_pos, (key_expr, key_fn) in list(KEYS.items())[:3]:
        for var, target in (("f", ("n", 0)), ("c", ("n", 1)), ("l", ("n", 2)), ("r1", ("r", 0)), ("r2", ("r", 1))):
            check_grouped(plain, head, key_expr, group_pos, key_fn, var, target, hops, TEAM_IDS)


def test_the_answer_reads_the_graph_it_runs_on_not_the_plan(plain):
    q = f"MATCH {HEAD} RETURN c.kind AS k, count(DISTINCT w) AS n"
    assert run_grouped(plain, q)[0] == {"big": 4, "small": 3}
    plain.cypher("MATCH (w:Task {id: 'w6'}), (p:Project {id: 'p4'}) CREATE (w)-[:IN_PROJECT]->(p)").to_list()
    assert run_grouped(plain, q)[0] == {"big": 5, "small": 3}


def test_parameters_resolve_in_the_chain(plain):
    q = (
        "MATCH (f:Team {id: $fid})-[:LED_BY]->(c:Dept)<-[:FUNDED_BY]-(l:Project)<-[:IN_PROJECT]-(w:Task) "
        "RETURN c.kind AS k, count(DISTINCT w) AS n"
    )
    paths = enumerate_paths(["t2"], _chain_hops())
    assert any(op.startswith(CLAUSE) for op in _ops(plain, q, params={"fid": "t2"}))
    assert run_grouped(plain, q, params={"fid": "t2"})[0] == grouped_golden(paths, 1, KEYS[1][1], ("n", 3))


# ── shapes that stay on the matcher ──────────────────────────────────────

GROUPED_BAILS = [
    "MATCH {head} WHERE c.kind <> 'small' RETURN c.kind AS k, count(DISTINCT w) AS n",
    "MATCH {head} RETURN c.kind AS k, count(DISTINCT w.id) AS n",
    "MATCH {head} RETURN c.kind AS k, count(DISTINCT w) AS n, count(*) AS m",
    "MATCH {head} RETURN f.region + c.kind AS k, count(DISTINCT w) AS n",
    "MATCH {head} RETURN toUpper(c.kind) AS k, count(DISTINCT w) AS n",
    "MATCH (f:Team)-[:LED_BY]->(c:Dept)<-[r2:FUNDED_BY]-(l:Project)<-[:IN_PROJECT]-(w:Task) "
    "RETURN r2.share AS k, count(DISTINCT w) AS n",
    "MATCH {head} RETURN DISTINCT c.kind AS k, count(DISTINCT w) AS n",
    "MATCH {head} WITH c.kind AS k, count(DISTINCT w) AS n WHERE n > 3 RETURN k, n",
    "MATCH {head} RETURN c.kind AS k, count(DISTINCT w) AS n ORDER BY c.id",
    "MATCH {head} RETURN c.kind AS k, count(DISTINCT w) AS n ORDER BY c.kind",
    "MATCH {head} RETURN c.kind AS k, count(DISTINCT w) AS n ORDER BY n + 1",
    "MATCH (a:Dept)-[:PARTNER]->(b:Dept)-[:PARTNER]->(c:Dept) RETURN a.kind AS k, count(DISTINCT c) AS n",
    "MATCH (f:Team)-[:LED_BY]->(c:Dept) RETURN c.kind AS k, count(DISTINCT f) AS n",
    "MATCH (f:Team)-[:LED_BY]->(c:Dept)<-[:FUNDED_BY*1..2]-(l:Project) RETURN c.kind AS k, count(DISTINCT l) AS n",
    "MATCH (f:Team)-[:LED_BY]->(c:Dept)<-[]-(l:Project) RETURN c.kind AS k, count(DISTINCT l) AS n",
]


@pytest.mark.parametrize("shape", GROUPED_BAILS)
def test_the_unfusable_grouped_shapes_stay_on_the_matcher(plain, shape):
    q = shape.format(head=ANON_HEAD)
    assert not any(op.startswith(f"{CLAUSE} (k=") and "grouped" in op for op in _ops(plain, q)), q

    def canon(rows):
        return sorted(repr(sorted(row.items())) for row in rows)

    assert canon(_n(plain, q)) == canon(_n(plain, q, disabled_passes=[PASS])), q


# ── valid time: group position x target x context against the model ──────

ORG3 = "(e:Emp)-[r1:WORKS_IN]->(d:Dept)-[r2:AT_SITE]->(s:Site)-[r3:IN_REGION]->(r:Region)"
ORG_KEYS = [
    ("e.id", lambda i: i),
    ("d.title", lambda i: "Lab" if i == 3 else "Ops"),  # d1 and d2 share a title
    ("s.id", lambda i: i),
    ("r.id", lambda i: i),
]
ORG_TARGETS = [(v, ("n", j)) for j, v in enumerate(("e", "d", "s", "r"))] + [
    (v, ("r", i)) for i, v in enumerate(("r1", "r2", "r3"))
]


@pytest.mark.parametrize("context", CONTEXTS)
def test_every_group_and_target_matches_the_model_under_every_context(org, context):
    t = INSTANTS[CONTEXTS.index(context)] if "AS OF" in context else None
    paths = _model_paths(t, 3)
    for group_pos, (key_expr, key_fn) in enumerate(ORG_KEYS):
        for var, target in ORG_TARGETS:
            golden = grouped_golden(paths, group_pos, key_fn, target)
            q = f"{context}MATCH {ORG3} RETURN {key_expr} AS k, count(DISTINCT {var}) AS n"
            assert _fused(org, q), q
            assert run_grouped(org, q)[0] == golden, q


@pytest.mark.parametrize("context", ["", "FOR VALID_TIME ALL ", CONTEXTS[3], CONTEXTS[5]])
def test_the_fused_groups_equal_the_matchers_with_no_model(org, context):
    for key_expr in ("e.id", "d.title", "s.id", "r.id", "d"):
        for var in ("e", "d", "s", "r", "r1", "r2", "r3"):
            q = f"{context}MATCH {ORG3} RETURN {key_expr} AS k, count(DISTINCT {var}) AS n"
            fused = sorted(map(repr, _n(org, q)))
            assert fused == sorted(map(repr, _n(org, q, disabled_passes=[PASS]))), q


def test_the_valid_set_not_the_forward_set_decides_a_group(org):
    # Instants where the model's groups disagree: the grouped counts must move.
    seen = {
        t: tuple(sorted(grouped_golden(_model_paths(t, 3), 1, ORG_KEYS[1][1], ("n", 0)).items()))
        for t in INSTANTS + [None]
    }
    assert len(set(seen.values())) >= 4


def test_a_secondary_label_start_with_a_group_key(org):
    lead = "MATCH (e:Lead)-[:WORKS_IN]->(d:Dept)-[:AT_SITE]->(s:Site) RETURN d.title AS k, count(DISTINCT s) AS n"
    assert _n(org, f"FOR VALID_TIME AS OF date('2005-06-01') {lead}") == []
    assert _n(org, f"FOR VALID_TIME AS OF date('2007-06-15') {lead}") == [{"k": "Lab", "n": 1}]
    assert _n(org, f"FOR VALID_TIME AS OF date('2012-06-01') {lead}") == []
    assert _fused(org, f"FOR VALID_TIME AS OF date('2007-06-15') {lead}")


# ── the budget ───────────────────────────────────────────────────────────


def test_a_budget_stops_the_grouped_sweeps():
    graph = kglite.KnowledgeGraph()
    graph.cypher(
        "UNWIND range(1, 3000) AS i CREATE (:A {id: i})-[:R]->(:B {id: i, k: i % 7})-[:S]->(:C {id: i})"
    ).to_list()
    q = "MATCH (a:A)-[:R]->(b:B)-[:S]->(c:C) RETURN b.k AS k, count(DISTINCT c) AS n"
    assert _fused(graph, q)
    assert sum(row["n"] for row in _n(graph, q)) == 3000
    with pytest.raises(Exception, match="work"):
        graph.cypher(q, max_work_units=100).to_list()
