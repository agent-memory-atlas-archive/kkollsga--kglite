"""Date-column and datetime-marshalling cells for the temporal value model.

These live outside ``test_bench_core.py`` on purpose: the CI perf gate pins the
core harness's cell set to ``baselines/current.json``, which only a release
captures (``tests/test_bench_harness_set.py``). Move them into the core harness
at the release that captures their baseline.

None of the cells touches a new ``Value`` variant, so any movement is the cost
of widened ``Value`` matches.
"""

from __future__ import annotations

from typing import NamedTuple

import numpy as np
import pandas as pd
import pytest

from kglite import KnowledgeGraph

TIMESTAMP_ROWS = 100_000
_TIMESTAMP_BASE = pd.Timestamp("2000-01-01")
_TIMESTAMP_STEP_US = 6_311_111

DATE_ROWS = 1_000_000
_DATE_DISTINCT = 3650  # about ten years of days
_DATE_EPOCH_DAY = 10_957  # 2000-01-01, in days since 1970


class FrameGraph(NamedTuple):
    graph: KnowledgeGraph
    frame: pd.DataFrame


@pytest.fixture(scope="module")
def timestamp_graph():
    """100k nodes with a µs-precision datetime64 column `t` (unique, unsorted)."""
    n = TIMESTAMP_ROWS
    nid = pd.RangeIndex(n).to_series()
    rank = (nid * 48_271) % n
    offset_us = rank * _TIMESTAMP_STEP_US + (nid % 997)
    t = (_TIMESTAMP_BASE + pd.to_timedelta(offset_us, unit="us")).astype("datetime64[us]")
    frame = pd.DataFrame({"nid": nid.to_numpy(), "name": [f"T{i}" for i in range(n)], "t": t.to_numpy()})
    graph = KnowledgeGraph()
    graph.add_nodes(frame, "T", "nid", "name")
    return FrameGraph(graph, frame)


@pytest.fixture(scope="module")
def date_graph():
    """1M nodes with a date-only datetime64 column `d` of 3650 distinct days.

    7919 is coprime with 3650, so the days are spread evenly and unsorted in
    insertion order.
    """
    n = DATE_ROWS
    nid = np.arange(n, dtype=np.int64)
    days = (nid * 7919) % _DATE_DISTINCT + _DATE_EPOCH_DAY
    frame = pd.DataFrame({"nid": nid, "name": ["x"] * n, "d": pd.to_datetime(days, unit="D")})
    graph = KnowledgeGraph()
    graph.add_nodes(frame, "D", "nid", "name")
    return FrameGraph(graph, frame)


@pytest.mark.benchmark
def test_bench_timestamp_marshal_to_python(benchmark, timestamp_graph):
    """100k timestamp cells converted to Python `datetime` objects."""
    graph, _ = timestamp_graph

    def query_and_consume():
        return graph.cypher("MATCH (n:T) RETURN n.t AS t").to_list()

    rows = benchmark(query_and_consume)
    assert len(rows) == TIMESTAMP_ROWS


@pytest.mark.benchmark
def test_bench_date_group_by_count(benchmark, date_graph):
    """Group 1M date cells into their 3650 distinct values (hash + equality)."""
    graph, _ = date_graph

    def query_and_consume():
        return graph.cypher("MATCH (n:D) RETURN n.d AS d, count(*) AS c").to_list()

    rows = benchmark(query_and_consume)
    assert len(rows) == _DATE_DISTINCT
    assert sum(row["c"] for row in rows) == DATE_ROWS


@pytest.mark.benchmark
def test_bench_date_where_ge_literal(benchmark, date_graph):
    """Range filter `n.d >= date(...)` over 1M date cells."""
    graph, frame = date_graph
    cutoff = pd.Timestamp("2005-01-01")
    expected = int((frame["d"] >= cutoff).sum())

    def query_and_consume():
        return graph.cypher("MATCH (n:D) WHERE n.d >= date('2005-01-01') RETURN count(*) AS c").to_list()

    assert benchmark(query_and_consume) == [{"c": expected}]


@pytest.mark.benchmark
def test_bench_date_marshal_to_python(benchmark, date_graph):
    """100k date cells converted to Python `date` objects."""
    graph, _ = date_graph

    def query_and_consume():
        return graph.cypher("MATCH (n:D) RETURN n.d AS d LIMIT 100000").to_list()

    assert len(benchmark(query_and_consume)) == 100_000
