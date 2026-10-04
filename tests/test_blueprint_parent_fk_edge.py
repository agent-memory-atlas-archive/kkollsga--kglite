"""A sub-node with ``parent_fk`` gets its ``OF_<PARENT>`` edge automatically."""

import json
import warnings

import pandas as pd
import pytest

from kglite.blueprint import from_blueprint


def _csv(path, df):
    df.to_csv(path, index=False, encoding="utf-8")


def _build(tmp_path, nodes):
    bp = {"settings": {"root": str(tmp_path)}, "nodes": nodes}
    (tmp_path / "bp.json").write_text(json.dumps(bp), encoding="utf-8")
    return from_blueprint(tmp_path / "bp.json", save=False)


def _employees(tmp_path):
    _csv(
        tmp_path / "employees.csv",
        pd.DataFrame({"employee_id": [1, 2, 3], "name": ["Ann", "Bo", "Cy"]}),
    )
    _csv(
        tmp_path / "reviews.csv",
        pd.DataFrame(
            {
                "review_id": [10, 11, 12, 13],
                "employee_id": [1, 1, 2, 3],
                "summary": ["a", "b", "c", "d"],
                "rating": [4, 3, 5, 2],
            }
        ),
    )


def _review(**extra):
    spec = {
        "csv": "reviews.csv",
        "pk": "review_id",
        "title": "summary",
        "parent_fk": "employee_id",
        "properties": {"rating": "int"},
        "skipped": ["employee_id"],
    }
    spec.update(extra)
    return spec


def _employee(sub):
    return {"Employee": {"csv": "employees.csv", "pk": "employee_id", "title": "name", "sub_nodes": {"Review": sub}}}


def _rows(g, q):
    return [tuple(r.values()) for r in g.cypher(q)]


def test_docs_review_example_links_each_review_to_its_employee(tmp_path):
    _employees(tmp_path)
    g = _build(tmp_path, _employee(_review()))
    rows = _rows(
        g,
        "MATCH (e:Employee)<-[:OF_EMPLOYEE]-(r:Review) RETURN e.name AS n, r.rating AS x ORDER BY r.rating DESC",
    )
    assert rows == [("Bo", 5), ("Ann", 4), ("Ann", 3), ("Cy", 2)]


def test_parent_fk_alone_gives_one_edge_per_node_with_auto_pk(tmp_path):
    _employees(tmp_path)
    g = _build(tmp_path, _employee(_review(pk="auto")))
    assert _rows(g, "MATCH (r:Review)-[x:OF_EMPLOYEE]->(:Employee) RETURN count(x) AS c") == [(4,)]
    assert _rows(g, "MATCH (r:Review) RETURN count(r) AS c") == [(4,)]
    assert _rows(g, "MATCH (r:Review)-[x]->() RETURN count(x) AS c") == [(4,)]


def test_explicit_same_name_fk_edge_wins_without_duplicate(tmp_path):
    _employees(tmp_path)
    sub = _review(connections={"fk_edges": {"OF_EMPLOYEE": {"target": "Employee", "fk": "employee_id"}}})
    g = _build(tmp_path, _employee(sub))
    assert _rows(g, "MATCH (:Review)-[x:OF_EMPLOYEE]->(:Employee) RETURN count(x) AS c") == [(4,)]


def test_differently_named_explicit_edge_replaces_the_implicit_one(tmp_path):
    _employees(tmp_path)
    sub = _review(connections={"fk_edges": {"REVIEWS": {"target": "Employee", "fk": "employee_id"}}})
    g = _build(tmp_path, _employee(sub))
    assert _rows(g, "MATCH (:Review)-[x:REVIEWS]->(:Employee) RETURN count(x) AS c") == [(4,)]
    assert _rows(g, "MATCH (:Review)-[x]->() RETURN count(x) AS c") == [(4,)]
    assert _rows(g, "MATCH ()-[x:OF_EMPLOYEE]->() RETURN count(x) AS c") == [(0,)]


def test_timeseries_sub_node_with_only_parent_fk_gets_one_edge_per_node(tmp_path):
    _csv(tmp_path / "fields.csv", pd.DataFrame({"field_id": [1, 2], "name": ["Troll", "Ekofisk"]}))
    _csv(
        tmp_path / "prod.csv",
        pd.DataFrame(
            {
                "field_id": [1, 1, 1, 2, 2, 2],
                "year": [2020] * 6,
                "month": [1, 2, 3, 1, 2, 3],
                "oil": [1.0, 1.5, 2.0, 0.5, 0.6, 0.7],
            }
        ),
    )
    sub = {
        "csv": "prod.csv",
        "pk": "field_id",
        "parent_fk": "field_id",
        "properties": {},
        "skipped": [],
        "timeseries": {
            "time_key": {"year": "year", "month": "month"},
            "resolution": "month",
            "channels": {"oil": "oil"},
        },
    }
    nodes = {
        "Field": {"csv": "fields.csv", "pk": "field_id", "title": "name", "sub_nodes": {"Production": sub}},
    }
    g = _build(tmp_path, nodes)
    assert _rows(g, "MATCH (p:Production)-[x:OF_FIELD]->(:Field) RETURN count(x) AS c") == [(2,)]
    assert _rows(g, "MATCH (p:Production) RETURN count(p) AS c") == [(2,)]


# --- the parent link is resolved once: name, explicit edges, no stubs ---------


def _units(tmp_path, ids=(1, 2)):
    ids = list(ids)
    _csv(tmp_path / "units.csv", pd.DataFrame({"unit_id": ids, "unit_name": ["U1", "U2"]}))
    _csv(
        tmp_path / "headcount.csv",
        pd.DataFrame(
            {
                "unit_id": [ids[0], ids[0], ids[1]],
                "unit_name": ["U1", "U1", "U2"],
                "note": ["a", "b", "c"],
            }
        ),
    )


def _unit_nodes(parent_pk="unit_id", **sub):
    spec = {"csv": "headcount.csv", "pk": "auto", "title": "note"}
    spec.update(sub)
    unit = {"csv": "units.csv", "pk": parent_pk, "title": "unit_name", "sub_nodes": {"Headcount": spec}}
    return {"OrgUnit": unit}


def _build_warned(tmp_path, nodes):
    with warnings.catch_warnings(record=True) as caught:
        warnings.simplefilter("always")
        g = _build(tmp_path, nodes)
    return g, [str(w.message) for w in caught]


@pytest.fixture(params=["buffered", "streamed"])
def loader(request, monkeypatch):
    if request.param == "streamed":
        monkeypatch.setenv("KGLITE_BLUEPRINT_STREAMING_THRESHOLD_MB", "0")
    return request.param


def _edge_types(g):
    return _rows(g, "MATCH (:Headcount)-[x]->() RETURN type(x) AS t, count(x) AS c ORDER BY t")


def _nodes(g, label):
    return _rows(g, f"MATCH (n:{label}) RETURN count(n) AS c")


def test_multi_word_parent_gets_a_word_split_implicit_name(tmp_path, loader):
    _units(tmp_path)
    g = _build(tmp_path, _unit_nodes(parent_fk="unit_id"))
    assert _edge_types(g) == [("OF_ORG_UNIT", 3)]


def test_top_level_parent_spec_gets_a_word_split_implicit_name(tmp_path):
    _units(tmp_path)
    nodes = {
        "OrgUnit": {"csv": "units.csv", "pk": "unit_id", "title": "unit_name"},
        "Headcount": {
            "csv": "headcount.csv",
            "pk": "auto",
            "title": "note",
            "parent": "OrgUnit",
            "parent_fk": "unit_id",
        },
    }
    g = _build(tmp_path, nodes)
    assert _edge_types(g) == [("OF_ORG_UNIT", 3)]


@pytest.mark.parametrize("storage", ["default", "mapped", "disk"])
def test_explicit_edge_with_a_name_column_parent_fk_writes_only_the_explicit_edge(tmp_path, storage, loader):
    _units(tmp_path)
    sub = {
        "parent_fk": "unit_name",
        "connections": {"fk_edges": {"OF_ORG_UNIT": {"target": "OrgUnit", "fk": "unit_id"}}},
    }
    bp = {"settings": {"root": str(tmp_path)}, "nodes": _unit_nodes(**sub)}
    (tmp_path / "bp.json").write_text(json.dumps(bp), encoding="utf-8")
    kwargs = {"storage": storage, "save": False}
    if storage == "disk":
        kwargs["path"] = str(tmp_path / "disk")
    with warnings.catch_warnings(record=True) as caught:
        warnings.simplefilter("always")
        g = from_blueprint(tmp_path / "bp.json", **kwargs)
    assert _edge_types(g) == [("OF_ORG_UNIT", 3)]
    assert _nodes(g, "OrgUnit") == [(2,)]
    assert _rows(g, "MATCH (n) RETURN count(n) AS c") == [(5,)]
    assert not [w for w in caught if "stub" in str(w.message)]


def test_unresolvable_parent_fk_without_an_explicit_edge_writes_no_edge_and_no_stub(tmp_path, loader):
    _units(tmp_path)
    g, caught = _build_warned(tmp_path, _unit_nodes(parent_fk="unit_name"))
    assert _edge_types(g) == []
    assert _nodes(g, "OrgUnit") == [(2,)]
    assert _rows(g, "MATCH (n) RETURN count(n) AS c") == [(5,)]
    msgs = [w for w in caught if "parent_fk 'unit_name' matched no 'OrgUnit' pk" in w]
    assert len(msgs) == 1, caught
    assert "3 row(s)" in msgs[0] and "U1" in msgs[0] and "OF_ORG_UNIT" in msgs[0]


def test_partly_resolvable_parent_fk_keeps_the_resolved_rows(tmp_path, loader):
    _units(tmp_path)
    _csv(
        tmp_path / "headcount.csv",
        pd.DataFrame({"unit_id": [1, 2, 99, 98], "unit_name": ["U1", "U2", "x", "y"], "note": list("abcd")}),
    )
    g, caught = _build_warned(tmp_path, _unit_nodes(parent_fk="unit_id"))
    assert _edge_types(g) == [("OF_ORG_UNIT", 2)]
    assert _nodes(g, "OrgUnit") == [(2,)]
    assert _nodes(g, "Headcount") == [(4,)]
    msgs = [w for w in caught if "matched no 'OrgUnit' pk" in w]
    assert len(msgs) == 1 and "2 row(s)" in msgs[0] and "99" in msgs[0], caught


def test_explicit_edges_keep_vivifying_their_missing_targets(tmp_path):
    _units(tmp_path)
    sub = {
        "parent_fk": "unit_name",
        "connections": {"fk_edges": {"OF_ORG_UNIT": {"target": "OrgUnit", "fk": "unit_name"}}},
    }
    g, _ = _build_warned(tmp_path, _unit_nodes(**sub))
    assert _edge_types(g) == [("OF_ORG_UNIT", 3)]
    assert _nodes(g, "OrgUnit") == [(4,)]


def test_auto_pk_parent_writes_no_implicit_edge_and_warns(tmp_path, loader):
    _units(tmp_path)
    g, caught = _build_warned(tmp_path, _unit_nodes(parent_pk="auto", parent_fk="unit_id"))
    assert _edge_types(g) == []
    assert _nodes(g, "OrgUnit") == [(2,)]
    msgs = [w for w in caught if "parent_fk 'unit_id' writes no edge" in w]
    assert len(msgs) == 1, caught
    assert "'OrgUnit' has pk \"auto\"" in msgs[0] and "fk_edges" in msgs[0]


def test_auto_pk_parent_with_an_explicit_edge_does_not_warn(tmp_path):
    _units(tmp_path)
    sub = {
        "parent_fk": "unit_id",
        "connections": {"fk_edges": {"OF_ORG_UNIT": {"target": "OrgUnit", "fk": "unit_id"}}},
    }
    g, caught = _build_warned(tmp_path, _unit_nodes(parent_pk="auto", **sub))
    assert _edge_types(g) == [("OF_ORG_UNIT", 3)]
    assert not [w for w in caught if "writes no edge" in w]


def test_missing_parent_fk_column_names_parent_fk(tmp_path, loader, capfd):
    _units(tmp_path)
    bp = {"settings": {"root": str(tmp_path)}, "nodes": _unit_nodes(parent_fk="no_such_column")}
    (tmp_path / "bp.json").write_text(json.dumps(bp), encoding="utf-8")
    with warnings.catch_warnings(record=True) as caught:
        warnings.simplefilter("always")
        try:
            from_blueprint(tmp_path / "bp.json", save=False)
            raised = ""
        except Exception as e:  # noqa: BLE001 - the report may raise or print
            raised = str(e)
    text = raised + " ".join(str(w.message) for w in caught) + capfd.readouterr().err
    assert "parent_fk column 'no_such_column' not found" in text, text
    assert "OF_ORG_UNIT" not in text
