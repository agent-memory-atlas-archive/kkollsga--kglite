"""`ZonedDateTime` as a real value: `datetime()` returns an aware
`datetime.datetime`, an aware parameter or DataFrame cell is stored as one,
equality and order are by instant, a stored local timestamp is read as UTC where
it meets a zoned value, and validity bounds and `AS OF` instants accept them.

`datetime()` used to return a zone-less timestamp with any offset already applied;
every expectation here is a hand-written value, not read back from the engine.
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


def test_datetime_returns_an_aware_datetime_that_keeps_its_offset(graph):
    got = one(graph, "RETURN datetime('2015-07-21T21:40:32.142+01:00') AS v")
    assert got == dt.datetime(2015, 7, 21, 21, 40, 32, 142000, tzinfo=tz(1))
    assert got.utcoffset() == dt.timedelta(hours=1)
    assert got.hour == 21, "the wall clock is the one written, not UTC"
    assert one(graph, "RETURN datetime('2015-07-21T21:40Z') AS v").utcoffset() == dt.timedelta(0)
    # No offset reads as UTC; localdatetime() is the zone-less kind.
    assert one(graph, "RETURN datetime('2015-07-21T21:40') AS v") == dt.datetime(2015, 7, 21, 21, 40, tzinfo=UTC)
    local = one(graph, "RETURN localdatetime('2015-07-21T21:40') AS v")
    assert local == dt.datetime(2015, 7, 21, 21, 40) and local.tzinfo is None
    assert one(graph, "RETURN datetime() AS v").utcoffset() == dt.timedelta(0)
    assert one(graph, "RETURN localdatetime() AS v").tzinfo is None
    # Aware and naive datetimes do not mix in Python arithmetic.
    with pytest.raises(TypeError):
        one(graph, "RETURN datetime() AS v") - dt.datetime.now()


def test_text_and_accessors(graph):
    row = graph.cypher(
        "WITH datetime('2015-07-21T21:40:32.142857123+01:00') AS z "
        "RETURN toString(z) AS text, z.year AS y, z.hour AS h, z.nanosecond AS ns, z.offset AS off, "
        "z.offsetSeconds AS secs, z.offsetMinutes AS mins, z.timezone AS zone, "
        "z.epochSeconds AS es, z.epochMillis AS em"
    ).to_list()[0]
    assert row == {
        "text": "2015-07-21T21:40:32.142857123+01:00",
        "y": 2015,
        "h": 21,
        "ns": 142857123,
        "off": "+01:00",
        "secs": 3600,
        "mins": 60,
        "zone": "+01:00",
        "es": 1437511232,
        "em": 1437511232142,
    }
    assert one(graph, "RETURN toString(datetime('1984-10-11T12:31:00Z')) AS v") == "1984-10-11T12:31Z"


def test_an_unknown_zone_is_refused_by_name(graph):
    with pytest.raises(Exception, match="Mars/Base"):
        graph.cypher("RETURN datetime('2015-07-21T21:40[Mars/Base]') AS v")
    with pytest.raises(Exception, match="Mars/Base"):
        graph.cypher("RETURN datetime({year: 2015, timezone: 'Mars/Base'}) AS v")


def test_aware_parameters_keep_their_offset_and_naive_ones_stay_local(graph):
    aware = dt.datetime(2024, 3, 9, 14, 30, 0, 250000, tzinfo=tz(2))
    naive = dt.datetime(2024, 3, 9, 12, 30, 0, 250000)
    row = graph.cypher(
        "RETURN $a AS a, $n AS n, $a = $n AS read_as_utc, $a = datetime('2024-03-09T12:30:00.25Z') AS same_instant, "
        "$a.offsetSeconds AS off, $n.offsetSeconds AS noff",
        params={"a": aware, "n": naive},
    ).to_list()[0]
    assert row["a"] == aware and row["a"].utcoffset() == dt.timedelta(hours=2)
    assert row["n"] == naive and row["n"].tzinfo is None
    assert row["read_as_utc"] is True, "a local datetime is read as UTC where it meets a zoned one"
    assert row["same_instant"] is True
    assert row["off"] == 7200
    assert row["noff"] is None


def test_an_offset_beyond_eighteen_hours_keeps_the_instant_at_utc(graph):
    wide = dt.datetime(2024, 3, 9, 14, 30, tzinfo=dt.timezone(dt.timedelta(hours=19)))
    got = one(graph, "RETURN $v AS v", {"v": wide})
    assert got == wide
    assert got.utcoffset() == dt.timedelta(0)


def test_datetime_now_compares_with_stored_local_and_zoned_stamps(graph):
    graph.cypher(
        "CREATE (:E {id: 1, at: localdatetime('2000-01-01T00:00:00')}), "
        "(:E {id: 2, at: localdatetime('2999-01-01T00:00:00')}), "
        "(:E {id: 3, at: datetime('2000-01-01T00:00:00+05:00')}), "
        "(:E {id: 4, at: datetime('2999-01-01T00:00:00-05:00')})"
    )
    ids = lambda q, **p: sorted(r["id"] for r in graph.cypher(q, params=p).to_list())  # noqa: E731
    assert ids("MATCH (n:E) WHERE n.at < datetime() RETURN n.id AS id") == [1, 3]
    assert ids("MATCH (n:E) WHERE n.at > datetime() RETURN n.id AS id") == [2, 4]
    assert ids("MATCH (n:E) WHERE datetime() > n.at RETURN n.id AS id") == [1, 3]
    aware_cutoff = dt.datetime(2500, 1, 1, tzinfo=tz(3))
    assert ids("MATCH (n:E) WHERE n.at > $c RETURN n.id AS id", c=aware_cutoff) == [2, 4]
    # 2000-01-01T00:00+05:00 is 1999-12-31T19:00Z.
    assert ids("MATCH (n:E) WHERE n.at = datetime('1999-12-31T19:00:00Z') RETURN n.id AS id") == [3]
    assert ids("MATCH (n:E) WHERE n.at = localdatetime('1999-12-31T19:00:00') RETURN n.id AS id") == [3]
    graph.cypher("CREATE INDEX FOR (n:E) ON (n.at)")
    assert ids("MATCH (n:E) WHERE n.at = datetime('1999-12-31T20:00:00+01:00') RETURN n.id AS id") == [3]
    assert ids("MATCH (n:E) WHERE n.at = datetime('2000-01-01T00:00:00Z') RETURN n.id AS id") == [1]


def test_equal_instants_merge_in_distinct_and_keep_the_first_offset(graph):
    rows = graph.cypher(
        "UNWIND [datetime('2015-07-21T21:40+01:00'), datetime('2015-07-21T20:40Z'), datetime('2015-07-21T09:00Z')] "
        "AS t RETURN DISTINCT t AS v"
    ).to_list()
    assert [r["v"] for r in rows] == [
        dt.datetime(2015, 7, 21, 21, 40, tzinfo=tz(1)),
        dt.datetime(2015, 7, 21, 9, 0, tzinfo=UTC),
    ]
    assert rows[0]["v"].utcoffset() == dt.timedelta(hours=1)
    assert one(graph, "RETURN datetime('2015-07-21T21:40+01:00') = datetime('2015-07-21T20:40Z') AS v") is True


def test_arithmetic_and_truncation_keep_the_offset(graph):
    assert one(graph, "RETURN datetime('2015-01-31T23:30+05:00') + duration({months: 1}) AS v") == dt.datetime(
        2015, 2, 28, 23, 30, tzinfo=tz(5)
    )
    assert one(graph, "RETURN datetime('2015-07-21T21:40:32Z') + duration({nanoseconds: 7}) AS v") == dt.datetime(
        2015, 7, 21, 21, 40, 32, tzinfo=UTC
    ), "the nanosecond is below the Python resolution"
    assert one(graph, "RETURN datetime('2015-07-21T22:40+01:00') - datetime('2015-07-21T20:00Z') AS v") == {
        "months": 0,
        "days": 0,
        "seconds": 6000,
    }
    assert one(graph, "RETURN datetime.truncate('day', datetime('2015-07-21T23:40:32-05:00')) AS v") == dt.datetime(
        2015, 7, 21, tzinfo=tz(-5)
    )
    assert one(
        graph, "RETURN duration.between(datetime('2015-07-21T21:40:32+01:00'), datetime('2015-07-23T19:40:32.5Z')) AS v"
    ) == {"months": 0, "days": 1, "seconds": 82800, "nanoseconds": 500000000}


def test_the_declared_type_accepts_zoned_values_only(graph):
    graph.cypher("CREATE CONSTRAINT zd FOR (n:E) REQUIRE n.at IS :: ZONED DATETIME")
    graph.cypher("CREATE (:E {id: 1, at: datetime('2015-07-21T21:40+01:00')})")
    for bad in ("localdatetime('2015-07-21T21:40')", "date('2015-07-21')", "'2015-07-21T21:40Z'"):
        with pytest.raises(Exception, match="ZONED DATETIME"):
            graph.cypher(f"CREATE (:E {{id: 2, at: {bad}}})")


def test_dataframe_columns_keep_utc_naive_unless_declared(graph):
    frame = pd.DataFrame(
        {
            "id": [1, 2],
            "name": ["a", "b"],
            "at": pd.to_datetime(["2024-03-09 14:30", "2024-03-10 08:00"]).tz_localize("Europe/Oslo"),
        }
    )
    graph.add_nodes(frame, "E", "id", "name")
    got = one(graph, "MATCH (n:E {id: 1}) RETURN n.at AS v")
    assert got == dt.datetime(2024, 3, 9, 13, 30) and got.tzinfo is None, "an aware DataFrame column stays UTC-naive"


def test_a_declared_zoneddatetime_column_keeps_each_offset(graph):
    frame = pd.DataFrame(
        {
            "id": [1, 2, 3],
            "name": ["a", "b", "c"],
            "at": [dt.datetime(2024, 3, 9, 14, 30, tzinfo=tz(2)), dt.datetime(2024, 3, 9, 8, 0), None],
        }
    )
    with warnings.catch_warnings():
        warnings.simplefilter("error")
        graph.add_nodes(frame, "E", "id", "name", column_types={"at": "zoneddatetime"})
    rows = graph.cypher("MATCH (n:E) RETURN n.id AS id, n.at AS at ORDER BY id").to_list()
    assert rows[0]["at"] == dt.datetime(2024, 3, 9, 14, 30, tzinfo=tz(2))
    assert rows[0]["at"].utcoffset() == dt.timedelta(hours=2)
    assert rows[1]["at"] == dt.datetime(2024, 3, 9, 8, 0, tzinfo=UTC), "a naive cell is read as UTC"
    assert rows[2]["at"] is None
    assert graph.cypher("MATCH (n:E) WHERE n.at = datetime('2024-03-09T12:30:00Z') RETURN n.id AS id").to_list() == [
        {"id": 1}
    ]


def _bounded(graph):
    graph.cypher(
        "CREATE (:W {id: 1, vf: datetime('2000-01-01T05:00:00+05:00'), vt: datetime('2000-01-01T12:00:00+02:00')}), "
        "(:W {id: 2, vf: localdatetime('2000-01-01T09:00:00'), vt: null}), "
        "(:W {id: 3, vf: datetime('2000-01-01T11:00:00Z'), vt: null})"
    )
    graph.cypher(
        "CALL db.temporal.declare({node: 'W', from: 'vf', to: 'vt', convention: 'closed'}) "
        "YIELD declared RETURN declared"
    )


def _valid(graph, **kw):
    return sorted(r["id"] for r in graph.cypher("MATCH (w:W) RETURN w.id AS id", **kw).to_list())


def test_valid_time_accepts_zoned_bounds_and_instants(graph):
    _bounded(graph)
    # Well 1 holds 00:00Z..=10:00Z, well 2 from 09:00Z, well 3 from 11:00Z.
    for at in ("2000-01-01T00:00:00Z", "2000-01-01T04:00:00+04:00", "1999-12-31T19:00:00-05:00"):
        got = sorted(
            r["id"]
            for r in graph.cypher(f"FOR VALID_TIME AS OF datetime('{at}') MATCH (w:W) RETURN w.id AS id").to_list()
        )
        assert got == [1], at
    assert sorted(
        r["id"]
        for r in graph.cypher(
            "FOR VALID_TIME AS OF datetime('2000-01-01T10:30:00+01:00') MATCH (w:W) RETURN w.id AS id"
        ).to_list()
    ) == [1, 2]
    # valid_at= takes an aware datetime, at any offset, and a parameter does too.
    twelve = dt.datetime(2000, 1, 1, 12, 0, tzinfo=UTC)
    for instant in (twelve, twelve.astimezone(tz(1)), twelve.astimezone(tz(-5))):
        assert _valid(graph, valid_at=instant) == [2, 3], instant
    assert sorted(
        r["id"]
        for r in graph.cypher(
            "FOR VALID_TIME AS OF $t MATCH (w:W) RETURN w.id AS id", params={"t": twelve.astimezone(tz(-5))}
        ).to_list()
    ) == [2, 3]


def test_a_view_frozen_at_a_zoned_instant(graph):
    _bounded(graph)
    view = graph.freeze(valid_at=dt.datetime(2000, 1, 1, 12, 0, tzinfo=tz(1)))  # 11:00Z
    got = sorted(r["id"] for r in view.cypher("MATCH (w:W) RETURN w.id AS id").to_list())
    assert got == [2, 3]


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
def test_zoned_datetimes_survive_every_storage_mode(mode, tmp_path):
    graph = _fresh(mode, tmp_path)
    graph.cypher(
        "CREATE (:E {id: 1, z: datetime('2015-07-21T21:40:32.000000007+01:00'), "
        "l: localdatetime('2015-07-21T20:40:32')}), "
        "(:E {id: 2, z: datetime('1969-12-31T23:59:59.999999999-18:00')}), "
        "(:E {id: 3, z: datetime('2015-07-21T20:40:32Z')}), (:E {id: 4})"
    )
    graph.cypher("MATCH (n:E {id: 4}) SET n.z = datetime('0001-01-01T00:00:00+18:00')")
    graph = _reopen(graph, mode, tmp_path)
    rows = graph.cypher(
        "MATCH (n:E) RETURN n.id AS id, toString(n.z) AS z, n.z.offsetSeconds AS off, "
        "n.z.epochSeconds AS es ORDER BY id"
    ).to_list()
    assert [(r["id"], r["z"], r["off"], r["es"]) for r in rows] == [
        (1, "2015-07-21T21:40:32.000000007+01:00", 3600, 1437511232),
        # 23:59:59.999999999 at -18:00 is 17:59:59.999999999Z the next day: 64799 s after the epoch.
        (2, "1969-12-31T23:59:59.999999999-18:00", -64800, 64799),
        (3, "2015-07-21T20:40:32Z", 0, 1437511232),
        (4, "0001-01-01T00:00+18:00", 64800, -62135596800 - 64800),
    ]
    # Equality is by instant: ids 1 and 3 name one second, 1 a nanosecond later.
    same = graph.cypher("MATCH (n:E) WHERE n.z = datetime('2015-07-21T20:40:32Z') RETURN n.id AS id").to_list()
    assert same == [{"id": 3}]
    # A local stamp is read as UTC against a zoned value, scan and range alike.
    local = graph.cypher("MATCH (n:E) WHERE n.z = n.l RETURN n.id AS id").to_list()
    assert local == []
    assert graph.cypher(
        "MATCH (n:E) WHERE n.z >= localdatetime('2015-07-21T20:40:32') RETURN n.id AS id ORDER BY id"
    ).to_list() == [
        {"id": 1},
        {"id": 3},
    ]
    assert graph.cypher("MATCH (n:E) WHERE n.z IS NOT NULL RETURN count(DISTINCT n.z) AS c").to_list() == [{"c": 4}]
    ordered = graph.cypher("MATCH (n:E) WHERE n.z IS NOT NULL RETURN n.id AS id ORDER BY n.z").to_list()
    assert [r["id"] for r in ordered] == [4, 2, 3, 1]


@pytest.mark.parity
@pytest.mark.parametrize("mode", MODES)
def test_valid_time_with_zoned_bounds_in_every_storage_mode(mode, tmp_path):
    graph = _fresh(mode, tmp_path)
    _bounded(graph)
    graph = _reopen(graph, mode, tmp_path)
    twelve = dt.datetime(2000, 1, 1, 12, 0, tzinfo=tz(2))  # 10:00Z, the closed end of well 1
    assert _valid(graph, valid_at=twelve) == [1, 2]
    assert _valid(graph, valid_at=twelve.astimezone(UTC) + dt.timedelta(seconds=1)) == [2]
    assert sorted(
        r["id"]
        for r in graph.cypher(
            "FOR VALID_TIME AS OF datetime('2000-01-01T11:00:00Z') MATCH (w:W) RETURN w.id AS id"
        ).to_list()
    ) == [2, 3]


# ---------------------------------------------------------------------------
# Named IANA zones. Oslo is +01:00 until 2026-03-29 01:00Z and +02:00 until
# 2026-10-25 01:00Z; every expectation is derived by hand from those rules.
# ---------------------------------------------------------------------------

zoneinfo = pytest.importorskip("zoneinfo")
OSLO = zoneinfo.ZoneInfo("Europe/Oslo")


def test_a_named_zone_reads_back_as_an_aware_datetime_in_that_zone(graph):
    got = one(graph, "RETURN datetime('2026-07-15T12:00[Europe/Oslo]') AS v")
    assert got == dt.datetime(2026, 7, 15, 12, 0, tzinfo=OSLO)
    assert got.tzinfo.key == "Europe/Oslo"
    assert got.utcoffset() == dt.timedelta(hours=2)
    assert got.hour == 12, "the wall clock is the one in the zone"
    # The same instant in a fixed offset is the same moment but not the same tzinfo.
    fixed = one(graph, "RETURN datetime('2026-07-15T12:00+02:00') AS v")
    assert fixed == got and not hasattr(fixed.tzinfo, "key")


def test_dst_gap_and_overlap_read_back_with_the_rules_of_the_zone(graph):
    # 02:30 on 2026-03-29 is skipped in Oslo: 03:30+02:00 (01:30Z).
    gap = one(graph, "RETURN datetime('2026-03-29T02:30[Europe/Oslo]') AS v")
    assert (gap.hour, gap.minute, gap.utcoffset()) == (3, 30, dt.timedelta(hours=2))
    assert gap == dt.datetime(2026, 3, 29, 1, 30, tzinfo=UTC)
    # 02:30 on 2026-10-25 happens twice: the earlier is +02:00 (fold=0), the
    # later +01:00 (fold=1).
    early = one(graph, "RETURN datetime('2026-10-25T02:30[Europe/Oslo]') AS v")
    late = one(graph, "RETURN datetime('2026-10-25T02:30+01:00[Europe/Oslo]') AS v")
    assert (early.hour, early.minute, early.fold, early.utcoffset()) == (2, 30, 0, dt.timedelta(hours=2))
    assert (late.hour, late.minute, late.fold, late.utcoffset()) == (2, 30, 1, dt.timedelta(hours=1))
    assert late - early == dt.timedelta(0), "Python compares two same-zone datetimes by wall clock"
    assert late.astimezone(UTC) - early.astimezone(UTC) == dt.timedelta(hours=1)
    # Southern hemisphere: Sydney repeats 02:30 on 2026-04-05 (earlier is +11:00).
    sydney = one(graph, "RETURN datetime('2026-04-05T02:30[Australia/Sydney]') AS v")
    assert sydney.utcoffset() == dt.timedelta(hours=11) and sydney.fold == 0


def test_an_aware_parameter_keeps_a_zoneinfo_zone_and_a_fixed_one_stays_fixed(graph):
    # 02:30 on 2026-10-25 with fold=1 is the later occurrence, +01:00.
    later = dt.datetime(2026, 10, 25, 2, 30, tzinfo=OSLO, fold=1)
    row = graph.cypher(
        "RETURN $z.timezone AS zone, $z.offset AS off, toString($z) AS text, "
        "$f.timezone AS fzone, toString($f) AS ftext",
        params={"z": later, "f": dt.datetime(2026, 7, 15, 12, 0, tzinfo=tz(2))},
    ).to_list()[0]
    assert row == {
        "zone": "Europe/Oslo",
        "off": "+01:00",
        "text": "2026-10-25T02:30+01:00[Europe/Oslo]",
        "fzone": "+02:00",
        "ftext": "2026-07-15T12:00+02:00",
    }
    # Zone identity never splits equality: the same instant in two zones is one value.
    assert graph.cypher(
        "RETURN $a = $b AS same",
        params={
            "a": dt.datetime(2026, 7, 15, 12, 0, tzinfo=OSLO),
            "b": dt.datetime(2026, 7, 15, 6, 0, tzinfo=zoneinfo.ZoneInfo("America/New_York")),
        },
    ).to_list() == [{"same": True}]


def test_a_named_zone_survives_a_nested_round_trip(graph):
    value = dt.datetime(2026, 7, 15, 12, 0, tzinfo=OSLO)
    back = graph.cypher("RETURN [$v, {k: $v}] AS v", params={"v": value}).to_list()[0]["v"]
    assert back[0].tzinfo.key == "Europe/Oslo" and back[1]["k"].tzinfo.key == "Europe/Oslo"


@pytest.mark.parity
@pytest.mark.parametrize("mode", MODES)
def test_named_zones_survive_every_storage_mode(mode, tmp_path):
    graph = _fresh(mode, tmp_path)
    graph.cypher(
        "CREATE (:Z {id: 1, z: datetime('2026-07-15T12:00[Europe/Oslo]')}), "
        "(:Z {id: 2, z: datetime('2026-03-29T02:30[Europe/Oslo]')}), "
        "(:Z {id: 3, z: datetime('2026-10-25T02:30+01:00[Europe/Oslo]')}), "
        "(:Z {id: 4, z: datetime('2026-04-05T02:30[Australia/Sydney]')}), "
        "(:Z {id: 5, z: datetime('2026-07-15T10:00Z')}), (:Z {id: 6})"
    )
    graph.cypher(
        "MATCH (n:Z {id: 6}) SET n.z = datetime({year: 2026, month: 1, day: 15, hour: 9, timezone: 'America/New_York'})"
    )
    graph = _reopen(graph, mode, tmp_path)
    rows = graph.cypher(
        "MATCH (n:Z) RETURN n.id AS id, toString(n.z) AS z, n.z.timezone AS zone, n.z.epochSeconds AS es ORDER BY id"
    ).to_list()
    assert [(r["id"], r["z"], r["zone"], r["es"]) for r in rows] == [
        (1, "2026-07-15T12:00+02:00[Europe/Oslo]", "Europe/Oslo", 1784109600),
        (2, "2026-03-29T03:30+02:00[Europe/Oslo]", "Europe/Oslo", 1774747800),
        (3, "2026-10-25T02:30+01:00[Europe/Oslo]", "Europe/Oslo", 1792891800),
        (4, "2026-04-05T02:30+11:00[Australia/Sydney]", "Australia/Sydney", 1775316600),
        (5, "2026-07-15T10:00Z", "Z", 1784109600),
        (6, "2026-01-15T09:00-05:00[America/New_York]", "America/New_York", 1768485600),
    ]
    # Identity is the instant: id 1 (Oslo) and id 5 (UTC) are one value, found by either.
    assert graph.cypher(
        "MATCH (n:Z) WHERE n.z = datetime('2026-07-15T12:00+02:00') RETURN n.id AS id ORDER BY id"
    ).to_list() == [{"id": 1}, {"id": 5}]
    assert graph.cypher("MATCH (n:Z) RETURN count(DISTINCT n.z) AS c").to_list() == [{"c": 5}]
    # Python reads the zone back as a zoneinfo tzinfo.
    got = graph.cypher("MATCH (n:Z {id: 3}) RETURN n.z AS z").to_list()[0]["z"]
    assert got.tzinfo.key == "Europe/Oslo" and got.fold == 1 and got.hour == 2


def test_exports_write_a_named_zone_as_text_and_rdf_as_its_offset(graph, tmp_path):
    graph.cypher("CREATE (:Z {id: 1, z: datetime('2026-07-15T12:00[Europe/Oslo]')})")
    # JSON and CSV carry the Neo4j text, zone name included.
    assert "2026-07-15T12:00+02:00[Europe/Oslo]" in graph.export_string("json")
    out = tmp_path / "csv"
    graph.export_csv(str(out))
    assert "2026-07-15T12:00+02:00[Europe/Oslo]" in (out / "nodes" / "Z.csv").read_text(encoding="utf-8")
    # RDF has no zone names: xsd:dateTime keeps the offset, and reading it back is
    # the same instant (a fixed-offset value).
    nq = tmp_path / "g.nq"
    graph.export_rdf(str(nq))
    text = nq.read_text(encoding="utf-8")
    assert '"2026-07-15T12:00:00+02:00"' in text and "Europe/Oslo" not in text
