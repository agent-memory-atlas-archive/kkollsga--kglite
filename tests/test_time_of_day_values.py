"""`LocalTime` and `Time` as real values: Cypher returns `datetime.time`, a
`datetime.time` parameter or DataFrame cell is stored as one, equality is by
instant of day, and the values survive every storage mode and a save and reload.

`localtime()` / `time()` used to return `HH:MM:SS` strings; every expectation
here is a hand-written value, not read back from the engine under test.
"""

import datetime as dt
import warnings

import pandas as pd
import pytest

import kglite

UTC = dt.timezone.utc


def tz(hours=0, minutes=0):
    return dt.timezone(dt.timedelta(hours=hours, minutes=minutes))


def one(graph, query, params=None):
    return graph.cypher(query, params=params).to_list()[0]["v"]


@pytest.fixture
def graph():
    return kglite.KnowledgeGraph()


def test_constructors_return_naive_and_aware_times(graph):
    assert one(graph, "RETURN localtime('10:15:30.123456') AS v") == dt.time(10, 15, 30, 123456)
    assert one(graph, "RETURN localtime('10:15') AS v").tzinfo is None
    zoned = one(graph, "RETURN time('21:40:32.142+01:00') AS v")
    assert zoned == dt.time(21, 40, 32, 142000, tzinfo=tz(1))
    assert zoned.utcoffset() == dt.timedelta(hours=1)
    assert one(graph, "RETURN time('10:00Z') AS v").utcoffset() == dt.timedelta(0)
    assert one(graph, "RETURN time({hour: 12, minute: 30, timezone: '-05:30'}) AS v") == dt.time(
        12, 30, tzinfo=tz(-5, -30)
    )


def test_text_and_accessors(graph):
    row = graph.cypher(
        "WITH time('21:40:32.142+01:00') AS t, localtime('09:05') AS l "
        "RETURN toString(t) AS ts, toString(l) AS ls, t.hour AS h, t.millisecond AS ms, "
        "t.offset AS off, t.offsetSeconds AS secs, t.timezone AS zone, l.minute AS m, l.offset AS loff"
    ).to_list()[0]
    assert row == {
        "ts": "21:40:32.142+01:00",
        "ls": "09:05",
        "h": 21,
        "ms": 142,
        "off": "+01:00",
        "secs": 3600,
        "zone": "+01:00",
        "m": 5,
        "loff": None,
    }


def test_a_time_of_day_cannot_take_a_named_zone(graph):
    # A named zone's offset depends on the date, which a time of day lacks.
    with pytest.raises(Exception, match="no date"):
        graph.cypher("RETURN time('10:00[Europe/Oslo]') AS v")
    with pytest.raises(Exception, match="no date"):
        graph.cypher("RETURN time({hour: 10, timezone: 'Europe/Oslo'}) AS v")
    # Reading the time of a datetime in the zone gives the offset it had that day.
    got = graph.cypher("RETURN time(datetime('2026-07-15T10:00[Europe/Oslo]')) AS v").to_list()[0]["v"]
    assert got.utcoffset() == dt.timedelta(hours=2) and got.hour == 10


def test_time_parameters_bind_by_kind(graph):
    naive = dt.time(10, 0, 0, 500000)
    aware = dt.time(10, 0, tzinfo=tz(1))
    row = graph.cypher(
        "RETURN $n AS n, $a AS a, $n = localtime('10:00:00.5') AS n_same, "
        "$a = time('09:00Z') AS a_same, $n = $a AS mixed, $a < time('09:35:14Z') AS before",
        params={"n": naive, "a": aware},
    ).to_list()[0]
    assert row["n"] == naive and row["n"].tzinfo is None
    assert row["a"] == aware and row["a"].utcoffset() == dt.timedelta(hours=1)
    assert row["n_same"] is True
    assert row["a_same"] is True, "10:00+01:00 and 09:00Z are one instant of day"
    assert row["mixed"] is False, "a local time is not a zoned time"
    assert row["before"] is True


def test_incomparable_orderings_are_null(graph):
    row = graph.cypher(
        "RETURN localtime('10:00') < time('11:00Z') AS v1, localtime('10:00') < date('2020-01-01') AS v2, "
        "localtime('10:00') = date('2020-01-01') AS v3"
    ).to_list()[0]
    assert row == {"v1": None, "v2": None, "v3": False}


def test_arithmetic_wraps_modulo_a_day(graph):
    assert one(graph, "RETURN localtime('23:30') + duration({hours: 1}) AS v") == dt.time(0, 30)
    assert one(graph, "RETURN time('00:10+01:00') - duration('PT20M') AS v") == dt.time(23, 50, tzinfo=tz(1))
    assert one(graph, "RETURN duration.between(localtime('10:00'), localtime('12:30')) AS v") == {
        "months": 0,
        "days": 0,
        "seconds": 9000,
    }


def test_sub_microsecond_digits_are_floored_on_the_python_side(graph):
    """`datetime.time` holds microseconds. The engine value keeps the nanoseconds
    (`toString` shows them); the Python object floors them, as a `Timestamp` already
    does until nanosecond support lands."""
    row = graph.cypher(
        "WITH localtime('10:00:00.123456789') AS t RETURN t AS v, toString(t) AS text, t.nanosecond AS ns"
    ).to_list()[0]
    assert row["v"] == dt.time(10, 0, 0, 123456)
    assert row["text"] == "10:00:00.123456789"
    assert row["ns"] == 123456789


def test_stored_times_round_trip_and_compare(graph):
    graph.cypher(
        "CREATE (:E {id: 1, l: $l, z: $z}), (:E {id: 2, l: $l2, z: $z2})",
        params={
            "l": dt.time(9, 0),
            "z": dt.time(9, 0, tzinfo=tz(1)),
            "l2": dt.time(11, 30, 15, 250000),
            "z2": dt.time(23, 59, 59, 999999, tzinfo=tz(-5, -30)),
        },
    )
    rows = graph.cypher("MATCH (n:E) RETURN n.id AS id, n.l AS l, n.z AS z ORDER BY id").to_list()
    assert rows[0]["l"] == dt.time(9, 0) and rows[0]["l"].tzinfo is None
    assert rows[1]["z"] == dt.time(23, 59, 59, 999999, tzinfo=tz(-5, -30))
    assert rows[1]["z"].utcoffset() == dt.timedelta(hours=-5, minutes=-30)
    # Order by instant of day: 09:00+01:00 is 08:00Z, before 23:59:59.999999-05:30 (05:29Z next day).
    ordered = graph.cypher("MATCH (n:E) RETURN n.id AS id ORDER BY n.z").to_list()
    assert [r["id"] for r in ordered] == [1, 2]
    found = graph.cypher("MATCH (n:E) WHERE n.z = time('08:00Z') RETURN n.id AS id").to_list()
    assert [r["id"] for r in found] == [1]
    assert graph.cypher("MATCH (n:E) WHERE n.l > localtime('10:00') RETURN n.id AS id").to_list() == [{"id": 2}]
    # A zoned stored time never matches a local probe, and the reverse.
    assert graph.cypher("MATCH (n:E) WHERE n.z = localtime('09:00') RETURN n.id AS id").to_list() == []


def test_dataframe_time_columns_load_typed(graph):
    frame = pd.DataFrame(
        {
            "id": [1, 2, 3],
            "name": ["a", "b", "c"],
            "naive": [dt.time(8, 30), None, dt.time(23, 59, 59, 5)],
            "aware": [dt.time(8, 30, tzinfo=tz(2)), dt.time(1, 2, 3, tzinfo=UTC), None],
        }
    )
    with warnings.catch_warnings():
        warnings.simplefilter("error")
        graph.add_nodes(frame, "E", "id", "name")
    rows = graph.cypher("MATCH (n:E) RETURN n.id AS id, n.naive AS naive, n.aware AS aware ORDER BY id").to_list()
    assert [r["naive"] for r in rows] == [dt.time(8, 30), None, dt.time(23, 59, 59, 5)]
    assert all(r["naive"] is None or r["naive"].tzinfo is None for r in rows)
    assert rows[0]["aware"] == dt.time(8, 30, tzinfo=tz(2))
    assert rows[0]["aware"].utcoffset() == dt.timedelta(hours=2)
    assert rows[1]["aware"].utcoffset() == dt.timedelta(0)
    assert rows[2]["aware"] is None
    # The same wall clock at +02:00 is 06:30Z, so it finds the stored value.
    assert graph.cypher("MATCH (n:E) WHERE n.aware = time('06:30Z') RETURN n.id AS id").to_list() == [{"id": 1}]


def test_a_column_mixing_naive_and_aware_times_is_refused_not_guessed_at(graph):
    frame = pd.DataFrame({"id": [1, 2], "name": ["a", "b"], "t": [dt.time(8, 30), dt.time(9, 0, tzinfo=tz(1))]})
    # Neither kind is silently reinterpreted as the other: the column has no single native type.
    with pytest.raises(kglite.ArgumentError, match="time values"):
        graph.add_nodes(frame, "E", "id", "name")
    # Asking for text is the explicit way out.
    graph.add_nodes(frame, "E", "id", "name", column_types={"t": "string"})
    assert one(graph, "MATCH (n:E {id: 1}) RETURN n.t AS v") == "08:30:00"


def test_column_types_names_the_time_kinds(graph):
    frame = pd.DataFrame(
        {"id": [1], "name": ["a"], "t": [dt.time(10, 0, 0, 500000)], "z": [dt.time(10, 0, tzinfo=tz(1))]}
    )
    graph.add_nodes(frame, "E", "id", "name", column_types={"t": "localtime", "z": "time"})
    row = graph.cypher("MATCH (n:E) RETURN n.t AS t, n.z AS z").to_list()[0]
    assert row["t"] == dt.time(10, 0, 0, 500000)
    assert row["z"] == dt.time(10, 0, tzinfo=tz(1))
    # A cell of the other kind is reported, not reinterpreted.
    with pytest.raises(kglite.ArgumentError, match="time of day"):
        graph.add_nodes(frame, "F", "id", "name", column_types={"t": "time"})


def test_declared_time_types_accept_their_kind_only(graph):
    graph.cypher("CREATE CONSTRAINT lt FOR (n:E) REQUIRE n.l IS :: LOCAL TIME")
    graph.cypher("CREATE CONSTRAINT zt FOR (n:E) REQUIRE n.z IS :: ZONED TIME")
    graph.cypher("CREATE (:E {id: 1, l: localtime('10:00'), z: time('10:00+01:00')})")
    with pytest.raises(Exception, match="LOCAL TIME"):
        graph.cypher("CREATE (:E {id: 2, l: time('10:00Z')})")
    with pytest.raises(Exception, match="ZONED TIME"):
        graph.cypher("CREATE (:E {id: 3, z: localtime('10:00')})")
    graph.cypher("CREATE CONSTRAINT zt2 FOR (n:F) REQUIRE n.z IS :: TIME")
    with pytest.raises(Exception):
        graph.cypher("CREATE (:F {id: 1, z: '10:00'})")


def test_valid_time_refuses_time_of_day_bounds(graph):
    graph.cypher("CREATE (:V {id: 1, name: 'a', vf: localtime('10:00'), vt: date('2030-01-01')})")
    with pytest.raises(Exception, match=r"(?i)date|datetime"):
        graph.cypher("FOR VALID_TIME AS OF localtime('10:00') MATCH (n:V) RETURN n.id AS id")


MODES = ["memory", "mapped", "disk", "disk_reopened", "memory_kgl", "mapped_kgl"]


def _fresh(mode, tmp_path):
    if mode.startswith("disk"):
        return kglite.KnowledgeGraph(storage="disk", path=str(tmp_path / "g"))
    if mode.startswith("mapped"):
        return kglite.KnowledgeGraph(storage="mapped")
    return kglite.KnowledgeGraph()


def _reopen(graph, mode, tmp_path):
    if mode == "disk_reopened":
        graph.save()
        del graph
        return kglite.load(str(tmp_path / "g"))
    if mode.endswith("_kgl"):
        path = str(tmp_path / "g.kgl")
        graph.save(path)
        del graph
        return kglite.load(path)
    return graph


@pytest.mark.parity
@pytest.mark.parametrize("mode", MODES)
def test_times_of_day_survive_every_storage_mode(mode, tmp_path):
    graph = _fresh(mode, tmp_path)
    graph.cypher(
        "CREATE (:E {id: 1, l: localtime('00:00'), z: time('10:00:00.000000007+01:00')}), "
        "(:E {id: 2, l: localtime('23:59:59.999999999')}), "
        "(:E {id: 3, z: time('09:00Z')}), (:E {id: 4})"
    )
    graph.cypher("MATCH (n:E {id: 4}) SET n.l = localtime('12:34:56.789'), n.z = time('00:00-18:00')")
    graph = _reopen(graph, mode, tmp_path)
    rows = graph.cypher(
        "MATCH (n:E) RETURN n.id AS id, toString(n.l) AS l, toString(n.z) AS z, n.z.offsetSeconds AS off ORDER BY id"
    ).to_list()
    assert [(r["id"], r["l"], r["z"], r["off"]) for r in rows] == [
        (1, "00:00", "10:00:00.000000007+01:00", 3600),
        (2, "23:59:59.999999999", None, None),
        (3, None, "09:00Z", 0),
        (4, "12:34:56.789", "00:00-18:00", -64800),
    ]
    # Equality is by instant of day across the stored values.
    same = graph.cypher("MATCH (n:E) WHERE n.z = time('09:00:00.000000007Z') RETURN n.id AS id").to_list()
    assert same == [{"id": 1}]
    assert graph.cypher("MATCH (n:E) WHERE n.l > localtime('12:00') RETURN n.id AS id ORDER BY id").to_list() == [
        {"id": 2},
        {"id": 4},
    ]
    distinct = graph.cypher("MATCH (n:E) RETURN count(DISTINCT n.z) AS c").to_list()
    assert distinct == [{"c": 3}]


def test_blueprint_time_keywords_read_csv_cells(tmp_path):
    import csv
    import json

    from kglite.blueprint import from_blueprint

    rows = [
        (1, "10:00", "10:00+01:00"),
        (2, "23:59:59.123456789", "00:00:00Z"),
        (3, "10:00+01:00", "10:00"),  # a local cell with an offset is NULL; a zoned cell without one is UTC
        (4, "garbage", ""),
    ]
    with (tmp_path / "cells.csv").open("w", newline="", encoding="utf-8") as stream:
        writer = csv.writer(stream)
        writer.writerow(["id", "l", "z"])
        writer.writerows(rows)
    blueprint = {
        "settings": {"root": str(tmp_path)},
        "nodes": {"Cell": {"csv": "cells.csv", "pk": "id", "properties": {"l": "localtime", "z": "time"}}},
    }
    (tmp_path / "bp.json").write_text(json.dumps(blueprint), encoding="utf-8")
    graph = from_blueprint(tmp_path / "bp.json", save=False)
    got = graph.cypher("MATCH (n:Cell) RETURN n.id AS id, toString(n.l) AS l, toString(n.z) AS z ORDER BY id").to_list()
    assert [(r["id"], r["l"], r["z"]) for r in got] == [
        (1, "10:00", "10:00+01:00"),
        (2, "23:59:59.123456789", "00:00Z"),
        (3, None, "10:00Z"),
        (4, None, None),
    ]


def _times_graph():
    g = kglite.KnowledgeGraph()
    g.cypher(
        "CREATE (:E {id: 1, title: 't', l: localtime('10:15:30.123456789'), z: time('10:00:00.5-05:30'), "
        "lst: [localtime('01:02'), time('03:04+01:00')]})"
    )
    return g


QUERY = (
    "MATCH (n:E) RETURN toString(n.l) AS l, toString(n.z) AS z, n.z.offsetSeconds AS off, toString(n.lst[1]) AS inner"
)
EXPECTED = [{"l": "10:15:30.123456789", "z": "10:00:00.500-05:30", "off": -19800, "inner": "03:04+01:00"}]


def test_rdf_export_writes_xsd_time_and_reads_it_back(tmp_path):
    g = _times_graph()
    path = tmp_path / "g.nq"
    g.export_rdf(str(path))
    text = path.read_text(encoding="utf-8")
    assert '"10:15:30.123456789"^^<http://www.w3.org/2001/XMLSchema#time>' in text
    assert '"10:00:00.5-05:30"^^<http://www.w3.org/2001/XMLSchema#time>' in text
    back = kglite.load_rdf(str(path))
    assert back.cypher(QUERY).to_list() == EXPECTED


def test_rdf_import_reads_xsd_time_with_and_without_an_offset(tmp_path):
    path = tmp_path / "t.nt"
    xsd = "http://www.w3.org/2001/XMLSchema#time"
    path.write_text(
        f'<http://e/a> <http://e/at> "08:30:00"^^<{xsd}> .\n'
        f'<http://e/a> <http://e/bt> "08:30:00Z"^^<{xsd}> .\n'
        f'<http://e/a> <http://e/ct> "08:30:00+02:00"^^<{xsd}> .\n',
        encoding="utf-8",
    )
    g = kglite.load_rdf(str(path))
    row = g.cypher("MATCH (n) RETURN n.at AS a, n.bt AS b, n.ct AS c").to_list()
    assert row[0]["a"] == dt.time(8, 30)
    assert row[0]["b"] == dt.time(8, 30, tzinfo=UTC)
    assert row[0]["c"] == dt.time(8, 30, tzinfo=tz(2))


def test_csv_tree_export_round_trips_through_the_blueprint(tmp_path):
    g = _times_graph()
    out = tmp_path / "tree"
    g.export_csv(str(out))
    blueprint = (out / "blueprint.json").read_text(encoding="utf-8")
    assert '"localtime"' in blueprint and '"time"' in blueprint
    back = kglite.from_blueprint(str(out / "blueprint.json"), save=False)
    assert back.cypher(QUERY).to_list() == EXPECTED


def test_json_and_sql_exports_carry_the_iso_text(tmp_path):
    g = _times_graph()
    d3 = g.export_string(format="json")
    assert "10:15:30.123456789" in d3 and "10:00:00.500-05:30" in d3
    sql_path = tmp_path / "g.sql"
    g.export(str(sql_path), format="sqlite")
    sql = sql_path.read_text(encoding="utf-8")
    assert "'10:15:30.123456789'" in sql and "'10:00:00.500-05:30'" in sql
