"""Rows appended to a reopened disk graph are served from a tail beside the
mapped column file, and every read answers as the same data does in memory.

A reopened type is served from its published column file. A row appended to it
lands in a tail store (base rows stay in the file; rows past them resolve in
the tail), and a save writes the file's regions and the tail's into the next
generation. The golden below runs org-chart data through two cycles of
reopen -> append (bulk load and Cypher ``CREATE``) -> failed statements ->
``SET`` on base and tail rows -> ``DELETE`` -> save, on a disk graph and on an
in-memory graph given the same operations, and compares every read path after
each step: property reads, id and title lookups, ``WHERE`` on hoisted typed
columns (integer, string, timestamp), ``ORDER BY ... LIMIT``, counts,
``DISTINCT``, edges resolved through the id index, valid-time counts and the
fluent ``where``.

Run: pytest tests/test_disk_tail_store.py
"""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path
import warnings

import numpy as np
import pandas as pd
import pytest

import kglite

TYPE = "Employee"
DEPARTMENTS = ["Sales", "Engineering", "Finance", "People", "Legal"]
FIRST_ID = 3_100_000_000_000  # above u32: the id index is the compact int64 one
EPOCH = pd.Timestamp("1995-03-01")
BOOM = "duration({months: 2147483648})"


def staff(first: int, count: int) -> pd.DataFrame:
    """``count`` employees numbered from ``first``; every column has nulls and
    sub-second timestamps, so the typed-column paths are exercised."""
    n = np.arange(first, first + count, dtype="int64")
    hired = (EPOCH + pd.to_timedelta(n * 86_400 * 10**6 + n % 997, unit="us")).astype("datetime64[us]")
    left = pd.Series(hired + pd.to_timedelta(400 * 86_400 * 10**6 + n % 13, unit="us")).astype("datetime64[us]")
    left = left.where(n % 3 == 0)  # two thirds are still employed
    salary = pd.Series(50_000.0 + (n % 40) * 1_250.5).where(n % 4 != 0)
    return pd.DataFrame(
        {
            "id": FIRST_ID + n,
            "name": [f"Employee {i}" for i in n],
            "dept": [DEPARTMENTS[i % 5] for i in n],
            "level": (n % 9).astype("int64"),
            "hired": hired,
            "left": left.astype("datetime64[us]"),
            "salary": salary,
        }
    )


def works_in(frame: pd.DataFrame) -> pd.DataFrame:
    return pd.DataFrame({"id": frame["id"], "dept": frame["dept"]})


def cypher_rows(first: int, count: int) -> str:
    return (
        f"UNWIND range({first}, {first + count - 1}) AS i "
        f"CREATE (:{TYPE} {{id: {FIRST_ID} + i, name: 'Employee ' + toString(i), dept: 'Legal', "
        "level: i % 9, hired: datetime('2019-06-01T00:00:00.000123'), salary: 61000.5}) "
    )


def failing_create(first: int) -> str:
    return (
        f"UNWIND range({first}, {first + 299}) AS i "
        f"CREATE (:{TYPE} {{id: {FIRST_ID} + i, name: 'never', dept: 'Ops', level: 1, "
        f"hired: datetime('2020-01-01T00:00:00'), salary: CASE WHEN i = {first + 250} THEN {BOOM} ELSE 1.0 END}}) "
    )


@dataclass
class Pair:
    disk: kglite.KnowledgeGraph
    memory: kglite.KnowledgeGraph
    path: str

    def both(self, fn):
        fn(self.disk)
        fn(self.memory)

    def reopen(self) -> None:
        self.disk.save(self.path)
        self.disk = kglite.load(self.path)


def rows(graph, query, **params):
    return graph.cypher(query, params=params or None, timeout_ms=0).to_list()


def canon(result):
    return sorted(repr(sorted(row.items())) for row in result)


# Every read path the tail has to route, by query.
PROBES = {
    "count": f"MATCH (e:{TYPE}) RETURN count(e) AS c",
    "timestamp_range": (
        f"MATCH (e:{TYPE}) WHERE e.hired >= datetime('2001-01-01T00:00:00') "
        "AND e.hired < datetime('2012-06-01T12:00:00.000500') RETURN count(e) AS c"
    ),
    "timestamp_null": f"MATCH (e:{TYPE}) WHERE e.left IS NULL RETURN count(e) AS c",
    "timestamp_after": f"MATCH (e:{TYPE}) WHERE e.left > datetime('2004-01-01T00:00:00') RETURN count(e) AS c",
    "int_and_string": f"MATCH (e:{TYPE}) WHERE e.level >= 5 AND e.dept = 'Sales' RETURN count(e) AS c",
    "int_sum_by_dept": (
        f"MATCH (e:{TYPE}) RETURN e.dept AS d, count(*) AS c, sum(e.level) AS s, count(e.salary) AS p ORDER BY d"
    ),
    "float_filter": (
        f"MATCH (e:{TYPE}) WHERE e.salary >= 70000.0 RETURN count(e) AS c, min(e.salary) AS lo, max(e.salary) AS hi"
    ),
    "order_limit": f"MATCH (e:{TYPE}) RETURN e.id AS id, e.name AS n ORDER BY e.hired DESC, e.id LIMIT 9",
    "order_limit_asc": f"MATCH (e:{TYPE}) WHERE e.left IS NOT NULL RETURN e.id AS id ORDER BY e.left, e.id LIMIT 9",
    "distinct": f"MATCH (e:{TYPE}) RETURN DISTINCT e.dept AS d ORDER BY d",
    "distinct_level": f"MATCH (e:{TYPE}) WHERE e.dept = 'Legal' RETURN DISTINCT e.level AS l ORDER BY l",
    "title_equals": f"MATCH (e:{TYPE}) WHERE e.name = $n RETURN e.id AS id",
    "title_prefix": f"MATCH (e:{TYPE}) WHERE e.name STARTS WITH 'Employee 31' RETURN count(e) AS c",
    "valid_time": (
        f"FOR VALID_TIME AS OF datetime('2002-06-30T12:34:56.789012') MATCH (e:{TYPE}) RETURN count(e) AS c"
    ),
    "edges": f"MATCH (e:{TYPE})-[:WORKS_IN]->(d:Dept) RETURN d.id AS d, count(e) AS c ORDER BY d",
}


def lookups(graph, ids):
    out = {}
    for i in ids:
        out[i] = canon(
            rows(
                graph,
                f"MATCH (e:{TYPE} {{id: $i}}) RETURN e.name AS name, e.dept AS dept, e.level AS level, "
                "e.hired AS hired, e.left AS left, e.salary AS salary, properties(e) AS props",
                i=FIRST_ID + i,
            )
        )
    return out


def fluent(graph):
    return {
        "sales": graph.select(TYPE).where({"dept": "Sales"}).len(),
        "senior_legal": graph.select(TYPE).where({"level": (">=", 6), "dept": "Legal"}).len(),
        "late_hired": graph.select(TYPE).where({"hired": (">=", pd.Timestamp("2010-01-01"))}).len(),
        "open": graph.select(TYPE).where({"left": "is_null"}).len(),
        "ids": sorted(graph.select(TYPE).where({"dept": "Ops"}).ids()),
    }


def total(graph) -> int:
    """Every employee ever stored, valid today or not."""
    return graph.select(TYPE, temporal=False).len()


def compare(pair: Pair, step: str, sample_ids) -> None:
    """Every probe on the disk graph equals the same probe in memory."""
    for name, query in PROBES.items():
        params = {"n": "Employee 77"} if name == "title_equals" else {}
        got, want = rows(pair.disk, query, **params), rows(pair.memory, query, **params)
        assert canon(got) == canon(want), f"{step}: {name}\n disk   {got}\n memory {want}"
    assert lookups(pair.disk, sample_ids) == lookups(pair.memory, sample_ids), f"{step}: lookups"
    assert fluent(pair.disk) == fluent(pair.memory), f"{step}: fluent"


@pytest.fixture
def pair(tmp_path):
    path = str(tmp_path / "staff")
    with warnings.catch_warnings():
        warnings.simplefilter("ignore")
        disk = kglite.KnowledgeGraph(storage="disk", path=path)
        memory = kglite.KnowledgeGraph()
        result = Pair(disk, memory, path)
        first = staff(0, 1500)
        second = staff(1500, 1500)
        departments = pd.DataFrame({"id": DEPARTMENTS})
        for frame in (first, second):
            result.both(lambda g, frame=frame: g.add_nodes(frame, TYPE, "id", "name"))
        result.both(lambda g: g.add_nodes(departments, "Dept", "id"))
        result.both(lambda g: g.set_temporal(TYPE, "hired", "left", convention="half_open"))
        result.both(lambda g: g.add_relationships(works_in(first), "WORKS_IN", TYPE, "id", "Dept", "dept"))
        result.both(lambda g: g.add_relationships(works_in(second), "WORKS_IN", TYPE, "id", "Dept", "dept"))
        result.reopen()
    return result


def cycle(pair: Pair, first: int, tag: str) -> None:
    """Append, fail, SET, DELETE and save once; compare after every step."""
    base = total(pair.disk)
    sample = [0, 5, 1499, 1500, 2999, 3000, first, first + 7, first + 400, 999_999]

    # 1. Bulk load into the reopened type (the batch append path) plus edges
    # whose endpoints resolve through the id index.
    loaded = staff(first, 300)
    pair.both(lambda g: g.add_nodes(loaded, TYPE, "id", "name"))
    pair.both(lambda g: g.add_relationships(works_in(loaded), "WORKS_IN", TYPE, "id", "Dept", "dept"))
    compare(pair, f"{tag}: bulk append", sample)

    # 2. Cypher CREATE (the single-node append path).
    pair.both(lambda g: g.cypher(cypher_rows(first + 300, 40), timeout_ms=0))
    sample += [first + 300, first + 339]
    compare(pair, f"{tag}: CREATE", sample)

    # 3. A CREATE that fails after 250 rows leaves nothing behind.
    for g in (pair.disk, pair.memory):
        with pytest.raises(Exception):
            g.cypher(failing_create(first + 400), timeout_ms=0)
    assert total(pair.disk) == base + 340
    compare(pair, f"{tag}: failed CREATE", sample)

    # 4. SET on rows of the file (base) and of the tail, on typed columns and a
    # new one; a timestamp SET on base rows overlays the mapped column.
    pair.both(
        lambda g: g.cypher(
            f"MATCH (e:{TYPE}) WHERE e.level = 3 AND e.id % 11 = 0 "
            "SET e.salary = 99000.25, e.dept = 'Ops', e.left = datetime('2030-05-05T05:05:05.000005')",
            timeout_ms=0,
        )
    )
    pair.both(
        lambda g: g.cypher(
            f"MATCH (e:{TYPE}) WHERE e.id >= {FIRST_ID + first} AND e.level = 4 "
            "SET e.level = 40, e.dept = 'Ops', e.badge = 7",
            timeout_ms=0,
        )
    )
    compare(pair, f"{tag}: SET", sample)

    # 5. A SET that fails after writing 250 rows of both parts is undone.
    for g in (pair.disk, pair.memory):
        with pytest.raises(Exception):
            g.cypher(
                f"UNWIND range(0, 299) AS i MATCH (e:{TYPE} {{id: {FIRST_ID} + i + {first - 150}}}) "
                f"SET e.level = 77, e.salary = CASE WHEN i = 250 THEN {BOOM} ELSE 2.5 END",
                timeout_ms=0,
            )
    compare(pair, f"{tag}: failed SET", sample)

    # 6. DELETE rows of the file and of the tail.
    doomed = [5 + first // 1000, 1502 + first // 1000, first + 7, first + 300]
    for i in doomed:
        pair.both(lambda g, i=i: g.cypher(f"MATCH (e:{TYPE} {{id: {FIRST_ID + i}}}) DETACH DELETE e", timeout_ms=0))
    assert total(pair.disk) == base + 340 - len(doomed)
    compare(pair, f"{tag}: DELETE", sample)

    # 7. Save, reopen, and the answers are the same from the published file.
    pair.reopen()
    compare(pair, f"{tag}: after save + reopen", sample)


def test_two_cycles_of_append_fail_set_delete_save_answer_as_memory_does(pair):
    cycle(pair, 3000, "cycle 1")
    cycle(pair, 4000, "cycle 2")
    # Non-vacuity: the probes found rows in every part.
    assert total(pair.disk) > 3600
    assert rows(pair.disk, PROBES["float_filter"])[0]["c"] > 0
    assert len(rows(pair.disk, PROBES["edges"])) == len(DEPARTMENTS)
    assert fluent(pair.disk)["ids"], "some employees were moved to Ops"


def test_a_failed_first_append_leaves_the_reopened_graph_as_it_was(pair):
    """The rollback of the very statement that starts the tail drops it again."""
    before = {name: rows(pair.disk, query, n="Employee 77") for name, query in PROBES.items()}
    with pytest.raises(Exception):
        pair.disk.cypher(failing_create(9000), timeout_ms=0)
    after = {name: rows(pair.disk, query, n="Employee 77") for name, query in PROBES.items()}
    assert before == after
    pair.reopen()
    assert {name: rows(pair.disk, query, n="Employee 77") for name, query in PROBES.items()} == before


def _workspace_bytes(path: str) -> int:
    """Bytes the writer's private workspace holds: where an append used to copy
    the whole type before it could add a row."""
    total = 0
    for entry in Path(path).glob(".working-*"):
        total += sum(f.stat().st_size for f in entry.rglob("*") if f.is_file())
    return total


def test_appends_to_a_reopened_type_do_not_grow_with_the_type(tmp_path):
    """A 1 k append costs the new rows: no copy of the type's columns is made in
    the writer's workspace and the heap holds the appended rows only."""
    path = str(tmp_path / "staff")
    with warnings.catch_warnings():
        warnings.simplefilter("ignore")
        graph = kglite.KnowledgeGraph(storage="disk", path=path)
        graph.add_nodes(staff(0, 60_000), TYPE, "id", "name")
        graph.save(path)
        del graph
        graph = kglite.load(path)
        graph.add_nodes(staff(60_000, 1_000), TYPE, "id", "name")
        info = graph.graph_info()
    assert rows(graph, f"MATCH (e:{TYPE}) RETURN count(e) AS c") == [{"c": 61_000}]
    # The type's columns are ~9 MB for 60 k rows (what the old append copied
    # into the workspace); the 1 k appended rows are ~150 kB.
    assert _workspace_bytes(path) < 300_000, _workspace_bytes(path)
    assert info["columnar_heap_bytes"] < 250_000, info["columnar_heap_bytes"]


@pytest.mark.parametrize("appended", [0, 50])
def test_a_property_index_built_after_a_set_covers_the_rows_the_set_did_not_touch(tmp_path, appended):
    """A ``SET`` on a reopened type adds an overlay column; an index built on that
    property afterwards must still see the rows stored in the file, and the ones
    appended after the reopen."""
    path = str(tmp_path / "staff")
    with warnings.catch_warnings():
        warnings.simplefilter("ignore")
        graph = kglite.KnowledgeGraph(storage="disk", path=path)
        graph.add_nodes(staff(0, 400), TYPE, "id", "name")
        graph.save(path)
        del graph
        graph = kglite.load(path)
        graph.cypher(f"MATCH (e:{TYPE} {{id: {FIRST_ID + 3}}}) SET e.dept = 'Ops'", timeout_ms=0)
        if appended:
            graph.add_nodes(staff(400, appended), TYPE, "id", "name")
        graph.create_index(TYPE, "dept")
    by_dept = {
        d: rows(graph, f"MATCH (e:{TYPE}) WHERE e.dept = $d RETURN count(e) AS c", d=d)[0]["c"]
        for d in [*DEPARTMENTS, "Ops"]
    }
    expected = staff(0, 400 + appended)["dept"].value_counts().to_dict()
    expected["Ops"] = 1
    expected[DEPARTMENTS[3 % 5]] -= 1  # employee 3 moved to Ops
    assert by_dept == {d: expected.get(d, 0) for d in by_dept}


@pytest.mark.parametrize(
    ("set_value", "appended"),
    [("5", ["x", "y"]), ("'x'", [5, 6])],
    ids=["integer_set_string_append", "string_set_integer_append"],
)
def test_a_property_the_file_lacks_survives_a_save_when_set_and_append_type_it_differently(
    tmp_path, set_value, appended
):
    """A ``SET`` adds a property to a base row, then appended rows carry the same
    property as another kind: the file column holds one kind, so the save has to
    keep both parts' cells rather than write the tail's as nulls."""
    path = str(tmp_path / "staff")
    with warnings.catch_warnings():
        warnings.simplefilter("ignore")
        graph = kglite.KnowledgeGraph(storage="disk", path=path)
        graph.add_nodes(staff(0, 40), TYPE, "id", "name")
        graph.save(path)
        del graph
        graph = kglite.load(path)
        graph.cypher(f"MATCH (e:{TYPE} {{id: {FIRST_ID}}}) SET e.badge = {set_value}", timeout_ms=0)
        tail = pd.DataFrame({"id": [FIRST_ID + 100, FIRST_ID + 101], "name": ["N1", "N2"], "badge": appended})
        graph.add_nodes(tail, TYPE, "id", "name")
        query = f"MATCH (e:{TYPE}) WHERE e.badge IS NOT NULL RETURN e.id - {FIRST_ID} AS i, e.badge AS badge ORDER BY i"
        expected = rows(graph, query)
        assert len(expected) == 3, expected  # premise: the SET row and both appended rows hold a value
        graph.save(path)
        del graph
        reloaded = kglite.load(path)
    assert rows(reloaded, query) == expected
