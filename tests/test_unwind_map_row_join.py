"""``WHERE m.key = row.key`` over a map-valued UNWIND row joins through the
type's index the way ``{key: row.key}`` and ``WITH row.key AS k`` do.

Before the planner pushed that WHERE into the pattern, every driving row
re-scanned the whole type: time grew with the square of the row count (release,
memory mode: 8,000 rows 8.3 s against 5.9 ms for the ``WITH`` spelling). The
scaling arm below is the detector; the answer arms pin that the pushed form
gives the WHERE's answers, including a projected value that is not a map.
"""

from __future__ import annotations

import datetime as dt
import time

import pandas as pd
import pytest

import kglite

SPELLINGS = {
    "where_row_key": "UNWIND $rows AS row MATCH (m:M) WHERE m.key = row.key RETURN count(m) AS c",
    "where_row_key_reversed": "UNWIND $rows AS row MATCH (m:M) WHERE row.key = m.key RETURN count(m) AS c",
    "inline_row_key": "UNWIND $rows AS row MATCH (m:M {key: row.key}) RETURN count(m) AS c",
    "with_alias": "UNWIND $rows AS row WITH row.key AS k MATCH (m:M) WHERE m.key = k RETURN count(m) AS c",
}


def _graph(n: int, storage: str = "memory") -> kglite.KnowledgeGraph:
    graph = kglite.KnowledgeGraph(storage=storage) if storage != "memory" else kglite.KnowledgeGraph()
    frame = pd.DataFrame({"id": range(n), "title": [f"t{i}" for i in range(n)], "key": [f"k{i}" for i in range(n)]})
    graph.add_nodes(frame, "M", "id", "title")
    return graph


def _rows(n: int) -> list[dict]:
    return [{"key": f"k{i}"} for i in range(n)]


@pytest.mark.parametrize("name", sorted(SPELLINGS))
def test_every_spelling_matches_each_driving_row_once(name):
    graph = _graph(300)
    rows = _rows(300) + [{"key": None}, {"other": 1}, {"key": "absent"}]
    assert graph.cypher(SPELLINGS[name], params={"rows": rows}).to_list() == [{"c": 300}]


@pytest.mark.parametrize("storage", ["memory", "mapped"])
def test_a_projected_map_value_keeps_its_cross_kind_answers(storage):
    graph = kglite.KnowledgeGraph(storage=storage) if storage != "memory" else kglite.KnowledgeGraph()
    graph.add_nodes(
        pd.DataFrame({"id": range(5), "title": list("abcde"), "y": [2020, 2021, 2022, 2023, 2024]}), "M", "id", "title"
    )
    where = "UNWIND $rows AS r MATCH (m:M) WHERE m.y = r.y RETURN m.id AS id"
    ids = lambda rows: sorted(r["id"] for r in graph.cypher(where, params={"rows": rows}).to_list())  # noqa: E731
    assert ids([{"y": 2021}, {"y": 2021.0}, {"y": "2021"}, {"y": None}, {"z": 1}]) == [1, 1]
    # A date is not a map: its component is evaluated, in the WHERE and in the pattern alike.
    date = dt.date(2022, 3, 1)
    for query in (
        "UNWIND $ds AS d MATCH (m:M) WHERE m.y = d.year RETURN m.id AS id",
        "UNWIND $ds AS d MATCH (m:M {y: d.year}) RETURN m.id AS id",
        "WITH date('2022-03-01') AS d MATCH (m:M {y: d.year}) RETURN m.id AS id",
    ):
        assert graph.cypher(query, params={"ds": [date]}).to_list() == [{"id": 2}], query


def _best_ms(graph, query, rows) -> float:
    best = float("inf")
    for _ in range(3):
        start = time.perf_counter()
        graph.cypher(query, params={"rows": rows}).to_list()
        best = min(best, (time.perf_counter() - start) * 1e3)
    return best


def test_the_where_spelling_scales_linearly_in_the_driving_rows():
    small, large = 1_000, 4_000
    ratio = {}
    for name in ("where_row_key", "inline_row_key"):
        small_ms = _best_ms(_graph(small), SPELLINGS[name], _rows(small))
        large_ms = _best_ms(_graph(large), SPELLINGS[name], _rows(large))
        ratio[name] = large_ms / small_ms
    # 4x the rows and 4x the nodes: linear work grows ~4x, a scan per row ~16x.
    assert ratio["where_row_key"] < 9, f"WHERE spelling grew {ratio['where_row_key']:.1f}x for 4x rows (linear ~4x)"
    assert ratio["inline_row_key"] < 9, f"pattern spelling grew {ratio['inline_row_key']:.1f}x for 4x rows"
