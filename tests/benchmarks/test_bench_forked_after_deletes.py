"""Writes and scans under a copy-on-write fork of a graph that has deleted nodes.

Issue #195 cost classes, measured on a 1M-node graph with 2,000 scattered
deletes so the base carries free-list slots:

* **Write transaction after deletes.** Every ``begin()``/``commit()`` forks the
  published graph. A fork that could not share a base with holes deep-copied
  it instead: 16.7–17.2 ms per transaction against 0.85 ms without holes
  (2026-10-05, release, mean of 30).
* **Scan under a held overlay after deletes.** Overlay nodes that took reused
  slots made every base-node read pay a scattered-key hash miss: an unlabeled
  scan went 20.8 → 27.0 ms and a labeled scan 9.1 → 14.4 ms while the overlay
  was live, until a bitmap in front of the overlay map answered those reads.

Read each ``holes`` cell against its ``no_holes`` or ``unforked`` control from
the same run. Judge the transaction cells by **mean**: every round forks, so
the cost is per event, not a tail. Judge the scans by **median**: on a loaded
machine these 1M-row scans are heavy-tailed.
"""

from __future__ import annotations

import gc
import itertools
import random

import pytest

from kglite import KnowledgeGraph

NODES = 1_000_000
DELETES = 2_000
OVERLAY_CREATES = 100

SCANS = {
    "unlabeled": "MATCH (n) WHERE n.uid < 0 RETURN count(n) AS c",
    "labeled": "MATCH (n:Item) WHERE n.uid < 0 RETURN count(n) AS c",
}


def _graph(holes: bool) -> KnowledgeGraph:
    graph = KnowledgeGraph()
    graph.cypher(f"UNWIND range(0, {NODES - 1}) AS i CREATE (:Item {{uid: i}})")
    if holes:
        doomed = random.Random(7).sample(range(NODES), DELETES)
        graph.cypher("UNWIND $ids AS i MATCH (n:Item {uid: i}) DELETE n", params={"ids": doomed})
    return graph


_NEXT_UID = itertools.count(10_000_000)


@pytest.mark.benchmark
@pytest.mark.parametrize("shape", ["holes", "no_holes"])
def test_bench_write_tx_after_deletes(benchmark, shape):
    """One ``begin()`` / ``CREATE`` / ``commit()`` per round; each one forks."""
    graph = _graph(shape == "holes")

    def write():
        tx = graph.begin()
        tx.cypher("CREATE (:Item {uid: $u})", params={"u": next(_NEXT_UID)})
        tx.commit()

    benchmark.pedantic(write, rounds=30, iterations=1, warmup_rounds=3)
    benchmark.extra_info["statistic"] = "mean"
    assert graph.cypher("MATCH (n:Item) RETURN count(n) AS c").to_list()[0]["c"] > NODES - DELETES


@pytest.fixture(scope="module")
def scan_graphs():
    """Three graphs: forked over holes, forked without holes, and unforked.

    The forked two keep their ``freeze()`` alive for the module, so the overlay
    holding the creates is never folded back while the scans run.
    """
    graphs = {}
    for name, holes, held in [
        ("holes", True, True),
        ("no_holes", False, True),
        ("unforked", True, False),
    ]:
        graph = _graph(holes)
        view = graph.freeze() if held else None
        graph.cypher(f"UNWIND range(1, {OVERLAY_CREATES}) AS i CREATE (:Item {{uid: 40000000 + i}})")
        graphs[name] = (graph, view)
    yield graphs
    graphs.clear()
    gc.collect()


@pytest.mark.benchmark
@pytest.mark.parametrize("query", sorted(SCANS))
@pytest.mark.parametrize("shape", ["holes", "no_holes", "unforked"])
def test_bench_scan_under_held_overlay_after_deletes(benchmark, scan_graphs, shape, query):
    """A full scan while a reader holds the base and the writer's overlay is live."""
    import kglite

    graph, _view = scan_graphs[shape]
    assert kglite._backend_is_forked(graph) is (shape != "unforked"), (
        f"{shape}: the cell measures the wrong representation"
    )
    benchmark.pedantic(lambda: graph.cypher(SCANS[query]).to_list(), rounds=20, iterations=1, warmup_rounds=2)
    benchmark.extra_info["statistic"] = "median"
