"""Export/import defects found in review: each test failed on the tree before its fix.

Naming is HR/org-chart only. Every round trip goes through the real exporter
and the real importer.
"""

import pandas as pd
import pytest

import kglite
from kglite import KnowledgeGraph


def _titles(g, label):
    return g.cypher(f"MATCH (n:{label}) RETURN n.id AS id, n.title AS title ORDER BY id").to_list()


def _null_title_graph():
    g = KnowledgeGraph()
    g.add_nodes(
        pd.DataFrame({"id": [1, 2, 3], "code": pd.array([5, None, 9], dtype="Int64")}),
        "Badge",
        "id",
        "code",
    )
    g.add_nodes(pd.DataFrame({"id": [1, 2, 3], "name": ["a", None, "c"]}), "Person", "id", "name")
    return g


def test_rdf_round_trip_keeps_a_null_title_null(tmp_path):
    g = _null_title_graph()
    path = str(tmp_path / "g.nq")
    g.export_rdf(path)
    back = kglite.load_rdf(path)
    for label in ("Badge", "Person"):
        assert _titles(back, label) == _titles(g, label), label
        assert back.cypher(f"MATCH (n:{label}) WHERE n.title IS NULL RETURN n.id AS id").to_list() == [{"id": 2}]


def test_empty_export_destinations_are_refused(tmp_path, monkeypatch):
    monkeypatch.chdir(tmp_path)
    g = _null_title_graph()
    with pytest.raises(OSError, match="must not be empty"):
        g.export_csv("")
    with pytest.raises((OSError, ValueError), match="must not be empty"):
        g.export_rdf("")
    assert list(tmp_path.iterdir()) == []


def _type_property_graph():
    g = KnowledgeGraph()
    g.cypher("CREATE (:Person {id: 1, title: 'Ada', type: 'manager', name: 'Ada L'})")
    g.cypher("CREATE (:Person {id: 2, title: 'Bo', type: 'contractor'})")
    g.cypher("CREATE (:Person {id: 3, title: 'Cy'})")
    return g


def _type_values(g):
    return g.cypher("MATCH (n:Person) RETURN n.id AS id, n.type AS t ORDER BY id").to_list()


def test_a_property_named_type_survives_csv(tmp_path):
    g = _type_property_graph()
    out = str(tmp_path / "csv")
    g.export_csv(out)
    back = kglite.from_blueprint(out + "/blueprint.json", save=False)
    assert _type_values(back) == _type_values(g)
    assert _type_values(g)[0] == {"id": 1, "t": "manager"}


@pytest.mark.parametrize("fmt", ["nq", "trig"])
def test_a_property_named_type_survives_rdf(tmp_path, fmt):
    g = _type_property_graph()
    path = str(tmp_path / f"g.{fmt}")
    g.export_rdf(path, format=fmt)
    back = kglite.load_rdf(path)
    assert _type_values(back) == _type_values(g)
