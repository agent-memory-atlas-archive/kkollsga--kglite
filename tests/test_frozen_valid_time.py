"""`freeze(valid_at=…)` and `Session.snapshot(valid_at=…)`: a FrozenGraph as
of one instant answers every query exactly as `cypher(query, valid_at=…)`
answers on the frozen state, in every storage mode; it refuses a query that
carries its own context, keeps answering the state it froze after the source
is written, and its materialised slice is bounded by its caps."""

from __future__ import annotations

import datetime as dt

import pytest

import kglite

SLICE_CAP_ENV = "KGLITE_TEMPORAL_SLICE_MAX_BYTES"
DISK_CAP_ENV = "KGLITE_TEMPORAL_DISK_SLICE_MAX_ELEMENTS"

QUERIES = [
    "MATCH (w:Well) RETURN w.id AS i",
    "MATCH (w) RETURN id(w) AS i",
    "MATCH (w:Well)-[r:HAS_LICENSEE]->(c) RETURN w.id AS w, c.id AS c",
    "MATCH (f:Field)<-[:IN]-(w) RETURN f.id AS f, count(w) AS n",
    "MATCH (n) RETURN count(n) AS n",
    "MATCH (w:Well {id: 2}) RETURN w.id AS i",
]
INSTANTS = ["2003-01-01", "2006-06-01", "2011-01-01", "2013-01-01"]


def _graph(storage, tmp_path):
    if storage == "disk":
        return kglite.KnowledgeGraph(storage="disk", path=str(tmp_path / "graph"))
    if storage == "mapped":
        return kglite.KnowledgeGraph(storage="mapped")
    return kglite.KnowledgeGraph()


def _sodir(graph):
    """The execution suite's fixture: wells (one closed in 2010, one with the
    declared secondary label `Pad` opening in 2012), an undeclared `Field`,
    and `HAS_LICENSEE` keyed per source type."""
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


def _count(graph):
    return graph.cypher("MATCH (n) RETURN count(n) AS n").to_list()[0]["n"]


def _rows(result):
    return sorted(tuple(sorted(row.items())) for row in result.to_list())


@pytest.fixture
def sodir():
    return _sodir(kglite.KnowledgeGraph())


@pytest.mark.parametrize("storage", ["memory", "mapped", "disk"])
def test_a_frozen_view_answers_as_valid_at_on_the_base(storage, tmp_path):
    graph = _sodir(_graph(storage, tmp_path))
    session = graph.session()
    for instant in INSTANTS:
        frozen = graph.freeze(valid_at=instant)
        snapshot = session.snapshot(valid_at=dt.date.fromisoformat(instant))
        for query in QUERIES:
            expected = _rows(graph.cypher(query, valid_at=instant))
            assert _rows(frozen.cypher(query)) == expected, (storage, instant, query)
            assert _rows(snapshot.cypher(query)) == expected, (storage, instant, query)
        # elementId is the snapshot's own identity, as on the base.
        element_ids = "MATCH (w:Well) RETURN elementId(w) AS e"
        assert _rows(frozen.cypher(element_ids)) == _rows(graph.cypher(element_ids, valid_at=instant))


def test_the_view_answers_fixed_goldens(sodir):
    frozen = sodir.freeze(valid_at="2011-01-01")
    assert [r["w.id"] for r in frozen.cypher("MATCH (w:Well) RETURN w.id").to_list()] == [2]
    # Well 3 carries Pad, whose own interval opens in 2012.
    later = sodir.freeze(valid_at=dt.datetime(2013, 1, 1, 12))
    assert sorted(r["w.id"] for r in later.cypher("MATCH (w:Well) RETURN w.id").to_list()) == [2, 3]
    assert "valid_at=datetime('2013-01-01T12:00:00')" in repr(later)
    # node_count and node_types describe the whole snapshot.
    assert frozen.node_count() == sodir.freeze().node_count()
    assert sorted(frozen.node_types) == sorted(sodir.freeze().node_types)


def test_a_query_with_its_own_context_is_refused(sodir):
    frozen = sodir.freeze(valid_at="2011-01-01")
    for query in (
        "FOR VALID_TIME AS OF date('2003-01-01') MATCH (w:Well) RETURN w.id",
        "EXPLAIN FOR VALID_TIME AS OF $t MATCH (w:Well) RETURN w.id",
    ):
        with pytest.raises(ValueError, match=r"already as of date\('2011-01-01'\)"):
            frozen.cypher(query)
    # A plain handle passes a prefixed query through.
    plain = sodir.freeze()
    rows = plain.cypher("FOR VALID_TIME AS OF date('2003-01-01') MATCH (w:Well) RETURN w.id").to_list()
    assert [r["w.id"] for r in rows] == [1]


def test_explain_and_profile_run_on_the_view(sodir):
    frozen = sodir.freeze(valid_at="2011-01-01")
    plan = frozen.cypher("EXPLAIN MATCH (w:Well) RETURN w.id").to_list()
    assert plan[0]["operation"].startswith("ValidTimeContext axis=VALID_TIME"), plan
    assert "(:Well [vf, vt] closed)" in plan[0]["operation"], plan
    result = frozen.cypher("PROFILE MATCH (w:Well) RETURN w.id")
    assert [r["w.id"] for r in result.to_list()] == [2]
    assert result.profile


def test_bad_instants_and_graphs_without_declarations_are_refused(sodir):
    for bad in ("soon", 3):
        with pytest.raises(ValueError, match="valid_at"):
            sodir.freeze(valid_at=bad)
    # A type no parameter takes fails conversion, as cypher(valid_at=) does.
    with pytest.raises(TypeError, match="valid_at"):
        sodir.freeze(valid_at=dt.time(12))
    with pytest.raises(ValueError, match="needs a validity declaration"):
        kglite.KnowledgeGraph().freeze(valid_at="2020-01-01")
    with pytest.raises(ValueError, match="needs a validity declaration"):
        kglite.KnowledgeGraph().session().snapshot(valid_at="2020-01-01")


def test_a_mutation_on_the_view_gets_the_frozen_message(sodir):
    frozen = sodir.freeze(valid_at="2011-01-01")
    with pytest.raises(kglite.ArgumentError, match="immutable snapshot"):
        frozen.cypher("MATCH (w:Well) SET w.x = 1")


def test_the_view_keeps_answering_the_state_it_froze(sodir):
    body = "MATCH (w:Well) RETURN w.id AS i"
    frozen = sodir.freeze(valid_at="2011-01-01")
    assert _rows(frozen.cypher(body)) == [(("i", 2),)]
    # Close well 2 before 2011 on the live graph.
    sodir.cypher("MATCH (w:Well {id: 2}) SET w.vt = date('2009-01-01')").to_list()
    assert _rows(sodir.cypher(body, valid_at="2011-01-01")) == []
    assert _rows(frozen.cypher(body)) == [(("i", 2),)]
    assert _rows(sodir.freeze(valid_at="2011-01-01").cypher(body)) == []


def test_an_open_transaction_and_the_view_see_their_own_states(sodir):
    body = "MATCH (f:Field) RETURN f.id AS i"
    before = sodir.freeze(valid_at="2011-01-01")
    with sodir.begin() as tx:
        tx.cypher("MATCH (f:Field) SET f.vf = date('2015-01-01'), f.vt = date('2030-01-01')").to_list()
        tx.cypher("CALL db.temporal.declare({node: 'Field', from: 'vf', to: 'vt', convention: 'closed'})").to_list()
        assert tx.cypher(f"FOR VALID_TIME AS OF date('2011-01-01') {body}").to_list() == []
        # Neither the base nor a view of it sees the uncommitted write.
        assert _rows(sodir.freeze(valid_at="2011-01-01").cypher(body)) == [(("i", 10),)]
        tx.commit()
    after = sodir.freeze(valid_at="2011-01-01")
    assert _rows(after.cypher(body)) == []
    assert _rows(sodir.freeze(valid_at="2016-01-01").cypher(body)) == [(("i", 10),)]
    # The view taken before the transaction still answers its own state.
    assert _rows(before.cypher(body)) == [(("i", 10),)]


def test_a_session_snapshot_view_survives_a_session_write(sodir):
    session = sodir.session()
    body = "MATCH (w:Well) RETURN w.id AS i"
    snapshot = session.snapshot(valid_at="2011-01-01")
    session.execute("MATCH (w:Well {id: 2}) SET w.vt = date('2009-01-01')")
    assert _rows(snapshot.cypher(body)) == [(("i", 2),)]
    assert _rows(session.snapshot(valid_at="2011-01-01").cypher(body)) == []


def test_many_views_on_one_graph_each_answer_their_instant(sodir):
    """More views than the mask cache holds entries, all alive at once: each
    still answers its own instant."""
    years = range(1999, 2016)
    views = {year: sodir.freeze(valid_at=f"{year}-06-01") for year in years}
    body = "MATCH (w:Well)-[:IN]->(f) RETURN w.id AS i"
    for year, view in views.items():
        assert _rows(view.cypher(body)) == _rows(sodir.cypher(body, valid_at=f"{year}-06-01")), year


@pytest.mark.parametrize("storage", ["memory", "mapped", "disk"])
def test_the_slice_holds_exactly_the_valid_elements(storage, tmp_path):
    graph = _sodir(_graph(storage, tmp_path))
    for instant in INSTANTS:
        sliced = graph.freeze(valid_at=instant)._valid_time_slice()
        for query in QUERIES:
            assert _rows(sliced.cypher(query)) == _rows(graph.cypher(query, valid_at=instant)), (instant, query)
        # The slice carries no declaration: it already is the instant.
        assert sliced.cypher("CALL db.temporal.declarations() YIELD name RETURN name").to_list() == []


def test_a_slice_over_the_byte_cap_is_refused(sodir, monkeypatch):
    monkeypatch.setenv(SLICE_CAP_ENV, "64")
    with pytest.raises(kglite.KgError, match="slice cap"):
        sodir.freeze(valid_at="2011-01-01")._valid_time_slice()
    monkeypatch.delenv(SLICE_CAP_ENV)
    assert _count(sodir.freeze(valid_at="2011-01-01")._valid_time_slice()) == 3


def test_a_disk_slice_over_the_element_cap_is_refused(tmp_path, monkeypatch):
    graph = _sodir(_graph("disk", tmp_path))
    # At 2011: well 2 (well 1 has closed, well 3's Pad interval has not
    # opened), the field and the company; IN from well 2 and its own
    # HAS_LICENSEE (the field's licence ended in 2004) — five elements.
    monkeypatch.setenv(DISK_CAP_ENV, "4")
    with pytest.raises(kglite.KgError, match="more than 4"):
        graph.freeze(valid_at="2011-01-01")._valid_time_slice()
    monkeypatch.setenv(DISK_CAP_ENV, "5")
    sliced = graph.freeze(valid_at="2011-01-01")._valid_time_slice()
    assert _count(sliced) == 3
    # The in-memory graph has no element cap.
    memory = _sodir(kglite.KnowledgeGraph())
    monkeypatch.setenv(DISK_CAP_ENV, "1")
    assert _count(memory.freeze(valid_at="2011-01-01")._valid_time_slice()) == 3


def test_a_plain_handle_has_no_slice(sodir):
    with pytest.raises(ValueError, match="no valid_at"):
        sodir.freeze()._valid_time_slice()
