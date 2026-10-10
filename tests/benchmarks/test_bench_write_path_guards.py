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
