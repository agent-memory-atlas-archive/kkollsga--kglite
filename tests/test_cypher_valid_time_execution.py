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
    # The fused aggregate masks its group nodes and counts through the guarded
    # per-node counters.
    rows, plan = profile(sodir, at(date, "MATCH (w:Well)-[:IN]->(f) RETURN f.id AS f, count(w) AS c"))
    assert rows == [{"f": 10, "c": 1}] and "FusedMatchReturnAggregate" in plan


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


def _storage(storage, tmp_path):
    if storage == "disk":
        return kglite.KnowledgeGraph(storage="disk", path=str(tmp_path / "graph"))
    return kglite.KnowledgeGraph(storage=storage) if storage != "memory" else kglite.KnowledgeGraph()


def _legacy_write(graph, label, node_id, **props):
    """Write bounds the write check refuses, through the one writer it does
    not judge (a fluent ``update()``) — the rows a graph saved by an earlier
    version, which accepted such writes, can hold."""
    return graph.select(label, temporal=False).where({"id": node_id}).update(props)["graph"]


@pytest.mark.parametrize("storage", ["memory", "mapped", "disk"])
def test_an_id_seek_returns_the_last_visible_version_in_node_order(storage, tmp_path):
    """Two of three versions sharing an id are valid: every mode returns the
    last in the type's node order (the id index's own choice), whichever node
    the index holds; an id whose only other version has an unreadable bound
    does not raise from a seek, though a scan that reads it does."""
    graph = _storage(storage, tmp_path)
    graph.cypher(
        "CREATE (:M {id: 1, name: 'a', vf: date('2000-01-01')}),"
        " (:M {id: 1, name: 'b', vf: date('2000-01-01')}),"
        " (:M {id: 1, name: 'c', vf: date('2030-01-01'), vt: date('2040-01-01')}),"
        " (:M {id: 363, name: 'old', vf: date('1900-01-01'), vt: date('1999-12-31')}),"
        " (:M {id: 363, name: 'new', vf: date('2000-01-01')}),"
        " (:M {id: 999, name: 'bad', vf: date('1900-01-01')})"
    ).to_list()
    graph.cypher("CALL db.temporal.declare({node: 'M', from: 'vf', to: 'vt', convention: 'closed'})").to_list()
    graph = _legacy_write(graph, "M", 999, vt=42)
    for body in (
        "MATCH (m:M {id: 1}) RETURN m.name",
        "MATCH (m {id: 1}) RETURN m.name",
        "MATCH (m:M) WHERE m.id IN [1] RETURN m.name",
        "UNWIND [1] AS x MATCH (m:M {id: x}) RETURN m.name",
    ):
        assert ids(graph, at("2020-01-01", body)) == ["b"], (storage, body)
        assert ids(graph, at("2035-01-01", body)) == ["c"], (storage, body)
    assert ids(graph, at("1950-01-01", "MATCH (m:M {id: 363}) RETURN m.name")) == ["old"]
    with pytest.raises(kglite.KgError, match=r"node '999'"):
        graph.cypher(at("1950-01-01", "MATCH (m:M) RETURN m.name")).to_list()


@pytest.mark.parametrize("storage", ["memory", "mapped", "disk"])
@pytest.mark.parametrize("loaded", ["old", "new"])
def test_an_id_seek_finds_versions_whose_ids_differ_in_numeric_kind(storage, loaded, tmp_path):
    """One version loaded from a DataFrame (its integer id column is stored
    as the loader's compact kind), the other written by Cypher `CREATE` (a
    plain integer): the seek finds whichever is valid, spelled as a literal
    or as a loaded id."""
    import pandas as pd

    graph = _storage(storage, tmp_path)
    graph.add_nodes(pd.DataFrame({"id": [1]}), "A", "id")
    versions = {"old": ("2000-01-01", "2009-12-31"), "new": ("2010-01-01", None)}
    vf, vt = versions[loaded]
    frame = {"id": [1], "name": [loaded], "vf": [pd.Timestamp(vf)], "vt": [pd.Timestamp(vt) if vt else None]}
    graph.add_nodes(pd.DataFrame(frame), "M", "id", "name")
    created = "new" if loaded == "old" else "old"
    vf, vt = versions[created]
    end = f", vt: date('{vt}')" if vt else ""
    graph.cypher(f"CREATE (:M {{id: 1, name: '{created}', vf: date('{vf}'){end}}})").to_list()
    graph.cypher("CALL db.temporal.declare({node: 'M', from: 'vf', to: 'vt', convention: 'closed'})").to_list()
    for body in (
        "MATCH (m:M {id: 1}) RETURN m.name",
        "MATCH (m:M {id: 1.0}) RETURN m.name",
        "MATCH (a:A {id: 1}) MATCH (m:M {id: a.id}) RETURN m.name",
        "UNWIND [1] AS x MATCH (m:M {id: x}) RETURN m.name",
    ):
        assert ids(graph, at("2005-01-01", body)) == ["old"], (storage, body)
        assert ids(graph, at("2020-01-01", body)) == ["new"], (storage, body)


def test_a_reopened_disk_graph_finds_a_float_id_by_any_numeric_spelling(tmp_path):
    """A disk graph's saved id index answers `{id: 1}` for a node stored with
    id `1.0`, as the in-memory index does, after the graph is reopened."""
    path = str(tmp_path / "graph")
    graph = kglite.KnowledgeGraph(storage="disk", path=path)
    graph.cypher("CREATE (:G {id: 1.0, name: 'f'}), (:G {id: 'x', name: 'x'})").to_list()
    assert ids(graph, "MATCH (n:G {id: 1}) RETURN n.name") == ["f"]
    graph.save()
    reopened = kglite.load(path)
    for spelling in ("1", "1.0"):
        assert ids(reopened, f"MATCH (n:G {{id: {spelling}}}) RETURN n.name") == ["f"], spelling


@pytest.mark.parametrize("storage", ["memory", "mapped", "disk"])
def test_an_id_seek_after_a_reload_follows_the_reloaded_node_order(storage, tmp_path):
    """A version created into a slot a delete freed sits last in node order
    until a reload orders the type by slot. The seek follows the node-order
    rule on each side of the reload — not creation order — and agrees with
    the unprefixed seek."""
    graph = _storage(storage, tmp_path)
    graph.cypher("CREATE (:X {id: 0})").to_list()
    graph.cypher("CREATE (:M {id: 1, name: 'a', vf: date('2000-01-01'), vt: date('2099-01-01')})").to_list()
    graph.cypher("MATCH (x:X) DETACH DELETE x").to_list()
    graph.cypher("CREATE (:M {id: 1, name: 'b', vf: date('2000-01-01')})").to_list()
    graph.cypher("CALL db.temporal.declare({node: 'M', from: 'vf', to: 'vt', convention: 'closed'})").to_list()

    def seek_and_order(g):
        order = [row["n"] for row in g.cypher("MATCH (m:M) RETURN m.name AS n").to_list()]
        seek = ids(g, at("2020-01-01", "MATCH (m:M {id: 1}) RETURN m.name"))
        assert ids(g, "MATCH (m:M {id: 1}) RETURN m.name") == seek
        return order, seek

    before, seek = seek_and_order(graph)
    assert seek == [before[-1]] == ["b"]
    if storage == "disk":
        graph.save()
        reloaded = kglite.load(str(tmp_path / "graph"))
    else:
        graph.save(str(tmp_path / "g.kgl"))
        reloaded = kglite.load(str(tmp_path / "g.kgl"))
    after, seek = seek_and_order(reloaded)
    assert after == list(reversed(before)), "premise: the reload orders the type by slot"
    assert seek == [after[-1]] == ["a"]


@pytest.mark.parametrize(
    "body",
    [
        "MATCH (w:Well {id: $k}) RETURN w.id",
        "MATCH (w:Well {id: $k}) RETURN count(w)",
        "MATCH (w:Well {id: $k}) RETURN w.id ORDER BY w.id LIMIT 1",
        "MATCH (w:Well {id: $k})-[:IN]->(f) RETURN f.id",
        "MATCH (f:Field) OPTIONAL MATCH (w:Well {id: $k})-[:IN]->(f) RETURN w.id",
        "MATCH (f:Field) RETURN COUNT { (w:Well {id: $k})-[:IN]->(f) }",
        "MATCH (f:Field) WHERE EXISTS { (:Well {id: $k})-[:IN]->(f) } RETURN f.id",
        "MATCH (f:Field) RETURN [(w:Well {id: $k})-[:IN]->(f) | w.id]",
        "MATCH (w:Well {id: $k})-[:IN*1..2]-(x:Field) RETURN x.id",
        "MATCH p = shortestPath((w:Well {id: $k})-[*]-(c:Company)) RETURN length(p)",
        "MATCH (a:Field) MATCH (w:Well {id: $k}) RETURN w.id",
    ],
)
def test_an_inline_map_expression_answers_as_its_value_under_a_context(sodir, body):
    """A statement under a context keeps its inline-map expressions (they are
    not folded at plan time without the filter): each shape answers as the
    same statement with the value written as a literal."""
    for date in ("2003-01-01", "2011-01-01"):
        for k in (1, 2):
            want = sodir.cypher(at(date, body.replace("$k", str(k)))).to_list()
            got = sodir.cypher(at(date, body.replace("$k", f"{k - 1} + 1"))).to_list()
            assert got == want, (date, k, body)


def test_a_count_in_an_inline_map_counts_only_valid_nodes(sodir):
    # At 2011 one Well is valid (Well 1 closed in 2010, the Pad carrier opens
    # in 2012), so the count is 1 and the company id 20; unguarded it is 3.
    body = "MATCH (c:Company {id: COUNT { (:Well) } + 19}) RETURN c.id"
    assert ids(sodir, at("2011-01-01", body)) == [20]
    assert ids(sodir, body) == []
    with pytest.raises(kglite.KgError, match=r"degree\(\)"):
        sodir.cypher(at("2011-01-01", "MATCH (f:Field) MATCH (c:Company {id: degree(f) + 17}) RETURN c.id")).to_list()


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
    graph = _legacy_write(graph, "Site", 1, vt=42)
    with pytest.raises(kglite.KgError, match=r"node '1'.*property 'vt'"):
        graph.cypher(at("2006-01-01", "MATCH (s:Site) RETURN s.id")).to_list()


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


# ── Variable-length relationships, OPTIONAL MATCH, subqueries, shortestPath ──


@pytest.fixture
def network():
    """Stops on a declared `LINK` network. Stop 2 closes in 2005, the direct
    1→3 link runs 2000–2005, and 4→5 has two parallel links — `xy1`
    (2000–2005) and `xy2` (from 2006). At 2008 the only valid route from 1
    to 3 is 1→4→5→3 over `xy2`; unguarded, 1→3 is one hop."""
    graph = kglite.KnowledgeGraph()
    graph.cypher(
        "CREATE (s1:Stop {id: 1}), (s2:Stop {id: 2, vf: date('2000-01-01'), vt: date('2005-01-01')}),"
        " (s3:Stop {id: 3}), (s4:Stop {id: 4}), (s5:Stop {id: 5}),"
        " (s1)-[:LINK {k: 'a2'}]->(s2), (s2)-[:LINK {k: '2b'}]->(s3),"
        " (s1)-[:LINK {k: 'ab', since: date('2000-01-01'), until: date('2005-01-01')}]->(s3),"
        " (s1)-[:LINK {k: 'ax'}]->(s4),"
        " (s4)-[:LINK {k: 'xy1', since: date('2000-01-01'), until: date('2005-01-01')}]->(s5),"
        " (s4)-[:LINK {k: 'xy2', since: date('2006-01-01')}]->(s5),"
        " (s5)-[:LINK {k: 'yb'}]->(s3)"
    ).to_list()
    graph.cypher("CALL db.temporal.declare({node: 'Stop', from: 'vf', to: 'vt', convention: 'closed'})").to_list()
    graph.cypher(
        "CALL db.temporal.declare({relationship: 'LINK', from: 'since', to: 'until', convention: 'half_open'})"
    ).to_list()
    return graph


STREAMING_SHAPES = [
    "MATCH (:Stop {id: 1})-[:LINK*1..3]->(t) RETURN count(DISTINCT t) AS c",
    "MATCH (s:Stop)-[:LINK]->(t) RETURN s.id AS s, count(t) AS c",
    "MATCH (s:Stop)-[r:LINK]->(t) RETURN count(DISTINCT t.id) AS d, count(*) AS c, count(r) AS r",
    "MATCH (s:Stop)-[:LINK]->(t) WITH s, count(t) AS c WHERE c > 0 RETURN s.id AS s, c",
    "MATCH (s:Stop)-[:LINK]->(t) RETURN s.id AS s, count(*) AS c ORDER BY s DESC LIMIT 2",
    "MATCH (s:Stop)-[:LINK]->(t) RETURN min(t.id) AS lo, max(t.id) AS hi, sum(t.id) AS s, avg(t.id) AS a",
    "MATCH (s:Stop)-[:LINK]->(t) RETURN s.id AS s, sum(COUNT { (t)-[:LINK]->() }) AS n",
]


@pytest.mark.parametrize("shape", STREAMING_SHAPES)
def test_every_read_handle_answers_the_streaming_shapes_as_the_eager_path(network, shape):
    """The streaming aggregate pipeline runs on a frozen view, a session and
    a transaction as on the live graph; each answers as the eager path
    (`streaming=False`), under a context and without one."""
    ordered = "ORDER BY" in shape
    for body in (at("2008-01-01", shape), shape):

        def rows(result, body=body):
            values = [tuple(sorted(row.items())) for row in result.to_list()]
            return values if ordered else sorted(values)

        eager = rows(network.cypher(body, streaming=False))
        session = network.session()
        tx = network.begin_read()
        handles = {
            "live": network.cypher(body),
            "frozen": network.freeze().cypher(body),
            "session": session.cypher(body),
            "session snapshot": session.snapshot().cypher(body),
            "transaction": tx.cypher(body),
        }
        if body == shape:
            handles["frozen at 2008"] = network.freeze(valid_at="2008-01-01").cypher(shape)
            eager_at = rows(network.cypher(at("2008-01-01", shape), streaming=False))
            assert rows(handles.pop("frozen at 2008")) == eager_at
        for name, result in handles.items():
            assert rows(result) == eager, (name, body)


def _explained(graph, query):
    return [row["operation"] for row in graph.cypher(f"EXPLAIN {query}").to_list()]


def test_var_length_paths_run_only_through_valid_elements(network):
    count = "MATCH (:Stop {{id: 1}})-[:LINK*1..{n}]->(:Stop {{id: 3}}) RETURN count(*) AS c"
    # Unguarded: 1→3, 1→2→3, and 1→4→5→3 once per parallel 4→5 link.
    assert ids(network, count.format(n=2)) == [2]
    assert ids(network, count.format(n=3)) == [4]
    # The direct link is closed and stop 2 is gone: nothing within two hops.
    assert ids(network, at("2008-01-01", count.format(n=2))) == [0]
    assert ids(network, at("2008-01-01", count.format(n=3))) == [1]
    # A bound relationship list holds only the valid relationships.
    body = "MATCH (:Stop {id: 1})-[r:LINK*1..3]->(:Stop {id: 3}) RETURN [x IN r | x.k] AS ks"
    assert ids(network, at("2008-01-01", body)) == [["ax", "xy2", "yb"]]
    assert ids(network, at("2003-01-01", body)) == [["a2", "2b"], ["ab"], ["ax", "xy1", "yb"]]
    # A fixed-length star is written out as hops and runs the same way.
    assert ids(network, at("2008-01-01", "MATCH (:Stop {id: 1})-[:LINK*1]->(t) RETURN t.id")) == [4]


@pytest.mark.parametrize(
    "body",
    [
        # The distance frontier (the planner marks the segment trail-free).
        "MATCH (:Stop {id: 1})-[:LINK*1..2]->(t) RETURN count(DISTINCT t) AS c",
        # The trail expansion (a named path needs the relationships).
        "MATCH p = (:Stop {id: 1})-[:LINK*1..2]->(t) RETURN count(DISTINCT t) AS c",
    ],
)
def test_an_invisible_intermediate_node_or_relationship_breaks_the_path(network, body):
    """Within two hops of stop 1 at 2008: 4 and 5 only — 2 is invisible, 3
    is reached over the closed direct link or through 2."""
    assert ids(network, body) == [4]
    assert ids(network, at("2008-01-01", body)) == [2]


def test_the_distance_frontier_runs_under_a_context(network):
    body = "MATCH (:Stop {id: 1})-[:LINK*1..2]->(t) RETURN count(DISTINCT t) AS c"
    assert "OptimizerPass mark_fast_var_length_paths" in _explained(network, at("2008-01-01", body))


@pytest.mark.parametrize("path", ["", "p = "])
def test_an_undirected_closed_trail_needs_valid_relationships(network, path):
    """Stop 4 reaches itself over the two parallel 4–5 links; at 2008 one
    of them is closed, so 4 is no longer its own two-hop neighbour."""
    body = f"MATCH {path}(s:Stop {{id: 4}})-[:LINK*1..2]-(t:Stop) RETURN DISTINCT t.id AS t"
    assert ids(network, body) == [1, 2, 3, 4, 5]
    assert ids(network, at("2008-01-01", body)) == [1, 3, 5]


def test_exists_with_a_var_length_pattern_is_guarded(network):
    body = "MATCH (s:Stop) WHERE EXISTS { (s)-[:LINK*2..2]->(:Stop {id: 3}) } RETURN s.id"
    assert ids(network, body) == [1, 4]
    assert ids(network, at("2008-01-01", body)) == [4]


def test_shortest_path_takes_the_longer_valid_route(network):
    body = (
        "MATCH p = shortestPath((a:Stop {id: 1})-[:LINK*]->(b:Stop {id: 3})) "
        "RETURN length(p) AS n, [r IN relationships(p) | r.k] AS ks"
    )
    assert network.cypher(body).to_list() == [{"n": 1, "ks": ["ab"]}]
    # Through the valid parallel link only.
    assert network.cypher(at("2008-01-01", body)).to_list() == [{"n": 3, "ks": ["ax", "xy2", "yb"]}]
    undirected = "MATCH p = shortestPath((a:Stop {id: 1})-[:LINK*]-(b:Stop {id: 3})) RETURN length(p) AS n"
    assert ids(network, at("2008-01-01", undirected)) == [3]
    reverse = "MATCH p = shortestPath((b:Stop {id: 3})<-[:LINK*]-(a:Stop {id: 1})) RETURN length(p) AS n"
    assert ids(network, at("2008-01-01", reverse)) == [3]
    # No valid route at all: stop 2 itself is invisible.
    none = "MATCH p = shortestPath((a:Stop {id: 1})-[:LINK*]->(b:Stop {id: 2})) RETURN length(p) AS n"
    assert ids(network, at("2008-01-01", none)) == []


def test_all_shortest_paths_skip_invisible_parallel_relationships(network):
    body = (
        "MATCH p = allShortestPaths((a:Stop {id: 1})-[:LINK*]->(b:Stop {id: 3})) "
        "RETURN [r IN relationships(p) | r.k] AS ks"
    )
    assert ids(network, body) == [["ab"]]
    assert ids(network, at("2008-01-01", body)) == [["ax", "xy2", "yb"]]
    # At 2003 the direct link is valid again; one shortest path.
    assert ids(network, at("2003-01-01", body)) == [["ab"]]
    # One node sequence, one path per valid parallel relationship.
    four = (
        "MATCH p = allShortestPaths((a:Stop {id: 4})-[:LINK*]-(b:Stop {id: 5})) "
        "RETURN [r IN relationships(p) | r.k] AS ks"
    )
    assert ids(network, four) == [["xy1"], ["xy2"]]
    assert ids(network, at("2008-01-01", four)) == [["xy2"]]
    assert ids(network, at("2003-01-01", four)) == [["xy1"]]


def test_optional_match_pads_nulls_when_every_match_is_invisible(sodir):
    body = "MATCH (f:Field) OPTIONAL MATCH (f)-[:HAS_LICENSEE]->(c) RETURN f.id AS f, c.id AS c"
    assert sodir.cypher(at("2006-01-01", body)).to_list() == [{"f": 10, "c": None}]
    assert sodir.cypher(at("2003-01-01", body)).to_list() == [{"f": 10, "c": 20}]
    wells = "MATCH (f:Field) OPTIONAL MATCH (f)<-[:IN]-(w:Well) RETURN f.id AS f, collect(w.id) AS w"
    assert sodir.cypher(at("2011-01-01", wells)).to_list() == [{"f": 10, "w": [2]}]


@pytest.mark.parametrize("storage", [None, "mapped"])
def test_count_subquery_equals_the_guarded_match_count(storage):
    """Memory takes the incident-relationship count, mapped the row join."""
    graph = kglite.KnowledgeGraph(storage=storage) if storage else kglite.KnowledgeGraph()
    graph.cypher(
        "CREATE (f:Field {id: 10}), (c1:Company {id: 20}), (c2:Company {id: 21}),"
        " (c3:Company {id: 22, vf: date('2007-01-01'), vt: date('2100-01-01')}),"
        " (f)-[:HAS_LICENSEE {lf: date('2000-01-01'), lt: date('2005-01-01')}]->(c1),"
        " (f)-[:HAS_LICENSEE {lf: date('2004-01-01')}]->(c2),"
        " (f)-[:HAS_LICENSEE {lf: date('2004-01-01')}]->(c3)"
    ).to_list()
    graph.cypher("CALL db.temporal.declare({node: 'Company', from: 'vf', to: 'vt', convention: 'closed'})").to_list()
    graph.cypher(
        "CALL db.temporal.declare({relationship: 'HAS_LICENSEE', from: 'lf', to: 'lt', convention: 'half_open'})"
    ).to_list()
    counted = "MATCH (f:Field) RETURN count { (f)-[:HAS_LICENSEE]->() } AS n"
    matched = "MATCH (f:Field)-[:HAS_LICENSEE]->() RETURN count(*) AS n"
    for date, want in [("2003-01-01", 1), ("2004-06-01", 2), ("2008-01-01", 2), ("1990-01-01", 0)]:
        assert ids(graph, at(date, counted)) == ids(graph, at(date, matched)) == [want], date
    assert ids(graph, counted) == [3]


def test_pattern_comprehensions_collect_only_valid_matches(sodir):
    names = "MATCH (f:Field) RETURN [(f)<--(w) | w.id] AS ws"
    paths = "MATCH (f:Field) RETURN [p = (f)<-[:IN]-(w) | length(p)] AS ls"
    collected = "MATCH (f:Field)<--(w) RETURN collect(w.id) AS ws"
    assert sorted(ids(sodir, names)[0]) == [1, 2, 3]
    for date, want in [("2006-01-01", [1, 2]), ("2011-01-01", [2]), ("2013-01-01", [2, 3])]:
        assert sorted(ids(sodir, at(date, names))[0]) == sorted(ids(sodir, at(date, collected))[0]) == want
        assert ids(sodir, at(date, paths))[0] == [1] * len(want)
    licensees = "MATCH (f:Field) RETURN [(f)-->(c) | c.id] AS cs"
    assert ids(sodir, at("2006-01-01", licensees)) == [[]]
    assert ids(sodir, at("2003-01-01", licensees)) == [[20]]


def test_exists_answers_under_the_guard_on_the_join_route(sodir):
    exists = "MATCH (f:Field) WHERE EXISTS { MATCH (f)-[:HAS_LICENSEE]->(c) WHERE c.id > 0 } RETURN f.id"
    assert ids(sodir, at("2006-01-01", exists)) == []
    assert ids(sodir, at("2003-01-01", exists)) == [10]


def test_gullfaks_q4_by_var_length_equals_the_written_out_hops():
    """The SODIR-shaped benchmark fixture at a small scale: the partners of a
    field's operator, `(op)<-[:HAS_LICENSEE]-(f2)-[:HAS_LICENSEE]->(p)`, as
    one undirected two-hop segment answers what the written-out hops do,
    with and without the context."""
    from tests.benchmarks import test_bench_temporal as bench

    scale = bench.Scale(260, 100, 600, 6_200, (6, 300), 20, 10)
    graph = bench._load(bench._frames(scale), declared=True)
    params = bench._params(scale)
    hops = (
        "MATCH (f:Field)-[o:HAS_OPERATOR]->(op:Company)<-[r1:HAS_LICENSEE]-(f2:Field)"
        "-[r2:HAS_LICENSEE]->(p:Company) WHERE f.id IN $fids AND p <> op RETURN count(*) AS n"
    )
    segment = (
        "MATCH (f:Field)-[o:HAS_OPERATOR]->(op:Company)-[:HAS_LICENSEE*2..2]-(p:Company) "
        "WHERE f.id IN $fids AND p <> op RETURN count(*) AS n"
    )
    for prefix in ("", bench.AS_OF_T):
        written = bench._rows(graph, prefix + hops, params)
        assert bench._rows(graph, prefix + segment, params) == written, prefix
    assert bench._rows(graph, bench.AS_OF_T + segment, params) != bench._rows(graph, segment, params)


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
    body = "MATCH (w:Well)-[:IN]->(f) WITH f, count(w) AS c RETURN f.id AS f, c"
    plain_rows, plain_plan = profile(current_only, body)
    assert plain_plan[0] == "FusedMatchWithAggregate"
    for prefix in ("FOR VALID_TIME AS OF date() ", "FOR VALID_TIME AS OF $t "):
        rows, plan = profile(current_only, prefix + body, params={"t": dt.date.today()})
        assert rows == plain_rows == [{"f": 10, "c": 2}]
        assert plan == plain_plan, prefix
    # Before either well started the filter removes rows, so the guard runs.
    rows, plan = profile(current_only, at("2003-01-01", body))
    assert rows == [{"f": 10, "c": 1}] and "FusedMatchWithAggregate" not in plan
    # EXPLAIN keeps the guarded plan: the instant is not in the plan.
    explained = [r["operation"] for r in current_only.cypher(f"EXPLAIN FOR VALID_TIME AS OF date() {body}")]
    assert explained[0].startswith("ValidTimeContext")
    assert not any(op.startswith("FusedMatchWithAggregate") for op in explained)
