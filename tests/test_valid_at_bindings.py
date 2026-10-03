"""``valid_at=`` on every Python query entry point, and the valid-time echo in
``ResultView.diagnostics["temporal"]``.

Each entry point writes the statement prefix ``FOR VALID_TIME AS OF`` through
the engine's one helper, so ``valid_at=`` answers exactly as the typed prefix
does, a query that already carries a context is refused naming both, and
``EXPLAIN`` may follow the prefix.
"""

from __future__ import annotations

import datetime as dt

import pytest

import kglite

WELLS = "MATCH (w:Well) RETURN w.id AS id ORDER BY id"
T = "2003-06-30"


@pytest.fixture
def wells():
    """Well 1 (2000–2010, closed), well 2 (from 2005) and a pipeline between
    them valid from 2005: as of 2003 only well 1 is visible."""
    graph = kglite.KnowledgeGraph()
    graph.cypher(
        "CREATE (w1:Well {id: 1, vf: date('2000-01-01'), vt: date('2010-01-01')}),"
        " (w2:Well {id: 2, vf: date('2005-01-01')}),"
        " (w1)-[:PIPE {since: date('2005-01-01'), until: date('2099-01-01')}]->(w2)"
    ).to_list()
    graph.cypher("CALL db.temporal.declare({node: 'Well', from: 'vf', to: 'vt', convention: 'closed'})").to_list()
    graph.cypher(
        "CALL db.temporal.declare({relationship: 'PIPE', from: 'since', to: 'until', convention: 'half_open'})"
    ).to_list()
    return graph


def _entry_points(graph):
    """Each entry point that takes query text, as ``run(query, **kwargs)``."""
    session = graph.session()
    tx = graph.begin_read()
    return {
        "KnowledgeGraph.cypher": graph.cypher,
        "Session.cypher": session.cypher,
        "Session.execute": session.execute,
        "Transaction.cypher": tx.cypher,
        "FrozenGraph.cypher": graph.freeze().cypher,
    }


def _ids(result):
    return [row["id"] for row in result.to_list()]


@pytest.mark.parametrize("name", list(_entry_points(kglite.KnowledgeGraph())))
def test_valid_at_answers_as_the_typed_prefix(wells, name):
    run = _entry_points(wells)[name]
    assert _ids(run(f"FOR VALID_TIME ALL {WELLS}")) == [1, 2]
    assert _ids(run(WELLS)) == [2], "without valid_at the declarations default to today"
    typed = _ids(run(f"FOR VALID_TIME AS OF date('{T}') {WELLS}"))
    for instant in (T, dt.date(2003, 6, 30), dt.datetime(2003, 6, 30, 12, 0)):
        assert _ids(run(WELLS, valid_at=instant)) == typed == [1], (name, instant)


@pytest.mark.parametrize("name", list(_entry_points(kglite.KnowledgeGraph())))
def test_a_doubled_context_or_a_bad_instant_is_refused(wells, name):
    run = _entry_points(wells)[name]
    with pytest.raises(ValueError, match=r"already has a FOR .* valid_at="):
        run(f"FOR VALID_TIME AS OF date('2001-01-01') {WELLS}", valid_at=T)
    with pytest.raises(ValueError, match="valid_at"):
        run(WELLS, valid_at="last tuesday")


@pytest.mark.parametrize("name", list(_entry_points(kglite.KnowledgeGraph())))
def test_explain_and_profile_follow_the_prefix(wells, name):
    run = _entry_points(wells)[name]
    plan = run(f"EXPLAIN {WELLS}", valid_at=T).to_list()
    assert plan[0]["operation"].startswith("ValidTimeContext axis=VALID_TIME"), plan
    profiled = run(f"PROFILE {WELLS}", valid_at=T)
    assert _ids(profiled) == [1]


@pytest.mark.parametrize("name", list(_entry_points(kglite.KnowledgeGraph())))
def test_the_echo_names_the_instant_the_targets_and_the_route(wells, name):
    run = _entry_points(wells)[name]
    echo = run(WELLS, valid_at=T).diagnostics["temporal"]
    assert echo["axis"] == "VALID_TIME"
    assert echo["source"] == "explicit"
    assert echo["instant"] == T
    assert echo["targets"] == ["(:Well)"]
    assert echo["hidden"] == {"(:Well)": 1}
    assert echo["endpoint_invalid"] == 0
    assert echo["route"] == "guarded"
    assert echo["retrieval"] is None
    assert echo["slice"] is False
    assert isinstance(echo["session_version"], int)
    # Every declared target is valid in full in 2007: the plain plan ran.
    assert run(WELLS, valid_at="2007-01-01").diagnostics["temporal"]["route"] == "plain"
    hop = run("MATCH (a:Well)-[:PIPE]->(b) RETURN a.id AS id", valid_at="2006-01-01T08:30:00")
    assert hop.diagnostics["temporal"]["instant"] == "2006-01-01T08:30:00"
    assert hop.diagnostics["temporal"]["targets"] == ["(:Well)", "[:PIPE]"]
    default = run(WELLS).diagnostics["temporal"]
    assert default["source"] == "default"
    assert default["instant"] == dt.datetime.now(dt.timezone.utc).date().isoformat()
    every = run(f"FOR VALID_TIME ALL {WELLS}").diagnostics["temporal"]
    assert (every["source"], every["instant"], every["route"]) == ("all", "all", "plain")


def test_a_routed_algorithm_echoes_the_slice(wells):
    query = "CALL pagerank() YIELD node, score RETURN node.id AS id"
    result = wells.cypher(query, valid_at=T)
    assert _ids(result) == [1]
    assert result.diagnostics["temporal"]["slice"] is True
    assert wells.freeze(valid_at=T).cypher(query).diagnostics["temporal"]["slice"] is True


def test_a_view_handle_echoes_the_view_and_refuses_a_second_instant(wells):
    frozen = wells.freeze(valid_at=T)
    result = frozen.cypher(WELLS)
    assert _ids(result) == [1]
    assert result.diagnostics["temporal"]["route"] == "view"
    assert result.diagnostics["temporal"]["instant"] == T
    snapshot = wells.session().snapshot(valid_at=T)
    assert snapshot.cypher(WELLS).diagnostics["temporal"]["route"] == "view"
    for handle in (frozen, snapshot):
        with pytest.raises(ValueError, match=r"already as of date\('2003-06-30'\).*valid_at="):
            handle.cypher(WELLS, valid_at="2001-01-01")
        # Even the same instant: the handle fixes it, the kwarg is a second one.
        with pytest.raises(ValueError, match="already as of"):
            handle.cypher(WELLS, valid_at=T)


def test_a_write_under_valid_at_is_refused_on_the_write_paths(wells):
    session = wells.session()
    with pytest.raises(kglite.KgError, match="cannot write"):
        session.execute("CREATE (:Well {id: 3})", valid_at=T)
    tx = wells.begin()
    with pytest.raises(kglite.KgError, match="cannot write"):
        tx.cypher("CREATE (:Well {id: 3})", valid_at=T)
    tx.rollback()
    assert _ids(wells.cypher(f"FOR VALID_TIME ALL {WELLS}")) == [1, 2]


def test_a_transaction_reads_its_own_writes_as_of_the_instant(wells):
    tx = wells.begin()
    tx.cypher("CREATE (:Well {id: 3, vf: date('2001-01-01')})")
    assert _ids(tx.cypher(WELLS, valid_at=T)) == [1, 3]
    tx.rollback()
