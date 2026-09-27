"""Execution under ``FOR VALID_TIME AS OF``: the guard's goldens.

Every guarded site answers as the plain query over only the elements valid
at the instant would: anchors and scans, untyped ``(n)`` and secondary
labels (a node passes only when valid under every declared label it
carries), both endpoints of a relationship, a relationship judged by the
declaration keyed on its own source type, id seeks among version nodes that
share an id, the counts and top-k operators the planner admits under a
guard, the timeless exit, open transactions and the plan cache. The
storage-mode parity set and the equivalence oracle are in
``test_valid_time_oracle.py``.
"""

from __future__ import annotations

import datetime as dt

import pytest

import kglite

NOT_YET = "not available under FOR VALID_TIME AS OF yet"


def at(date: str, body: str) -> str:
    return f"FOR VALID_TIME AS OF date('{date}') {body}"


def ids(graph, query, column=None, **kwargs):
    rows = graph.cypher(query, **kwargs).to_list()
    return sorted(row[column] if column else next(iter(row.values())) for row in rows)


def profile(graph, query, **kwargs):
    result = graph.cypher(f"PROFILE {query}", **kwargs)
    return result.to_list(), [step["clause"] for step in result.profile]


@pytest.fixture
def sodir():
    """Wells (declared, one closed in 2010, one carrying the declared
    secondary label `Pad` whose own interval opens in 2012), an undeclared
    `Field`, and `HAS_LICENSEE` keyed per source type: from a `Field` it is
    governed by `f_from`/`f_to` (closed), from anything else by the unkeyed
    `from`/`to` (half-open)."""
    graph = kglite.KnowledgeGraph()
    graph.cypher(
        "CREATE (w1:Well {id: 1, vf: date('2000-01-01'), vt: date('2010-01-01')}),"
        " (w2:Well {id: 2, vf: date('2005-01-01')}),"
        " (w3:Well {id: 3, vf: date('2001-01-01'), p_from: date('2012-01-01'), p_to: date('2030-01-01')}),"
        " (f:Field {id: 10}), (c:Company {id: 20}),"
        " (w1)-[:IN]->(f), (w2)-[:IN]->(f), (w3)-[:IN]->(f),"
        " (f)-[:HAS_LICENSEE {f_from: date('2000-01-01'), f_to: date('2004-12-31')}]->(c),"
        " (w2)-[:HAS_LICENSEE {from: date('2008-01-01'), to: date('2030-01-01')}]->(c)"
    ).to_list()
    graph.cypher("MATCH (w:Well {id: 3}) SET w:Pad").to_list()
    for declaration in (
        "{node: 'Well', from: 'vf', to: 'vt', convention: 'closed'}",
        "{node: 'Pad', from: 'p_from', to: 'p_to', convention: 'closed'}",
        "{relationship: 'HAS_LICENSEE', source_type: 'Field', from: 'f_from', to: 'f_to', convention: 'closed'}",
        "{relationship: 'HAS_LICENSEE', from: 'from', to: 'to', convention: 'half_open'}",
    ):
        graph.cypher(f"CALL db.temporal.declare({declaration})").to_list()
    return graph


def test_node_scan_and_anchored_hop(sodir):
    assert ids(sodir, at("2003-01-01", "MATCH (w:Well) RETURN w.id")) == [1]
    assert ids(sodir, at("2011-01-01", "MATCH (w:Well) RETURN w.id")) == [2]
    # Both endpoints of an anchored hop, the far one unnamed.
    body = "MATCH (f:Field)<-[:IN]-() RETURN count(*) AS c"
    assert ids(sodir, at("2006-01-01", body)) == [2]
    assert ids(sodir, at("2011-01-01", body)) == [1]
    assert ids(sodir, at("2013-01-01", body)) == [2]


def test_untyped_nodes_pass_every_declared_label_they_carry(sodir):
    """`MATCH (n)` and `MATCH (n:Well)` agree: well 3 is valid as a Well from
    2001 but as a Pad only from 2012, so it is invisible until then."""
    for date, want in [("2006-01-01", [1, 2, 10, 20]), ("2013-01-01", [2, 3, 10, 20])]:
        assert ids(sodir, at(date, "MATCH (n) RETURN n.id")) == want
        wells = [i for i in want if i < 10]
        assert ids(sodir, at(date, "MATCH (n:Well) RETURN n.id")) == wells
        assert ids(sodir, at(date, "MATCH (n:Pad) RETURN n.id")) == [i for i in wells if i == 3]


def test_a_relationship_is_keyed_on_its_own_source_type(sodir):
    body = "MATCH (a)-[:HAS_LICENSEE]->(c:Company) RETURN a.id"
    assert ids(sodir, at("2003-01-01", body)) == [10]  # the Field licence, keyed
    assert ids(sodir, at("2006-01-01", body)) == []
    assert ids(sodir, at("2009-01-01", body)) == [2]  # the Well licence, unkeyed
    # Walked from the target side, the rule still keys on the source.
    back = "MATCH (c:Company)<-[:HAS_LICENSEE]-(a) RETURN a.id"
    assert ids(sodir, at("2003-01-01", back)) == [10]
    assert ids(sodir, at("2009-01-01", back)) == [2]


def test_readmitted_fusions_answer_under_the_guard(sodir):
    date = "2011-01-01"
    rows, plan = profile(sodir, at(date, "MATCH (w:Well) RETURN count(w) AS c"))
    assert rows == [{"c": 1}] and plan == ["FusedCountTypedNode :Well"]
    rows, plan = profile(sodir, at(date, "MATCH (n) RETURN count(n) AS c"))
    assert rows == [{"c": 3}] and plan == ["FusedCountAll"]
    rows, _ = profile(sodir, at(date, "MATCH ()-[r:IN]->() RETURN count(*) AS c"))
    assert rows == [{"c": 1}]
    rows, plan = profile(sodir, at(date, "MATCH (w:Well) RETURN w.id AS id ORDER BY id DESC LIMIT 5"))
    assert rows == [{"id": 2}] and plan[0].startswith("FusedNodeScanTopK")
    rows, plan = profile(sodir, at(date, "MATCH (w:Well) RETURN w.id AS id, count(*) AS c"))
    assert rows == [{"id": 2, "c": 1}] and plan[0].startswith("FusedNodeScanAggregate")
    # The heap over matcher rows: a two-node pattern, so no node-scan fusion.
    rows = sodir.cypher(at("2013-01-01", "MATCH (w:Well)-[:IN]->(f) RETURN w.id AS id ORDER BY id LIMIT 1")).to_list()
    assert rows == [{"id": 2}]
    explained = [r["operation"] for r in sodir.cypher(f"EXPLAIN {at(date, 'MATCH (w:Well) RETURN count(w) AS c')}")]
    assert "OptimizerPass fuse_count_short_circuits" in explained
    # The fused aggregate that reads the store beside the matcher stays out.
    rows, plan = profile(sodir, at(date, "MATCH (w:Well)-[:IN]->(f) RETURN f.id AS f, count(w) AS c"))
    assert rows == [{"f": 10, "c": 1}] and "FusedMatchReturnAggregate" not in plan


def test_profile_rows_match_the_plain_context_rows(sodir):
    query = at("2006-01-01", "MATCH (w:Well)-[:IN]->(f:Field) RETURN w.id AS id")
    plain = sodir.cypher(query).to_list()
    result = sodir.cypher(f"PROFILE {query}")
    assert sorted(r["id"] for r in result.to_list()) == sorted(r["id"] for r in plain) == [1, 2]
    match_step = next(step for step in result.profile if step["clause"].startswith("Match"))
    assert match_step["rows_out"] == 2


def test_count_subquery_and_exists_are_guarded(sodir):
    body = "MATCH (f:Field) RETURN COUNT { (f)<-[:IN]-(w) } AS c"
    assert ids(sodir, at("2006-01-01", body)) == [2]
    assert ids(sodir, at("2011-01-01", body)) == [1]
    exists = "MATCH (c:Company) WHERE EXISTS { (c)<-[:HAS_LICENSEE]-() } RETURN c.id"
    assert ids(sodir, at("2006-01-01", exists)) == []
    assert ids(sodir, at("2009-01-01", exists)) == [20]


def test_an_element_id_anchor_on_an_invisible_node_matches_nothing(sodir):
    element = sodir.cypher("MATCH (w:Well {id: 1}) RETURN elementId(w) AS e").to_list()[0]["e"]
    body = "MATCH (w:Well) WHERE elementId(w) = $e RETURN w.id"
    assert ids(sodir, at("2003-01-01", body), params={"e": element}) == [1]
    assert ids(sodir, at("2011-01-01", body), params={"e": element}) == []


def test_a_transient_index_join_sees_only_valid_nodes(sodir):
    """80 driving rows probe the wells by a per-row key: the join the
    transient equality index serves unguarded runs through the matcher."""
    body = "UNWIND range(1, 80) AS i WITH 1 + i % 2 AS k MATCH (w:Well {id: k}) RETURN count(*) AS c"
    assert ids(sodir, body) == [80]
    assert ids(sodir, at("2006-01-01", body)) == [80]
    assert ids(sodir, at("2003-01-01", body)) == [40]
    assert ids(sodir, at("2011-01-01", body)) == [40]


def test_id_seeks_find_the_version_valid_at_the_instant():
    """A registry reuses one code across versions (the index keeps one node
    per (type, id)): the seek finds the version valid at the instant."""
    graph = kglite.KnowledgeGraph()
    graph.cypher(
        "CREATE (:Muni {id: 363, name: 'old', vf: date('1900-01-01'), vt: date('1999-12-31')}),"
        " (:Muni {id: 363, name: 'new', vf: date('2000-01-01')})"
    ).to_list()
    graph.cypher("CALL db.temporal.declare({node: 'Muni', from: 'vf', to: 'vt', convention: 'closed'})").to_list()
    for date, name in [("1950-06-01", "old"), ("2020-06-01", "new")]:
        for body in ("MATCH (m:Muni {id: 363}) RETURN m.name", "MATCH (m {id: 363}) RETURN m.name"):
            assert ids(graph, at(date, body)) == [name], (date, body)
        assert ids(graph, "MATCH (m:Muni {id: $i}) RETURN m.name", params={"i": 363}, valid_at=date) == [name]


@pytest.mark.parametrize(
    "convention,on_boundary",
    [("closed", ["Amsterdam-new", "Amsterdam-old"]), ("half_open", ["Amsterdam-new"])],
)
def test_the_registry_boundary_day_under_both_conventions(convention, on_boundary):
    """The Dutch-registry shape: a municipality's old version ends the day
    the new one starts. Closed keeps the old one on that day, half-open the
    new one; the day after, only the new one is valid either way."""
    graph = kglite.KnowledgeGraph()
    graph.cypher(
        "CREATE (:Gemeente {code: 363, name: 'Amsterdam-old', vf: date('1900-01-01'), vt: date('2020-01-01')}),"
        " (:Gemeente {code: 363, name: 'Amsterdam-new', vf: date('2020-01-01')})"
    ).to_list()
    graph.cypher(
        f"CALL db.temporal.declare({{node: 'Gemeente', from: 'vf', to: 'vt', convention: '{convention}'}})"
    ).to_list()
    body = "MATCH (g:Gemeente {code: 363}) RETURN g.name"
    # Closed: the old version holds its last day, and the new one has begun.
    assert ids(graph, at("2020-01-01", body)) == on_boundary
    assert ids(graph, at("2020-01-02", body)) == ["Amsterdam-new"]
    assert ids(graph, at("2019-12-31", body)) == ["Amsterdam-old"]


def test_a_wrong_typed_bound_raises():
    graph = kglite.KnowledgeGraph()
    graph.cypher("CREATE (:Site {id: 1, vf: date('2000-01-01'), vt: date('2010-01-01')})").to_list()
    graph.cypher("CALL db.temporal.declare({node: 'Site', from: 'vf', to: 'vt', convention: 'closed'})").to_list()
    graph.cypher("MATCH (s:Site) SET s.vt = 42").to_list()
    with pytest.raises(kglite.KgError, match=r"node '1'.*property 'vt'"):
        graph.cypher(at("2006-01-01", "MATCH (s:Site) RETURN s.id")).to_list()


@pytest.mark.parametrize(
    "body",
    [
        "MATCH (w:Well)-[:IN*1..2]-(x) RETURN x.id",
        "MATCH p = shortestPath((w:Well {id: 1})-[:IN*]-(f:Field)) RETURN length(p)",
        "MATCH (w:Well) WHERE EXISTS { (w)-[:IN*1..3]-() } RETURN w.id",
    ],
)
def test_var_length_and_shortest_path_are_not_available_yet(sodir, body):
    with pytest.raises(kglite.KgError, match=NOT_YET):
        sodir.cypher(at("2006-01-01", body)).to_list()
    # A fixed-length star lowers to explicit hops and runs.
    assert ids(sodir, at("2006-01-01", "MATCH (w:Well)-[:IN*1]->(f) RETURN w.id")) == [1, 2]


def test_the_plan_cache_never_carries_an_instant(sodir):
    body = "MATCH (w:Well) RETURN w.id"
    # One text, two parameter values: two answers.
    query = f"FOR VALID_TIME AS OF $t {body}"
    assert ids(sodir, query, params={"t": dt.date(2003, 1, 1)}) == [1]
    assert ids(sodir, query, params={"t": dt.date(2011, 1, 1)}) == [2]
    # Two literal instants: two plans, two answers, each repeatable.
    for _ in range(2):
        assert ids(sodir, at("2003-01-01", body)) == [1]
        assert ids(sodir, at("2011-01-01", body)) == [2]
    # A declaration between executions changes the answer.
    sodir.cypher("CALL db.temporal.undeclare({node: 'Well'})").to_list()
    assert ids(sodir, at("2011-01-01", body)) == [1, 2]


def test_an_open_transaction_sees_its_own_declaration_and_writes(sodir):
    body = "MATCH (f:Field) RETURN f.id"
    with sodir.begin() as tx:
        tx.cypher("MATCH (f:Field) SET f.vf = date('2015-01-01'), f.vt = date('2030-01-01')").to_list()
        tx.cypher("CALL db.temporal.declare({node: 'Field', from: 'vf', to: 'vt', convention: 'closed'})").to_list()
        assert sorted(r["f.id"] for r in tx.cypher(at("2011-01-01", body)).to_list()) == []
        assert sorted(r["f.id"] for r in tx.cypher(at("2016-01-01", body)).to_list()) == [10]
        # The committed graph has neither the write nor the declaration yet.
        assert ids(sodir, at("2011-01-01", body)) == [10]
        tx.commit()
    assert ids(sodir, at("2011-01-01", body)) == []
    assert ids(sodir, at("2016-01-01", body)) == [10]


def test_valid_at_runs_the_query_as_of_the_date(sodir):
    assert ids(sodir, "MATCH (w:Well) RETURN w.id", valid_at="2003-01-01") == [1]
    assert ids(sodir, "MATCH (w:Well) RETURN w.id", valid_at=dt.date(2011, 1, 1)) == [2]
    assert ids(sodir, "MATCH (w:Well) RETURN w.id", valid_at=dt.datetime(2011, 1, 1, 12)) == [2]


# ── The timeless exit ────────────────────────────────────────────────────────


@pytest.fixture
def current_only():
    """Every declared row valid today: open-ended, started in the past."""
    graph = kglite.KnowledgeGraph()
    graph.cypher(
        "CREATE (f:Field {id: 10}),"
        " (:Well {id: 1, vf: date('2000-01-01'), vt: date('2999-12-31')})"
        "-[:IN {since: date('2001-01-01'), until: date('2999-12-31')}]->(f),"
        " (:Well {id: 2, vf: date('2005-01-01')})-[:IN {since: date('2006-01-01')}]->(f)"
    ).to_list()
    graph.cypher("CALL db.temporal.declare({node: 'Well', from: 'vf', to: 'vt', convention: 'closed'})").to_list()
    graph.cypher(
        "CALL db.temporal.declare({relationship: 'IN', from: 'since', to: 'until', convention: 'closed'})"
    ).to_list()
    return graph


def test_as_of_today_on_a_current_state_graph_runs_the_plain_plan(current_only):
    body = "MATCH (w:Well)-[:IN]->(f) RETURN f.id AS f, count(w) AS c"
    plain_rows, plain_plan = profile(current_only, body)
    assert plain_plan == ["FusedMatchReturnAggregate"]
    for prefix in ("FOR VALID_TIME AS OF date() ", "FOR VALID_TIME AS OF $t "):
        rows, plan = profile(current_only, prefix + body, params={"t": dt.date.today()})
        assert rows == plain_rows == [{"f": 10, "c": 2}]
        assert plan == plain_plan, prefix
    # Before either well started the filter removes rows, so the guard runs.
    rows, plan = profile(current_only, at("2003-01-01", body))
    assert rows == [{"f": 10, "c": 1}] and "FusedMatchReturnAggregate" not in plan
    # EXPLAIN keeps the guarded plan: the instant is not in the plan.
    explained = [r["operation"] for r in current_only.cypher(f"EXPLAIN FOR VALID_TIME AS OF date() {body}")]
    assert explained[0].startswith("ValidTimeContext")
    assert not any(op.startswith("FusedMatchReturnAggregate") for op in explained)
