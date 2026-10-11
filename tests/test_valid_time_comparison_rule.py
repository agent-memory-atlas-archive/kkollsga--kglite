"""The one valid-time comparison rule, on every path that reads a bound or an instant.

Every bound and every query instant is a point on one UTC timeline: a date is its
whole UTC day (date grain whenever either side is a date), a local datetime is
read as UTC, a zoned datetime (fixed offset or named zone) is its UTC instant,
an ISO string is read by `date()` / `datetime()`. Anything else is refused with
a message that names the accepted kinds and the kind it found.

Expectations are hand-derived from the rule, not read back from the engine.
"""

import datetime as dt
import re
import warnings

import pandas as pd
import pytest

import kglite

UTC = dt.timezone.utc
ACCEPTED = "a date, a local datetime, a zoned datetime or an ISO date/datetime string"


def tz(hours=0, minutes=0):
    return dt.timezone(dt.timedelta(hours=hours, minutes=minutes))


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


def _ledger(graph):
    """Four versions whose bounds are a date, local datetimes, zoned datetimes
    in two offsets and an open end; all closed intervals."""
    graph.cypher(
        "CREATE (:W {id: 1, vf: date('2020-01-01'), vt: date('2020-01-31')}),"
        " (:W {id: 2, vf: localdatetime('2020-02-01T10:00:00'), vt: localdatetime('2020-02-01T12:00:00')}),"
        " (:W {id: 3, vf: datetime('2020-03-01T10:00:00+02:00'), vt: datetime('2020-03-01T12:00:00+02:00')}),"
        " (:W {id: 4, vf: datetime('2020-04-01T00:30:00+01:00')})"
    )
    graph.cypher("CALL db.temporal.declare({node: 'W', from: 'vf', to: 'vt', convention: 'closed'})")
    return graph


# (label, instant, ids valid then). Id 3 is valid 08:00Z-10:00Z on 2020-03-01 and
# id 4 from 23:30Z on 2020-03-31 with no end.
INSTANTS = [
    ("date inside", dt.date(2020, 1, 15), [1]),
    ("date is a whole day: the last day", dt.date(2020, 1, 31), [1]),
    ("date grain against a stamp bound: 03-31 midnight is before 23:30Z", dt.date(2020, 3, 31), [4]),
    ("local datetime inside", dt.datetime(2020, 2, 1, 11, 0), [2]),
    ("local datetime one second past the end", dt.datetime(2020, 2, 1, 12, 0, 1), []),
    ("zoned +02:00 -> 09:00Z", dt.datetime(2020, 3, 1, 11, 0, tzinfo=tz(2)), [3]),
    ("zoned -05:00 -> 09:00Z", dt.datetime(2020, 3, 1, 4, 0, tzinfo=tz(-5)), [3]),
    ("zoned past the end -> 11:00Z", dt.datetime(2020, 3, 1, 13, 0, tzinfo=tz(2)), []),
    ("zoned before the open start -> 23:15Z", dt.datetime(2020, 4, 1, 0, 15, tzinfo=tz(1)), []),
    ("zoned after the open start -> 23:45Z", dt.datetime(2020, 4, 1, 0, 45, tzinfo=tz(1)), [4]),
    ("ISO string with an offset -> 09:00Z", "2020-03-01T11:00:00+02:00", [3]),
    ("ISO date string", "2020-01-15", [1]),
]


def _ids(rows):
    return sorted(r["id"] for r in rows.to_list())


@pytest.mark.parity
@pytest.mark.parametrize("mode", MODES)
def test_every_instant_kind_reads_the_same_on_every_path(mode, tmp_path):
    graph = _reopen(_ledger(_fresh(mode, tmp_path)), mode, tmp_path)
    for label, instant, want in INSTANTS:
        paths = {
            "valid_at=": _ids(graph.cypher("MATCH (w:W) RETURN w.id AS id", valid_at=instant)),
            "AS OF $t": _ids(
                graph.cypher("FOR VALID_TIME AS OF $t MATCH (w:W) RETURN w.id AS id", params={"t": instant})
            ),
            "freeze": _ids(graph.freeze(valid_at=instant).cypher("MATCH (w:W) RETURN w.id AS id")),
            "valid_at()": _ids(
                graph.cypher(
                    "FOR VALID_TIME ALL MATCH (w:W) WHERE valid_at(w, $t) RETURN w.id AS id",
                    params={"t": instant},
                )
            ),
        }
        for path, got in paths.items():
            assert got == want, (label, path)


@pytest.mark.parity
@pytest.mark.parametrize("mode", MODES)
def test_literals_in_the_statement_prefix_follow_the_rule(mode, tmp_path):
    graph = _reopen(_ledger(_fresh(mode, tmp_path)), mode, tmp_path)
    for literal, want in [
        ("datetime('2020-03-01T11:00:00+02:00')", [3]),
        ("datetime('2020-03-01T10:00[Europe/Oslo]')", [3]),  # +01:00 in March -> 09:00Z
        ("datetime('2020-03-01T08:00:00')", [3]),  # no offset: UTC
        ("localdatetime('2020-02-01T11:00:00')", [2]),
        ("date('2020-01-31')", [1]),
        ("'2020-01-31'", [1]),
    ]:
        got = _ids(graph.cypher(f"FOR VALID_TIME AS OF {literal} MATCH (w:W) RETURN w.id AS id"))
        assert got == want, literal


BAD_INSTANTS = [
    (dt.time(10, 0), "LOCAL TIME"),
    (dt.time(10, 0, tzinfo=UTC), "ZONED TIME"),
    (dt.timedelta(days=1), "DURATION"),
    (5, "INTEGER"),
    (1.5, "FLOAT"),
    (True, "BOOLEAN"),
    ([1], "LIST"),
    ("garbage", "STRING"),
]


def _declared():
    return _ledger(kglite.KnowledgeGraph())


@pytest.mark.parametrize(("bad", "kind"), BAD_INSTANTS)
def test_an_instant_the_rule_refuses_names_the_accepted_kinds(bad, kind):
    graph = _declared()
    runs = {
        "valid_at=": lambda: graph.cypher("MATCH (w:W) RETURN w.id", valid_at=bad),
        "AS OF $t": lambda: graph.cypher("FOR VALID_TIME AS OF $t MATCH (w:W) RETURN w.id", params={"t": bad}),
        "freeze": lambda: graph.freeze(valid_at=bad),
        "valid_at()": lambda: graph.cypher(
            "FOR VALID_TIME ALL MATCH (w:W) RETURN valid_at(w, $t) AS v", params={"t": bad}
        ),
        "valid_during()": lambda: graph.cypher(
            "FOR VALID_TIME ALL MATCH (w:W) RETURN valid_during(w, $t, date('2021-01-01')) AS v", params={"t": bad}
        ),
    }
    for path, run in runs.items():
        with pytest.raises((ValueError, kglite.CypherExecutionError)) as caught:
            run()
        message = str(caught.value)
        assert f"({kind}) is not {ACCEPTED}" in message, (path, message)


def test_a_null_query_instant_is_refused_by_name():
    graph = _declared()
    with pytest.raises(kglite.CypherExecutionError, match=re.escape(f"(NULL) is not {ACCEPTED}")):
        graph.cypher("FOR VALID_TIME AS OF $t MATCH (w:W) RETURN w.id", params={"t": None})


def test_the_fluent_filters_name_the_accepted_kinds_for_a_wrong_type():
    graph = _declared()
    for bad in (dt.time(10, 0), dt.timedelta(days=1), 5):
        for call in (
            lambda: graph.select("W", temporal=False).valid_at(bad),
            lambda: graph.select("W", temporal=False).valid_during(bad, "2021"),
            lambda: graph.date(bad),
        ):
            with pytest.raises(TypeError, match=re.escape(f"must be {ACCEPTED}")):
                call()


def test_a_fluent_zoned_datetime_is_read_at_its_utc_date():
    graph = _declared()
    ids = lambda g: sorted(n["id"] for n in g.collect())  # noqa: E731
    # 2020-01-31T23:30-05:00 is 2020-02-01T04:30Z: the fluent filters work at date grain on the UTC date.
    late = dt.datetime(2020, 1, 31, 23, 30, tzinfo=tz(-5))
    assert ids(graph.select("W", temporal=False).valid_at(late)) == [2]
    early = dt.datetime(2020, 1, 31, 0, 30, tzinfo=tz(2))  # 2020-01-30T22:30Z
    assert ids(graph.select("W", temporal=False).valid_at(early)) == [1]


@pytest.mark.parametrize(("bad", "kind"), BAD_INSTANTS[:5])
def test_a_bound_write_the_rule_refuses_names_the_accepted_kinds(bad, kind):
    for statement in (
        "MATCH (w:W {id: 1}) SET w.vt = $v",
        "CREATE (:W {id: 9, vf: date('2020-01-01'), vt: $v})",
    ):
        graph = _declared()
        with pytest.raises(kglite.CypherExecutionError) as caught:
            graph.cypher(statement, params={"v": bad})
        assert "the to bound " in str(caught.value)
        assert f"({kind}) is not {ACCEPTED}" in str(caught.value), statement


def test_declaring_over_a_bound_the_rule_refuses_names_the_accepted_kinds():
    graph = _declared()
    graph.cypher("CREATE (:Raw {id: 1, a: localtime('10:00')})")
    with pytest.raises(kglite.CypherExecutionError) as caught:
        graph.cypher("CALL db.temporal.declare({node: 'Raw', from: 'a', to: 'b', convention: 'closed'})")
    assert f"the from bound 10:00 (LOCAL TIME) is not {ACCEPTED}" in str(caught.value)


# ---------------------------------------------------------------------------
# Ingestion: zoned values are accepted as bounds and compared by instant.
# ---------------------------------------------------------------------------


def _bounds(graph):
    rows = graph.cypher("FOR VALID_TIME ALL MATCH (w:W) RETURN w.id AS id, toString(w.vt) AS vt ORDER BY id").to_list()
    return [(r["id"], r["vt"]) for r in rows]


@pytest.mark.parity
@pytest.mark.parametrize("mode", MODES)
def test_zoned_bounds_written_by_cypher_are_compared_by_instant(mode, tmp_path):
    graph = _fresh(mode, tmp_path)
    graph.cypher("CREATE (:W {id: 1, vf: date('2020-01-01')}), (:W {id: 2, vf: date('2020-01-01')})")
    graph.cypher("CALL db.temporal.declare({node: 'W', from: 'vf', to: 'vt', convention: 'closed'})")
    # 2020-03-01T12:00+02:00 is 10:00Z; the named-zone form is 10:00Z too (+01:00 in March).
    graph.cypher("MATCH (w:W {id: 1}) SET w.vt = $z", params={"z": dt.datetime(2020, 3, 1, 12, 0, tzinfo=tz(2))})
    graph.cypher("MATCH (w:W {id: 2}) SET w.vt = datetime('2020-03-01T11:00[Europe/Oslo]')")
    graph = _reopen(graph, mode, tmp_path)
    at = lambda hour, minute=0: _ids(  # noqa: E731
        graph.cypher("MATCH (w:W) RETURN w.id AS id", valid_at=dt.datetime(2020, 3, 1, hour, minute, tzinfo=UTC))
    )
    assert at(10, 0) == [1, 2]
    assert at(10, 1) == []
    # A date instant is a whole-day comparison against the bound's UTC day.
    assert _ids(graph.cypher("MATCH (w:W) RETURN w.id AS id", valid_at=dt.date(2020, 3, 1))) == [1, 2]


@pytest.mark.parametrize("mode", ["memory", "mapped", "disk"])
def test_text_bounds_with_an_offset_are_stored_as_the_utc_instant(mode, tmp_path):
    graph = _fresh(mode, tmp_path)
    graph.add_nodes(
        pd.DataFrame({"id": [0], "vf": [dt.date(2019, 1, 1)], "vt": [dt.date(2019, 2, 1)]}),
        "W",
        "id",
        column_types={"vf": "date", "vt": "date"},
    )
    graph.cypher("CALL db.temporal.declare({node: 'W', from: 'vf', to: 'vt', convention: 'closed'})")
    reload = pd.DataFrame({"id": [1], "vf": ["2020-03-01T08:00:00Z"], "vt": ["2020-03-01T12:00:00+02:00"]})
    with warnings.catch_warnings():
        warnings.simplefilter("ignore")
        graph.add_nodes(reload, "W", "id")
    assert _bounds(graph)[1] == (1, "2020-03-01T10:00:00")


@pytest.mark.parity
@pytest.mark.parametrize("mode", ["memory", "mapped", "disk"])
def test_aware_frame_bounds_are_stored_as_utc_instants(mode, tmp_path):
    oslo = dt.datetime(2020, 3, 1, 10, 30, tzinfo=__import__("zoneinfo").ZoneInfo("Europe/Oslo"))
    cases = {
        "object column": pd.Series([oslo], dtype=object),
        "pandas tz column": pd.Series(pd.to_datetime(["2020-03-01 10:30"]).tz_localize("Europe/Oslo")),
        # Local midnight in Oslo is 23:00Z the day before: it must not collapse to the wall date.
        "pandas tz midnight": pd.Series(pd.to_datetime(["2020-03-01 00:00"]).tz_localize("Europe/Oslo")),
        "object tz midnight": pd.Series([dt.datetime(2020, 3, 1, 0, 0, tzinfo=tz(1))], dtype=object),
    }
    want = {
        "object column": "2020-03-01T09:30:00",
        "pandas tz column": "2020-03-01T09:30:00",
        "pandas tz midnight": "2020-02-29T23:00:00",
        "object tz midnight": "2020-02-29T23:00:00",
    }
    for name, column in cases.items():
        graph = _fresh(mode, tmp_path / name.replace(" ", "_")) if mode == "disk" else _fresh(mode, tmp_path)
        graph.cypher("CREATE (:W {id: 0, vf: date('2019-01-01'), vt: date('2019-02-01')})")
        graph.cypher("CALL db.temporal.declare({node: 'W', from: 'vf', to: 'vt', convention: 'closed'})")
        frame = pd.DataFrame({"id": [1], "title": ["a"], "vf": [dt.date(2020, 1, 1)], "vt": column})
        with warnings.catch_warnings():
            warnings.simplefilter("ignore")
            graph.add_nodes(frame, "W", "id", "title")
        assert _bounds(graph)[1] == (1, want[name]), name


def test_a_declared_zoned_column_keeps_the_zone_and_compares_by_instant():
    graph = kglite.KnowledgeGraph()
    graph.cypher("CREATE (:W {id: 0, vf: date('2019-01-01'), vt: date('2019-02-01')})")
    graph.cypher("CALL db.temporal.declare({node: 'W', from: 'vf', to: 'vt', convention: 'closed'})")
    zone = __import__("zoneinfo").ZoneInfo("Europe/Oslo")
    frame = pd.DataFrame(
        {
            "id": [1],
            "title": ["a"],
            "vf": [dt.date(2020, 1, 1)],
            "vt": pd.Series([dt.datetime(2020, 3, 1, 11, 0, tzinfo=zone)], dtype=object),
        }
    )
    with warnings.catch_warnings():
        warnings.simplefilter("ignore")
        graph.add_nodes(frame, "W", "id", "title", column_types={"vt": "zoneddatetime"})
    assert _bounds(graph)[1] == (1, "2020-03-01T11:00+01:00[Europe/Oslo]")
    assert _ids(graph.cypher("MATCH (w:W) RETURN w.id AS id", valid_at=dt.datetime(2020, 3, 1, 10, 0, tzinfo=UTC))) == [
        1
    ]
    assert (
        _ids(graph.cypher("MATCH (w:W) RETURN w.id AS id", valid_at=dt.datetime(2020, 3, 1, 10, 1, tzinfo=UTC))) == []
    )


def test_zoned_relationship_bounds_are_compared_by_instant():
    graph = kglite.KnowledgeGraph()
    graph.cypher("CREATE (:A {id: 1}), (:B {id: 1})")
    edges = pd.DataFrame(
        {
            "a": [1],
            "b": [1],
            "f": pd.Series([dt.datetime(2020, 3, 1, 10, 0, tzinfo=tz(2))], dtype=object),
            "t": pd.Series([dt.datetime(2020, 3, 1, 12, 0, tzinfo=tz(2))], dtype=object),
        }
    )
    graph.add_relationships(edges, "R", "A", "a", "B", "b")
    graph.cypher("CALL db.temporal.declare({relationship: 'R', from: 'f', to: 't', convention: 'closed'})")
    count = lambda hour: graph.cypher(  # noqa: E731
        "MATCH (:A)-[r:R]->(:B) RETURN count(r) AS c", valid_at=dt.datetime(2020, 3, 1, hour, 0, tzinfo=UTC)
    ).to_list()[0]["c"]
    assert [count(7), count(8), count(10), count(11)] == [0, 1, 1, 0]


# ---------------------------------------------------------------------------
# One clock per statement.
# ---------------------------------------------------------------------------


def test_no_argument_temporal_constructors_hold_one_reading_per_statement():
    graph = kglite.KnowledgeGraph()
    for constructor in ("datetime()", "localdatetime()", "date()", "time()", "localtime()"):
        rows = graph.cypher(
            f"UNWIND range(1, 1500) AS i RETURN sum(CASE WHEN {constructor} = {constructor} THEN 0 ELSE 1 END) AS apart"
        ).to_list()
        assert rows == [{"apart": 0}], constructor
    rows = graph.cypher(
        "UNWIND range(1, 1500) AS i "
        "WITH duration.inSeconds(datetime(), datetime()) AS d "
        "RETURN sum(CASE WHEN toString(d) = 'PT0S' THEN 0 ELSE 1 END) AS apart"
    ).to_list()
    assert rows == [{"apart": 0}]


def test_a_write_stamps_one_instant_on_every_row():
    graph = kglite.KnowledgeGraph()
    graph.cypher("UNWIND range(1, 800) AS i CREATE (:T {id: i})")
    graph.cypher("MATCH (n:T) SET n.at = datetime(), n.again = datetime()")
    rows = graph.cypher(
        "MATCH (n:T) RETURN count(DISTINCT n.at) AS stamps, sum(CASE WHEN n.at = n.again THEN 0 ELSE 1 END) AS apart"
    ).to_list()
    assert rows == [{"stamps": 1, "apart": 0}]
