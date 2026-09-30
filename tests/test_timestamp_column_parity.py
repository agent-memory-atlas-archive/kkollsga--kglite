"""Datetime properties are stored in a typed column; every value reads back as
it was written, in every storage mode and after a save and reload.

A timestamp the typed column cannot hold exactly — finer than a microsecond,
which `datetime('...123456789')` and `localdatetime()` on Linux both produce —
must keep its precision and must never shift the rows after it. A leap second
cannot be written from Cypher (`datetime('...:60')` reads as `:59`), so that
case is a Rust unit test (`timestamp_column_tests`).
"""

import datetime as dt

import pandas as pd
import pytest

import kglite

pytestmark = pytest.mark.parity

MODES = ["memory", "mapped", "disk", "disk_reopened", "memory_kgl", "mapped_kgl"]

NS = "2009-06-30T12:00:00.123456789"  # nanosecond precision
US = "2010-01-01T00:00:00.500"  # exact in microseconds


def _fresh(mode, tmp_path):
    if mode.startswith("disk"):
        return kglite.KnowledgeGraph(storage="disk", path=str(tmp_path / "g"))
    if mode.startswith("mapped"):
        return kglite.KnowledgeGraph(storage="mapped")
    return kglite.KnowledgeGraph()


def _reopen(graph, mode, tmp_path):
    """Save and reload where the mode has a file; a no-op for the live modes."""
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


def _strings(graph, label="E", prop="t"):
    rows = graph.cypher(f"MATCH (n:{label}) RETURN n.id AS id, toString(n.{prop}) AS s ORDER BY id").to_list()
    return [(r["id"], r["s"]) for r in rows]


@pytest.mark.parametrize("mode", MODES)
def test_microsecond_timestamps_round_trip_with_nulls_and_set(mode, tmp_path):
    graph = _fresh(mode, tmp_path)
    times = [
        dt.datetime(2009, 11, 6, 12, 0, 0, 123456),
        None,
        dt.datetime(1601, 1, 1),
        dt.datetime(2262, 4, 12, 1, 2, 3),
    ]
    for i, t in enumerate(times, 1):
        graph.cypher("CREATE (:E {id: $id, t: $t})", params={"id": i, "t": t})
    graph.cypher("MATCH (n:E {id: 2}) SET n.t = $t", params={"t": dt.datetime(2020, 2, 29, 23, 59, 59, 999999)})
    graph.cypher("MATCH (n:E {id: 1}) SET n.t = null")
    graph = _reopen(graph, mode, tmp_path)
    rows = graph.cypher("MATCH (n:E) RETURN n.id AS id, n.t AS t ORDER BY id").to_list()
    expected = [
        None,
        dt.datetime(2020, 2, 29, 23, 59, 59, 999999),
        dt.datetime(1601, 1, 1),
        dt.datetime(2262, 4, 12, 1, 2, 3),
    ]
    assert [r["t"] for r in rows] == expected
    assert all(type(r["t"]) is (dt.datetime if r["t"] else type(None)) for r in rows)


@pytest.mark.parametrize("mode", MODES)
def test_a_row_without_a_value_then_a_nanosecond_value_keeps_later_rows_aligned(mode, tmp_path):
    graph = _fresh(mode, tmp_path)
    # One statement, so the type's property set is known when the first row
    # (which carries no timestamp) is stored.
    graph.cypher(
        f"CREATE (:E {{id: 1}}), (:E {{id: 2, t: datetime('{NS}Z')}}), "
        f"(:E {{id: 3, t: datetime('{US}Z')}}), (:E {{id: 4, t: datetime('{NS}Z')}})"
    )
    graph.cypher("CREATE (:E {id: 5})")
    graph = _reopen(graph, mode, tmp_path)
    assert _strings(graph) == [(1, None), (2, NS), (3, US), (4, NS), (5, None)]


@pytest.mark.parametrize("mode", MODES)
def test_set_of_a_nanosecond_value_over_a_typed_column_keeps_every_row(mode, tmp_path):
    graph = _fresh(mode, tmp_path)
    frame = pd.DataFrame(
        {
            "id": [1, 2, 3],
            "name": ["a", "b", "c"],
            "t": pd.to_datetime(["2010-01-01T00:00:00.5", None, "2011-02-03T04:05:06.000007"]).astype("datetime64[us]"),
        }
    )
    graph.add_nodes(frame, "E", "id", "name")
    graph.cypher(f"MATCH (n:E {{id: 2}}) SET n.t = datetime('{NS}Z')")
    graph = _reopen(graph, mode, tmp_path)
    assert _strings(graph) == [(1, US), (2, NS), (3, "2011-02-03T04:05:06.000007")]


@pytest.mark.parametrize("mode", MODES)
def test_set_of_a_nanosecond_value_over_an_all_null_column_is_not_lost(mode, tmp_path):
    graph = _fresh(mode, tmp_path)
    frame = pd.DataFrame({"id": [1, 2], "name": ["a", "b"], "t": pd.to_datetime([None, None]).astype("datetime64[us]")})
    graph.add_nodes(frame, "E", "id", "name")
    graph.cypher(f"MATCH (n:E {{id: 1}}) SET n.t = datetime('{NS}Z')")
    graph.cypher(f"CREATE (:E {{id: 3, t: datetime('{US}Z')}})")
    graph = _reopen(graph, mode, tmp_path)
    assert _strings(graph) == [(1, NS), (2, None), (3, US)]


@pytest.mark.parametrize("mode", ["memory", "mapped", "disk"])
def test_rollback_restores_a_timestamp_column(mode, tmp_path):
    graph = _fresh(mode, tmp_path)
    graph.cypher(
        "CREATE (:E {id: 1, t: $a}), (:E {id: 2, t: $b})",
        params={"a": dt.datetime(2010, 1, 1, 0, 0, 0, 500000), "b": dt.datetime(2011, 1, 1)},
    )
    before = _strings(graph)
    tx = graph.begin()
    tx.cypher(f"MATCH (n:E {{id: 1}}) SET n.t = datetime('{NS}Z')")
    tx.cypher(f"CREATE (:E {{id: 3, t: datetime('{NS}Z')}})")
    assert _strings(tx)[0] == (1, NS)
    tx.rollback()
    assert _strings(graph) == before
    graph.cypher("CREATE (:E {id: 3, t: $t})", params={"t": dt.datetime(2012, 1, 1)})
    assert _strings(graph)[2] == (3, "2012-01-01T00:00:00")


@pytest.mark.parametrize("mode", MODES)
def test_localdatetime_rows_read_back_in_order(mode, tmp_path):
    """`localdatetime()` carries nanoseconds on Linux; none may shift a row."""
    graph = _fresh(mode, tmp_path)
    for i in range(1, 41):
        graph.cypher("CREATE (:E {id: $id, t: localdatetime()})", params={"id": i})
    graph = _reopen(graph, mode, tmp_path)
    rows = _strings(graph)
    assert [i for i, _ in rows] == list(range(1, 41))
    assert all(s is not None for _, s in rows), rows
    assert [s for _, s in rows] == sorted(s for _, s in rows)
    assert graph.cypher("MATCH (n:E) RETURN count(n.t) AS c").to_list() == [{"c": 40}]


# ── valid time on timestamp bounds ────────────────────────────────────────────

ROLES = {
    1: (dt.datetime(2020, 1, 1), dt.datetime(2020, 6, 30, 12)),
    2: (dt.datetime(2020, 3, 1, 8, 30, 0, 1), None),
    3: (None, dt.datetime(2020, 2, 1)),
    4: (dt.datetime(2020, 6, 30, 12), dt.datetime(2020, 6, 30, 12)),
    5: (dt.datetime(2019, 12, 31, 23, 59, 59, 999999), dt.datetime(2020, 1, 1)),
    6: (None, None),
}
INSTANTS = [
    dt.datetime(2019, 12, 31, 23, 59, 59, 999999),
    dt.datetime(2020, 1, 1),
    dt.datetime(2020, 1, 1, 0, 0, 0, 1),
    dt.datetime(2020, 3, 1, 8, 30, 0, 1),
    dt.datetime(2020, 6, 30, 11, 59, 59, 999999),
    dt.datetime(2020, 6, 30, 12),
    dt.datetime(2020, 6, 30, 12, 0, 0, 1),
    dt.date(2020, 1, 1),
    dt.date(2020, 2, 1),
    dt.date(2020, 6, 30),
    dt.date(2020, 7, 1),
]


def _oracle(instant, convention):
    """Ids valid at `instant`: a timestamp instant compares exactly, a date at
    day grain (half-open, a timestamp end against a date is the day's midnight)."""
    valid = []
    for role, (start, end) in ROLES.items():
        half_open = convention == "half_open"
        if start is not None and end is not None and (end <= start if half_open else end < start):
            continue
        if isinstance(instant, dt.datetime):
            starts = start is None or start <= instant
            ends = end is None or (instant < end if half_open else instant <= end)
        else:
            starts = start is None or start.date() <= instant
            if end is None:
                ends = True
            elif half_open:
                ends = end > dt.datetime.combine(instant, dt.time.min)
            else:
                ends = instant <= end.date()
        if starts and ends:
            valid.append(role)
    return valid


@pytest.mark.filterwarnings("ignore:.*rows of node label:UserWarning")
@pytest.mark.parametrize("index", ["indexed", "residual"])
@pytest.mark.parametrize("convention", ["closed", "half_open"])
@pytest.mark.parametrize("mode", MODES)
def test_valid_at_on_timestamp_bounds(mode, convention, index, tmp_path, monkeypatch):
    if index == "residual":
        # A byte cap the endpoint index cannot fit under: every element is
        # judged by its own bounds instead of a prebuilt index.
        monkeypatch.setenv("KGLITE_TEMPORAL_INDEX_MAX_BYTES", "1")
    graph = _fresh(mode, tmp_path)
    for role, (start, end) in ROLES.items():
        graph.cypher("CREATE (:Role {id: $id, vf: $vf, vt: $vt})", params={"id": role, "vf": start, "vt": end})
    graph.set_temporal("Role", "vf", "vt", convention=convention)
    graph = _reopen(graph, mode, tmp_path)
    for instant in INSTANTS:
        literal = "datetime($t)" if isinstance(instant, dt.datetime) else "date($t)"
        text = instant.isoformat()
        rows = graph.cypher(
            f"FOR VALID_TIME AS OF {literal} MATCH (r:Role) RETURN r.id AS id ORDER BY id", params={"t": text}
        ).to_list()
        assert [r["id"] for r in rows] == _oracle(instant, convention), (mode, convention, index, instant)
        count = graph.cypher(f"FOR VALID_TIME AS OF {literal} MATCH (r:Role) RETURN count(*) AS c", params={"t": text})
        assert count.to_list() == [{"c": len(_oracle(instant, convention))}]


@pytest.mark.filterwarnings("ignore:.*rows of node label:UserWarning")
@pytest.mark.parametrize("mode", MODES)
def test_valid_at_with_a_nanosecond_bound_reads_the_exact_bound(mode, tmp_path):
    """One nanosecond-precision bound keeps its column out of the typed form; the
    answers are the same exact ones."""
    graph = _fresh(mode, tmp_path)
    graph.cypher("CREATE (:Role {id: 1, vf: $vf}), (:Role {id: 2, vf: $vf})", params={"vf": dt.datetime(2020, 1, 1)})
    graph.cypher("MATCH (r:Role {id: 2}) SET r.vt = datetime('2020-06-30T12:00:00.000000001Z')")
    graph.cypher("MATCH (r:Role {id: 1}) SET r.vt = $vt", params={"vt": dt.datetime(2020, 6, 30, 12)})
    graph.set_temporal("Role", "vf", "vt", convention="closed")
    graph = _reopen(graph, mode, tmp_path)

    def at(t):
        query = "FOR VALID_TIME AS OF datetime($t) MATCH (r:Role) RETURN r.id AS id ORDER BY id"
        return [r["id"] for r in graph.cypher(query, params={"t": t}).to_list()]

    assert at("2020-06-30T12:00:00") == [1, 2]
    assert at("2020-06-30T12:00:00.000001") == []


# ── ordering and filtering ───────────────────────────────────────────────────


@pytest.mark.parametrize("mode", MODES)
def test_order_by_and_where_over_a_timestamp_column(mode, tmp_path):
    graph = _fresh(mode, tmp_path)
    base = dt.datetime(2015, 5, 5, 5, 5, 5)
    times = {i: base + dt.timedelta(microseconds=(i * 7919) % 101 * 1_000_003) for i in range(1, 31)}
    for i, t in times.items():
        graph.cypher("CREATE (:E {id: $id, t: $t})", params={"id": i, "t": t})
    graph.cypher("CREATE (:E {id: 31})")
    graph = _reopen(graph, mode, tmp_path)
    by_time = sorted(times, key=lambda i: (times[i], i))
    top = graph.cypher("MATCH (n:E) WHERE n.t IS NOT NULL RETURN n.id AS id ORDER BY n.t DESC LIMIT 5").to_list()
    assert [r["id"] for r in top] == by_time[::-1][:5]
    ascending = graph.cypher("MATCH (n:E) WHERE n.t IS NOT NULL RETURN n.id AS id, n.t AS t ORDER BY n.t ASC").to_list()
    assert [r["t"] for r in ascending] == sorted(times.values())
    cutoff = sorted(times.values())[15]
    count = graph.cypher("MATCH (n:E) WHERE n.t >= $t RETURN count(*) AS c", params={"t": cutoff}).to_list()
    assert count == [{"c": sum(1 for t in times.values() if t >= cutoff)}]
    equal = graph.cypher("MATCH (n:E) WHERE n.t = $t RETURN n.id AS id", params={"t": cutoff}).to_list()
    assert {r["id"] for r in equal} == {i for i, t in times.items() if t == cutoff}
    assert graph.cypher("MATCH (n:E) WHERE n.t IS NULL RETURN n.id AS id").to_list() == [{"id": 31}]


@pytest.mark.parametrize("mode", MODES)
def test_timestamp_comparisons_agree_with_a_python_oracle(mode, tmp_path):
    """Every comparison operator, a range, equality and a NULL row, with a bound
    on a stored value (the boundary rows) and one between two."""
    graph = _fresh(mode, tmp_path)
    base = dt.datetime(1969, 12, 31, 23, 59, 59, 999990)  # straddles the epoch
    times = {i: base + dt.timedelta(microseconds=(i * 37) % 23 * 3) for i in range(1, 41)}
    for i, t in times.items():
        graph.cypher("CREATE (:E {id: $id, t: $t})", params={"id": i, "t": t})
    graph.cypher("CREATE (:E {id: 41})")
    graph = _reopen(graph, mode, tmp_path)
    stored = sorted(set(times.values()))
    bounds = [stored[5], stored[5] + dt.timedelta(microseconds=1), stored[-1], stored[0] - dt.timedelta(microseconds=1)]
    ops = {
        ">": lambda t, b: t > b,
        ">=": lambda t, b: t >= b,
        "<": lambda t, b: t < b,
        "<=": lambda t, b: t <= b,
        "=": lambda t, b: t == b,
    }
    for bound in bounds:
        for op, holds in ops.items():
            rows = graph.cypher(f"MATCH (n:E) WHERE n.t {op} $b RETURN n.id AS id ORDER BY id", params={"b": bound})
            expected = [i for i, t in times.items() if holds(t, bound)]
            assert [r["id"] for r in rows.to_list()] == expected, (op, bound)
            # The aggregate form takes the column-major filter.
            counted = graph.cypher(f"MATCH (n:E) WHERE n.t {op} $b RETURN count(*) AS c", params={"b": bound})
            assert counted.to_list() == [{"c": len(expected)}], (op, bound)
        low, high = bound, bound + dt.timedelta(microseconds=6)
        rows = graph.cypher(
            "MATCH (n:E) WHERE n.t >= $lo AND n.t < $hi RETURN n.id AS id ORDER BY id", params={"lo": low, "hi": high}
        )
        in_range = [i for i, t in times.items() if low <= t < high]
        assert [r["id"] for r in rows.to_list()] == in_range, bound
        counted = graph.cypher(
            "MATCH (n:E) WHERE n.t >= $lo AND n.t < $hi RETURN count(*) AS c", params={"lo": low, "hi": high}
        )
        assert counted.to_list() == [{"c": len(in_range)}], bound
        inline = graph.cypher("MATCH (n:E {t: $b}) RETURN n.id AS id ORDER BY id", params={"b": bound})
        assert [r["id"] for r in inline.to_list()] == [i for i, t in times.items() if t == bound]


@pytest.mark.parametrize("mode", MODES)
def test_top_k_over_timestamps_agrees_with_a_stable_sort(mode, tmp_path):
    """LIMIT k over a timestamp key: ties, NULL rows, both directions and a
    second key, for k below, at and above the row count."""
    graph = _fresh(mode, tmp_path)
    base = dt.datetime(2001, 1, 1, 0, 0, 0)
    # Only 7 distinct instants over 60 rows, so most keys tie; every 9th row has none.
    times = {i: (None if i % 9 == 0 else base + dt.timedelta(microseconds=(i * 5) % 7)) for i in range(1, 61)}
    graph.cypher("CREATE (:E {id: 0, t: $t})", params={"t": base})
    for i, t in times.items():
        graph.cypher("CREATE (:E {id: $id, t: $t})", params={"id": i, "t": t})
    graph = _reopen(graph, mode, tmp_path)
    everything = {0: base, **times}
    for k in (1, 3, 10, 59, 61, 100):
        asc = sorted(everything, key=lambda i: (everything[i] is None, everything[i] or base, i))
        desc = sorted(everything, key=lambda i: (everything[i] is not None, -(everything[i] or base).timestamp(), i))
        got = graph.cypher(f"MATCH (n:E) RETURN n.id AS id ORDER BY n.t ASC LIMIT {k}").to_list()
        assert [r["id"] for r in got] == asc[:k], ("ASC", k)
        got = graph.cypher(f"MATCH (n:E) RETURN n.id AS id ORDER BY n.t DESC LIMIT {k}").to_list()
        assert [r["id"] for r in got] == desc[:k], ("DESC", k)
        got = graph.cypher(f"MATCH (n:E) RETURN n.id AS id ORDER BY n.t DESC, n.id DESC LIMIT {k}").to_list()
        by_pair = sorted(
            everything, key=lambda i: (everything[i] is not None, -(everything[i] or base).timestamp(), -i)
        )
        assert [r["id"] for r in got] == by_pair[:k], ("DESC, id DESC", k)
        got = graph.cypher(f"MATCH (n:E) WHERE n.id > 0 RETURN n.id AS id ORDER BY n.t DESC LIMIT {k}").to_list()
        assert [r["id"] for r in got] == [i for i in desc if i > 0][:k], ("WHERE", k)
