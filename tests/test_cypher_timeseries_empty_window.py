"""``ts_sum`` over a window holding no finite value is ``+0.0``, never ``-0.0``.

The ``ts_*`` family over an all-missing channel or an empty window: ``ts_sum``
is ``0.0`` (an empty sum), ``ts_count`` is ``0``, and ``ts_avg`` / ``ts_min`` /
``ts_max`` are ``null`` (no identity element). Float's ``Iterator::sum`` starts
at ``-0.0``, which printed as ``-0.0`` for an empty window.

Red proof: before the fix every ``ts_sum`` case below returned ``-0.0``.
"""

from __future__ import annotations

import math

import pandas as pd
import pytest

import kglite

NAN = float("nan")


@pytest.fixture
def graph() -> kglite.KnowledgeGraph:
    g = kglite.KnowledgeGraph()
    g.add_nodes(pd.DataFrame({"id": [1], "title": ["S"]}), "S", "id", "title")
    g.set_timeseries("S", resolution="year", channels=["allnull", "mixed", "ok"])
    g.set_time_index(1, ["2001", "2002", "2003"])
    g.add_ts_channel(1, "allnull", [NAN, NAN, NAN])
    g.add_ts_channel(1, "mixed", [NAN, 2.0, NAN])
    g.add_ts_channel(1, "ok", [1.0, 2.0, 3.0])
    return g


def _value(g: kglite.KnowledgeGraph, expression: str):
    return g.cypher(f"MATCH (s:S) RETURN {expression} AS v").to_list()[0]["v"]


def _is_positive_zero(value) -> bool:
    return value == 0.0 and math.copysign(1.0, value) == 1.0


@pytest.mark.parametrize(
    "window",
    ["", ", '2001'", ", '2001', '2003'"],
    ids=["whole", "one year", "range"],
)
def test_an_all_missing_channel_sums_to_positive_zero(graph, window) -> None:
    assert _is_positive_zero(_value(graph, f"ts_sum(s.allnull{window})"))
    assert _value(graph, f"ts_count(s.allnull{window})") == 0
    for sibling in ("ts_avg", "ts_min", "ts_max"):
        assert _value(graph, f"{sibling}(s.allnull{window})") is None, sibling


@pytest.mark.parametrize("window", [", '1990'", ", '2010'", ", '1990', '1995'"])
def test_a_window_outside_the_series_sums_to_positive_zero(graph, window) -> None:
    for channel in ("ok", "mixed"):
        assert _is_positive_zero(_value(graph, f"ts_sum(s.{channel}{window})")), (channel, window)
        assert _value(graph, f"ts_count(s.{channel}{window})") == 0
        for sibling in ("ts_avg", "ts_min", "ts_max"):
            assert _value(graph, f"{sibling}(s.{channel}{window})") is None


def test_a_mixed_channel_sums_only_its_finite_values(graph) -> None:
    assert _value(graph, "ts_sum(s.mixed)") == 2.0
    assert _value(graph, "ts_count(s.mixed)") == 1
    # The window that holds only the missing years has no finite value.
    assert _is_positive_zero(_value(graph, "ts_sum(s.mixed, '2001')"))
    assert _is_positive_zero(_value(graph, "ts_sum(s.mixed, '2003')"))
    assert _value(graph, "ts_sum(s.mixed, '2001', '2002')") == 2.0


def test_the_empty_sum_never_prints_as_negative_zero(graph) -> None:
    result = graph.cypher("MATCH (s:S) RETURN ts_sum(s.allnull) AS v, ts_sum(s.ok, '1990') AS w").to_list()[0]
    assert repr(result["v"]) == "0.0" and repr(result["w"]) == "0.0", result
    # Arithmetic over the empty sum stays a number.
    assert _value(graph, "ts_sum(s.allnull) + 1") == 1.0
