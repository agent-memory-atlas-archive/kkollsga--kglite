"""date / datetime +/- duration shift by calendar months, then days, then time.

A month shift clamps to the end of a shorter target month, as add_months()
does and as Neo4j does; it used to count every month as 30 days.
"""

import datetime

import pytest

import kglite


@pytest.fixture(scope="module")
def g():
    return kglite.KnowledgeGraph()


def val(g, expr, **params):
    return g.cypher(f"RETURN {expr} AS v", params=params)[0]["v"]


D = datetime.date


@pytest.mark.parametrize(
    "expr, expected",
    [
        ("date('2015-06-15') - duration({months: 11})", D(2014, 7, 15)),
        ("date('2015-06-15') + duration({months: 11})", D(2016, 5, 15)),
        ("date('2024-01-15') + duration({months: 1})", D(2024, 2, 15)),  # not 02-14
        ("date('2024-01-15') - duration({months: 1})", D(2023, 12, 15)),
        ("date('2024-01-31') + duration({months: 1})", D(2024, 2, 29)),  # leap-year clamp
        ("date('2023-01-31') + duration({months: 1})", D(2023, 2, 28)),
        ("date('2024-03-31') - duration({months: 1})", D(2024, 2, 29)),
        ("date('2023-03-31') - duration({months: 1})", D(2023, 2, 28)),
        ("date('2024-08-31') + duration({months: 1})", D(2024, 9, 30)),
        ("date('2016-02-29') + duration({years: 1})", D(2017, 2, 28)),
        ("date('2016-02-29') + duration({years: 4})", D(2020, 2, 29)),
        ("date('2016-02-29') - duration({years: 1})", D(2015, 2, 28)),
        ("date('2015-06-15') + duration({years: 1, months: 2})", D(2016, 8, 15)),
        ("duration({months: 2}) + date('2015-06-15')", D(2015, 8, 15)),
        ("date('2015-12-31') + duration({months: 2})", D(2016, 2, 29)),
        # a negative component subtracts
        ("date('2015-06-15') + duration({months: -3})", D(2015, 3, 15)),
        ("date('2015-06-15') - duration({months: -3})", D(2015, 9, 15)),
        # months first, then days: Jan 30 + 1 month clamps to Feb 29, then + 2 days
        ("date('2024-01-30') + duration({months: 1, days: 2})", D(2024, 3, 2)),
        ("date('2024-03-31') - duration({months: 1, days: 2})", D(2024, 2, 27)),
        ("date('2024-01-15') + duration({weeks: 1, days: 2})", D(2024, 1, 24)),
        # days-only is unchanged
        ("date('2024-01-15') + duration({days: 30})", D(2024, 2, 14)),
    ],
)
def test_date_with_duration(g, expr, expected):
    assert val(g, expr) == expected


@pytest.mark.parametrize(
    "expr, expected",
    [
        (
            "datetime('2024-01-31T10:30:00') + duration({months: 1, hours: 2})",
            datetime.datetime(2024, 2, 29, 12, 30),
        ),
        (
            "datetime('2024-03-31T23:00:00') - duration({months: 1, days: 1, hours: 1})",
            datetime.datetime(2024, 2, 28, 22, 0),
        ),
        (
            "datetime('2016-02-29T06:15:30') + duration({years: 1})",
            datetime.datetime(2017, 2, 28, 6, 15, 30),
        ),
        (
            "datetime('2015-06-15T13:45:00') - duration({months: 11})",
            datetime.datetime(2014, 7, 15, 13, 45),
        ),
        (
            "duration({months: 1, days: 1, minutes: 90}) + datetime('2024-01-31T22:30:00')",
            datetime.datetime(2024, 3, 2, 0, 0),
        ),
        ("datetime('2024-01-15T00:00:00') + duration({days: 30})", datetime.datetime(2024, 2, 14)),
    ],
)
def test_datetime_with_duration(g, expr, expected):
    assert val(g, expr) == expected


def test_matches_add_months_and_add_years(g):
    rows = g.cypher(
        "UNWIND ['2024-01-31', '2023-01-31', '2016-02-29', '2015-06-15', "
        "'2024-03-31', '2000-02-29', '1999-12-31'] AS s "
        "UNWIND [-25, -12, -11, -1, 0, 1, 2, 11, 12, 13, 25] AS n "
        "WITH date(s) AS d, n "
        "RETURN d + duration({months: n}) = add_months(d, n) AS plus, "
        "d - duration({months: n}) = add_months(d, -n) AS minus, "
        "d + duration({years: n}) = add_years(d, n) AS years"
    ).to_list()
    assert len(rows) == 7 * 11
    assert all(r["plus"] and r["minus"] and r["years"] for r in rows)


def test_out_of_range_is_null(g):
    # far beyond the representable calendar (about +/-262,000 years)
    assert val(g, "(date('2015-01-01') + duration({months: 2147483647})) IS NULL") is True
    assert val(g, "(date('2015-01-01') - duration({months: 2147483647})) IS NULL") is True
    assert val(g, "(datetime('2015-01-01T00:00:00') + duration({months: 2147483647})) IS NULL") is True


def test_duration_between_round_trips_through_days(g):
    # `between` only fills in days, so adding it back lands on the end date
    assert val(g, "date('2024-01-31') + duration.between(date('2024-01-31'), date('2025-03-01'))") == D(2025, 3, 1)
