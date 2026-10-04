"""The abutment count and warning of a validity declaration compare versions of
one entity, not unrelated rows of a label.

Red proof: before grouping by entity, every node label was one group, so two
projects whose phases shared a boundary day were counted and warned about as if
one project's period ended where its successor began.
"""

import json
import warnings

import pandas as pd

import kglite
from kglite.blueprint import from_blueprint

DECLARATIONS = (
    "CALL db.temporal.declarations() YIELD kind, name, convention, abutting_rows "
    "RETURN kind, name, convention, abutting_rows"
)


def _declare(g, convention):
    res = g.cypher(
        f"CALL db.temporal.declare({{node: 'Project', from: 'vf', to: 'vt', convention: '{convention}'}}) "
        "YIELD declared, abutting_rows RETURN declared, abutting_rows"
    )
    return res.to_list(), list(res.warnings)


def _graph(rows):
    g = kglite.KnowledgeGraph()
    g.cypher("CREATE " + ", ".join(f"(:Project {{id: {i}, vf: '{vf}', vt: {vt}}})" for i, vf, vt in rows))
    return g


def test_different_entities_sharing_a_boundary_day_are_not_counted_as_abutting():
    data = [(1, "2000-01-01", "'2005-01-01'"), (2, "2005-01-01", "'2009-01-01'"), (3, "2009-01-01", "null")]
    g = _graph(data)
    rows, warns = _declare(g, "closed")
    assert rows == [{"declared": True, "abutting_rows": 0}]
    assert g.cypher(DECLARATIONS).to_list()[0]["abutting_rows"] == 0
    # The remainder is reported apart, worded as possibly unrelated.
    assert len(warns) == 1
    assert warns[0].startswith(
        "2 of 3 rows of node label 'Project' end on the day another row with a different node id begins; "
        "they belong to different entities and may be unrelated"
    )
    assert "same node id" not in warns[0]
    rows, warns = _declare(_graph(data), "half_open")
    assert rows == [{"declared": True, "abutting_rows": 0}]
    assert warns == []


def test_versions_of_one_id_abut_and_warn_only_when_closed():
    data = [(1, "2000-01-01", "'2005-01-01'"), (1, "2005-01-01", "null"), (2, "2005-01-01", "'2007-01-01'")]
    rows, warns = _declare(_graph(data), "closed")
    assert rows == [{"declared": True, "abutting_rows": 1}]
    assert any(
        w.startswith("1 of 3 rows of node label 'Project' end on the day another row with the same node id begins")
        for w in warns
    ), warns
    rows, warns = _declare(_graph(data), "half_open")
    assert rows == [{"declared": True, "abutting_rows": 1}]
    assert not any("end on the day" in w for w in warns), warns


def _blueprint(tmp_path, convention, explicit_edge=None):
    pd.DataFrame({"project_id": [1, 2], "name": ["Alpha", "Beta"]}).to_csv(tmp_path / "projects.csv", index=False)
    pd.DataFrame(
        {
            "phase_id": [10, 11, 12],
            "project_id": [1, 1, 2],
            "project_name": ["Alpha", "Alpha", "Beta"],
            "label": ["design", "build", "survey"],
            # Alpha's design ends the day its build begins. Beta's survey ends
            # the day Alpha's design begins, which is another project's row.
            "sf": ["2020-01-01", "2020-06-01", "2019-01-01"],
            "st": ["2020-06-01", "2020-12-31", "2020-01-01"],
        }
    ).to_csv(tmp_path / "phases.csv", index=False)
    phase = {
        "csv": "phases.csv",
        "pk": "phase_id",
        "title": "label",
        "parent_fk": "project_id",
        "skipped": ["project_id"],
        "properties": {"sf": "validFrom", "st": "validTo"},
        "temporal": {"from": "sf", "to": "st", "convention": convention},
    }
    if explicit_edge:
        # `parent_fk` names a column the parent's pk cannot match, so only the
        # declared edge links a phase to its project.
        phase["parent_fk"] = "project_name"
        phase["skipped"] = ["project_id", "project_name"]
        phase["connections"] = {"fk_edges": {explicit_edge: {"target": "Project", "fk": "project_id"}}}
    bp = {
        "settings": {"root": str(tmp_path)},
        "nodes": {
            "Project": {"csv": "projects.csv", "pk": "project_id", "title": "name", "sub_nodes": {"Phase": phase}}
        },
    }
    path = tmp_path / "bp.json"
    path.write_text(json.dumps(bp), encoding="utf-8")
    with warnings.catch_warnings(record=True) as caught:
        warnings.simplefilter("always")
        g = from_blueprint(path, save=False)
    return g, [str(w.message) for w in caught]


def _phase_abutting(g):
    rows = g.cypher(DECLARATIONS).to_list()
    return [r["abutting_rows"] for r in rows if r["name"] == "Phase"]


def test_blueprint_sub_node_versions_group_by_parent_edge_closed(tmp_path):
    g, caught = _blueprint(tmp_path, "closed")
    assert _phase_abutting(g) == [1]
    assert any(
        "1 of 3 rows of node label 'Phase' end on the day another row of the same parent node begins" in w
        for w in caught
    ), caught


def test_blueprint_sub_node_versions_group_by_parent_edge_half_open(tmp_path):
    g, caught = _blueprint(tmp_path, "half_open")
    assert _phase_abutting(g) == [1]
    assert not any("end on the day" in w for w in caught), caught


def test_blueprint_sub_node_versions_group_by_the_explicit_parent_edge(tmp_path):
    g, caught = _blueprint(tmp_path, "closed", explicit_edge="BELONGS_TO_PROJECT")
    assert g.cypher("FOR VALID_TIME ALL MATCH ()-[x]->() RETURN DISTINCT type(x) AS t").to_list() == [
        {"t": "BELONGS_TO_PROJECT"}
    ]
    assert g.cypher("MATCH (p:Project) RETURN count(p) AS c").to_list() == [{"c": 2}]
    assert _phase_abutting(g) == [1]
    assert any("end on the day another row of the same parent node begins" in w for w in caught), caught
