"""Identical relationship rows: one warning by default, `distinct` collapses them.

A first load of a relationship type writes one relationship per row, so a
source with repeated rows (an assignment history fed to a loader that lists no
property columns) stores identical parallel relationships and every count over
them multiplies. Nothing changes by default except one warning per type.
"""

import json
import warnings

import pandas as pd
import pytest

from kglite import KnowledgeGraph
from kglite.blueprint import from_blueprint

MODES = ["memory", "mapped", "disk"]


def _new_graph(mode, tmp_path):
    if mode == "memory":
        return KnowledgeGraph()
    if mode == "mapped":
        return KnowledgeGraph(storage="mapped")
    return KnowledgeGraph(storage="disk", path=str(tmp_path / "disk_graph"))


def _people_and_projects(g):
    g.add_nodes(pd.DataFrame({"eid": [1, 2, 3], "name": ["Ann", "Bo", "Cy"]}), "Employee", "eid", "name")
    g.add_nodes(pd.DataFrame({"pid": [10, 20], "name": ["Apollo", "Borealis"]}), "Project", "pid", "name")


def _count(g, q="MATCH (:Employee)-[r:WORKS_ON]->(:Project) RETURN count(r) AS n"):
    return g.cypher(q).to_list()[0]["n"]


def _load(g, frame, **kw):
    with warnings.catch_warnings(record=True) as caught:
        warnings.simplefilter("always")
        report = g.add_relationships(frame, "WORKS_ON", "Employee", "eid", "Project", "pid", **kw)
    return report, [str(w.message) for w in caught if "identical" in str(w.message)]


HISTORY = pd.DataFrame({"eid": [1, 1, 1, 2, 2, 3], "pid": [10, 10, 10, 20, 20, 10]})


@pytest.mark.parametrize("mode", MODES)
def test_identical_rows_warn_once_and_change_nothing(mode, tmp_path):
    g = _new_graph(mode, tmp_path)
    _people_and_projects(g)
    report, msgs = _load(g, HISTORY)
    assert _count(g) == 6
    assert report["connections_created"] == 6
    assert len(msgs) == 1, msgs
    m = msgs[0]
    assert "'WORKS_ON'" in m
    assert "6 relationships" in m and "3 distinct (source, target) pairs" in m
    assert "up to 3 identical copies" in m
    assert "distinct=True" in m


@pytest.mark.parametrize("mode", MODES)
def test_distinct_keeps_one_relationship_per_pair_and_is_silent(mode, tmp_path):
    g = _new_graph(mode, tmp_path)
    _people_and_projects(g)
    report, msgs = _load(g, HISTORY, distinct=True)
    assert _count(g) == 3
    assert report["connections_created"] == 3
    assert msgs == []


def test_distinct_rows_never_warn(tmp_path):
    g = KnowledgeGraph()
    _people_and_projects(g)
    _, msgs = _load(g, pd.DataFrame({"eid": [1, 2, 3], "pid": [10, 20, 10]}))
    assert msgs == []
    assert _count(g) == 3


def test_rows_that_differ_in_a_property_are_neither_warned_nor_collapsed(tmp_path):
    g = KnowledgeGraph()
    _people_and_projects(g)
    frame = pd.DataFrame({"eid": [1, 1, 1], "pid": [10, 10, 10], "role": ["dev", "lead", "dev"]})
    _, msgs = _load(g, frame, distinct=True)
    # dev, lead: the repeated "dev" collapses, the differing "lead" survives.
    assert _count(g) == 2
    assert msgs == []
    roles = sorted(r["role"] for r in g.cypher("MATCH ()-[r:WORKS_ON]->() RETURN r.role AS role").to_list())
    assert roles == ["dev", "lead"]


def test_property_values_decide_what_is_identical_in_the_warning(tmp_path):
    g = KnowledgeGraph()
    _people_and_projects(g)
    differing = pd.DataFrame({"eid": [1, 1], "pid": [10, 10], "role": ["dev", "lead"]})
    _, msgs = _load(g, differing)
    assert msgs == []
    g2 = KnowledgeGraph()
    _people_and_projects(g2)
    same = pd.DataFrame({"eid": [1, 1, 2], "pid": [10, 10, 20], "role": ["dev", "dev", "qa"]})
    _, msgs = _load(g2, same)
    assert len(msgs) == 1 and "(source, target, properties) combinations" in msgs[0], msgs
    assert "3 relationships" in msgs[0] and "2 distinct" in msgs[0] and "up to 2 identical" in msgs[0]


def test_distinct_keeps_the_first_row_with_its_properties(tmp_path):
    g = KnowledgeGraph()
    _people_and_projects(g)
    frame = pd.DataFrame({"eid": [1, 1], "pid": [10, 10], "role": ["dev", "dev"], "hours": [5, 5]})
    _load(g, frame, distinct=True)
    rows = g.cypher("MATCH ()-[r:WORKS_ON]->() RETURN r.role AS role, r.hours AS hours").to_list()
    assert rows == [{"role": "dev", "hours": 5}]


def test_a_later_load_from_the_same_source_merges_and_never_warns(tmp_path):
    g = KnowledgeGraph()
    _people_and_projects(g)
    _load(g, pd.DataFrame({"eid": [1], "pid": [10]}))
    _, msgs = _load(g, HISTORY)
    assert msgs == []
    assert _count(g) == 3


def test_declared_valid_time_rows_keep_their_distinct_history(tmp_path):
    g = KnowledgeGraph()
    _people_and_projects(g)
    frame = pd.DataFrame(
        {
            "eid": [1, 1, 1],
            "pid": [10, 10, 10],
            "validFrom": pd.to_datetime(["2020-01-01", "2021-01-01", "2020-01-01"]),
            "validTo": pd.to_datetime(["2098-12-31", "2099-12-31", "2098-12-31"]),
        }
    )
    types = {"validFrom": "validFrom", "validTo": "validTo"}
    _, msgs = _load(g, frame, column_types=types, distinct=True)
    assert msgs == []
    assert _count(g) == 2
    g2 = KnowledgeGraph()
    _people_and_projects(g2)
    _, msgs = _load(g2, frame, column_types=types)
    assert msgs == []
    assert _count(g2) == 2


def test_the_connections_pointer_takes_distinct_too(tmp_path):
    g = KnowledgeGraph()
    _people_and_projects(g)
    g.add_connections(HISTORY, "WORKS_ON", "Employee", "eid", "Project", "pid", distinct=True)
    assert _count(g) == 3


def test_bulk_loader_spec_key_distinct(tmp_path):
    frame = pd.DataFrame({"source_id": [1, 1, 2], "target_id": [10, 10, 20]})
    spec = {"source_type": "Employee", "target_type": "Project", "connection_name": "WORKS_ON", "data": frame}

    g = KnowledgeGraph()
    _people_and_projects(g)
    with warnings.catch_warnings(record=True) as caught:
        warnings.simplefilter("always")
        out = g.add_relationships_bulk([dict(spec)])
    assert out["WORKS_ON"] == 3 and _count(g) == 3
    assert len([w for w in caught if "identical" in str(w.message)]) == 1

    g = KnowledgeGraph()
    _people_and_projects(g)
    with warnings.catch_warnings(record=True) as caught:
        warnings.simplefilter("always")
        out = g.add_relationships_bulk([dict(spec, distinct=True)])
    assert out["WORKS_ON"] == 2 and _count(g) == 2
    assert not [w for w in caught if "identical" in str(w.message)]


# --- blueprint junction edges -------------------------------------------------


def _blueprint(tmp_path, junction_extra=None, properties=None, history=None):
    pd.DataFrame({"eid": [1, 2, 3], "name": ["Ann", "Bo", "Cy"]}).to_csv(
        tmp_path / "employees.csv", index=False, encoding="utf-8"
    )
    pd.DataFrame({"pid": [10, 20], "title": ["Apollo", "Borealis"]}).to_csv(
        tmp_path / "projects.csv", index=False, encoding="utf-8"
    )
    if history is None:
        history = pd.DataFrame(
            {
                "eid": [1, 1, 1, 2, 2, 3],
                "pid": [10, 10, 10, 20, 20, 10],
                "week": [1, 2, 3, 1, 2, 1],
            }
        )
    history.to_csv(tmp_path / "history.csv", index=False, encoding="utf-8")
    junction = {
        "csv": "history.csv",
        "source_fk": "eid",
        "target": "Project",
        "target_fk": "pid",
        "properties": properties or [],
    }
    junction.update(junction_extra or {})
    bp = {
        "settings": {"root": str(tmp_path)},
        "nodes": {
            "Employee": {
                "csv": "employees.csv",
                "pk": "eid",
                "title": "name",
                "connections": {"junction_edges": {"WORKS_ON": junction}},
            },
            "Project": {"csv": "projects.csv", "pk": "pid", "title": "title"},
        },
    }
    (tmp_path / "bp.json").write_text(json.dumps(bp), encoding="utf-8")
    return tmp_path / "bp.json"


def _build(blueprint, **kw):
    with warnings.catch_warnings(record=True) as caught:
        warnings.simplefilter("always")
        g = from_blueprint(blueprint, save=False, **kw)
    return g, [str(w.message) for w in caught if "identical" in str(w.message)]


@pytest.mark.parametrize("mode", MODES)
def test_blueprint_junction_without_properties_warns_once(mode, tmp_path):
    kw = {}
    if mode != "memory":
        kw["storage"] = mode
    if mode == "disk":
        kw["path"] = str(tmp_path / "bp_disk")
    g, msgs = _build(_blueprint(tmp_path), **kw)
    assert _count(g) == 6
    assert len(msgs) == 1, msgs
    m = msgs[0]
    assert "'WORKS_ON'" in m and "6 relationships" in m
    assert "3 distinct (source, target) pairs" in m and "up to 3 identical copies" in m
    assert "`distinct: true`" in m


@pytest.mark.parametrize("mode", MODES)
def test_blueprint_junction_distinct_collapses_and_is_silent(mode, tmp_path):
    kw = {}
    if mode != "memory":
        kw["storage"] = mode
    if mode == "disk":
        kw["path"] = str(tmp_path / "bp_disk")
    g, msgs = _build(_blueprint(tmp_path, {"distinct": True}), **kw)
    assert _count(g) == 3
    assert msgs == []


def test_blueprint_distinct_is_a_known_junction_key(tmp_path):
    with warnings.catch_warnings(record=True) as caught:
        warnings.simplefilter("always")
        from_blueprint(_blueprint(tmp_path, {"distinct": True}), save=False)
    assert not [w for w in caught if "unknown key" in str(w.message)], [str(w.message) for w in caught]


def test_blueprint_junction_listing_the_distinguishing_column_needs_no_option(tmp_path):
    g, msgs = _build(_blueprint(tmp_path, properties=["week"]))
    assert _count(g) == 6
    assert msgs == []


def test_blueprint_distinct_with_properties_keeps_differing_rows(tmp_path):
    history = pd.DataFrame({"eid": [1, 1, 1], "pid": [10, 10, 10], "role": ["dev", "dev", "lead"]})
    g, msgs = _build(_blueprint(tmp_path, {"distinct": True}, properties=["role"], history=history))
    assert _count(g) == 2
    assert msgs == []


def test_blueprint_junction_dedupes_across_chunks(tmp_path, monkeypatch):
    monkeypatch.setenv("KGLITE_BLUEPRINT_JUNCTION_CHUNK_SIZE", "2")
    g, msgs = _build(_blueprint(tmp_path, {"distinct": True}))
    assert _count(g) == 3
    assert msgs == []
    g, msgs = _build(_blueprint(tmp_path))
    assert _count(g) == 6
    assert len(msgs) == 1 and "up to 3 identical copies" in msgs[0], msgs


def test_blueprint_distinct_must_be_a_boolean(tmp_path):
    path = _blueprint(tmp_path, {"distinct": "yes"})
    with pytest.raises(ValueError, match="expected a boolean"):
        from_blueprint(path, save=False)


def test_rows_to_missing_endpoints_collapse_and_warn_like_any_other(tmp_path):
    frame = pd.DataFrame({"eid": [1, 1, 2], "pid": [10, 10, 20]})
    g = KnowledgeGraph()
    _, msgs = _load(g, frame)
    assert _count(g) == 3
    assert len(msgs) == 1 and "up to 2 identical copies" in msgs[0], msgs
    g = KnowledgeGraph()
    _, msgs = _load(g, frame, distinct=True)
    assert _count(g) == 2
    assert msgs == []
