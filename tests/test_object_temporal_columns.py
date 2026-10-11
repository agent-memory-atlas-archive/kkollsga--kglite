"""Object-dtype columns of `datetime.date` / `datetime.datetime` load as temporal values.

pandas has no date dtype, so a column of `datetime.date` objects is the normal
way to hold dates. Such a column used to be stored as text (and, under
`on_invalid="error"`, refused as "mixed-type"). Homogeneous object columns with
a native value type now load as that type, with `None`/`NaN`/`NaT` as null.
"""

from __future__ import annotations

import datetime as dt
from decimal import Decimal
import warnings

import pandas as pd
import pytest

import kglite

D = dt.date


def _obj(values):
    return pd.Series(values, dtype=object)


def _graph(values, **kw):
    g = kglite.KnowledgeGraph()
    with warnings.catch_warnings():
        warnings.simplefilter("error")
        g.add_nodes(pd.DataFrame({"id": list(range(len(values))), "d": _obj(values)}), "T", "id", **kw)
    return g


def _col(g):
    return [r["d"] for r in g.cypher("MATCH (n:T) RETURN n.d AS d ORDER BY n.id").to_list()]


def test_date_objects_load_as_dates_under_default_policy():
    g = _graph([D(2024, 3, 4), None, D(2024, 1, 2), float("nan"), pd.NaT])
    assert _col(g) == [D(2024, 3, 4), None, D(2024, 1, 2), None, None]


def test_date_comparison_and_chronological_order():
    g = _graph([D(2024, 3, 4), None, D(2024, 1, 2), D(2023, 12, 31)])
    early = g.cypher("MATCH (n:T) WHERE n.d < date('2024-02-01') RETURN n.id AS i ORDER BY n.id").to_list()
    assert [r["i"] for r in early] == [2, 3]
    ordered = g.cypher("MATCH (n:T) WHERE n.d IS NOT NULL RETURN n.d AS d ORDER BY n.d").to_list()
    assert [r["d"] for r in ordered] == [D(2023, 12, 31), D(2024, 1, 2), D(2024, 3, 4)]


@pytest.mark.parametrize(
    "cell",
    [
        dt.datetime(2024, 1, 2, 3, 4, 5, 6),
        pd.Timestamp("2024-01-02 03:04:05.000006"),
    ],
)
def test_datetime_objects_load_as_datetimes(cell):
    g = _graph([cell, None, dt.datetime(2024, 1, 3)])
    assert _col(g) == [dt.datetime(2024, 1, 2, 3, 4, 5, 6), None, dt.datetime(2024, 1, 3)]
    n = g.cypher("MATCH (n:T) WHERE n.d < datetime('2024-01-02T12:00:00') RETURN count(n) AS c").scalar()
    assert n == 1


def test_aware_datetimes_are_stored_as_utc():
    tz = dt.timezone(dt.timedelta(hours=2))
    g = _graph([dt.datetime(2024, 1, 2, 3, 0, tzinfo=tz)])
    assert _col(g) == [dt.datetime(2024, 1, 2, 1, 0)]


def test_relationship_properties_take_the_same_path():
    g = kglite.KnowledgeGraph()
    g.add_nodes(pd.DataFrame({"id": [1, 2]}), "P", "id")
    frame = pd.DataFrame(
        {
            "a": [1, 2],
            "b": [2, 1],
            "since": _obj([D(2024, 3, 4), None]),
            "at": _obj([dt.datetime(2024, 1, 2, 3), dt.datetime(2024, 1, 2, 4)]),
        }
    )
    with warnings.catch_warnings():
        warnings.simplefilter("error")
        g.add_relationships(frame, "KNOWS", "P", "a", "P", "b", columns=["since", "at"])
    rows = g.cypher("MATCH (:P)-[r:KNOWS]->(:P) RETURN r.since AS s, r.at AS t ORDER BY r.at").to_list()
    assert rows == [
        {"s": D(2024, 3, 4), "t": dt.datetime(2024, 1, 2, 3)},
        {"s": None, "t": dt.datetime(2024, 1, 2, 4)},
    ]


def test_homogeneous_int_and_float_objects_load_as_numbers():
    ints = _graph([1, None, 3])
    assert _col(ints) == [1, None, 3]
    floats = _graph([1.5, None, 2.5])
    assert _col(floats) == [1.5, None, 2.5]
    assert floats.cypher("MATCH (n:T) RETURN sum(n.d) AS s").scalar() == 4.0


def test_timedelta_objects_are_durations_with_their_sub_second_part():
    g = _graph([dt.timedelta(seconds=5), None])
    assert g.cypher("MATCH (n:T) RETURN n.d IS NOT NULL AS ok ORDER BY n.id").to_list()[0]["ok"] is True
    fine = _graph([dt.timedelta(milliseconds=5)])
    assert fine.cypher("MATCH (n:T) RETURN n.d AS d").to_list() == [
        {"d": {"months": 0, "days": 0, "seconds": 0, "nanoseconds": 5_000_000}}
    ]


@pytest.mark.parametrize(
    "values",
    [
        [D(2024, 1, 2), dt.datetime(2024, 1, 2, 3)],
        [10, "N/A"],
        [1, 2**70],
        [Decimal("1.5"), None],
        [dt.time(1, 2), None],
    ],
)
def test_mixed_or_unmapped_object_columns_stay_refused_under_error(values):
    with pytest.raises(kglite.ArgumentError, match="object dtype"):
        _graph(values)
