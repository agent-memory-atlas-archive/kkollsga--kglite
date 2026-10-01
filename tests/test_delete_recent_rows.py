"""Deleting rows a statement created moments ago removes exactly those rows.

A create reuses a slot a delete freed, so the new node lands at the end of its
type's index with a low node index, out of order. The delete of such rows used
to fall back to a pass over the whole type index; it now finds them among the
newest entries and edits the index in place. These goldens check the result,
not the speed (the bench cell ``delete_after_create`` has that): after each
round of delete-old / create / delete-new, every employee the model says is
present is present with its values, every deleted one is gone, and the same
holds after a failed delete statement is rolled back and after a save and
reload, in every storage mode.

Run: pytest tests/test_delete_recent_rows.py
"""

from __future__ import annotations

import warnings

import pandas as pd
import pytest

import kglite

TYPE = "Employee"
SIZE = 8_000  # above 32 x the batch, so the delete takes the in-place index edit
BATCH = 200
BOOM = "duration({months: 2147483648})"
BOOM_ERROR = "calendar months exceed"  # what BOOM raises: the failure this test means to provoke
MODES = ["memory", "mapped", "disk", "disk_reopened"]


def _frame(ids: range) -> pd.DataFrame:
    return pd.DataFrame(
        {"id": list(ids), "name": [f"Employee {i}" for i in ids], "level": [i % 7 for i in ids]},
    )


def _rows(graph, query, **params):
    return graph.cypher(query, params=params or None, timeout_ms=0).to_list()


@pytest.fixture(params=MODES)
def built(request, tmp_path):
    mode = request.param
    path = str(tmp_path / "staff")
    with warnings.catch_warnings():
        warnings.simplefilter("ignore")
        if mode == "memory":
            graph = kglite.KnowledgeGraph()
        elif mode == "mapped":
            graph = kglite.KnowledgeGraph(storage="mapped")
        else:
            graph = kglite.KnowledgeGraph(storage="disk", path=path)
        graph.add_nodes(_frame(range(1, SIZE + 1)), TYPE, "id", "name")
        if mode == "disk_reopened":
            graph.save(path)
            del graph
            graph = kglite.load(path)
    model = {i: (f"Employee {i}", i % 7) for i in range(1, SIZE + 1)}
    return graph, model, mode, path


def _check(graph, model, step):
    got = {
        r["id"]: (r["name"], r["level"])
        for r in _rows(graph, f"MATCH (e:{TYPE}) RETURN e.id AS id, e.name AS name, e.level AS level")
    }
    assert len(got) == len(model), f"{step}: {len(got)} employees, expected {len(model)}"
    assert got == model, f"{step}: stored employees differ from the model"
    assert _rows(graph, f"MATCH (e:{TYPE}) RETURN count(e) AS c") == [{"c": len(model)}]


def _create(graph, ids) -> None:
    graph.cypher(
        "UNWIND $rows AS c CREATE (:Employee {id: c.id, name: 'new ' + toString(c.id), level: 99})",
        params={"rows": [{"id": i} for i in ids]},
        timeout_ms=0,
    )


def _delete(ids):
    return "UNWIND $rows AS c MATCH (e:Employee {id: c.id}) DELETE e", {"rows": [{"id": i} for i in ids]}


def test_deleting_recently_created_rows_removes_exactly_those_rows(built):
    graph, model, mode, path = built
    next_id = SIZE + 10
    older = 1
    for cycle in range(3):
        # Free low slots, so the created rows reuse them and land out of order.
        gone = range(older, older + BATCH)
        older += BATCH
        query, params = _delete(gone)
        graph.cypher(query, params=params, timeout_ms=0)
        for i in gone:
            del model[i]
        fresh = range(next_id, next_id + BATCH)
        next_id += BATCH + 5
        _create(graph, fresh)
        for i in fresh:
            model[i] = (f"new {i}", 99)
        _check(graph, model, f"cycle {cycle}: created")

        # Delete the ones just created, keeping two of them.
        keep = {next_id - BATCH - 5, next_id - BATCH - 5 + 7}
        query, params = _delete([i for i in fresh if i not in keep])
        graph.cypher(query, params=params, timeout_ms=0)
        for i in fresh:
            if i not in keep:
                del model[i]
        _check(graph, model, f"cycle {cycle}: deleted the new rows")
        assert _rows(graph, f"MATCH (e:{TYPE} {{id: $i}}) RETURN count(e) AS c", i=min(keep) + 1) == [{"c": 0}]

    if mode != "memory":  # the .kgl route is the last test
        graph.save(path)
        _check(kglite.load(path), model, "after save + reload")


def test_a_failed_delete_of_recently_created_rows_restores_them(built):
    graph, model, _mode, _path = built
    query, params = _delete(range(1, 1 + BATCH))
    graph.cypher(query, params=params, timeout_ms=0)
    for i in range(1, 1 + BATCH):
        del model[i]
    fresh = range(SIZE + 10, SIZE + 10 + BATCH)
    _create(graph, fresh)
    for i in fresh:
        model[i] = (f"new {i}", 99)
    before_order = [r["id"] for r in _rows(graph, f"MATCH (e:{TYPE}) RETURN e.id AS id")]

    with pytest.raises(kglite.CypherExecutionError, match=BOOM_ERROR):
        graph.cypher(
            "UNWIND $rows AS c MATCH (e:Employee {id: c.id}) DELETE e WITH count(*) AS n "
            f"CREATE (:Probe {{v: {BOOM}}})",
            params={"rows": [{"id": i} for i in fresh]},
            timeout_ms=0,
        )
    _check(graph, model, "after the failed delete")
    assert [r["id"] for r in _rows(graph, f"MATCH (e:{TYPE}) RETURN e.id AS id")] == before_order
    assert _rows(graph, "MATCH (p:Probe) RETURN count(p) AS c") in ([], [{"c": 0}])

    # The same delete, without the failure, then does remove them.
    query, params = _delete(fresh)
    graph.cypher(query, params=params, timeout_ms=0)
    for i in fresh:
        del model[i]
    _check(graph, model, "after the delete that succeeds")


def test_deleted_recent_rows_stay_gone_across_a_kgl_save_and_load(tmp_path):
    graph = kglite.KnowledgeGraph()
    graph.add_nodes(_frame(range(1, SIZE + 1)), TYPE, "id", "name")
    model = {i: (f"Employee {i}", i % 7) for i in range(1, SIZE + 1)}
    query, params = _delete(range(1, 1 + BATCH))
    graph.cypher(query, params=params, timeout_ms=0)
    for i in range(1, 1 + BATCH):
        del model[i]
    fresh = range(SIZE + 10, SIZE + 10 + BATCH)
    _create(graph, fresh)
    query, params = _delete(fresh)
    graph.cypher(query, params=params, timeout_ms=0)
    target = str(tmp_path / "staff.kgl")
    graph.save(target)
    _check(kglite.load(target), model, "after a .kgl round trip")
