"""``valid_at='all'`` and the graph's configurable valid-time default.

``valid_at='all'`` is the entry-point spelling of ``FOR VALID_TIME ALL``;
``set_valid_time_default`` changes what an unprefixed statement and a fluent
cursor read on a graph with validity declarations. The setting is runtime
state: it is never written into a ``.kgl`` file.
"""

from __future__ import annotations

import datetime as dt

import pytest

import kglite

LIST = "MATCH (e:Employee) RETURN e.id AS id ORDER BY id"


def _staff() -> kglite.KnowledgeGraph:
    """Employee 1 left in 2010, 2 has been employed since 2005, 3 starts in 2999."""
    graph = kglite.KnowledgeGraph()
    graph.cypher(
        "CREATE (:Employee {id: 1, vf: date('2000-01-01'), vt: date('2010-01-01')}),"
        " (:Employee {id: 2, vf: date('2005-01-01')}),"
        " (:Employee {id: 3, vf: date('2999-01-01')})"
    ).to_list()
    graph.cypher("CALL db.temporal.declare({node: 'Employee', from: 'vf', to: 'vt', convention: 'closed'})").to_list()
    return graph


def _ids(graph, query=LIST, **kwargs):
    return [row["id"] for row in graph.cypher(query, **kwargs).to_list()]


def _fluent_ids(graph):
    return sorted(row["id"] for row in graph.select("Employee").collect())


def test_valid_at_all_reads_every_version_on_every_entry_point():
    graph = _staff()
    assert _ids(graph) == [2]
    result = graph.cypher(LIST, valid_at="all")
    assert [row["id"] for row in result.to_list()] == [1, 2, 3]
    echo = result.diagnostics["temporal"]
    assert (echo["source"], echo["instant"]) == ("all", "all")

    session = graph.session()
    assert [r["id"] for r in session.cypher(LIST, valid_at="all").to_list()] == [1, 2, 3]
    assert [r["id"] for r in session.cypher(LIST).to_list()] == [2]
    frozen = graph.freeze()
    assert _ids(frozen, valid_at="all") == [1, 2, 3]
    assert _ids(frozen) == [2]
    tx = graph.begin()
    try:
        assert [r["id"] for r in tx.cypher(LIST, valid_at="all").to_list()] == [1, 2, 3]
    finally:
        tx.rollback()


def test_valid_at_none_means_the_default_and_a_date_still_pins():
    graph = _staff()
    assert _ids(graph, valid_at=None) == [2]
    assert _ids(graph, valid_at="2008-01-01") == [1, 2]
    graph.set_valid_time_default("all")
    assert _ids(graph, valid_at=None) == [1, 2, 3]
    assert _ids(graph, valid_at="2008-01-01") == [1, 2]


def test_valid_at_all_refuses_a_doubled_context_and_a_frozen_view():
    graph = _staff()
    with pytest.raises(ValueError, match=r"already has a FOR .* valid_at= adds another \(FOR VALID_TIME ALL\)"):
        graph.cypher(f"FOR VALID_TIME ALL {LIST}", valid_at="all")
    with pytest.raises(ValueError, match="already has a FOR"):
        graph.cypher(f"FOR VALID_TIME AS OF date('2008-01-01') {LIST}", valid_at="all")
    view = graph.freeze(valid_at="2008-01-01")
    assert _ids(view) == [1, 2]
    with pytest.raises(ValueError, match="already as of"):
        view.cypher(LIST, valid_at="all")
    with pytest.raises(ValueError, match="already as of"):
        view.cypher(f"FOR VALID_TIME ALL {LIST}")
    with pytest.raises(ValueError):
        graph.freeze(valid_at="all")


def test_set_valid_time_default_governs_cypher_and_fluent():
    graph = _staff()
    assert graph.get_valid_time_default() == "today"
    assert _ids(graph) == [2]
    assert _fluent_ids(graph) == [2]

    graph.set_valid_time_default("all")
    assert graph.get_valid_time_default() == "all"
    assert _ids(graph) == [1, 2, 3], "the same query text, a new setting"
    assert _fluent_ids(graph) == [1, 2, 3]
    echo = graph.cypher(LIST).diagnostics["temporal"]
    assert (echo["source"], echo["instant"]) == ("default", "all")
    assert any("(default)" in str(row) for row in graph.cypher("EXPLAIN " + LIST).to_list())
    # An explicit statement prefix and an explicit fluent date still win.
    assert _ids(graph, f"FOR VALID_TIME AS OF date('2008-01-01') {LIST}") == [1, 2]
    assert sorted(r["id"] for r in graph.date("2008-01-01").select("Employee").collect()) == [1, 2]
    assert sorted(r["id"] for r in graph.date().select("Employee").collect()) == [1, 2, 3], (
        "date() resets the cursor to the graph's default"
    )

    graph.set_valid_time_default("today")
    assert _ids(graph) == [2]
    assert _fluent_ids(graph) == [2]


@pytest.mark.parametrize(
    "value",
    ["2008-01-01", dt.date(2008, 1, 1), dt.datetime(2008, 1, 1, 15, 30)],
    ids=["str", "date", "datetime"],
)
def test_a_fixed_date_pins_the_default(value):
    graph = _staff()
    graph.set_valid_time_default(value)
    assert graph.get_valid_time_default() == "2008-01-01"
    assert _ids(graph) == [1, 2]
    assert _fluent_ids(graph) == [1, 2]
    echo = graph.cypher(LIST).diagnostics["temporal"]
    assert (echo["source"], echo["instant"]) == ("default", "2008-01-01")
    assert _ids(graph, valid_at="all") == [1, 2, 3]


def test_the_setting_rejects_what_it_cannot_read():
    graph = _staff()
    for bad in ("yesterday", "2008-13-40", ""):
        with pytest.raises(kglite.ArgumentError, match="not 'today', 'all' or a date"):
            graph.set_valid_time_default(bad)
    with pytest.raises(TypeError):
        graph.set_valid_time_default(7)
    assert graph.get_valid_time_default() == "today"


def test_the_plan_cache_does_not_serve_a_plan_across_setting_changes():
    graph = _staff()
    answers = []
    for setting in ("today", "all", "2008-01-01", "all", "today", "2008-01-01"):
        graph.set_valid_time_default(setting)
        answers.append(_ids(graph))
    assert answers == [[2], [1, 2, 3], [1, 2], [1, 2, 3], [2], [1, 2]]


def test_a_write_under_a_default_all_is_not_reported_as_skipped():
    graph = _staff()
    graph.set_valid_time_default("all")
    result = graph.cypher("MATCH (e:Employee) SET e.seen = true RETURN count(e) AS n")
    assert result.to_list() == [{"n": 3}]
    assert result.diagnostics["temporal"]["source"] == "default"


def test_the_setting_is_not_persisted(tmp_path):
    graph = _staff()
    graph.set_valid_time_default("all")
    path = str(tmp_path / "staff.kgl")
    graph.save(path)
    loaded = kglite.load(path)
    assert loaded.get_valid_time_default() == "today"
    assert _ids(loaded) == [2]
    assert _fluent_ids(loaded) == [2]
    assert graph.get_valid_time_default() == "all", "saving leaves the live setting alone"


def test_a_graph_without_declarations_ignores_the_setting():
    graph = kglite.KnowledgeGraph()
    graph.cypher("CREATE (:Employee {id: 1}), (:Employee {id: 2})").to_list()
    for setting in ("today", "all", "2008-01-01"):
        graph.set_valid_time_default(setting)
        assert _ids(graph) == [1, 2]
    assert graph.cypher(LIST, valid_at="all").to_list() == [{"id": 1}, {"id": 2}]
