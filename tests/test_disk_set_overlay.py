"""A disk type served from its column file, with ``SET`` values laid over it and
rows appended past it, answers every valid-time read as the same data does in
memory, and keeps answering after the save that writes those cells into the
next generation.

The declared type here has timestamp bounds. A column-direct read of a bound
(the validity filter's fast path) must see the file's cells *and* the ``SET``
cells and the appended rows; a read that took the overlay column alone would
treat every row the statements did not touch as unbounded. That needs *both*
bound columns overlaid, so the ``SET`` statements below write ``valid_from`` and
``valid_to`` together.

Run: pytest tests/test_disk_set_overlay.py
"""

from __future__ import annotations

from pathlib import Path
import warnings

import numpy as np
import pandas as pd

import kglite

TYPE = "Posting"
FIRST_ID = 3_000_000_000_000
EPOCH = pd.Timestamp("1995-01-01")
BASE_ROWS = 3_000

INSTANTS = [
    "1996-03-01T00:00:00",
    "1999-07-15T12:00:00.000250",
    "2004-01-01T00:00:00",
    "2009-06-30T23:59:59.999999",
    "2015-02-01T00:00:00",
    "2040-01-01T00:00:00",
]


def postings(first: int, count: int) -> pd.DataFrame:
    """HR postings: an employee holds a role from `valid_from` until `valid_to`
    (open for one in three), with sub-second parts so the µs path is exercised."""
    n = np.arange(first, first + count, dtype="int64")
    start = (EPOCH + pd.to_timedelta(n * 3 * 86_400 * 10**6 + n % 997, unit="us")).astype("datetime64[us]")
    end = pd.Series(start + pd.to_timedelta(900 * 86_400 * 10**6 + n % 13, unit="us")).astype("datetime64[us]")
    end = end.where(n % 3 != 0)
    return pd.DataFrame(
        {
            "id": FIRST_ID + n,
            "name": [f"Posting {i}" for i in n],
            "role": [["analyst", "engineer", "manager", "director"][i % 4] for i in n],
            "valid_from": start,
            "valid_to": end.astype("datetime64[us]"),
        }
    )


def build(storage: str, path: Path | None) -> kglite.KnowledgeGraph:
    with warnings.catch_warnings():
        warnings.simplefilter("ignore")
        graph = (
            kglite.KnowledgeGraph(storage="disk", path=str(path)) if path else kglite.KnowledgeGraph(storage=storage)
        )
        graph.add_nodes(postings(0, BASE_ROWS), TYPE, "id", "name")
        graph.set_temporal(TYPE, "valid_from", "valid_to", convention="half_open")
    return graph


def counts(graph) -> dict[str, int]:
    out = {}
    for instant in INSTANTS:
        out[instant] = graph.cypher(
            f"FOR VALID_TIME AS OF datetime('{instant}') MATCH (p:{TYPE}) RETURN count(p) AS c", timeout_ms=0
        ).scalar()
        out[f"{instant}/analyst"] = graph.cypher(
            f"FOR VALID_TIME AS OF datetime('{instant}') MATCH (p:{TYPE}) "
            "WHERE p.role = 'analyst' RETURN count(p) AS c",
            timeout_ms=0,
        ).scalar()
    # Plain comparisons of a bound read the typed column directly where they can.
    for name, where in {
        "from_range": (
            "p.valid_from >= datetime('1997-01-01T00:00:00') AND p.valid_from < datetime('2006-01-01T00:00:00.000500')"
        ),
        "to_after": "p.valid_to > datetime('2000-06-01T00:00:00')",
        "to_before": "p.valid_to <= datetime('2003-03-03T00:00:00')",
        "from_equals": "p.valid_from = datetime('1997-01-01T00:00:00.000123')",
    }.items():
        out[name] = graph.cypher(f"MATCH (p:{TYPE}) WHERE {where} RETURN count(p) AS c", timeout_ms=0).scalar()
    out["order"] = graph.cypher(
        f"MATCH (p:{TYPE}) WHERE p.valid_to IS NOT NULL RETURN p.id AS id ORDER BY p.valid_to DESC, p.id LIMIT 7",
        timeout_ms=0,
    ).to_list()
    out["total"] = graph.cypher(f"MATCH (p:{TYPE}) RETURN count(p) AS c").scalar()
    out["open"] = graph.cypher(f"MATCH (p:{TYPE}) WHERE p.valid_to IS NULL RETURN count(p) AS c").scalar()
    return out


def rebounded(graph) -> None:
    """A `SET` that writes both bounds of every seventh row and closes another third."""
    graph.cypher(
        f"MATCH (p:{TYPE}) WHERE p.id % 7 = 0 "
        "SET p.valid_from = datetime('1997-01-01T00:00:00.000123'), "
        "p.valid_to = datetime('2001-01-01T00:00:00.000456')"
    )
    graph.cypher(
        f"MATCH (p:{TYPE}) WHERE p.id % 11 = 0 AND p.valid_to IS NULL "
        "SET p.valid_to = datetime('2050-05-05T05:05:05.000555')"
    )


def appended(graph, first: int, count: int) -> None:
    with warnings.catch_warnings():
        warnings.simplefilter("ignore")
        graph.add_nodes(postings(first, count), TYPE, "id", "name")


def test_declared_bounds_over_a_mapped_type_with_set_cells_and_a_tail_match_memory(tmp_path):
    path = tmp_path / "graph"
    disk = build("disk", path)
    memory = build("memory", None)
    disk.save(str(path))
    disk = kglite.load(str(path))  # the type is now served from its column file
    assert counts(disk) == counts(memory)

    for graph in (disk, memory):
        rebounded(graph)
    assert counts(disk) == counts(memory), "after SET of both bounds on base rows"

    for graph in (disk, memory):
        appended(graph, BASE_ROWS, 400)
    assert counts(disk) == counts(memory), "after appending rows past the base"

    for graph in (disk, memory):
        graph.cypher(
            f"MATCH (p:{TYPE}) WHERE p.id % 13 = 0 "
            "SET p.valid_from = datetime('1998-02-02T00:00:00.000777'), p.valid_to = datetime('2003-03-03T00:00:00')"
        )
    assert counts(disk) == counts(memory), "after SET over base and appended rows"

    # The save writes the overlay and the tail into the next generation; the
    # reloaded type answers as before, and again after a second cycle.
    disk.save(str(path))
    assert counts(disk) == counts(memory), "after the save, on the live handle"
    reloaded = kglite.load(str(path))
    assert counts(reloaded) == counts(memory), "after the save, reloaded"

    for graph in (reloaded, memory):
        graph.cypher(f"MATCH (p:{TYPE}) WHERE p.id % 17 = 0 SET p.valid_to = datetime('2060-01-01T00:00:00.000999')")
    reloaded.save(str(path))
    assert counts(kglite.load(str(path))) == counts(memory), "after a second SET, save and reload"
