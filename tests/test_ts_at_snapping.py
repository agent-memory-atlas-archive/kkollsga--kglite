"""ts_at reads the period containing a date / datetime key; coarser keys are refused.

Range functions keep key-in-range semantics: a period counts when its key (the
first day of the period) lies in [start, end].
"""

import pytest

import kglite


def _graph(resolution, keys, values):
    g = kglite.KnowledgeGraph()
    g.cypher("CREATE (:Unit {id: 1, title: 'A'})")
    g.set_timeseries("Unit", resolution=resolution, channels=["ch"])
    g.set_time_index(1, keys)
    g.add_ts_channel(1, "ch", values)
    return g


@pytest.fixture
def month_graph():
    return _graph("month", ["2015-01", "2015-02", "2015-03", "2015-04", "2015-06"], [10.0, 20.0, 30.0, 40.0, 60.0])


@pytest.fixture
def year_graph():
    return _graph("year", ["2014", "2015", "2016"], [1.0, 2.0, 3.0])


@pytest.fixture
def day_graph():
    return _graph("day", ["2015-06-14", "2015-06-15", "2015-06-16"], [1.0, 2.0, 3.0])


def val(g, expr):
    return g.cypher(f"MATCH (u:Unit) RETURN {expr} AS v")[0]["v"]


@pytest.mark.parametrize(
    "key",
    [
        "'2015-6'",
        "'2015-06-01'",
        "'2015-6-15'",
        "'2015-6-30'",
        "date('2015-06-15')",
        "date('2015-06-30')",
        "datetime('2015-06-15T13:45:00')",
        "datetime('2015-06-30T23:59:59')",
    ],
)
def test_month_series_reads_the_containing_month(month_graph, key):
    assert val(month_graph, f"ts_at(u.ch, {key})") == 60.0


def test_month_series_gap_and_edges(month_graph):
    assert val(month_graph, "ts_at(u.ch, date('2015-05-15'))") is None  # a month with no entry
    assert val(month_graph, "ts_at(u.ch, date('2015-03-31'))") == 30.0
    assert val(month_graph, "ts_at(u.ch, date('2015-04-01'))") == 40.0
    assert val(month_graph, "ts_at(u.ch, date('2014-12-31'))") is None


@pytest.mark.parametrize(
    "key", ["'2015'", "'2015-6'", "'2015-06-15'", "2015", "date('2015-12-31')", "datetime('2015-01-01T00:00:00')"]
)
def test_year_series_reads_the_containing_year(year_graph, key):
    assert val(year_graph, f"ts_at(u.ch, {key})") == 2.0


@pytest.mark.parametrize("key", ["'2015-06-15'", "date('2015-06-15')", "datetime('2015-06-15T23:00:00')"])
def test_day_series_reads_the_day(day_graph, key):
    assert val(day_graph, f"ts_at(u.ch, {key})") == 2.0


@pytest.mark.parametrize(
    "key, resolution",
    [("'2015'", "month"), ("2015", "month"), ("'2015'", "day"), ("'2015-6'", "day")],
)
def test_coarser_key_is_an_error(key, resolution):
    keys = {"month": ["2015-01"], "day": ["2015-01-01"]}[resolution]
    g = _graph(resolution, keys, [1.0])
    with pytest.raises(Exception, match=f"coarser than the series resolution '{resolution}'"):
        val(g, f"ts_at(u.ch, {key})")


def test_null_key_is_null(month_graph):
    assert val(month_graph, "ts_at(u.ch, null)") is None


def test_range_functions_use_key_in_range_with_dates(month_graph):
    # keys 2015-02-01.. 2015-04-01 lie in [02-15, 04-15] only for 03-01 and 04-01
    assert val(month_graph, "ts_sum(u.ch, date('2015-02-15'), date('2015-04-15'))") == 70.0
    assert val(month_graph, "ts_sum(u.ch, '2015-2', '2015-4')") == 90.0
    assert val(month_graph, "ts_sum(u.ch, date('2015-02-01'), date('2015-04-01'))") == 90.0


def test_range_functions_accept_datetime_bounds(month_graph):
    assert val(month_graph, "ts_sum(u.ch, datetime('2015-02-15T10:00:00'), datetime('2015-04-15T10:00:00'))") == 70.0
    assert val(month_graph, "ts_count(u.ch, datetime('2015-02-01T00:00:00'), date('2015-06-30'))") == 4
    assert val(month_graph, "ts_max(u.ch, date('2015-01-01'), datetime('2015-03-31T23:59:59'))") == 30.0
    assert val(month_graph, "ts_avg(u.ch, datetime('2015-01-01T00:00:00'), date('2015-02-01'))") == 15.0


def test_ts_delta_accepts_a_datetime(month_graph):
    assert val(month_graph, "ts_delta(u.ch, '2015-1', datetime('2015-03-20T00:00:00'))") == 20.0
