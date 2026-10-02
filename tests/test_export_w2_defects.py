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
