"""The graph-level stored valid-time default: a blueprint's
``settings.valid_time_default`` or ``set_valid_time_default(..., persist=True)``
writes it into the file; a load starts from it. A session setter or a server
flag overwrites only the default in force, and an explicit prefix beats both.
The build summary says when a graph that declares validity reads valid-today."""

import json
import warnings

import pytest

import kglite
from kglite import from_blueprint

DEPARTMENT = {
    "csv": "dept.csv",
    "pk": "did",
    "title": "name",
    "properties": {"vf": "validFrom", "vt": "validTo"},
    "temporal": {"from": "vf", "to": "vt", "convention": "closed"},
}
DEPT_CSV = "did,name,vf,vt\nd1,Ops,2020-01-01,2999-01-01\nd2,Sales,2005-01-01,2010-01-01\n"
COUNT = "MATCH (d:Department) RETURN count(d) AS n"
TODAY_NOTE = "default to valid-today"


def _build(tmp_path, settings=None, nodes=None, **kw):
    (tmp_path / "dept.csv").write_text(DEPT_CSV, encoding="utf-8")
    bp = {
        "settings": {"root": str(tmp_path), **(settings or {})},
        "nodes": nodes if nodes is not None else {"Department": DEPARTMENT},
    }
    path = tmp_path / "bp.json"
    path.write_text(json.dumps(bp), encoding="utf-8")
    with warnings.catch_warnings(record=True) as caught:
        warnings.simplefilter("always")
        graph = from_blueprint(path, save=False, **kw)
    return graph, [str(w.message) for w in caught if issubclass(w.category, UserWarning)]


def _n(graph, query=COUNT):
    return graph.cypher(query).to_list()[0]["n"]


@pytest.fixture(params=["default", "mapped", "disk"])
def mode(request, tmp_path):
    return request.param


def _build_in_mode(tmp_path, mode, settings=None):
    kw = {"storage": mode}
    if mode == "disk":
        kw["path"] = str(tmp_path / "disk_graph")
    return _build(tmp_path, settings, **kw)[0]


def _roundtrip(graph, tmp_path, mode):
    """Save and reload in the same storage mode (a disk graph is its directory)."""
    if mode == "disk":
        path = tmp_path / "disk_graph"
        graph.save(str(path))
        loaded = kglite.load(str(path))
    else:
        path = tmp_path / "g.kgl"
        graph.save(str(path))
        loaded = kglite.load(str(path), storage=mode)
    assert loaded.graph_info()["storage_mode"] == {"default": "memory"}.get(mode, mode)
    return loaded


def test_the_blueprint_setting_persists_across_save_and_load(tmp_path, mode):
    graph = _build_in_mode(tmp_path, mode, {"valid_time_default": "all"})
    assert graph.get_valid_time_default() == "all"
    assert _n(graph) == 2
    loaded = _roundtrip(graph, tmp_path, mode)
    assert loaded.get_valid_time_default() == "all"
    assert loaded.graph_info()["valid_time_default"] == {"effective": "all", "stored": "all"}
    assert _n(loaded) == 2


def test_a_fixed_date_is_stored_and_a_bad_value_names_the_key(tmp_path):
    graph, _ = _build(tmp_path, {"valid_time_default": "2008-01-01"})
    assert _n(graph) == 1 and graph.get_valid_time_default() == "2008-01-01"
    with pytest.raises(ValueError, match="settings.valid_time_default"):
        _build(tmp_path, {"valid_time_default": "yesterday"})


def test_persist_stores_and_the_plain_setter_does_not(tmp_path, mode):
    graph = _build_in_mode(tmp_path, mode)
    assert _n(graph) == 1
    graph.set_valid_time_default("all")
    assert _n(graph) == 2
    plain = _roundtrip(graph, tmp_path, mode)
    assert plain.get_valid_time_default() == "today" and _n(plain) == 1
    graph.set_valid_time_default("all", persist=True)
    stored = _roundtrip(graph, tmp_path, mode)
    assert stored.get_valid_time_default() == "all" and _n(stored) == 2
    assert stored.graph_info()["valid_time_default"] == {"effective": "all", "stored": "all"}


def test_a_session_setting_overrides_the_stored_default_without_changing_it(tmp_path):
    graph, _ = _build(tmp_path, {"valid_time_default": "all"})
    graph.set_valid_time_default("today")
    assert _n(graph) == 1
    assert graph.graph_info()["valid_time_default"] == {"effective": "today", "stored": "all"}
    # The explicit prefix beats the setter and the stored default alike.
    assert _n(graph, "FOR VALID_TIME ALL " + COUNT) == 2
    assert _n(graph, "FOR VALID_TIME AS OF date('2008-01-01') " + COUNT) == 1


def test_a_stored_default_reaches_frozen_views_and_sessions(tmp_path):
    graph, _ = _build(tmp_path, {"valid_time_default": "all"})
    frozen = graph.freeze()
    assert frozen.cypher(COUNT).to_list()[0]["n"] == 2
    assert graph.copy().get_valid_time_default() == "all"
    path = tmp_path / "g.kgl"
    graph.save(str(path))
    session = kglite.open_session(str(path))
    assert session.execute(COUNT).to_list()[0]["n"] == 2


def test_the_echo_reports_a_stored_all_default_as_the_default_source(tmp_path):
    graph, _ = _build(tmp_path, {"valid_time_default": "all"})
    echo = graph.cypher(COUNT).diagnostics["temporal"]
    assert echo["source"] == "default"
    assert echo["instant"] == "all"


def test_the_build_says_a_declared_graph_reads_valid_today(tmp_path):
    graph, warned = _build(tmp_path)
    notes = [d for d in graph.graph_info()["build"]["diagnostics"] if d["kind"] == "default_today"]
    assert len(notes) == 1 and notes[0]["group"] == "declarations"
    assert TODAY_NOTE in notes[0]["message"]
    assert any(TODAY_NOTE in w for w in warned)


def test_the_build_note_is_silent_when_the_stored_default_is_not_today(tmp_path):
    for value in ("all", "2008-01-01"):
        graph, warned = _build(tmp_path, {"valid_time_default": value})
        kinds = [d["kind"] for d in graph.graph_info()["build"]["diagnostics"]]
        assert "default_today" not in kinds
        assert not any(TODAY_NOTE in w for w in warned)


def test_the_build_note_is_silent_without_declarations(tmp_path):
    plain = {"Department": {"csv": "dept.csv", "pk": "did", "title": "name"}}
    graph, warned = _build(tmp_path, nodes=plain)
    assert "default_today" not in [d["kind"] for d in graph.graph_info()["build"]["diagnostics"]]
    assert not any(TODAY_NOTE in w for w in warned)


@pytest.mark.parametrize("strict", [True, ["declarations"], ["declarations", "stubs"]])
def test_strict_never_fails_on_the_informational_note(tmp_path, strict):
    graph, _ = _build(tmp_path, strict=strict)
    assert _n(graph) == 1
