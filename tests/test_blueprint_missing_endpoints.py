"""Blueprint edges to endpoints no node row supplies.

An endpoint type the build declares valid-time on is dropped by default (a
stub of it carries no bounds, so it is valid at every instant and counts in
every default-today read); an undeclared type still gets a provisional stub.
``on_missing_endpoint`` on an ``fk_edges`` / ``junction_edges`` entry, or under
``settings``, picks ``vivify``, ``drop`` or ``error``.
"""

import json
import warnings

import pytest

from kglite import from_blueprint

DEPARTMENT = {
    "csv": "dept.csv",
    "pk": "did",
    "title": "name",
    "properties": {"vf": "validFrom", "vt": "validTo"},
    "temporal": {"from": "vf", "to": "vt", "convention": "closed"},
}
TEAM = {"csv": "team.csv", "pk": "tid", "title": "name"}
CSVS = {
    "dept.csv": "did,name,vf,vt\nd1,Ops,2020-01-01,2030-01-01\nd2,Sales,2020-01-01,2030-01-01\n",
    "team.csv": "tid,name\nt1,Core\n",
    "person.csv": "pid,name,dept,team\np1,Ann,d1,t1\np2,Bo,d9,t1\np3,Cy,d2,t9\np4,Di,d9,t9\n",
    "member.csv": "pid,did\np1,d1\np2,d9\np3,d9\n",
}


def _person(dept_edge=None, team_edge=None, junction=None):
    dept = {"target": "Department", "fk": "dept", **(dept_edge or {})}
    team = {"target": "Team", "fk": "team", **(team_edge or {})}
    connections = {"fk_edges": {"WORKS_IN": dept, "IN_TEAM": team}}
    if junction is not None:
        connections["junction_edges"] = {
            "MEMBER_OF": {
                "csv": "member.csv",
                "source_fk": "pid",
                "target": "Department",
                "target_fk": "did",
                **junction,
            }
        }
    return {"csv": "person.csv", "pk": "pid", "title": "name", "connections": connections}


def _build(tmp_path, person, settings=None, department=DEPARTMENT, **kw):
    for name, text in CSVS.items():
        (tmp_path / name).write_text(text, encoding="utf-8")
    nodes = {"Department": department, "Team": TEAM, "Person": person}
    bp = {"settings": {"root": str(tmp_path), **(settings or {})}, "nodes": nodes}
    path = tmp_path / "bp.json"
    path.write_text(json.dumps(bp), encoding="utf-8")
    with warnings.catch_warnings(record=True) as caught:
        warnings.simplefilter("always")
        graph = from_blueprint(path, save=False, **kw)
    return graph, [str(w.message) for w in caught]


def _count(graph, query):
    return graph.cypher(query).to_list()[0]["n"]


@pytest.fixture(params=["default", "mapped", "disk"])
def storage_kw(request, tmp_path):
    if request.param == "disk":
        return {"storage": "disk", "path": str(tmp_path / "disk_graph")}
    return {"storage": request.param}


def _kinds(graph):
    return {d["kind"]: d for d in graph.graph_info()["build"]["diagnostics"]}


def test_declared_endpoint_is_dropped_and_counted(tmp_path, storage_kw):
    graph, _ = _build(tmp_path, _person(), **storage_kw)
    assert _count(graph, "FOR VALID_TIME ALL MATCH (d:Department) RETURN count(d) AS n") == 2
    assert _count(graph, "FOR VALID_TIME ALL MATCH (:Person)-[r:WORKS_IN]->() RETURN count(r) AS n") == 2
    assert _count(graph, "MATCH (d:Department) WHERE d.id = 'd9' RETURN count(d) AS n") == 0
    dropped = _kinds(graph)["endpoints_dropped_declared"]
    assert dropped["group"] == "stubs"
    assert "[Person] -[WORKS_IN]-> Department: dropped 2 row(s) naming 1 'Department' id(s)" in dropped["message"]
    assert '(e.g. "d9")' in dropped["message"]


def test_undeclared_endpoint_still_vivifies(tmp_path, storage_kw):
    graph, _ = _build(tmp_path, _person(), **storage_kw)
    assert _count(graph, "MATCH (t:Team) WHERE t.id = 't9' RETURN count(t) AS n") == 1
    assert _count(graph, "MATCH (:Person)-[r:IN_TEAM]->() RETURN count(r) AS n") == 4
    info = _kinds(graph)
    assert info["stubs_vivified"]["group"] == "stubs"
    assert "Team" in info["stubs_vivified"]["message"]


def test_a_dropped_row_vivifies_no_stub_for_its_other_endpoint(tmp_path, storage_kw):
    graph, _ = _build(tmp_path, _person(junction={}), **storage_kw)
    # p4 names d9 (dropped) and t9 (a stub) on separate edges; neither row of a
    # single edge loses its other endpoint, so the junction check is: no Person
    # stub for the source of a dropped junction row.
    assert _count(graph, "MATCH (p:Person) RETURN count(p) AS n") == 4
    assert _count(graph, "FOR VALID_TIME ALL MATCH (:Person)-[r:MEMBER_OF]->() RETURN count(r) AS n") == 1
    messages = [d["message"] for d in graph.graph_info()["build"]["diagnostics"]]
    assert any("-[MEMBER_OF]-> Department: dropped 2 row(s)" in m for m in messages)


def test_valid_time_reads_show_no_stub_of_a_declared_type(tmp_path, storage_kw):
    graph, _ = _build(tmp_path, _person(), **storage_kw)
    assert _count(graph, "MATCH (d:Department) RETURN count(d) AS n") == 2
    assert _count(graph, "FOR VALID_TIME ALL MATCH (d:Department) RETURN count(d) AS n") == 2
    other = tmp_path / "vivify"
    other.mkdir()
    vivified, _ = _build(other, _person(dept_edge={"on_missing_endpoint": "vivify"}))
    # The stub a vivify policy creates is valid at every instant: default-today
    # and ALL reads both count it, which is what auto avoids.
    assert _count(vivified, "MATCH (d:Department) RETURN count(d) AS n") == 3
    assert _count(vivified, "FOR VALID_TIME ALL MATCH (d:Department) RETURN count(d) AS n") == 3


@pytest.mark.parametrize("policy, departments, edges", [("vivify", 3, 4), ("drop", 2, 2)])
def test_per_edge_override(tmp_path, storage_kw, policy, departments, edges):
    graph, _ = _build(tmp_path, _person(dept_edge={"on_missing_endpoint": policy}), **storage_kw)
    assert _count(graph, "FOR VALID_TIME ALL MATCH (d:Department) RETURN count(d) AS n") == departments
    assert _count(graph, "FOR VALID_TIME ALL MATCH (:Person)-[r:WORKS_IN]->() RETURN count(r) AS n") == edges


def test_per_edge_drop_on_an_undeclared_type_is_counted(tmp_path, storage_kw):
    graph, _ = _build(tmp_path, _person(team_edge={"on_missing_endpoint": "drop"}), **storage_kw)
    assert _count(graph, "MATCH (t:Team) WHERE t.id = 't9' RETURN count(t) AS n") == 0
    assert _count(graph, "MATCH (:Person)-[r:IN_TEAM]->() RETURN count(r) AS n") == 2
    info = _kinds(graph)
    assert "endpoints_dropped" in info and info["endpoints_dropped"]["group"] == "stubs"
    assert "stubs_vivified" not in info


def test_error_policy_fails_the_build(tmp_path, storage_kw):
    with pytest.raises(ValueError, match=r"on_missing_endpoint 'error'.*\[Person\] -\[WORKS_IN\]-> Department.*d9"):
        _build(tmp_path, _person(dept_edge={"on_missing_endpoint": "error"}), **storage_kw)


def test_junction_error_policy_fails_the_build(tmp_path):
    with pytest.raises(ValueError, match=r"on_missing_endpoint 'error'.*MEMBER_OF"):
        _build(tmp_path, _person(junction={"on_missing_endpoint": "error"}))


def test_settings_default_applies_and_the_edge_overrides_it(tmp_path):
    graph, _ = _build(tmp_path, _person(), settings={"on_missing_endpoint": "vivify"})
    assert _count(graph, "FOR VALID_TIME ALL MATCH (d:Department) RETURN count(d) AS n") == 3
    graph, _ = _build(
        tmp_path,
        _person(dept_edge={"on_missing_endpoint": "drop"}),
        settings={"on_missing_endpoint": "vivify"},
    )
    assert _count(graph, "FOR VALID_TIME ALL MATCH (d:Department) RETURN count(d) AS n") == 2
    graph, _ = _build(tmp_path, _person(), settings={"on_missing_endpoint": "drop"})
    assert _count(graph, "MATCH (t:Team) WHERE t.id = 't9' RETURN count(t) AS n") == 0
    with pytest.raises(ValueError, match="unknown variant `sometimes`, expected one of `auto`, `vivify`"):
        _build(tmp_path, _person(), settings={"on_missing_endpoint": "sometimes"})


def test_strict_stubs_catches_dropped_endpoints(tmp_path):
    # The Team stubs are vivified (stubs group too), so pin the declared-type
    # drop through its own message.
    with pytest.raises(ValueError, match=r"(?s)\[stubs\].*dropped 2 row\(s\)"):
        _build(tmp_path, _person(team_edge={"on_missing_endpoint": "vivify"}), strict=["stubs"])
    graph, _ = _build(tmp_path, _person(), strict=["data_quality"])
    assert graph is not None


def test_a_declared_type_with_no_stray_endpoints_is_silent(tmp_path):
    (tmp_path / "person.csv").write_text("pid,name,dept,team\np1,Ann,d1,t1\n", encoding="utf-8")
    for name, text in CSVS.items():
        if name != "person.csv":
            (tmp_path / name).write_text(text, encoding="utf-8")
    nodes = {"Department": DEPARTMENT, "Team": TEAM, "Person": _person()}
    path = tmp_path / "bp.json"
    path.write_text(json.dumps({"settings": {"root": str(tmp_path)}, "nodes": nodes}), encoding="utf-8")
    with warnings.catch_warnings():
        warnings.simplefilter("error")
        graph = from_blueprint(path, save=False, strict=True)
    assert graph.graph_info()["build"]["summary"] == {}
