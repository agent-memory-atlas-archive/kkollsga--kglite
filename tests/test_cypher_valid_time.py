"""The statement prefix ``FOR VALID_TIME AS OF`` and ``cypher(valid_at=...)``.

In this build a context parses, lowers to a per-scope guard template and
renders under ``EXPLAIN``; executing a statement that carries one is refused.
Two properties matter as much as the feature: a query without a prefix plans
exactly as before, and a declaration alone changes no plan and no answer.
"""

from __future__ import annotations

import datetime

import pytest

import kglite
from tests import test_cypher_differential as _differential
from tests.test_cypher_differential import (
    DIFFERENTIAL_QUERIES,
    ORDERED_CASES,
    PASS_TRIGGER_CASES,
    _normalize,
)

# The corpus's fixtures, so an entry's fixture name resolves here too.
globals().update(
    {
        name: value
        for name, value in vars(_differential).items()
        if type(value).__name__ == "FixtureFunctionDefinition" or hasattr(value, "_pytestfixturefunction")
    }
)

NOT_YET = "not executable yet in this build"
AS_OF = "FOR VALID_TIME AS OF date('2006-01-01') "


@pytest.fixture
def wells():
    """`Well` and `LICENSED` are declared; `Field` holds the same bound
    properties undeclared."""
    graph = kglite.KnowledgeGraph()
    graph.cypher(
        "CREATE (w1:Well {id: 1, vf: date('2000-01-01'), vt: date('2010-01-01')}),"
        " (w2:Well {id: 2, vf: date('2005-01-01')}),"
        " (f:Field {id: 10, vf: date('2000-01-01')}),"
        " (w1)-[:LICENSED {vf: date('2000-01-01'), vt: date('2020-01-01')}]->(f)"
    ).to_list()
    graph.cypher("CALL db.temporal.declare({node: 'Well', from: 'vf', to: 'vt', convention: 'closed'})").to_list()
    graph.cypher(
        "CALL db.temporal.declare({relationship: 'LICENSED', from: 'vf', to: 'vt', convention: 'half_open'})"
    ).to_list()
    return graph


def _plan(graph, query, **kwargs):
    return [row["operation"] for row in graph.cypher(query, **kwargs).to_list()]


def test_explain_renders_the_context_in_either_order(wells):
    body = "MATCH (w:Well)-[l:LICENSED]->(f:Field) RETURN w.id, f.id"
    before = _plan(wells, f"EXPLAIN {AS_OF}{body}")
    after = _plan(wells, f"{AS_OF}EXPLAIN {body}")
    assert before == after
    assert before[0] == (
        "ValidTimeContext axis=VALID_TIME targets=(:Well [vf, vt] closed), "
        "[:LICENSED [vf, vt] half_open] instant: per execution"
    )
    # The rest of the plan is the plan of the same query without a context.
    plain = _plan(wells, f"EXPLAIN {body}")
    assert not any(op.startswith("ValidTimeContext") for op in plain)
    assert [op for op in before[1:] if not op.startswith("OptimizerPass")] == [
        op for op in plain if not op.startswith("OptimizerPass")
    ]


def test_the_template_lists_only_declared_targets(wells):
    """The declared-vs-undeclared twin: `Field` carries the same bound
    properties as `Well` but is not declared."""
    assert _plan(wells, f"EXPLAIN {AS_OF}MATCH (f:Field) RETURN f")[0] == (
        "ValidTimeContext axis=VALID_TIME targets=no declared targets instant: per execution"
    )
    assert (
        "targets=(:Well [vf, vt] closed) instant"
        in _plan(wells, f"EXPLAIN {AS_OF}MATCH (w:Well), (f:Field) RETURN w, f")[0]
    )


@pytest.mark.parametrize(
    "query",
    [
        f"{AS_OF}MATCH (w:Well) RETURN w.id",
        f"PROFILE {AS_OF}MATCH (w:Well) RETURN w.id",
        f"{AS_OF}PROFILE MATCH (w:Well) RETURN w.id",
        f"{AS_OF}CALL db.labels()",
        f"{AS_OF}MATCH (w:Well) RETURN w.id UNION MATCH (f:Field) RETURN f.id AS `w.id`",
    ],
)
def test_execution_is_refused_in_this_build(wells, query):
    with pytest.raises(kglite.KgError, match=NOT_YET):
        wells.cypher(query).to_list()


@pytest.mark.parametrize(
    "query,message",
    [
        ("FOR SYSTEM_TIME AS OF date('2006-01-01') MATCH (n) RETURN n", "axis SYSTEM_TIME is not supported"),
        (f"{AS_OF}MATCH (w:Well) SET w.x = 1", "cannot write"),
        (f"{AS_OF}CALL pagerank() YIELD node RETURN node", "procedure pagerank"),
    ],
)
def test_lowering_refusals_stop_explain_too(wells, query, message):
    for text in (query, f"EXPLAIN {query}"):
        with pytest.raises(kglite.KgError, match=message):
            wells.cypher(text).to_list()


def test_a_graph_without_declarations_refuses_a_context():
    graph = kglite.KnowledgeGraph()
    with pytest.raises(kglite.KgError, match="has none"):
        graph.cypher(f"EXPLAIN {AS_OF}RETURN 1 AS x").to_list()


def test_metadata_procedures_lower_under_a_context(wells):
    plan = _plan(wells, f"EXPLAIN {AS_OF}CALL db.labels()")
    assert plan[0].startswith("ValidTimeContext")


@pytest.mark.parametrize(
    "query,message",
    [
        (f"{AS_OF}{AS_OF}RETURN 1 AS x", "takes one FOR <axis> AS OF context"),
        (f"RETURN 1 AS x UNION {AS_OF}RETURN 2 AS x", "one context per statement"),
        (f"MATCH (w:Well) CALL {{ {AS_OF}MATCH (m) RETURN m }} RETURN w, m", "one context per statement"),
        ("FOR VALID_TIME AS OF w.vf MATCH (w:Well) RETURN w", "takes a constant instant"),
    ],
)
def test_prefix_syntax_errors(wells, query, message):
    with pytest.raises(kglite.CypherSyntaxError, match=message):
        wells.cypher(query)


@pytest.mark.parametrize("arm", ["EXPLAIN", "PROFILE"])
def test_union_arm_rejects_explain(arm):
    """Red before the fix: the arm parsed as a whole statement, so
    `RETURN 1 AS x UNION EXPLAIN RETURN 2 AS x` ran and returned both rows."""
    graph = kglite.KnowledgeGraph()
    with pytest.raises(kglite.CypherSyntaxError, match="must lead the statement"):
        graph.cypher(f"RETURN 1 AS x UNION {arm} RETURN 2 AS x")


@pytest.mark.parametrize(
    "valid_at,literal",
    [
        (datetime.date(2006, 1, 1), "date('2006-01-01')"),
        ("2006-01-01", "date('2006-01-01')"),
        (datetime.datetime(2006, 1, 1, 12, 30), "datetime('2006-01-01T12:30:00')"),
        ("2006-01-01T12:30:00", "datetime('2006-01-01T12:30:00')"),
    ],
)
def test_valid_at_writes_the_prefix(wells, valid_at, literal):
    body = "MATCH (w:Well) RETURN w.id"
    via_kwarg = _plan(wells, f"EXPLAIN {body}", valid_at=valid_at)
    by_hand = _plan(wells, f"FOR VALID_TIME AS OF {literal} EXPLAIN {body}")
    assert via_kwarg == by_hand
    assert via_kwarg[0].startswith("ValidTimeContext axis=VALID_TIME")
    with pytest.raises(kglite.KgError, match=NOT_YET):
        wells.cypher(body, valid_at=valid_at).to_list()


def test_valid_at_on_a_query_with_a_prefix_names_both(wells):
    with pytest.raises(ValueError, match=r"already has a FOR .* valid_at= adds another"):
        wells.cypher(f"{AS_OF}MATCH (w:Well) RETURN w", valid_at="2010-01-01")
    with pytest.raises(ValueError, match="valid_at"):
        wells.cypher("MATCH (w:Well) RETURN w", valid_at="next tuesday")


def test_valid_at_none_changes_nothing(wells):
    assert wells.cypher("MATCH (w:Well) RETURN w.id AS id ORDER BY id", valid_at=None).to_list() == [
        {"id": 1},
        {"id": 2},
    ]


_DIFFERENTIAL_TRIGGERS = [
    (pass_name, case_id) for pass_name, (source, case_id) in PASS_TRIGGER_CASES.items() if source == "differential"
]
_CASES = {entry[0]: entry for entry in DIFFERENTIAL_QUERIES}
# Mirrors planner/guard.rs; the Rust walk test holds that list against PASSES.
GUARD_SAFE_PASSES = {
    "optimize_nested_queries",
    "lower_fixed_var_length_hops",
    "rewrite_count_bound_var_to_star",
    "hoist_with_where",
    "push_where_into_match.1",
    "fold_or_to_in",
    "push_where_into_match.2",
    "extract_pushable_rel_predicates",
    "fold_pass_through_with",
    "fold_aliasing_with",
    "hoist_terminal_return_over_with_top_k",
    "narrow_unwind_source",
    "desugar_multi_match_return_aggregate",
    "reorder_match_clauses",
    "reorder_cyclic_pattern_edges",
    "optimize_pattern_start_node",
    "reorder_match_patterns",
    "push_distinct_into_match",
    "reorder_predicates_by_cost",
    "mark_disjoint_fixed_trails",
}


def _seed_declaration(graph):
    """A declared label nothing in the corpus names, so a context lowers
    (a graph with no declaration refuses one) without touching the query."""
    graph.cypher("CREATE (:ZzValidity {vf: date('2000-01-01'), vt: date('2001-01-01')})").to_list()
    graph.cypher("CALL db.temporal.declare({node: 'ZzValidity', from: 'vf', to: 'vt', convention: 'closed'})").to_list()


def test_guard_allow_list_matches_the_engine():
    assert GUARD_SAFE_PASSES <= set(kglite.cypher_pass_names())


@pytest.mark.parametrize("pass_name,case_id", _DIFFERENTIAL_TRIGGERS)
def test_a_guarded_plan_runs_only_allow_listed_passes(pass_name, case_id, request):
    """Walk every pass's trigger: under a context a denied pass never fires,
    and an allow-listed one still does."""
    _, fixture, query, params = _CASES[case_id]
    graph = request.getfixturevalue(fixture)
    _seed_declaration(graph)
    kwargs = {"params": params} if params else {}
    assert f"OptimizerPass {pass_name}" in _plan(graph, f"EXPLAIN {query}", **kwargs)
    guarded = _plan(graph, f"EXPLAIN {AS_OF}{query}", **kwargs)
    assert guarded[0].startswith("ValidTimeContext")
    fired = {op.removeprefix("OptimizerPass ") for op in guarded if op.startswith("OptimizerPass ")}
    assert fired <= GUARD_SAFE_PASSES, fired - GUARD_SAFE_PASSES
    if pass_name in GUARD_SAFE_PASSES:
        assert pass_name in fired, guarded


@pytest.mark.parametrize("name,fixture,query,params", DIFFERENTIAL_QUERIES, ids=[e[0] for e in DIFFERENTIAL_QUERIES])
def test_a_declaration_alone_changes_no_plan_and_no_answer(name, fixture, query, params, request):
    """The twin: the same seeded graph before and after a declaration gives a
    byte-identical EXPLAIN and equal rows for every prefix-less corpus query."""
    graph = request.getfixturevalue(fixture)
    graph.cypher("CREATE (:ZzValidity {vf: date('2000-01-01'), vt: date('2001-01-01')})").to_list()
    kwargs = {"params": params} if params else {}
    order = "ordered" if name in ORDERED_CASES else "bag"

    def observe():
        plan = graph.cypher(f"EXPLAIN {query}", **kwargs).to_list()
        rows = _normalize(graph.cypher(query, **kwargs).to_list(), order=order)
        return plan, rows

    undeclared = observe()
    graph.cypher("CALL db.temporal.declare({node: 'ZzValidity', from: 'vf', to: 'vt', convention: 'closed'})").to_list()
    declared = observe()
    assert declared[0] == undeclared[0]
    assert declared[1] == undeclared[1]
