"""WAL recovery after a bulk-loaded id column widens to Int64.

``add_nodes`` stores ids that fit a ``u32`` as ``UniqueId``; the first Cypher
``CREATE`` writes an ``Int64`` id and widens the column. Frames logged after
that name the checkpoint's nodes by their ``Int64`` spelling, so recovery must
treat both spellings as one id — a ``SET`` must not resurrect a duplicate and a
``DETACH DELETE`` must not be lost.
"""

import gc
import os

import pandas as pd
import pytest

import kglite

STORAGE_MODES = ("memory", "mapped")
LEVELS = ("normal", "full")
WRITES = {
    "set": "MATCH (n:Item {id: 5}) SET n.v = 55",
    "detach_delete": "MATCH (n:Item {id: 2}) DETACH DELETE n",
}


def _open(path, storage, level):
    kwargs = {"durable": level}
    if storage != "memory" and not os.path.exists(path):
        kwargs["storage"] = storage
    return kglite.open(path, **kwargs)


def _state(g):
    rows = g.cypher("MATCH (n:Item) RETURN n.id AS id, n.v AS v ORDER BY id")
    edges = g.cypher("MATCH (a:Item)-[:NEXT]->(b:Item) RETURN a.id AS a, b.id AS b ORDER BY a")
    return [dict(r) for r in rows], [dict(r) for r in edges]


@pytest.mark.parametrize("storage", STORAGE_MODES)
@pytest.mark.parametrize("level", LEVELS)
@pytest.mark.parametrize("write", sorted(WRITES))
def test_a_write_after_the_widening_recovers_to_the_live_state(tmp_path, storage, level, write):
    path = str(tmp_path / "g.kgl")
    g = _open(path, storage, level)
    g.add_nodes(
        pd.DataFrame({"id": [1, 2, 3, 4, 5], "name": list("abcde"), "v": [1, 2, 3, 4, 5]}),
        "Item",
        "id",
        "name",
    )
    g.cypher("MATCH (a:Item), (b:Item) WHERE b.id = a.id + 1 CREATE (a)-[:NEXT]->(b)")
    g.save()
    g.cypher("CREATE (:Item {id: 6, name: 'f', v: 6})")
    g.cypher(WRITES[write])
    live = _state(g)
    del g
    gc.collect()

    reopened = kglite.open(path, durable=level)
    assert _state(reopened) == live
