"""Load cells for a wide, typed-column graph: what the load path's per-cell work costs.

Every complete load checks for legacy endpoint references before publishing
the graph. For a graph whose properties all sit in typed columns that check
has nothing to read: a typed column cannot hold a reference. These cells
guard that it stays so, on the two load routes — a `.kgl` load, and a disk
directory reopen — with 200k nodes x 10 short-string properties, where a
per-cell pass is plainly visible (it roughly doubled the `.kgl` load).

Outside the frozen core harness: a new core cell needs a versioned capture on
both platforms before CI's exact-set gate can pass.
"""

import pandas as pd
import pytest

import kglite
from kglite import KnowledgeGraph

pytestmark = pytest.mark.benchmark

NODES = 200_000
PROPS = 10


def _wide_frame():
    frame = {"nid": range(NODES), "name": [f"N{i}" for i in range(NODES)]}
    for p in range(PROPS):
        frame[f"p{p}"] = [f"v{p}_{i % 997}" for i in range(NODES)]
    return pd.DataFrame(frame)


@pytest.fixture(scope="module")
def wide_kgl_path(tmp_path_factory):
    graph = KnowledgeGraph()
    graph.add_nodes(_wide_frame(), "Item", "nid", "name")
    path = str(tmp_path_factory.mktemp("wide_load") / "wide.kgl")
    graph.save(path)
    return path


@pytest.fixture(scope="module")
def wide_disk_dir(tmp_path_factory):
    root = str(tmp_path_factory.mktemp("wide_disk") / "graph")
    graph = KnowledgeGraph(storage="disk", path=root)
    graph.add_nodes(_wide_frame(), "Item", "nid", "name")
    graph.save(root)
    del graph
    return root


def test_bench_load_kgl_wide_typed(benchmark, wide_kgl_path):
    graph = benchmark(kglite.load, wide_kgl_path)
    assert graph.cypher("MATCH (n:Item) RETURN count(n) AS c").to_list() == [{"c": NODES}]


def test_bench_disk_reopen_wide_typed(benchmark, wide_disk_dir):
    """`pedantic` with a setup that drops the previous handle: `open()` takes
    the directory's writer lease, which the previous round's handle holds."""
    handle = {}

    def release_previous_handle():
        handle.pop("graph", None)

    def reopen():
        handle["graph"] = kglite.open(wide_disk_dir)

    benchmark.pedantic(reopen, setup=release_previous_handle, rounds=30, warmup_rounds=3, iterations=1)
    assert handle["graph"].cypher("MATCH (n:Item) RETURN count(n) AS c").to_list() == [{"c": NODES}]
