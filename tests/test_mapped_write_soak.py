"""Long-run growth of a mapped-loaded graph under mixed writes and saves.

A graph above the topology floors that is loaded `mapped` and written to for a
long time must stay what a fresh load is: heap proportional to the data not yet
saved, back to nothing after a `save()`, topology served from the file again
(nothing dropped to the heap), commit and read costs that do not climb with the
number of writes, and answers equal to a `memory` graph that took the same
writes. The fast test runs tens of thousands of writes in the default suite;
the soak (`pytest -m stress tests/test_mapped_write_soak.py`) runs millions.
"""

from __future__ import annotations

import sys

import pytest

import kglite
from tests.mapped_soak import Workload, build_base, replay_in_memory, run_mapped_rounds

pytestmark = pytest.mark.skipif(sys.platform == "win32", reason="Auto never maps on Windows")

NODES = 70_000
EDGES = 270_000
#: Heap the overlay may hold per write between saves. Measured 0.3-0.45 KB; a leak
#: that keeps entries across rollbacks or saves is orders above it.
BUDGET_BYTES_PER_WRITE = 2048


def _assert_round(record, fresh, *, cost_ratio, writes):
    n = record["round"]
    dirty, saved = record["dirty"], record["saved"]
    assert dirty["status"] == "overlay", dirty
    assert 0 < dirty["budget_used"] <= BUDGET_BYTES_PER_WRITE * record["writes_in_round"], dirty
    assert dirty["entries"] > 0
    assert record["rollback_entries_delta"] == 0, f"round {n}: rolled-back transactions kept overlay entries"
    # Eligibility survives the whole history: after every save the topology is
    # served from the file and no part was written without a mapped form.
    assert saved["status"] == "mapped", saved
    assert saved["fallbacks"] == [], saved
    assert saved["budget_used"] == 0, saved
    assert saved["entries"] == 0, saved
    for cell in ("commit_ms", "edge_ms", "read_ms"):
        floor = max(fresh[cell], 0.15)
        assert record[cell] <= cost_ratio * floor, (
            f"round {n} {cell}: {record[cell]:.3f} ms vs fresh {fresh[cell]:.3f} ms"
        )


def _check(tmp_path, rounds, ops, *, cost_ratio, footprint=False):
    records, fresh = run_mapped_rounds(tmp_path, NODES, EDGES, rounds, ops, measure_footprint=footprint)
    assert fresh["status"] == "mapped" and fresh["fallbacks"] == []
    for record in records[1:]:
        _assert_round(record, fresh, cost_ratio=cost_ratio, writes=ops)
    memory = replay_in_memory(tmp_path, NODES, EDGES, rounds, ops)
    for record, expected in zip(records[1:], memory):
        assert record["answers"] == expected, f"round {record['round']} diverged from the memory replay"
    last = records[-1]
    assert last["reloaded_status"] == "mapped"
    assert last["reloaded_answers"] == last["answers"], "the saved file reloads to the same answers"
    return records


def test_a_mapped_graph_stays_eligible_flat_and_equal_through_writes_and_saves(tmp_path):
    _check(tmp_path, rounds=3, ops=6_000, cost_ratio=8.0)


@pytest.mark.stress
def test_soak_millions_of_mixed_writes_keep_memory_and_cost_flat(tmp_path):
    rounds, ops = 6, 250_000
    records = _check(tmp_path, rounds, ops, cost_ratio=5.0, footprint=True)
    for r in records:
        dirty, saved = r.get("dirty", {}), r.get("saved", {})
        print(
            f"soak round {r['round']}: commit {r['commit_ms']:.3f} edge {r['edge_ms']:.3f} "
            f"read {r['read_ms']:.3f} ms, overlay {dirty.get('budget_used', 0) >> 10} KB / "
            f"{dirty.get('entries', 0)} entries -> after save {saved.get('budget_used', 0)} / "
            f"{saved.get('entries', 0)}, footprint {(r['footprint'] or 0) >> 20} MB, "
            f"save {r.get('save_seconds', 0):.1f} s"
        )
    fresh = records[0]["footprint"]
    if fresh is None:
        return
    for record in records[1:]:
        growth = record["footprint"] - fresh
        # The graph itself grows by the nodes and relationships the rounds net;
        # anything beyond a few hundred bytes per surviving write is a leak.
        assert growth <= 200 << 20, f"round {record['round']}: footprint +{growth >> 20} MB after save"
    # Footprint after a save does not climb round over round.
    assert records[-1]["footprint"] - records[1]["footprint"] <= 100 << 20


def test_commits_stay_flat_after_nodes_are_created_and_deleted_between_them(tmp_path):
    """A node removed from the middle of the overlay's appended run once turned every later append
    into a scattered slot, and each commit then cloned all of them: 0.16 ms fresh, 0.68 ms after
    24,000 writes of this shape, growing without bound."""
    base_path = str(tmp_path / "base.kgl")
    build_base(NODES, EDGES).save(base_path)
    graph = kglite.load(base_path, storage="mapped")
    work = Workload(NODES)
    fresh = work.timed(graph, "tx_commit", 50)
    for i in range(30_000):
        work.op(graph, ("node", "delete", "tx_commit")[i % 3])
    after = work.timed(graph, "tx_commit", 50)
    assert graph.graph_info()["topology_status"] == "overlay"
    assert after <= 2.5 * max(fresh, 0.15), f"commit {after:.3f} ms after 30,000 writes vs {fresh:.3f} ms fresh"
