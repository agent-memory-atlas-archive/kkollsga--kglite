"""``UNWIND <instants> AS d MATCH (x:T) WHERE valid_at(x, e)``: one scan, per-instant mask.

The fused join (`fuse_unwind_valid_at`) scans the pattern once and keeps the
matches whose node the endpoint index admits at each driving row's instant.
Every shape here must answer exactly as the same statement with that pass
disabled, hand-computed goldens pin the half-open and closed conventions on
their boundary days, and every condition the fused path declines (no index,
secondary labels, an instant that is not one, other bound names) must give the
unfused plan's rows and errors.
"""

from __future__ import annotations

import contextlib
import datetime as dt
import os
import tempfile

import pytest

import kglite

ALL = "FOR VALID_TIME ALL "
PASS = "fuse_unwind_valid_at"
FUSED = "FusedValidAtJoin"

# Half-open `Emp` versions (vf inclusive, vt exclusive; NULL is open):
#   1a [2000-01-01, 2005-01-01)   1b [2005-01-01, open)   2 [2001-01-01, 2003-01-01)
#   3 (open, 2002-01-01)          4 [2004-06-01, 2004-06-01) valid on no day
#   5 [2010-01-01, open)
# Closed `Staff` (vt inclusive): 11 [2000-01-01, 2005-01-01]   12 [2005-01-01, open)
EMP_DAYS = {
    "1999-12-31": 1,
    "2000-01-01": 2,
    "2001-01-01": 3,
    "2001-12-31": 3,
    "2002-01-01": 2,
    "2003-01-01": 1,
    "2004-06-01": 1,
    "2004-12-31": 1,
    "2005-01-01": 1,
    "2009-12-31": 1,
    "2010-01-01": 2,
}
STAFF_DAYS = {
    "1999-12-31": 0,
    "2000-01-01": 1,
    "2004-12-31": 1,
    "2005-01-01": 2,
    "2005-01-02": 1,
    "2030-01-01": 1,
}

EMP_ROWS = [
    ("1a", 1, "a", "2000-01-01", "2005-01-01", 10),
    ("1b", 1, "a", "2005-01-01", None, 10),
    ("2", 2, "b", "2001-01-01", "2003-01-01", 11),
    ("3", 3, "a", None, "2002-01-01", 10),
    ("4", 4, "b", "2004-06-01", "2004-06-01", 11),
    ("5", 5, "b", "2010-01-01", None, 11),
]
STAFF_ROWS = [("11", "x", "2000-01-01", "2005-01-01"), ("12", "y", "2005-01-01", None)]


def _date(text):
    return "null" if text is None else f"date('{text}')"


def build(graph: kglite.KnowledgeGraph) -> kglite.KnowledgeGraph:
    graph.cypher("CREATE (:Dept {id: 10, name: 'Ops'}), (:Dept {id: 11, name: 'Lab'})").to_list()
    for version, emp, role, start, end, dept in EMP_ROWS:
        graph.cypher(
            f"MATCH (d:Dept {{id: {dept}}}) CREATE (e:Emp {{id: '{version}', emp: {emp}, role: '{role}', "
            f"vf: {_date(start)}, vt: {_date(end)}}}), (e)-[:IN]->(d)"
        ).to_list()
    for sid, role, start, end in STAFF_ROWS:
        graph.cypher(f"CREATE (:Staff {{id: '{sid}', role: '{role}', sf: {_date(start)}, st: {_date(end)}}})").to_list()
    graph.cypher("CALL db.temporal.declare({node: 'Emp', from: 'vf', to: 'vt', convention: 'half_open'})").to_list()
    graph.cypher("CALL db.temporal.declare({node: 'Staff', from: 'sf', to: 'st', convention: 'closed'})").to_list()
    return graph


@contextlib.contextmanager
def _graph(mode: str):
    with tempfile.TemporaryDirectory() as directory:
        if mode == "mapped":
            yield build(kglite.KnowledgeGraph(storage="mapped"))
        elif mode == "disk":
            yield build(kglite.KnowledgeGraph(storage="disk", path=os.path.join(directory, "graph")))
        else:
            yield build(kglite.KnowledgeGraph())


@pytest.fixture(scope="module")
def org():
    with _graph("memory") as graph:
        yield graph


def norm(rows):
    return sorted(repr(sorted(row.items())) for row in rows)


def ops(graph, query, **kwargs):
    return [row["operation"] for row in graph.cypher(f"EXPLAIN {query}", **kwargs)]


def both(graph, query, **kwargs):
    fused = graph.cypher(query, **kwargs).to_list()
    plain = graph.cypher(query, disabled_passes=[PASS], **kwargs).to_list()
    assert norm(fused) == norm(plain), query
    return fused


COUNT = "UNWIND $ds AS d MATCH (e:Emp) WHERE valid_at(e, date(d)) RETURN d, count(*) AS n"
STAFF_COUNT = "UNWIND $ds AS d MATCH (s:Staff) WHERE valid_at(s, date(d)) RETURN d, count(*) AS n"


def counts(rows):
    return {row["d"]: row["n"] for row in rows}


# ---------------------------------------------------------------------------
# Goldens, per storage mode (Disk has no index and runs the unfused plan)
# ---------------------------------------------------------------------------


@pytest.mark.parametrize("mode", ["memory", "mapped", "disk"])
def test_goldens_half_open_and_closed(mode):
    with _graph(mode) as graph:
        emp = counts(graph.cypher(COUNT, params={"ds": list(EMP_DAYS)}).to_list())
        assert emp == {d: n for d, n in EMP_DAYS.items() if n > 0}
        staff = counts(graph.cypher(STAFF_COUNT, params={"ds": list(STAFF_DAYS)}).to_list())
        assert staff == {d: n for d, n in STAFF_DAYS.items() if n > 0}


@pytest.mark.parametrize("mode", ["memory", "mapped", "disk"])
def test_goldens_rows_and_hop(mode):
    rows = "UNWIND $ds AS d MATCH (e:Emp) WHERE valid_at(e, date(d)) RETURN d, e.id AS v ORDER BY d, v"
    hop = (
        "UNWIND $ds AS d MATCH (e:Emp)-[:IN]->(x:Dept) WHERE valid_at(e, date(d)) "
        "RETURN d, x.name AS dept, count(*) AS n ORDER BY d, dept"
    )
    ds = ["2002-01-01", "2005-01-01", "2010-01-01"]
    with _graph(mode) as graph:
        got = [(r["d"], r["v"]) for r in graph.cypher(rows, params={"ds": ds}).to_list()]
        assert got == [
            ("2002-01-01", "1a"),
            ("2002-01-01", "2"),
            ("2005-01-01", "1b"),
            ("2010-01-01", "1b"),
            ("2010-01-01", "5"),
        ]
        got = [(r["d"], r["dept"], r["n"]) for r in graph.cypher(hop, params={"ds": ds}).to_list()]
        assert got == [
            ("2002-01-01", "Lab", 1),
            ("2002-01-01", "Ops", 1),
            ("2005-01-01", "Ops", 1),
            ("2010-01-01", "Lab", 1),
            ("2010-01-01", "Ops", 1),
        ]


def test_duplicate_instants_count_each_occurrence(org):
    got = counts(org.cypher(COUNT, params={"ds": ["2001-01-01", "2001-01-01", "2010-01-01"]}).to_list())
    assert got == {"2001-01-01": 6, "2010-01-01": 2}
    rows = org.cypher(
        "UNWIND $ds AS d MATCH (e:Emp) WHERE valid_at(e, d) RETURN d, e.id AS v",
        params={"ds": ["2003-01-01", "2003-01-01"]},
    ).to_list()
    assert [(r["d"], r["v"]) for r in rows] == [("2003-01-01", "1a")] * 2


def test_an_instant_with_no_valid_node_leaves_no_group(org):
    got = counts(org.cypher(STAFF_COUNT, params={"ds": ["1999-12-31", "2000-01-01"]}).to_list())
    assert got == {"2000-01-01": 1}


def test_datetime_and_date_instants_agree_on_a_day(org):
    ds = [dt.date(2005, 1, 1), dt.datetime(2005, 1, 1, 12, 0, 0), "2005-01-01T00:00:00"]
    for d in ds:
        got = org.cypher("UNWIND $ds AS d MATCH (e:Emp) WHERE valid_at(e, d) RETURN count(e) AS n", params={"ds": [d]})
        assert got.to_list() == [{"n": 1}]


# ---------------------------------------------------------------------------
# Differential: the fused plan against the same statement without the pass
# ---------------------------------------------------------------------------

ALL_DAYS = sorted({*EMP_DAYS, *STAFF_DAYS, "1990-06-15", "2006-06-01", "2031-01-01"})

SHAPES = [
    "UNWIND $ds AS d MATCH (e:Emp) WHERE valid_at(e, date(d)) RETURN d, count(*) AS n",
    "UNWIND $ds AS d MATCH (e:Emp) WHERE valid_at(e, d) RETURN d, count(e) AS n",
    "UNWIND $ds AS d MATCH (e:Emp) WHERE valid_at(e, d) RETURN d AS day, count(*) AS total",
    "UNWIND $ds AS d MATCH (e:Emp) WHERE valid_at(e, d) RETURN d, e.id AS v",
    "UNWIND $ds AS d MATCH (e:Emp) WHERE valid_at(e, d) RETURN d, e.role AS role, count(*) AS n",
    "UNWIND $ds AS d MATCH (e:Emp) WHERE valid_at(e, d) RETURN count(*) AS n",
    "UNWIND $ds AS d MATCH (e:Emp) WHERE valid_at(e, d) RETURN DISTINCT d",
    "UNWIND $ds AS d MATCH (e:Emp) WHERE valid_at(e, d) RETURN d, count(DISTINCT e.emp) AS n",
    "UNWIND $ds AS d MATCH (e:Emp) WHERE valid_at(e, d) RETURN d, collect(e.id) AS ids",
    "UNWIND $ds AS d MATCH (e:Emp) WHERE valid_at(e, d) RETURN e.emp AS emp, count(*) AS n",
    "UNWIND $ds AS d MATCH (e:Emp) WHERE valid_at(e, d) AND e.role = 'a' RETURN d, count(*) AS n",
    "UNWIND $ds AS d MATCH (e:Emp) WHERE e.role = 'b' AND valid_at(e, d) RETURN d, e.id AS v",
    "UNWIND $ds AS d MATCH (e:Emp) WHERE valid_at(e, d) AND e.role <> e.id RETURN d, count(*) AS n",
    "UNWIND $ds AS d MATCH (e:Emp) WHERE e.emp > 1 AND valid_at(e, d) RETURN d, e.id AS v",
    "UNWIND $ds AS d MATCH (e:Emp) WHERE valid_at(e, d) AND size(e.id) = 1 RETURN d, count(*) AS n",
    "UNWIND $ds AS d MATCH (e:Emp {role: 'a'}) WHERE valid_at(e, d) RETURN d, count(*) AS n",
    "UNWIND $ds AS d MATCH (e:Emp) WHERE valid_at(e, d, 'vf', 'vt') RETURN d, count(*) AS n",
    "UNWIND $ds AS d MATCH (e:Emp) WHERE valid_at(e, d, 'vt', 'vf') RETURN d, count(*) AS n",
    "UNWIND $ds AS d MATCH (e:Emp) WHERE valid_at(e, d) = true RETURN d, count(*) AS n",
    "UNWIND $ds AS d MATCH (e:Emp) WHERE valid_at(e, d) AND valid_at(e, date('2005-01-01')) RETURN d, count(*) AS n",
    "UNWIND $ds AS d MATCH (e:Emp) WHERE valid_at(e, d) RETURN d, count(*) AS n ORDER BY n DESC, d LIMIT 3",
    "UNWIND $ds AS d MATCH (e:Emp) WHERE valid_at(e, d) WITH d, count(*) AS n WHERE n > 1 RETURN d, n",
    "UNWIND $ds AS d MATCH (e:Emp)-[:IN]->(x:Dept) WHERE valid_at(e, d) RETURN d, x.name AS dept, count(*) AS n",
    "UNWIND $ds AS d MATCH (e:Emp)-[r:IN]->(x:Dept) WHERE valid_at(e, d) RETURN d, count(r) AS n",
    "UNWIND $ds AS d MATCH (x:Dept)<-[:IN]-(e:Emp) WHERE valid_at(e, d) RETURN d, x.id AS dept, e.id AS v",
    "UNWIND $ds AS d MATCH (s:Staff) WHERE valid_at(s, d) RETURN d, count(*) AS n",
    "UNWIND $ds AS d MATCH (s:Staff) WHERE valid_at(s, d) RETURN d, s.id AS v",
    "UNWIND $ds AS d MATCH (e:Emp), (s:Staff) WHERE valid_at(e, d) RETURN d, count(*) AS n",
    "UNWIND $ds AS d MATCH (e:Emp) WHERE valid_at(e, d) RETURN d, count(*) AS n SKIP 1",
    "UNWIND range(0, 2) AS i UNWIND $ds AS d MATCH (e:Emp) WHERE valid_at(e, d) RETURN i, d, count(*) AS n",
    "WITH $ds AS ds UNWIND ds AS d MATCH (e:Emp) WHERE valid_at(e, d) RETURN d, count(*) AS n",
    "UNWIND $ds AS d MATCH (e:Emp) WHERE valid_at(e, d) RETURN d, count(*) AS n, count(e) AS m",
    "UNWIND $ds AS d MATCH (e:Emp) WHERE valid_at(e, d) RETURN d.year AS y, count(*) AS n",
]

FUSED_SHAPES = [s for s in SHAPES if "(e:Emp), (s:Staff)" not in s and "valid_at(e, d, 'vt', 'vf')" not in s]


@pytest.mark.parametrize("shape", SHAPES)
def test_fused_answers_as_the_unfused_plan(org, shape):
    ds = ALL_DAYS if "d.year" not in shape else [dt.date(2005, 1, 1), dt.date(2005, 6, 1)]
    both(org, shape, params={"ds": ds})
    both(org, shape, params={"ds": ds + ds[:3]})
    both(org, shape, params={"ds": []})


@pytest.mark.parametrize("shape", FUSED_SHAPES)
def test_the_trigger_shapes_run_the_fused_join(org, shape):
    names = ops(org, shape, params={"ds": ["2005-01-01"]})
    assert any(name.startswith(FUSED) for name in names), (shape, names)
    assert f"OptimizerPass {PASS}" in names


@pytest.mark.parametrize(
    "shape",
    [
        "UNWIND $ds AS d MATCH (e:Emp) WHERE valid_at(e, d) OR e.role = 'a' RETURN d, count(*) AS n",
        "UNWIND $ds AS d MATCH (e:Emp) WHERE NOT valid_at(e, d) RETURN d, count(*) AS n",
        "UNWIND $ds AS d OPTIONAL MATCH (e:Emp) WHERE valid_at(e, d) RETURN d, count(e) AS n",
        "UNWIND $ds AS d MATCH (e:Emp) WHERE valid_at(e, e.vf) RETURN d, count(*) AS n",
        "UNWIND $ds AS d MATCH (e:Emp) WHERE size(e.id) = 1 AND valid_at(e, d) RETURN d, count(*) AS n",
        "UNWIND $ds AS d MATCH (e:Emp) WHERE valid_at(e, date(d) + duration({days: rand()})) RETURN d, count(*) AS n",
    ],
)
def test_declined_shapes_keep_match_and_where_and_the_answer(org, shape):
    names = ops(org, shape, params={"ds": ["2005-01-01"]})
    assert not any(name.startswith(FUSED) for name in names), (shape, names)


def test_the_statement_echo_is_unchanged(org):
    ds = ["2005-01-01", "2010-01-01"]
    fused = org.cypher(COUNT, params={"ds": ds})
    plain = org.cypher(COUNT, params={"ds": ds}, disabled_passes=[PASS])
    keep = ("axis", "source", "instant", "targets", "route", "hidden")
    pick = lambda result: {k: result.diagnostics["temporal"][k] for k in keep}  # noqa: E731
    assert pick(fused) == pick(plain)
    assert pick(fused)["source"] == "skipped:valid_at"
    assert pick(fused)["instant"] == "all"


def test_a_statement_context_keeps_the_guarded_plan(org):
    query = f"FOR VALID_TIME AS OF date('2005-01-01') {COUNT}"
    assert not any(name.startswith(FUSED) for name in ops(org, query, params={"ds": ["2010-01-01"]}))
    both(org, f"FOR VALID_TIME ALL {COUNT}", params={"ds": ALL_DAYS})
    assert any(
        name.startswith(FUSED) for name in ops(org, f"FOR VALID_TIME ALL {COUNT}", params={"ds": ["2010-01-01"]})
    )


# ---------------------------------------------------------------------------
# Instants the fused path cannot take: the unfused plan's rows and errors
# ---------------------------------------------------------------------------


def _outcome(graph, query, **kwargs):
    try:
        return ("rows", norm(graph.cypher(query, **kwargs).to_list()))
    except Exception as error:  # noqa: BLE001
        return ("error", str(error))


@pytest.mark.parametrize(
    "ds",
    [
        [None],
        ["2005-01-01", None],
        [None, "2005-01-01"],
        ["2005-01-01", 5],
        ["not a date"],
        [{"y": 2005}],
        ["2005-01-01", "2005-13-45"],
    ],
)
def test_an_instant_that_is_not_one_answers_as_the_unfused_plan(org, ds):
    for shape in (
        COUNT.replace("date(d)", "d"),
        "UNWIND $ds AS d MATCH (e:Emp) WHERE valid_at(e, d) RETURN d, e.id AS v",
    ):
        fused = _outcome(org, shape, params={"ds": ds})
        plain = _outcome(org, shape, params={"ds": ds}, disabled_passes=[PASS])
        assert fused == plain, (ds, shape, fused, plain)
    assert _outcome(org, COUNT.replace("date(d)", "d"), params={"ds": [None]})[0] == "error"


def test_no_candidate_means_the_instant_is_never_read():
    graph = build(kglite.KnowledgeGraph())
    q = "UNWIND [null] AS d MATCH (e:Emp {id: 'nobody'}) WHERE valid_at(e, d) RETURN d, count(*) AS n"
    assert _outcome(graph, q) == _outcome(graph, q, disabled_passes=[PASS]) == ("rows", [])


def test_named_bounds_other_than_the_declared_pair_read_closed():
    graph = build(kglite.KnowledgeGraph())
    q = (
        "UNWIND ['2005-01-01', '2010-01-01'] AS d MATCH (e:Emp) "
        "WHERE valid_at(e, d, 'vf', 'vt') RETURN d, count(*) AS n"
    )
    swapped = q.replace("'vf', 'vt'", "'vt', 'vf'")
    assert both(graph, q) == [{"d": "2005-01-01", "n": 1}, {"d": "2010-01-01", "n": 2}]
    both(graph, swapped)


def test_an_undeclared_type_raises_the_unfused_error():
    graph = kglite.KnowledgeGraph()
    graph.cypher("CREATE (:Emp {id: 1, vf: date('2000-01-01')})").to_list()
    q = "UNWIND ['2005-01-01'] AS d MATCH (e:Emp) WHERE valid_at(e, d) RETURN d, count(*) AS n"
    fused = _outcome(graph, q)
    assert fused == _outcome(graph, q, disabled_passes=[PASS])
    assert fused[0] == "error" and "no declared validity interval" in fused[1]


# ---------------------------------------------------------------------------
# Environments that decline the index: still the unfused plan's answer
# ---------------------------------------------------------------------------


def test_a_byte_cap_that_refuses_the_index_runs_the_unfused_plan(monkeypatch):
    monkeypatch.setenv("KGLITE_TEMPORAL_INDEX_MAX_BYTES", "1")
    graph = build(kglite.KnowledgeGraph())
    ds = list(EMP_DAYS)
    got = counts(graph.cypher(COUNT, params={"ds": ds}).to_list())
    assert got == {d: n for d, n in EMP_DAYS.items() if n > 0}
    both(graph, SHAPES[3], params={"ds": ds})


def test_secondary_labels_run_the_unfused_plan():
    graph = build(kglite.KnowledgeGraph())
    graph.cypher("MATCH (e:Emp {id: '1a'}) SET e:Lead").to_list()
    graph.cypher("CALL db.temporal.declare({node: 'Lead', from: 'vf', to: 'vt', convention: 'closed'})").to_list()
    ds = list(EMP_DAYS)
    for shape in (COUNT, SHAPES[3]):
        both(graph, shape, params={"ds": ds})
    got = counts(graph.cypher(COUNT, params={"ds": ds}).to_list())
    plain = counts(graph.cypher(COUNT, params={"ds": ds}, disabled_passes=[PASS]).to_list())
    assert got == plain


# ---------------------------------------------------------------------------
# Plan-cache reuse: the plan holds neither the instants nor the graph's data
# ---------------------------------------------------------------------------


def test_one_plan_serves_different_parameter_lists_and_writes():
    graph = build(kglite.KnowledgeGraph())
    assert counts(graph.cypher(COUNT, params={"ds": ["2001-01-01"]}).to_list()) == {"2001-01-01": 3}
    assert counts(graph.cypher(COUNT, params={"ds": ["2010-01-01", "2002-01-01"]}).to_list()) == {
        "2010-01-01": 2,
        "2002-01-01": 2,
    }
    graph.cypher("CREATE (:Emp {id: '6', emp: 6, role: 'a', vf: date('2001-01-01'), vt: date('2002-01-01')})").to_list()
    graph.cypher("MATCH (e:Emp {id: '2'}) SET e.vt = date('2001-06-01')").to_list()
    assert counts(graph.cypher(COUNT, params={"ds": ["2001-01-01", "2001-07-01"]}).to_list()) == {
        "2001-01-01": 4,
        "2001-07-01": 3,
    }
    graph.cypher("CALL db.temporal.undeclare({node: 'Emp'})").to_list()
    graph.cypher("CALL db.temporal.declare({node: 'Emp', from: 'vf', to: 'vt', convention: 'closed'})").to_list()
    # Closed now: 1a also holds on its end day, 2 ended 2001-06-01 inclusive.
    assert counts(graph.cypher(COUNT, params={"ds": ["2001-06-01", "2005-01-01"]}).to_list()) == {
        "2001-06-01": 4,
        "2005-01-01": 2,
    }
    both(graph, COUNT, params={"ds": ALL_DAYS})


def test_a_call_body_inside_a_writing_statement_sees_what_the_statement_wrote():
    graph = build(kglite.KnowledgeGraph())
    query = (
        "CREATE (:Emp {id: 'new', emp: 7, role: 'a', vf: date('2001-01-01'), vt: date('2002-01-01')}) "
        "WITH 1 AS one CALL { UNWIND $ds AS d MATCH (e:Emp) WHERE valid_at(e, d) "
        "RETURN d AS day, count(*) AS n } RETURN day, n ORDER BY day"
    )
    ds = ["2001-06-01", "2005-01-01"]
    fused = _outcome(graph, query, params={"ds": ds})
    twin = build(kglite.KnowledgeGraph())
    plain = _outcome(twin, query, params={"ds": ds}, disabled_passes=[PASS])
    assert fused == plain
    assert fused[0] == "rows" and "'n', 4" in fused[1][0]


# ---------------------------------------------------------------------------
# The WHERE folded into a MATCH that joins incoming rows
# ---------------------------------------------------------------------------


def test_a_where_after_a_joining_match_filters_each_drivers_matches(org):
    q = (
        ALL + "UNWIND [1, 2, 3] AS i MATCH (e:Emp) WHERE e.emp = i AND e.role <> 'zzz' "
        "RETURN i, e.id AS v ORDER BY i, v"
    )
    got = [(r["i"], r["v"]) for r in org.cypher(q).to_list()]
    assert got == [(1, "1a"), (1, "1b"), (2, "2"), (3, "3")]
    q = "FOR VALID_TIME ALL UNWIND [0, 5] AS i MATCH (e:Emp) WHERE size(e.id) > i RETURN i, count(*) AS n ORDER BY i"
    assert [(r["i"], r["n"]) for r in org.cypher(q).to_list()] == [(0, 6)]


def test_a_folded_where_reads_the_driving_row_and_keeps_null_semantics(org):
    q = (
        "FOR VALID_TIME ALL UNWIND [{k: 'a'}, {k: null}, {k: 'b'}] AS m MATCH (e:Emp) "
        "WHERE e.role = m.k AND e.emp < 3 RETURN m.k AS k, count(*) AS n ORDER BY k"
    )
    got = [(r["k"], r["n"]) for r in org.cypher(q).to_list()]
    assert got == [("a", 2), ("b", 1)]


def test_a_folded_where_error_still_raises(org):
    q = "FOR VALID_TIME ALL UNWIND [1, 0] AS i MATCH (e:Emp) WHERE 10 / i > e.emp RETURN count(*) AS n"
    outcome = _outcome(org, q)
    assert outcome == _outcome(org, q, disable_optimizer=True)


def test_a_folded_where_with_a_row_cap_and_distinct_keeps_its_answer(org):
    for q in (
        ALL + "UNWIND [1, 2] AS i MATCH (e:Emp) WHERE e.emp >= i RETURN i, e.id AS v ORDER BY i, v LIMIT 4",
        ALL + "UNWIND [1, 2] AS i MATCH (e:Emp) WHERE e.emp >= i RETURN DISTINCT e.emp AS emp ORDER BY emp",
        ALL + "UNWIND [1, 2] AS i MATCH (e:Emp) WHERE e.emp >= i RETURN count(DISTINCT e) AS n",
    ):
        assert norm(org.cypher(q).to_list()) == norm(org.cypher(q, disable_optimizer=True).to_list()), q
