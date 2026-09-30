"""Whole-number float id columns outside the compact 32-bit key range.

pandas turns an integer column that has a missing value into ``float64``, so an
employee-number column with one blank arrives as floats. A float id column was
sent to the compact ``u32`` key type unconditionally: every whole number from
``2**32`` up (and every negative) failed the conversion and its row was dropped
with only a "null values in ID field" warning, although the value was a perfectly
good integer. A float id that is *not* a whole number has no integer to keep and
is refused rather than skipped.
"""

import pandas as pd
import pytest

import kglite
from kglite import KnowledgeGraph

# Employee numbers as 13-digit registry-style keys; every one is exactly
# representable as a float64.
BIG = [3_100_000_000_001.0, 3_100_000_000_002.0, 3_100_000_000_003.0]
NAMES = ["Ada Lovelace", "Grace Hopper", "Edsger Dijkstra"]


def _employees(ids, names=NAMES):
    return pd.DataFrame({"emp_id": pd.Series(ids, dtype="float64"), "name": names})


class TestFloatNodeIds:
    def test_whole_number_floats_beyond_u32_load_every_row(self, recwarn):
        graph = KnowledgeGraph()
        report = graph.add_nodes(_employees(BIG), "Employee", "emp_id", "name")
        assert report["nodes_created"] == 3
        assert report["nodes_skipped"] == 0
        assert report["has_errors"] is False
        assert [str(w.message) for w in recwarn] == []
        loaded = graph.cypher("MATCH (n:Employee) RETURN n.id AS i ORDER BY i").to_df()
        assert loaded["i"].tolist() == [int(v) for v in BIG]
        assert graph.node("Employee", 3_100_000_000_002)["title"] == "Grace Hopper"
        assert graph.schema()["node_types"]["Employee"]["properties"]["emp_id"] == "Int64"

    def test_a_blank_among_wide_floats_is_the_only_skipped_row(self):
        graph = KnowledgeGraph()
        ids = [3_100_000_000_001.0, float("nan"), 3_100_000_000_003.0]
        with pytest.warns(UserWarning, match="1 of 3 rows skipped"):
            report = graph.add_nodes(_employees(ids), "Employee", "emp_id", "name")
        assert (report["nodes_created"], report["nodes_skipped"]) == (2, 1)

    def test_negative_whole_floats_are_kept(self):
        graph = KnowledgeGraph()
        report = graph.add_nodes(_employees([-1.0, 0.0, 7.0]), "Employee", "emp_id", "name")
        assert report["nodes_created"] == 3
        assert graph.node("Employee", -1)["title"] == "Ada Lovelace"

    def test_floats_that_all_fit_u32_keep_the_compact_key(self):
        # Without this the wide-float assertion above would not show that the
        # narrow shape is still narrow: it keeps the compact key it always had.
        graph = KnowledgeGraph()
        report = graph.add_nodes(_employees([1.0, 2.0, 4294967295.0]), "Employee", "emp_id", "name")
        assert report["nodes_created"] == 3
        assert graph.schema()["node_types"]["Employee"]["properties"]["emp_id"] == "UniqueId"

    def test_wide_float_ids_survive_a_save_and_reload(self, tmp_path):
        graph = KnowledgeGraph()
        graph.add_nodes(_employees(BIG), "Employee", "emp_id", "name")
        path = tmp_path / "staff.kgl"
        graph.save(str(path))
        back = kglite.load(str(path))
        assert back.node("Employee", 3_100_000_000_003)["title"] == "Edsger Dijkstra"

    def test_a_fractional_float_id_is_refused_not_skipped(self):
        graph = KnowledgeGraph()
        with pytest.raises(kglite.ArgumentError) as excinfo:
            graph.add_nodes(_employees([1.0, 2.5, 3.0]), "Employee", "emp_id", "name")
        message = str(excinfo.value)
        assert "emp_id" in message and "2.5" in message, message
        assert graph.cypher("MATCH (n:Employee) RETURN count(n) AS c")[0]["c"] == 0

    def test_a_float_id_beyond_int64_is_refused_not_skipped(self):
        graph = KnowledgeGraph()
        with pytest.raises(kglite.ArgumentError, match="emp_id"):
            graph.add_nodes(_employees([1.0, 1e30, 3.0]), "Employee", "emp_id", "name")

    def test_bulk_loader_keeps_wide_float_ids(self):
        graph = KnowledgeGraph()
        counts = graph.add_nodes_bulk(
            [
                {
                    "node_type": "Employee",
                    "unique_id_field": "emp_id",
                    "node_title_field": "name",
                    "data": _employees(BIG),
                }
            ]
        )
        assert counts["Employee"] == 3


class TestFloatEndpointIds:
    def _org(self, node_ids):
        graph = KnowledgeGraph()
        graph.add_nodes(pd.DataFrame({"emp_id": node_ids, "name": NAMES}), "Employee", "emp_id", "name")
        return graph

    def test_float_endpoints_beyond_u32_create_every_edge(self):
        graph = self._org([int(v) for v in BIG])
        reports = pd.DataFrame(
            {
                "emp": pd.Series([BIG[1], BIG[2]], dtype="float64"),
                "boss": pd.Series([BIG[0], BIG[0]], dtype="float64"),
            }
        )
        report = graph.add_relationships(reports, "REPORTS_TO", "Employee", "emp", "Employee", "boss")
        assert report["connections_created"] == 2
        assert report["connections_skipped"] == 0
        rows = graph.cypher("MATCH (a:Employee)-[:REPORTS_TO]->(b:Employee) RETURN a.name AS a, b.name AS b ORDER BY a")
        assert [(r["a"], r["b"]) for r in rows] == [
            ("Edsger Dijkstra", "Ada Lovelace"),
            ("Grace Hopper", "Ada Lovelace"),
        ]

    def test_float_endpoints_match_float_loaded_nodes(self):
        graph = KnowledgeGraph()
        graph.add_nodes(_employees(BIG), "Employee", "emp_id", "name")
        reports = pd.DataFrame(
            {
                "emp": pd.Series([BIG[1]], dtype="float64"),
                "boss": pd.Series([BIG[0]], dtype="float64"),
            }
        )
        report = graph.add_relationships(reports, "REPORTS_TO", "Employee", "emp", "Employee", "boss")
        assert report["connections_created"] == 1

    def test_a_fractional_float_endpoint_is_refused_not_skipped(self):
        graph = self._org([int(v) for v in BIG])
        reports = pd.DataFrame({"emp": [BIG[1], BIG[2] + 0.5], "boss": [BIG[0], BIG[0]]})
        with pytest.raises(kglite.ArgumentError, match="emp"):
            graph.add_relationships(reports, "REPORTS_TO", "Employee", "emp", "Employee", "boss")
        assert graph.cypher("MATCH ()-[r:REPORTS_TO]->() RETURN count(r) AS c")[0]["c"] == 0

    def test_bulk_relationship_loader_keeps_wide_float_endpoints(self):
        graph = self._org([int(v) for v in BIG])
        reports = pd.DataFrame(
            {
                "source_id": pd.Series([BIG[1]], dtype="float64"),
                "target_id": pd.Series([BIG[0]], dtype="float64"),
            }
        )
        counts = graph.add_relationships_bulk(
            [
                {
                    "source_type": "Employee",
                    "target_type": "Employee",
                    "connection_name": "REPORTS_TO",
                    "data": reports,
                }
            ]
        )
        assert counts["REPORTS_TO"] == 1
