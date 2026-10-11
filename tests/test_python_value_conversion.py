"""Ordinary conversion goldens; recursion guards use synthetic Rust state tests."""

import datetime as dt

import numpy as np
import pytest

import kglite


@pytest.fixture(params=["graph", "session", "frozen", "transaction"])
def reader(request):
    graph = kglite.KnowledgeGraph()
    if request.param == "graph":
        yield graph
    elif request.param == "session":
        yield graph.session()
    elif request.param == "frozen":
        yield graph.freeze()
    else:
        with graph.begin_read() as transaction:
            yield transaction


def _roundtrip(reader, value):
    return reader.cypher("RETURN $value AS value", params={"value": value}).to_list()[0]["value"]


def test_shared_acyclic_containers_are_converted_each_time(reader):
    shared_list = [1, True, None, "雪"]
    shared_map = {"values": shared_list}
    value = [shared_map, shared_map, (shared_list, shared_list)]
    expected_list = [1, True, None, "雪"]
    assert _roundtrip(reader, value) == [
        {"values": expected_list},
        {"values": expected_list},
        [expected_list, expected_list],
    ]


@pytest.mark.parametrize("value, expected", [(None, None), (True, True), (17, 17), (2.5, 2.5), ("雪", "雪")])
def test_scalar_controls(reader, value, expected):
    actual = _roundtrip(reader, value)
    assert actual == expected
    assert type(actual) is type(expected)


def test_ndarray_shapes_and_shared_objects(reader):
    shared = {"items": [1, 2]}
    objects = np.empty(2, dtype=object)
    objects[0] = shared
    objects[1] = shared
    value = [np.array([1.5]), np.array([[1, 2], [3, 4]]), np.array(7), objects]
    assert _roundtrip(reader, value) == [[1.5], [[1, 2], [3, 4]], 7, [shared, shared]]


def test_recovery_after_ordinary_map_key_error(reader):
    with pytest.raises(TypeError):
        _roundtrip(reader, [{1: "map keys must be strings"}])
    assert _roundtrip(reader, [{"id": 1}]) == [{"id": 1}]


@pytest.mark.parametrize(
    "value, offset",
    [
        (dt.datetime(2025, 1, 2, 3, 4, 5, 123456, tzinfo=dt.timezone.utc), dt.timedelta(0)),
        (
            dt.datetime(2025, 1, 2, 0, 30, 5, 123456, tzinfo=dt.timezone(dt.timedelta(hours=2))),
            dt.timedelta(hours=2),
        ),
        (
            dt.datetime(2025, 1, 2, 23, 30, 5, 123456, tzinfo=dt.timezone(dt.timedelta(hours=-5))),
            dt.timedelta(hours=-5),
        ),
        # An offset that is not whole seconds keeps the instant at UTC.
        (
            dt.datetime(2025, 1, 2, 3, 4, 5, 123456, tzinfo=dt.timezone(dt.timedelta(microseconds=500000))),
            dt.timedelta(0),
        ),
    ],
)
def test_aware_datetime_keeps_its_instant_and_offset_with_fraction(reader, value, offset):
    actual = _roundtrip(reader, {"events": [value]})["events"][0]
    assert type(actual) is dt.datetime
    assert actual == value
    assert actual.utcoffset() == offset


def test_naive_datetime_stays_naive_with_fraction(reader):
    value = dt.datetime(2025, 1, 2, 3, 4, 5, 123456)
    actual = _roundtrip(reader, {"events": [value]})["events"][0]
    assert actual == value
    assert actual.tzinfo is None


class SeasonalOffset(dt.tzinfo):
    def utcoffset(self, value):
        if value is None:
            return None
        return dt.timedelta(hours=2 if 4 <= value.month <= 10 else 1)

    def dst(self, value):
        return dt.timedelta(0)


def test_timezone_offset_is_evaluated_for_the_actual_date(reader):
    value = dt.datetime(2025, 7, 2, 0, 30, 5, 123456, tzinfo=SeasonalOffset())
    actual = _roundtrip(reader, value)
    assert actual == dt.datetime(2025, 7, 1, 22, 30, 5, 123456, tzinfo=dt.timezone.utc)
    assert actual.utcoffset() == dt.timedelta(hours=2), "the offset of that date, July, is +02:00"


class NoOffset(dt.tzinfo):
    def utcoffset(self, value):
        return None


def test_timezone_without_an_offset_remains_naive(reader):
    value = dt.datetime(2025, 1, 2, 3, 4, 5, 123456, tzinfo=NoOffset())
    actual = _roundtrip(reader, value)
    assert actual == dt.datetime(2025, 1, 2, 3, 4, 5, 123456)
    assert actual.tzinfo is None


def test_pure_date_remains_date_only(reader):
    got = _roundtrip(reader, dt.date(2025, 1, 2))
    # A date comes back as a date — not a datetime, and not text.
    assert got == dt.date(2025, 1, 2) and type(got) is dt.date


def test_aware_datetime_persists_with_its_offset(tmp_path):
    graph = kglite.KnowledgeGraph()
    value = dt.datetime(2025, 1, 2, 0, 30, 5, 123456, tzinfo=dt.timezone(dt.timedelta(hours=2)))
    graph.cypher("CREATE (:Event {id: 1, ts: $ts})", params={"ts": value})
    path = str(tmp_path / "aware.kgl")
    graph.save(path)
    loaded = kglite.load(path)
    actual = loaded.cypher("MATCH (e:Event) RETURN e.ts AS ts").to_list()[0]["ts"]
    assert actual == dt.datetime(2025, 1, 1, 22, 30, 5, 123456, tzinfo=dt.timezone.utc)
    assert actual.utcoffset() == dt.timedelta(hours=2)
    assert type(actual) is dt.datetime
