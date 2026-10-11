"""Duration values through every tabular and record input.

A pandas ``timedelta64`` column, a pyarrow-backed duration column, an object
column of ``datetime.timedelta``, a polars ``Duration`` or pyarrow table given
as a blueprint frame, a ``from_records`` value and a query parameter all load
as a duration: equal to the ``duration()`` literal, ordered and added as one,
and kept across save and load.
"""

import datetime as dt
import json
import warnings

import pandas as pd
import pytest

import kglite

DAY_2H = "duration({days: 1, hours: 2})"


def node_graph(frame, **kwargs):
    g = kglite.KnowledgeGraph()
    g.add_nodes(frame, "T", "id", **kwargs, on_invalid="warn")
    return g


def durations(g, query="MATCH (n:T) RETURN n.id AS id, n.d AS d ORDER BY id"):
    return [row["d"] for row in g.cypher(query).to_list()]


def assert_duration_column(g, label="T"):
    rows = g.cypher(
        f"MATCH (n:{label}) RETURN n.id AS id, n.d = {DAY_2H} AS eq, date('2020-01-01') + n.d AS shifted ORDER BY id"
    ).to_list()
    assert [r["eq"] for r in rows] == [True, None]
    assert str(rows[0]["shifted"]) == "2020-01-02"


def test_timedelta64_column_loads_as_duration():
    frame = pd.DataFrame({"id": [1, 2], "d": pd.to_timedelta(["1 days 02:00:00", None])})
    g = node_graph(frame)
    assert_duration_column(g)
    assert durations(g) == [{"months": 0, "days": 1, "seconds": 7200}, None]


def test_negative_and_whole_second_values_keep_their_sign():
    frame = pd.DataFrame({"id": [1, 2], "d": pd.to_timedelta(["-1 hours", "90 seconds"])})
    g = node_graph(frame)
    rows = g.cypher(
        "MATCH (n:T) RETURN n.id AS id, n.d = duration({hours: -1}) AS neg, "
        "n.d = duration({seconds: 90}) AS pos ORDER BY id"
    ).to_list()
    assert [(r["neg"], r["pos"]) for r in rows] == [(True, False), (False, True)]


def test_sub_second_values_keep_their_nanoseconds():
    frame = pd.DataFrame({"id": [1, 2, 3], "d": pd.to_timedelta(["1.5 seconds", "2 seconds", "-1500000001 ns"])})
    with warnings.catch_warnings():
        warnings.simplefilter("error")
        g = node_graph(frame)
    assert durations(g) == [
        {"months": 0, "days": 0, "seconds": 1, "nanoseconds": 500_000_000},
        {"months": 0, "days": 0, "seconds": 2},
        {"months": 0, "days": 0, "seconds": -2, "nanoseconds": 499_999_999},
    ]


def test_object_column_of_timedelta_loads_as_duration():
    frame = pd.DataFrame({"id": [1, 2], "d": pd.Series([dt.timedelta(days=1, hours=2), None], dtype=object)})
    with warnings.catch_warnings():
        warnings.simplefilter("error")
        g = node_graph(frame)
    assert_duration_column(g)


def test_declared_duration_column_type():
    frame = pd.DataFrame({"id": [1, 2], "d": pd.to_timedelta(["1 days 02:00:00", None])})
    g = node_graph(frame, column_types={"d": "duration"})
    assert_duration_column(g)


def test_pyarrow_backed_duration_column():
    pa = pytest.importorskip("pyarrow")
    frame = pd.DataFrame(
        {
            "id": [1, 2],
            "d": pd.array([dt.timedelta(days=1, hours=2), None], dtype=pd.ArrowDtype(pa.duration("us"))),
        }
    )
    assert_duration_column(node_graph(frame))


def test_relationship_duration_property():
    g = kglite.KnowledgeGraph()
    g.add_nodes(pd.DataFrame({"id": [1, 2]}), "T", "id")
    g.add_connections(
        pd.DataFrame({"s": [1], "t": [2], "d": pd.to_timedelta(["1 days 02:00:00"])}),
        "R",
        "T",
        "s",
        "T",
        "t",
    )
    assert g.cypher(f"MATCH ()-[r:R]->() RETURN r.d = {DAY_2H} AS eq").to_list() == [{"eq": True}]


def test_duration_survives_save_and_load(tmp_path):
    frame = pd.DataFrame({"id": [1, 2], "d": pd.to_timedelta(["1 days 02:00:00", None])})
    g = node_graph(frame)
    path = tmp_path / "g.kgl"
    g.save(str(path))
    assert_duration_column(kglite.load(str(path)))


def test_query_parameter_timedelta_binds_a_duration():
    g = kglite.KnowledgeGraph()
    rows = g.cypher(
        f"RETURN $d = {DAY_2H} AS eq, $neg = duration({{hours: -1}}) AS neg",
        params={"d": dt.timedelta(days=1, hours=2), "neg": dt.timedelta(hours=-1)},
    ).to_list()
    assert rows == [{"eq": True, "neg": True}]


def test_from_records_types_timedelta_date_and_datetime():
    g = kglite.from_records(
        {
            "nodes": [
                {
                    "type": "T",
                    "id_field": "id",
                    "records": [
                        {
                            "id": 1,
                            "d": dt.timedelta(days=1, hours=2),
                            "on": dt.date(2020, 1, 1),
                            "at": dt.datetime(2020, 1, 1, 10, tzinfo=dt.timezone(dt.timedelta(hours=2))),
                        },
                        {"id": 2, "d": {"$duration": {"days": 1, "seconds": 7200}}},
                    ],
                }
            ]
        }
    )
    rows = g.cypher(
        f"MATCH (n:T) RETURN n.id AS id, n.d = {DAY_2H} AS eq, n.on = date('2020-01-01') AS on, "
        "n.at = datetime('2020-01-01T08:00:00') AS at ORDER BY id"
    ).to_list()
    assert rows == [{"id": 1, "eq": True, "on": True, "at": True}, {"id": 2, "eq": True, "on": None, "at": None}]


def _frame_blueprint(tmp_path):
    bp = {
        "settings": {"root": str(tmp_path)},
        "files": {"rows": {"format": "frame"}},
        "nodes": {"T": {"file": "rows", "pk": "id"}},
    }
    (tmp_path / "bp.json").write_text(json.dumps(bp), encoding="utf-8")
    return tmp_path / "bp.json"


def test_blueprint_frame_duration_column(tmp_path):
    frame = pd.DataFrame({"id": [1, 2], "d": pd.to_timedelta(["1 days 02:00:00", None])})
    g = kglite.from_blueprint(_frame_blueprint(tmp_path), save=False, frames={"rows": frame})
    assert_duration_column(g)


def test_blueprint_polars_and_pyarrow_frames(tmp_path):
    pa = pytest.importorskip("pyarrow")
    table = pa.table({"id": [1, 2], "d": pa.array([dt.timedelta(days=1, hours=2), None], pa.duration("us"))})
    g = kglite.from_blueprint(_frame_blueprint(tmp_path), save=False, frames={"rows": table})
    assert_duration_column(g)
    pl = pytest.importorskip("polars")
    frame = pl.DataFrame({"id": [1, 2], "d": [dt.timedelta(days=1, hours=2), None]})
    g = kglite.from_blueprint(_frame_blueprint(tmp_path), save=False, frames={"rows": frame})
    assert_duration_column(g)


def test_blueprint_csv_duration_declaration(tmp_path):
    (tmp_path / "rows.csv").write_text(
        'id,d\n1,"{""days"": 1, ""seconds"": 7200}"\n2,\n3,not a duration\n', encoding="utf-8"
    )
    bp = {
        "settings": {"root": str(tmp_path)},
        "files": {"rows": {"path": "rows.csv", "format": "csv"}},
        "nodes": {"T": {"file": "rows", "pk": "id", "properties": {"d": "duration"}}},
    }
    (tmp_path / "bp.json").write_text(json.dumps(bp), encoding="utf-8")
    g = kglite.from_blueprint(tmp_path / "bp.json", save=False)
    rows = g.cypher(f"MATCH (n:T) RETURN n.id AS id, n.d = {DAY_2H} AS eq, n.d AS d ORDER BY id").to_list()
    assert [(r["eq"], r["d"]) for r in rows] == [
        (True, {"months": 0, "days": 1, "seconds": 7200}),
        (None, None),
        (None, None),
    ]


def test_query_parameter_timedelta_keeps_microseconds_and_pandas_nanoseconds():
    g = kglite.KnowledgeGraph()
    rows = g.cypher(
        "RETURN $us AS us, $ns AS ns, $neg AS neg, $us = duration({microseconds: 5}) AS eq",
        params={
            "us": dt.timedelta(microseconds=5),
            "ns": pd.Timedelta(1, "ns"),
            "neg": dt.timedelta(microseconds=-5),
        },
    ).to_list()
    assert rows == [
        {
            "us": {"months": 0, "days": 0, "seconds": 0, "nanoseconds": 5000},
            "ns": {"months": 0, "days": 0, "seconds": 0, "nanoseconds": 1},
            "neg": {"months": 0, "days": 0, "seconds": -1, "nanoseconds": 999_995_000},
            "eq": True,
        }
    ]


def test_cypher_nanosecond_duration_reads_back_with_the_key_only_when_non_zero():
    g = kglite.KnowledgeGraph()
    row = g.cypher("RETURN duration({nanoseconds: 1}) AS a, duration({seconds: 3}) AS b").to_list()[0]
    assert row["a"] == {"months": 0, "days": 0, "seconds": 0, "nanoseconds": 1}
    assert row["b"] == {"months": 0, "days": 0, "seconds": 3}


def test_from_records_timedelta_keeps_its_sub_second_part():
    spec = {
        "nodes": [
            {
                "type": "T",
                "id_field": "id",
                "records": [
                    {"id": 1, "d": dt.timedelta(seconds=1, microseconds=5)},
                    {"id": 2, "d": dt.timedelta(microseconds=-5)},
                    {"id": 3, "d": pd.Timedelta(7, "ns")},
                ],
            }
        ]
    }
    g = kglite.from_records(spec)
    assert durations(g) == [
        {"months": 0, "days": 0, "seconds": 1, "nanoseconds": 5000},
        {"months": 0, "days": 0, "seconds": -1, "nanoseconds": 999_995_000},
        {"months": 0, "days": 0, "seconds": 0, "nanoseconds": 7},
    ]
