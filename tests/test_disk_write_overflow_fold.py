"""A disk graph built in-process folds its edge overflow on the first write.

An unsaved in-process-built disk graph holds every edge as overflow, and each
write copies the overflow bag, so writes cost O(edges) until `save()` or
`compact()`. The first Cypher write folds a large bag; `compact()` returns the
number of overflow edges it merged, so a second call returning 0 is the
observable that the bag is gone.
"""

import numpy as np
import pandas as pd
import pytest

import kglite

pytestmark = pytest.mark.filterwarnings("ignore::UserWarning")


def _built_disk_graph(tmp_path, nodes: int, edges: int):
    rng = np.random.default_rng(7)
    g = kglite.KnowledgeGraph(storage="disk", path=str(tmp_path / "g"))
    g.add_nodes(
        pd.DataFrame({"id": np.arange(nodes), "name": [f"n{i}" for i in range(nodes)], "hits": 0}),
        "Item",
        "id",
        "name",
    )
    g.add_relationships(
        pd.DataFrame(
            {
                "a": rng.integers(0, nodes, edges),
                "b": rng.integers(0, nodes, edges),
            }
        ),
        "LINKS",
        "Item",
        "a",
        "Item",
        "b",
    )
    return g


def test_first_write_folds_a_large_overflow_bag(tmp_path):
    g = _built_disk_graph(tmp_path, nodes=5_000, edges=150_000)
    g.cypher("MATCH (n:Item {id: 3}) SET n.hits = 1")
    assert g.compact() == 0, "the first write left the build overflow unfolded"
    assert g.cypher("MATCH ()-[r:LINKS]->() RETURN count(r) AS c")[0]["c"] == 150_000
    assert g.cypher("MATCH (n:Item {id: 3}) RETURN n.hits AS h")[0]["h"] == 1


def test_small_overflow_is_left_for_compact(tmp_path):
    g = _built_disk_graph(tmp_path, nodes=500, edges=2_000)
    g.cypher("MATCH (n:Item {id: 3}) SET n.hits = 1")
    assert g.compact() == 2_000, "a small overflow bag must not be folded by a write"
