"""Write-path ratio guards for the Linux perf gate.

The core harness runs under the published 0.13.2 wheel, which predates the
write-path wins below, so a win-guard there would be vacuous (0.13.2 is slow
by construction). Each cell here instead asserts a **ratio against a control
measured in the same process**: the wins are O(1) per write and the
regressions they replace are O(graph), so the ratios sit orders of magnitude
under the failure they catch and runner speed cancels. CI runs this file on the
release-built candidate wheel as a blocking step of the perf job. Locally,
release build only::

    uv run --no-sync maturin develop --release
    pytest tests/benchmarks/test_bench_write_path_guards.py -m benchmark -v -s

What each guard catches:

* **Relationship CREATE in a transaction** -- a ``MATCH ... CREATE
  (a)-[:E]->(b)`` commit may cost at most ``MAX_REL_VS_NODE_WRITE`` times the
  same transaction shape writing a node property. Adjacency edits live in the
  fork overlay (e387f649); a deep copy of the edge store per commit costs
  6 ms against 0.013 ms at 100k nodes / 300k edges.
* **First relationship CREATE under a held view** -- the first write after
  ``freeze()`` forks the base. It may cost at most ``MAX_HELD_VS_UNHELD``
  times the unheld write and times a node-property write. A fork that copies the graph costs 3.3 ms against
  0.019 ms. A once-per-event cost, so the statistic is the mean over fresh
  views (Performance protocol item 4a).
* **Lazy property NDV** -- a Session relationship write whose MATCH plans an
  equality estimate may cost at most ``MAX_NDV_REL_VS_NODE_WRITE`` times a
  node write. With NDV tagged to the graph version every commit forced an
  O(type) rescan: 2.1 ms against 0.006 ms at 100k nodes (e7723266).

* **Mapped-load write curve** -- a graph above the topology floors, loaded
  ``mapped``, is measured fresh, after ``MAPPED_WRITES`` unsaved writes and
  after a ``save()``: a one-node transaction commit, a relationship CREATE and
  a one-hop read. The overlay over the file's topology is a shared paged
  container, so each cell may cost at most ``MAX_MAPPED_AFTER_VS_FRESH`` times
  its fresh cost after the writes and ``MAX_MAPPED_SAVED_VS_FRESH`` after the
  save, and the commit at most ``MAX_MAPPED_VS_MEMORY`` times the same commit
  on a ``memory`` load of the file. A fork that copies the base costs 33 us
  fresh and 7,029 us after 1M writes; a save that does not serve the graph
  from the file again leaves the overlay's costs in place.

Statistic: ``median`` for the steady-state cells (the heavy tail is the
occasional collection, not the work), mean for the first-write cell. Control
values are printed with every verdict.
"""

from __future__ import annotations

import json
import statistics
import time

import numpy as np
import pandas as pd
import pytest

import kglite

pytestmark = [pytest.mark.benchmark]

NODES = 100_000
#: A relationship CREATE commit may cost this multiple of a node-property SET
#: commit of the same MATCH shape. Measured 0.5-0.7x on macOS; the deep-copy
#: path it replaced is hundreds of times the control.
MAX_REL_VS_NODE_WRITE = 6.0
#: The first relationship write after ``freeze()`` may cost this multiple of
#: the unheld write. Measured 0.9x (0.029 ms against 0.032 ms); a graph copy is >100x.
MAX_HELD_VS_UNHELD = 10.0
#: Session relationship write (indexed MATCH x2 + CREATE) vs a node CREATE, at
#: 100k nodes. Measured 2.6-2.7x; the version-tagged NDV rescan is ~1000x.
MAX_NDV_REL_VS_NODE_WRITE = 20.0


#: A mapped commit, relationship CREATE or read after ``MAPPED_WRITES`` writes
#: may cost this multiple of its fresh cost. Measured 1.7-1.8x at 300k writes
#: (8 us fresh, 14 us after; release, macOS); the overlay-slot copy fixed with the
#: cell read 17.5x, and the base copy before the paged overlay grew 213x.
MAX_MAPPED_AFTER_VS_FRESH = 3.0
#: After a save the graph is served from the file again with an empty overlay.
MAX_MAPPED_SAVED_VS_FRESH = 2.0
#: A mapped commit may cost this multiple of the same commit on a memory load.
#: Measured 1.2x fresh.
MAX_MAPPED_VS_MEMORY = 4.0
MAPPED_NODES = 70_000
MAPPED_EDGES = 270_000
MAPPED_WRITES = 300_000


def _report(cell: str, **values) -> None:
    print(
        f"write-guard {cell}: " + json.dumps({k: round(v, 5) if isinstance(v, float) else v for k, v in values.items()})
    )


def _ms(fn, rounds: int) -> list[float]:
    for j in range(20):
        fn(-1 - j)
    out = []
    for j in range(rounds):
        started = time.perf_counter()
        fn(j)
        out.append((time.perf_counter() - started) * 1e3)
    return out


@pytest.fixture(scope="module")
def edge_graph() -> kglite.KnowledgeGraph:
    graph = kglite.KnowledgeGraph()
    graph.add_nodes(pd.DataFrame({"uid": range(NODES), "name": [f"n{i}" for i in range(NODES)]}), "Item", "uid", "name")
    rng = np.random.default_rng(7)
    graph.add_connections(
        pd.DataFrame({"s": rng.integers(0, NODES, 3 * NODES), "t": rng.integers(0, NODES, 3 * NODES)}),
        "LINKS",
        "Item",
        "s",
        "Item",
        "t",
        distinct=True,
    )
    return graph


def _tx_node_write(graph, j: int) -> None:
    tx = graph.begin()
    tx.cypher("MATCH (a:Item {uid: $a}) SET a.touch = $j", params={"a": j % NODES, "j": j})
    tx.commit()


def _tx_rel_write(graph, j: int) -> None:
    tx = graph.begin()
    tx.cypher(
        "MATCH (a:Item {uid: $a}), (b:Item {uid: $b}) CREATE (a)-[:E {w: $j}]->(b)",
        params={"a": j % NODES, "b": (j * 7 + 1) % NODES, "j": j},
    )
    tx.commit()


def test_relationship_create_in_transaction_costs_a_node_write(edge_graph):
    control = statistics.median(_ms(lambda j: _tx_node_write(edge_graph, j), 300))
    rel = statistics.median(_ms(lambda j: _tx_rel_write(edge_graph, j), 300))
    ratio = rel / control
    _report("rel-in-tx", rel_ms=rel, node_write_ms=control, ratio=ratio)
    assert ratio <= MAX_REL_VS_NODE_WRITE, (
        f"relationship CREATE commit {rel:.4f} ms is {ratio:.1f}x a node-property commit "
        f"({control:.4f} ms); limit {MAX_REL_VS_NODE_WRITE}x (adjacency copied per commit?)"
    )


def test_first_relationship_create_under_a_held_view_does_not_copy_the_graph(edge_graph):
    def first_write_mean(write, held: bool, rounds: int = 40) -> float:
        samples = []
        for j in range(rounds + 5):
            view = edge_graph.freeze() if held else None
            started = time.perf_counter()
            write(edge_graph, 500_000 + j)
            elapsed = (time.perf_counter() - started) * 1e3
            del view
            if j >= 5:
                samples.append(elapsed)
        return statistics.mean(samples)

    node_control = first_write_mean(_tx_node_write, held=False)
    unheld = first_write_mean(_tx_rel_write, held=False)
    held = first_write_mean(_tx_rel_write, held=True)
    _report(
        "rel-held-first-write",
        held_ms=held,
        unheld_ms=unheld,
        node_write_ms=node_control,
        vs_unheld=held / unheld,
        vs_node_write=held / node_control,
    )
    # Two controls: the unheld write catches a held-only copy; the node write
    # catches a copy that the unheld transaction pays as well.
    assert held <= MAX_HELD_VS_UNHELD * unheld, (
        f"first relationship write under a held view {held:.4f} ms is {held / unheld:.1f}x the unheld write "
        f"({unheld:.4f} ms); limit {MAX_HELD_VS_UNHELD}x (fork deep-copies the graph?)"
    )
    assert held <= MAX_HELD_VS_UNHELD * node_control, (
        f"first relationship write under a held view {held:.4f} ms is {held / node_control:.1f}x a node-property "
        f"write ({node_control:.4f} ms); limit {MAX_HELD_VS_UNHELD}x (fork deep-copies the graph?)"
    )


def test_session_relationship_write_does_not_rescan_property_ndv():
    graph = kglite.KnowledgeGraph()
    session = graph.session()
    rows = [{"i": i, "g": i % 50, "k": i % 7} for i in range(NODES)]
    session.execute("UNWIND $rows AS r CREATE (:R {i: r.i, g: r.g, k: r.k})", params={"rows": rows})
    session.execute("CREATE INDEX FOR (n:R) ON (n.i)")

    def rel(j: int) -> None:
        a = j % (NODES - 1) if j >= 0 else NODES - 2 + j
        session.execute(
            "MATCH (a:R {i: $a}), (b:R {i: $b}) CREATE (a)-[:E {w: $j}]->(b)", params={"a": a, "b": a + 1, "j": j}
        )

    def node(j: int) -> None:
        session.execute("CREATE (:Q {x: $j})", params={"j": j})

    control = statistics.median(_ms(node, 300))
    rel_ms = statistics.median(_ms(rel, 300))
    ratio = rel_ms / control
    _report("ndv-rel-write", rel_ms=rel_ms, node_write_ms=control, ratio=ratio)
    assert ratio <= MAX_NDV_REL_VS_NODE_WRITE, (
        f"Session relationship write {rel_ms:.4f} ms is {ratio:.1f}x a node CREATE ({control:.4f} ms); "
        f"limit {MAX_NDV_REL_VS_NODE_WRITE}x (property NDV rescanned per commit?)"
    )


def _mapped_cells(graph, n: int, offset: int) -> dict[str, float]:
    """Median ms of a one-node commit, a relationship CREATE and a one-hop read."""
    ids = [(offset + 7919 * j) % n for j in range(1, 301)]

    def commit(j: int) -> None:
        tx = graph.begin()
        tx.cypher("CREATE (:Doc {id: $i, n: 1})", params={"i": 10_000_000 + offset * 10_000 + j})
        tx.commit()

    def edge(j: int) -> None:
        graph.cypher(
            "MATCH (a:Doc {id: $a}), (b:Doc {id: $b}) CREATE (a)-[:LINKS]->(b)",
            params={"a": ids[j % 300], "b": ids[(j * 3 + 1) % 300]},
        )

    def read(j: int) -> None:
        graph.cypher("MATCH (a:Doc {id: $a})-[:LINKS]->(b) RETURN count(b) AS c", params={"a": ids[j % 300]}).to_list()

    return {
        "commit_ms": statistics.median(_ms(commit, 300)),
        "edge_ms": statistics.median(_ms(edge, 300)),
        "read_ms": statistics.median(_ms(read, 300)),
    }


def test_mapped_write_curve_stays_flat_across_writes_and_a_save(tmp_path):
    rng = np.random.default_rng(5)
    base = kglite.KnowledgeGraph()
    base.add_nodes(
        pd.DataFrame(
            {
                "id": np.arange(MAPPED_NODES),
                "title": [f"d{i}" for i in range(MAPPED_NODES)],
                "n": rng.integers(0, 100, MAPPED_NODES),
            }
        ),
        "Doc",
        "id",
        "title",
    )
    base.add_relationships(
        pd.DataFrame(
            {"s": rng.integers(0, MAPPED_NODES, MAPPED_EDGES), "t": rng.integers(0, MAPPED_NODES, MAPPED_EDGES)}
        ),
        "LINKS",
        "Doc",
        "s",
        "Doc",
        "t",
        distinct=True,
    )
    path = str(tmp_path / "mapped.kgl")
    base.save(path)
    del base

    memory = kglite.load(path, storage="memory")
    memory_cells = _mapped_cells(memory, MAPPED_NODES, 0)
    del memory

    graph = kglite.load(path, storage="mapped")
    assert graph.graph_info()["topology_status"] == "mapped", graph.graph_info()["topology_heap_reason"]
    fresh = _mapped_cells(graph, MAPPED_NODES, 1)
    for j in range(MAPPED_WRITES):
        if j % 16 == 3 and j >= 403:
            # Removing a node the overlay created long ago: from the middle of its run.
            graph.cypher("MATCH (d:Doc {id: $i}) DETACH DELETE d", params={"i": 20_000_000 + j - 403})
        elif j % 4 == 0:
            graph.cypher("CREATE (:Doc {id: $i, n: 2})", params={"i": 20_000_000 + j})
        elif j % 4 == 1:
            graph.cypher(
                "MATCH (a:Doc {id: $a}), (b:Doc {id: $b}) CREATE (a)-[:LINKS]->(b)",
                params={"a": (j * 31) % MAPPED_NODES, "b": (j * 17 + 5) % MAPPED_NODES},
            )
        else:
            graph.cypher("MATCH (d:Doc {id: $i}) SET d.n = $j", params={"i": (j * 13) % MAPPED_NODES, "j": j})
    assert graph.graph_info()["topology_status"] == "overlay"
    after = _mapped_cells(graph, MAPPED_NODES, 2)
    graph.save(str(tmp_path / "saved.kgl"))
    assert graph.graph_info()["topology_status"] == "mapped"
    saved = _mapped_cells(graph, MAPPED_NODES, 3)

    _report(
        "mapped-curve",
        **{f"fresh_{k}": v for k, v in fresh.items()},
        **{f"after_{k}": v for k, v in after.items()},
        **{f"saved_{k}": v for k, v in saved.items()},
        memory_commit_ms=memory_cells["commit_ms"],
        commit_after_vs_fresh=after["commit_ms"] / fresh["commit_ms"],
    )
    for cell in fresh:
        assert after[cell] <= MAX_MAPPED_AFTER_VS_FRESH * fresh[cell], (
            f"mapped {cell} after {MAPPED_WRITES} writes {after[cell]:.4f} ms is {after[cell] / fresh[cell]:.1f}x "
            f"its fresh cost ({fresh[cell]:.4f} ms); limit {MAX_MAPPED_AFTER_VS_FRESH}x (overlay copied per commit?)"
        )
        assert saved[cell] <= MAX_MAPPED_SAVED_VS_FRESH * fresh[cell], (
            f"mapped {cell} after a save {saved[cell]:.4f} ms is {saved[cell] / fresh[cell]:.1f}x its fresh cost "
            f"({fresh[cell]:.4f} ms); limit {MAX_MAPPED_SAVED_VS_FRESH}x (graph not served from the file again?)"
        )
    for label, cells in (("fresh", fresh), ("after", after)):
        assert cells["commit_ms"] <= MAX_MAPPED_VS_MEMORY * memory_cells["commit_ms"], (
            f"mapped commit ({label}) {cells['commit_ms']:.4f} ms is "
            f"{cells['commit_ms'] / memory_cells['commit_ms']:.1f}x the memory commit "
            f"({memory_cells['commit_ms']:.4f} ms); limit {MAX_MAPPED_VS_MEMORY}x"
        )
