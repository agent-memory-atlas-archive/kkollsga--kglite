"""Several `valid_at` call sites in one statement each resolve to their own bounds.

The per-statement resolution cache holds one entry per (element type, bound-name
pair). A second type, a second name pair on the same type, or more pairs than
the cache has slots must all still answer from their own declaration.
"""

from __future__ import annotations

import datetime as dt

import pandas as pd
import pytest

import kglite

DAY = dt.date(2005, 6, 1)
N = 40


def _bounds(i: int, shift: int) -> tuple[dt.date | None, dt.date | None]:
    start = dt.date(2000 + (i + shift) % 10, 1, 1) if i % 7 else None
    end = dt.date(2004 + (i * 3 + shift) % 9, 1, 1) if i % 5 else None
    return start, end


def _open_at(start, end) -> bool:
    return (start is None or start <= DAY) and (end is None or DAY <= end)


@pytest.fixture
def graph() -> kglite.KnowledgeGraph:
    g = kglite.KnowledgeGraph()
    rows = {"A": [], "B": []}
    for label in rows:
        for i in range(N):
            a_from, a_to = _bounds(i, 0)
            b_from, b_to = _bounds(i, 3)
            rows[label].append({"id": i, "name": f"{label}{i}", "f1": a_from, "t1": a_to, "f2": b_from, "t2": b_to})
    for label, data in rows.items():
        frame = pd.DataFrame(data)
        for column in ("f1", "t1", "f2", "t2"):
            frame[column] = pd.to_datetime(frame[column])
        g.add_nodes(frame, label, "id", "name")
    g.add_relationships(
        pd.DataFrame({"a": range(N), "b": [(i * 7) % N for i in range(N)]}), "LINKS", "A", "a", "B", "b"
    )
    return g


def _expected(site_checks) -> int:
    count = 0
    for i in range(N):
        j = (i * 7) % N
        if all(_open_at(*check(i, j)) for check in site_checks):
            count += 1
    return count


def _run(g, where: str, disable_optimizer: bool) -> int:
    rows = g.cypher(
        f"MATCH (a:A)-[:LINKS]->(b:B) WHERE {where} RETURN count(*) AS c", disable_optimizer=disable_optimizer
    ).to_list()
    return rows[0]["c"]


@pytest.mark.parametrize("disable_optimizer", [False, True])
def test_two_types_and_two_name_pairs_in_one_statement(graph, disable_optimizer) -> None:
    d = "date('2005-06-01')"
    where = (
        f"valid_at(a, {d}, 'f1', 't1') AND valid_at(b, {d}, 'f2', 't2') "
        f"AND valid_at(a, {d}, 'f2', 't2') AND valid_at(b, {d}, 'f1', 't1')"
    )
    expected = _expected(
        [
            lambda i, j: _bounds(i, 0),
            lambda i, j: _bounds(j, 3),
            lambda i, j: _bounds(i, 3),
            lambda i, j: _bounds(j, 0),
        ]
    )
    assert 0 < expected < N
    assert _run(graph, where, disable_optimizer) == expected


@pytest.mark.parametrize("disable_optimizer", [False, True])
def test_more_name_pairs_than_cache_slots_each_answer_correctly(graph, disable_optimizer) -> None:
    d = "date('2005-06-01')"
    pairs = [("f1", "t1"), ("f2", "t2"), ("f1", "t2"), ("f2", "t1"), ("t1", "f1"), ("t2", "f2")]
    sites, checks = [], []
    for variable, pick in (("a", lambda i, j: i), ("b", lambda i, j: j)):
        for start, end in pairs:
            sites.append(f"valid_at({variable}, {d}, '{start}', '{end}')")
            shifts = {"f1": 0, "t1": 0, "f2": 3, "t2": 3}

            def check(i, j, start=start, end=end, pick=pick, shifts=shifts):
                k = pick(i, j)
                return tuple(_bounds(k, shifts[name])[0 if name[0] == "f" else 1] for name in (start, end))

            checks.append(check)
    assert len(sites) > 8  # more distinct keys than the cache has slots
    assert _run(graph, " AND ".join(sites), disable_optimizer) == _expected(checks)


def test_the_unknown_property_error_is_unchanged_on_a_cached_pair(graph) -> None:
    query = "MATCH (a:A) WHERE valid_at(a, date('2005-06-01'), 'f1', 'nope') RETURN count(*) AS c"
    with pytest.raises(kglite.CypherExecutionError) as excinfo:
        graph.cypher(query).to_list()
    assert (
        "valid_at(): property 'nope' does not exist on node type 'A' — no element of that type has it "
        "— so it cannot bound an interval. Check the property name"
    ) in str(excinfo.value)
