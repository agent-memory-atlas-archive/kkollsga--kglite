"""A file written by a build with a known data-shape bug warns at load, but only
when the data shows the bug: a writer-version precondition AND a data predicate,
against the oldest version that ever wrote the data (``oldest_writer``)."""

import json
import struct
import warnings

import pandas as pd
import pytest

import kglite
from kglite import KnowledgeGraph

CURRENT = kglite.__version__


def _stamp(path, library_version, oldest_writer=None):
    """Rewrite the metadata head of a saved ``.kgl`` as an older build wrote it."""
    raw = path.read_bytes()
    head = raw[:9]
    (length,) = struct.unpack("<I", raw[9:13])
    meta = json.loads(raw[13 : 13 + length])
    meta["library_version"] = library_version
    meta.pop("oldest_writer", None)
    if oldest_writer is not None:
        meta["oldest_writer"] = oldest_writer
    body = json.dumps(meta, separators=(",", ":")).encode("utf-8")
    path.write_bytes(head + struct.pack("<I", len(body)) + body + raw[13 + length :])


def _metadata(path):
    raw = path.read_bytes()
    (length,) = struct.unpack("<I", raw[9:13])
    return json.loads(raw[13 : 13 + length])


def _load(path):
    with warnings.catch_warnings(record=True) as caught:
        warnings.simplefilter("always")
        graph = kglite.load(str(path))
    return graph, [str(w.message) for w in caught if issubclass(w.category, UserWarning)]


def _multi_source_graph():
    g = KnowledgeGraph()
    g.add_nodes(pd.DataFrame({"id": [1, 2], "name": ["a", "b"]}), "Person", "id", "name")
    g.add_nodes(pd.DataFrame({"id": [10], "name": ["t"]}), "Team", "id", "name")
    g.add_nodes(pd.DataFrame({"id": [100], "name": ["r"]}), "Role", "id", "name")
    g.add_connections(
        pd.DataFrame({"s": [1, 2], "t": [100, 100], "since": [2020, 2021]}),
        "HAS_ROLE",
        "Person",
        "s",
        "Role",
        "t",
        columns=["since"],
    )
    g.add_connections(
        pd.DataFrame({"s": [10], "t": [100], "since": [2019]}),
        "HAS_ROLE",
        "Team",
        "s",
        "Role",
        "t",
        columns=["since"],
    )
    return g


def _single_source_graph():
    g = KnowledgeGraph()
    g.add_nodes(pd.DataFrame({"id": [1], "name": ["a"]}), "Person", "id", "name")
    g.add_nodes(pd.DataFrame({"id": [100], "name": ["r"]}), "Role", "id", "name")
    g.add_connections(
        pd.DataFrame({"s": [1], "t": [100], "since": [2020]}),
        "HAS_ROLE",
        "Person",
        "s",
        "Role",
        "t",
        columns=["since"],
    )
    return g


def _timeseries_graph(duplicate_edges):
    g = KnowledgeGraph()
    g.add_nodes(pd.DataFrame({"id": [1], "name": ["p"]}), "Parent", "id", "name")
    g.add_nodes(pd.DataFrame({"id": [7], "name": ["s"]}), "Series", "id", "name")
    g.set_timeseries("Series", resolution="month", channels=["v"])
    for _ in range(2 if duplicate_edges else 1):
        g.cypher("MATCH (s:Series {id: 7}), (p:Parent {id: 1}) CREATE (s)-[:OF_PARENT]->(p)")
    return g


def _implicit_edge_graph(both):
    g = KnowledgeGraph()
    g.add_nodes(pd.DataFrame({"id": [1], "name": ["p"]}), "ProjectPhase", "id", "name")
    g.add_nodes(pd.DataFrame({"id": [5], "name": ["s"]}), "Task", "id", "name")
    for edge in ("OF_PROJECT_PHASE", "OF_PROJECTPHASE") if both else ("OF_PROJECTPHASE",):
        g.cypher(f"MATCH (s:Task {{id: 5}}), (p:ProjectPhase {{id: 1}}) CREATE (s)-[:{edge}]->(p)")
    return g


def _saved(tmp_path, graph, stamp=None, oldest=None):
    path = tmp_path / "g.kgl"
    graph.save(str(path))
    if stamp:
        _stamp(path, stamp, oldest)
    return path


def test_folded_history_fires_for_an_old_writer_with_the_shape(tmp_path):
    path = _saved(tmp_path, _multi_source_graph(), "0.18.1")
    graph, warned = _load(path)
    assert len(warned) == 1 and "HAS_ROLE" in warned[0] and "0.18.1" in warned[0]
    advisory = graph.graph_info()["advisories"][0]
    assert advisory["code"] == "folded_history_edges"
    assert advisory["writer"] == "0.18.1"
    assert advisory["affected"] == ["HAS_ROLE"]
    assert "data-advisory" in graph.describe()


def test_the_data_predicate_keeps_a_clean_old_file_silent(tmp_path):
    graph, warned = _load(_saved(tmp_path, _single_source_graph(), "0.18.1"))
    assert warned == []
    assert "advisories" not in graph.graph_info()


def test_the_version_precondition_keeps_a_current_file_silent(tmp_path):
    graph, warned = _load(_saved(tmp_path, _multi_source_graph(), "0.19.0"))
    assert warned == []
    assert "advisories" not in graph.graph_info()
    assert "data-advisory" not in graph.describe()


def test_a_resave_under_the_current_version_does_not_launder_the_writer(tmp_path):
    graph, warned = _load(_saved(tmp_path, _multi_source_graph(), "0.18.1"))
    assert len(warned) == 1
    again = tmp_path / "again.kgl"
    graph.save(str(again))
    meta = _metadata(again)
    assert meta["library_version"] == CURRENT
    assert meta["oldest_writer"] == "0.18.1"
    _, warned_again = _load(again)
    assert len(warned_again) == 1


def test_a_fresh_save_carries_no_oldest_writer_key(tmp_path):
    path = _saved(tmp_path, _multi_source_graph())
    assert "oldest_writer" not in _metadata(path)
    _, warned = _load(path)
    assert warned == []


def test_an_explicit_oldest_writer_outranks_the_library_version(tmp_path):
    path = _saved(tmp_path, _multi_source_graph(), CURRENT, oldest="0.17.0")
    _, warned = _load(path)
    assert len(warned) == 1 and "0.17.0" in warned[0]


def test_timeseries_parent_copies_need_the_window_and_the_repeat(tmp_path):
    graph, warned = _load(_saved(tmp_path, _timeseries_graph(True), "0.19.1"))
    assert len(warned) == 1
    assert graph.graph_info()["advisories"][0]["code"] == "timeseries_parent_copies"
    assert graph.graph_info()["advisories"][0]["affected"] == ["Series"]
    # Clean data in the same window, and repeats under a fixed writer: silent.
    assert _load(_saved(tmp_path, _timeseries_graph(False), "0.19.1"))[1] == []
    assert _load(_saved(tmp_path, _timeseries_graph(True), "0.19.2"))[1] == []


def test_the_implicit_parent_edge_advisory_is_for_that_writer_only(tmp_path):
    graph, warned = _load(_saved(tmp_path, _implicit_edge_graph(True), "0.19.2"))
    assert len(warned) == 1 and "purge_provisional" in warned[0]
    assert graph.graph_info()["advisories"][0]["code"] == "implicit_parent_edge_duplicates"
    assert _load(_saved(tmp_path, _implicit_edge_graph(False), "0.19.2"))[1] == []
    assert _load(_saved(tmp_path, _implicit_edge_graph(True), "0.19.1"))[1] == []


def test_open_session_warns_too(tmp_path):
    path = _saved(tmp_path, _multi_source_graph(), "0.18.1")
    with pytest.warns(UserWarning, match="folded history"):
        kglite.open_session(str(path))
