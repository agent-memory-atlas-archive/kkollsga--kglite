"""A mapped load that serves the topology from the `.kgl` file's raw sections.

A save of a graph with 65,536 nodes or 262,144 relationships or more records
its node records and adjacency as raw sections; a load into `mapped` storage
reads them in place instead of decoding the compressed topology into the heap.
A write puts a copy-on-write overlay over that topology, so the heap grows with
the writes and not with the graph, however many there are; a save writes the
merged graph and serves the graph from that file again, with an empty overlay.
The Rust suite
covers the backend (every read against a heap load, slot identity, rollback
shapes, damaged sections, the overlay against a model); these tests pin what
the Python surface promises: answers do not change, a write works from the
first statement, and transactions, copies, snapshots, saves and the log behave.
"""

from __future__ import annotations

import os
import subprocess
import sys

import numpy as np
import pandas as pd
import pytest

import kglite

NODES = 70_000
EDGES = 270_000

pytestmark = pytest.mark.skipif(sys.platform == "win32", reason="Auto never maps on Windows")

PROBES = [
    "MATCH (d:Doc {id: 1234}) RETURN d.n",
    "MATCH (d:Doc) RETURN count(d), sum(d.n)",
    "MATCH (a:Doc)-[:LINKS]->(b:Doc) RETURN b.id AS i, count(a) AS c ORDER BY c DESC, i LIMIT 5",
    "MATCH (a:Doc)-[:LINKS]->(b:Doc) RETURN a.id AS i, count(b) AS c ORDER BY c DESC, i LIMIT 5",
    "MATCH (a:Doc {id: 77})-[:LINKS]->(b)-[:LINKS]->(c) RETURN count(c)",
    "MATCH (a:Doc {id: 77})<-[:LINKS]-(b) RETURN count(b), min(b.id)",
    "MATCH (a:Doc {id: 5})-[:LINKS*1..2]->(c) RETURN count(DISTINCT c)",
    "MATCH (a:Doc)-[r]->(b) WHERE a.id < 200 RETURN count(r)",
    "MATCH (t:Tag) RETURN t.id ORDER BY t.id LIMIT 3",
    "MATCH (a:Doc)-[:HAS]->(t:Tag) RETURN t.id AS i, count(a) AS c ORDER BY c DESC, i LIMIT 3",
]


def _build() -> kglite.KnowledgeGraph:
    rng = np.random.default_rng(5)
    graph = kglite.KnowledgeGraph()
    graph.add_nodes(
        pd.DataFrame(
            {
                "id": np.arange(NODES),
                "title": [f"d{i}" for i in range(NODES)],
                "n": rng.integers(0, 100, NODES),
            }
        ),
        "Doc",
        "id",
        "title",
    )
    graph.add_nodes(pd.DataFrame({"id": np.arange(40), "title": [f"t{i}" for i in range(40)]}), "Tag", "id", "title")
    edges = pd.DataFrame({"s": rng.integers(0, NODES, EDGES), "t": rng.integers(0, NODES, EDGES)})
    graph.add_relationships(edges, "LINKS", "Doc", "s", "Doc", "t", distinct=True)
    tags = pd.DataFrame({"s": np.arange(0, NODES, 7), "t": np.arange(0, NODES, 7) % 40})
    graph.add_relationships(tags, "HAS", "Doc", "s", "Tag", "t")
    graph.cypher("MATCH (d:Doc {id: 4242}) DETACH DELETE d")
    return graph


@pytest.fixture(scope="module")
def saved(tmp_path_factory):
    path = str(tmp_path_factory.mktemp("topo") / "g.kgl")
    _build().save(path)
    return path


def _answers(graph: kglite.KnowledgeGraph) -> list:
    return [graph.cypher(q).to_list() for q in PROBES]


def test_a_mapped_load_is_frozen_and_a_memory_load_is_not(saved):
    mapped = kglite.load(saved, storage="mapped")
    memory = kglite.load(saved, storage="memory")
    assert kglite._backend_is_frozen(mapped)
    assert not kglite._backend_is_frozen(memory)
    assert mapped.graph_info()["storage_mode"] == "mapped"


def test_a_frozen_graph_answers_like_a_memory_graph_and_stays_frozen(saved):
    mapped = kglite.load(saved, storage="mapped")
    memory = kglite.load(saved, storage="memory")
    assert _answers(mapped) == _answers(memory)
    assert kglite._backend_is_frozen(mapped), "reads never promote"
    assert mapped.cypher("MATCH (d:Doc {id: 4242}) RETURN d.n").to_list() == []


def test_a_write_overlays_the_file_topology_and_the_graph_keeps_answering(saved):
    mapped = kglite.load(saved, storage="mapped")
    memory = kglite.load(saved, storage="memory")
    writes = [
        "CREATE (:Doc {id: 900000, n: 1, title: 'new'})",
        "MATCH (a:Doc {id: 900000}), (b:Doc {id: 77}) CREATE (a)-[:LINKS]->(b)",
        "MATCH (d:Doc {id: 100}) DETACH DELETE d",
        "MATCH (d:Doc {id: 4243}) SET d.n = 777",
        "CREATE (:Doc {id: 4242, n: 5, title: 'reuse'})",
    ]
    for i, write in enumerate(writes):
        mapped.cypher(write)
        memory.cypher(write)
        if i == 0:
            assert not kglite._backend_is_frozen(mapped)
            assert mapped.graph_info()["storage_mode"] == "mapped"
        assert kglite._backend_is_frozen(mapped, include_overlay=True), "a write copies nothing"
        assert kglite._backend_is_forked(mapped)
        assert _answers(mapped) == _answers(memory), write


def test_a_failed_statement_leaves_a_frozen_graph_as_it_was(saved):
    mapped = kglite.load(saved, storage="mapped")
    before = _answers(mapped)
    with pytest.raises(Exception):
        mapped.cypher(
            "CREATE (:Doc {id: 800001, n: 1, title: 'a'}), "
            "(:Doc {id: 800002, title: 'b', bad: duration({months: 2147483648})})"
        )
    assert _answers(mapped) == before
    assert mapped.cypher("MATCH (d:Doc {id: 800001}) RETURN d.n").to_list() == []
    mapped.cypher("MATCH (d:Doc {id: 100}) DETACH DELETE d")
    assert mapped.cypher("MATCH (d:Doc {id: 100}) RETURN d.n").to_list() == []


def test_a_rolled_back_transaction_leaves_the_original_frozen(saved):
    mapped = kglite.load(saved, storage="mapped")
    before = _answers(mapped)
    tx = mapped.begin()
    tx.cypher("MATCH (d:Doc {id: 100}) DETACH DELETE d")
    tx.cypher("CREATE (:Doc {id: 910000, n: 1, title: 'tx'})")
    assert tx.cypher("MATCH (d:Doc {id: 910000}) RETURN d.n").to_list() == [{"d.n": 1}]
    tx.rollback()
    assert _answers(mapped) == before
    assert kglite._backend_is_frozen(mapped), "the transaction wrote to its own copy"


def test_a_committed_transaction_applies_to_the_graph(saved):
    mapped = kglite.load(saved, storage="mapped")
    tx = mapped.begin()
    tx.cypher("MATCH (d:Doc {id: 100}) DETACH DELETE d")
    tx.commit()
    assert mapped.cypher("MATCH (d:Doc {id: 100}) RETURN d.n").to_list() == []
    assert mapped.cypher("MATCH (d:Doc) RETURN count(d) AS c").to_list() == [{"c": NODES - 2}]
    assert kglite._backend_is_frozen(mapped, include_overlay=True), "a commit publishes the overlay, not a copy"


def test_transactions_over_the_overlay_match_a_memory_graph(saved):
    mapped = kglite.load(saved, storage="mapped")
    memory = kglite.load(saved, storage="memory")
    for graph in (mapped, memory):
        graph.cypher("MATCH (d:Doc {id: 100}) DETACH DELETE d")
        tx = graph.begin()
        tx.cypher("CREATE (:Doc {id: 940000, n: 4, title: 'rolled'})")
        tx.cypher("MATCH (d:Doc {id: 101}) DETACH DELETE d")
        tx.rollback()
        tx = graph.begin()
        tx.cypher("CREATE (:Doc {id: 940001, n: 5, title: 'kept'})")
        tx.cypher("MATCH (a:Doc {id: 940001}), (b:Doc {id: 77}) CREATE (a)-[:LINKS]->(b)")
        tx.commit()
    assert kglite._backend_is_frozen(mapped, include_overlay=True)
    assert _answers(mapped) == _answers(memory)
    assert mapped.cypher("MATCH (d:Doc {id: 940000}) RETURN d.n").to_list() == []
    assert mapped.cypher("MATCH (d:Doc {id: 101}) RETURN d.n").to_list() != []


def test_an_overlay_past_a_quarter_of_the_graph_stays_an_overlay(saved):
    mapped = kglite.load(saved, storage="mapped")
    memory = kglite.load(saved, storage="memory")
    bulk = "UNWIND range(1, 100000) AS i CREATE (:Doc {id: 2000000 + i, n: i % 100, title: 'bulk'})"
    for graph in (mapped, memory):
        graph.cypher("MATCH (d:Doc {id: 100}) DETACH DELETE d")
        graph.cypher(bulk)
    assert kglite._backend_is_frozen(mapped, include_overlay=True), "size alone never collapses the overlay"
    assert kglite._backend_is_forked(mapped)
    assert mapped.graph_info()["storage_mode"] == "mapped"
    assert _answers(mapped) == _answers(memory)
    mapped.cypher("MATCH (d:Doc {id: 2000001}) DETACH DELETE d")
    memory.cypher("MATCH (d:Doc {id: 2000001}) DETACH DELETE d")
    assert _answers(mapped) == _answers(memory)


def test_transactions_after_many_writes_match_a_memory_graph(saved):
    mapped = kglite.load(saved, storage="mapped")
    memory = kglite.load(saved, storage="memory")
    bulk = "UNWIND range(1, 20000) AS i CREATE (:Doc {id: 3000000 + i, n: i % 100, title: 'bulk'})"
    links = "UNWIND range(1, 5000) AS i MATCH (a:Doc {id: 3000000 + i}), (b:Doc {id: i}) CREATE (a)-[:LINKS]->(b)"
    for graph in (mapped, memory):
        graph.cypher(bulk)
        graph.cypher(links)
        tx = graph.begin()
        tx.cypher("CREATE (:Doc {id: 960000, n: 1, title: 'rolled'})")
        tx.cypher("MATCH (d:Doc {id: 3000007}) DETACH DELETE d")
        tx.rollback()
        tx = graph.begin()
        tx.cypher("CREATE (:Doc {id: 960001, n: 2, title: 'kept'})")
        tx.cypher("MATCH (a:Doc {id: 960001}), (b:Doc {id: 3000008}) CREATE (a)-[:LINKS]->(b)")
        tx.cypher("MATCH (d:Doc {id: 3000009}) SET d.n = 777")
        tx.commit()
        tx = graph.begin()
        tx.cypher("MATCH (d:Doc {id: 3000010}) DETACH DELETE d")
        tx.rollback()
    assert kglite._backend_is_forked(mapped)
    assert _answers(mapped) == _answers(memory)
    for q in (
        "MATCH (d:Doc {id: 960000}) RETURN d.n",
        "MATCH (d:Doc {id: 960001})-[:LINKS]->(b) RETURN b.id",
        "MATCH (d:Doc {id: 3000007}) RETURN d.n",
        "MATCH (d:Doc {id: 3000009}) RETURN d.n",
        "MATCH (d:Doc) WHERE d.id > 3000000 RETURN count(d), sum(d.n)",
    ):
        assert mapped.cypher(q).to_list() == memory.cypher(q).to_list(), q


def test_a_snapshot_keeps_the_frozen_topology_through_a_write(saved):
    mapped = kglite.load(saved, storage="mapped")
    snapshot = mapped.freeze()
    before = [snapshot.cypher(q).to_list() for q in PROBES]
    mapped.cypher("MATCH (d:Doc {id: 100}) DETACH DELETE d")
    assert [snapshot.cypher(q).to_list() for q in PROBES] == before
    assert snapshot.cypher("MATCH (d:Doc {id: 100}) RETURN d.n").to_list() != []
    assert mapped.cypher("MATCH (d:Doc {id: 100}) RETURN d.n").to_list() == []


def test_a_copy_is_independent_of_the_frozen_original(saved):
    mapped = kglite.load(saved, storage="mapped")
    copy = mapped.copy()
    copy.cypher("MATCH (d:Doc {id: 100}) DETACH DELETE d")
    assert copy.cypher("MATCH (d:Doc {id: 100}) RETURN d.n").to_list() == []
    assert mapped.cypher("MATCH (d:Doc {id: 100}) RETURN d.n").to_list() != []
    assert kglite._backend_is_frozen(mapped)


def test_a_save_of_a_frozen_graph_stays_frozen_and_reloads_the_same(saved, tmp_path):
    mapped = kglite.load(saved, storage="mapped")
    again = str(tmp_path / "again.kgl")
    mapped.save(again)
    assert kglite._backend_is_frozen(mapped), "a save does not promote"
    reloaded = kglite.load(again, storage="mapped")
    assert kglite._backend_is_frozen(reloaded)
    assert _answers(reloaded) == _answers(mapped)


def test_a_save_after_a_write_records_the_write(saved, tmp_path):
    mapped = kglite.load(saved, storage="mapped")
    mapped.cypher("MATCH (d:Doc {id: 100}) DETACH DELETE d")
    mapped.cypher("CREATE (:Doc {id: 920000, n: 3, title: 'kept'})")
    again = str(tmp_path / "after-write.kgl")
    mapped.save(again)
    memory = kglite.load(again, storage="memory")
    reloaded = kglite.load(again, storage="mapped")
    assert kglite._backend_is_frozen(reloaded)
    assert _answers(reloaded) == _answers(memory) == _answers(mapped)
    assert reloaded.cypher("MATCH (d:Doc {id: 920000}) RETURN d.n").to_list() == [{"d.n": 3}]
    assert kglite._backend_is_frozen(mapped), "the graph serves from the file it saved"
    assert not kglite._backend_is_forked(mapped), "with no overlay left"


def test_the_overlay_keeps_taking_writes_and_saves_after_a_save(saved, tmp_path):
    mapped = kglite.load(saved, storage="mapped")
    memory = kglite.load(saved, storage="memory")
    first, second = str(tmp_path / "first.kgl"), str(tmp_path / "second.kgl")
    for graph in (mapped, memory):
        graph.cypher("MATCH (d:Doc {id: 100}) DETACH DELETE d")
        graph.cypher("CREATE (:Doc {id: 950000, n: 1, title: 'a'})")
    mapped.save(first)
    for graph in (mapped, memory):
        graph.cypher("MATCH (d:Doc {id: 950000}) SET d.n = 2")
        graph.cypher("MATCH (a:Doc {id: 950000}), (b:Doc {id: 78}) CREATE (a)-[:LINKS]->(b)")
        graph.cypher("CREATE (:Doc {id: 4242, n: 8, title: 'reuse'})")
    mapped.save(second)
    assert kglite._backend_is_frozen(mapped, include_overlay=True)
    for path in (first, second):
        assert kglite._backend_is_frozen(kglite.load(path, storage="mapped"))
    final = kglite.load(second, storage="mapped")
    assert _answers(final) == _answers(kglite.load(second, storage="memory")) == _answers(memory)
    assert _answers(kglite.load(first, storage="mapped")) != _answers(final)


def test_a_save_rebases_the_graph_and_a_held_snapshot_keeps_the_old_state(saved, tmp_path):
    mapped = kglite.load(saved, storage="mapped")
    memory = kglite.load(saved, storage="memory")
    target = str(tmp_path / "rebase.kgl")
    for graph in (mapped, memory):
        graph.cypher("MATCH (d:Doc {id: 100}) DETACH DELETE d")
        graph.cypher("CREATE (:Doc {id: 970000, n: 1, title: 'a'})")
    snapshot = mapped.freeze()
    before = _answers(snapshot)
    mapped.cypher("CREATE (:Doc {id: 970001, n: 2, title: 'after the freeze'})")
    memory.cypher("CREATE (:Doc {id: 970001, n: 2, title: 'after the freeze'})")
    mapped.save(target)
    assert kglite._backend_is_frozen(mapped) and not kglite._backend_is_forked(mapped)
    assert _answers(mapped) == _answers(memory)
    assert _answers(snapshot) == before, "the snapshot still reads the state it froze"
    # More writes over the rebased graph, a save over the same file, a reload.
    for graph in (mapped, memory):
        graph.cypher("MATCH (d:Doc {id: 970000}) SET d.n = 9")
        graph.cypher("MATCH (a:Doc {id: 970000}), (b:Doc {id: 79}) CREATE (a)-[:LINKS]->(b)")
    assert kglite._backend_is_forked(mapped), "a write overlays the new file"
    mapped.save(target)
    assert kglite._backend_is_frozen(mapped) and not kglite._backend_is_forked(mapped)
    assert _answers(mapped) == _answers(memory)
    assert _answers(kglite.load(target, storage="mapped")) == _answers(memory)


def test_compact_layout_keeps_the_compressed_topology_only(saved, tmp_path):
    graph = kglite.load(saved, storage="memory")
    compact = str(tmp_path / "compact.kgl")
    graph.save(compact, layout="compact")
    mapped = kglite.load(compact, storage="mapped")
    assert not kglite._backend_is_frozen(mapped)
    assert _answers(mapped) == _answers(graph)


def test_a_plain_load_follows_the_recorded_storage_mode(tmp_path):
    graph = _build()
    memory_path = str(tmp_path / "memory.kgl")
    graph.save(memory_path)
    assert not kglite._backend_is_frozen(kglite.load(memory_path))
    mapped = kglite.load(memory_path, storage="mapped")
    mapped_path = str(tmp_path / "mapped.kgl")
    mapped.save(mapped_path)
    assert kglite._backend_is_frozen(kglite.load(mapped_path))


def test_graph_algorithms_run_on_a_frozen_graph(saved):
    mapped = kglite.load(saved, storage="mapped")
    memory = kglite.load(saved, storage="memory")
    assert kglite._backend_is_frozen(mapped)
    assert len(mapped.connected_components()) == len(memory.connected_components())


def test_a_durable_open_of_a_frozen_file_logs_and_replays_its_writes(saved, tmp_path):
    path = str(tmp_path / "app.kgl")
    kglite.load(saved, storage="mapped").save(path)
    graph = kglite.open(path, storage="mapped", durable="full")
    assert kglite._backend_is_frozen(graph)
    graph.cypher("CREATE (:Doc {id: 930000, n: 9, title: 'logged'})")
    graph.cypher("MATCH (d:Doc {id: 100}) DETACH DELETE d")
    assert not kglite._backend_is_frozen(graph)
    assert kglite._backend_is_frozen(graph, include_overlay=True), "the log wraps the overlay"
    expected = _answers(graph)
    del graph
    reopened = kglite.open(path, storage="mapped", durable="full")
    assert reopened.cypher("MATCH (d:Doc {id: 930000}) RETURN d.n").to_list() == [{"d.n": 9}]
    assert reopened.cypher("MATCH (d:Doc {id: 100}) RETURN d.n").to_list() == []
    assert _answers(reopened) == expected


def test_vacuum_after_deletes_on_a_frozen_graph_keeps_every_answer(saved):
    mapped = kglite.load(saved, storage="mapped")
    memory = kglite.load(saved, storage="memory")
    for graph in (mapped, memory):
        graph.cypher("MATCH (d:Doc) WHERE d.id >= 100 AND d.id < 300 DETACH DELETE d")
        graph.vacuum()
    assert _answers(mapped) == _answers(memory)


def test_the_topology_policy_decides_whether_a_mapped_load_reads_the_sections(saved):
    memory = kglite.load(saved, storage="memory")
    for policy, frozen in [("auto", True), ("mapped", True), ("memory", False)]:
        graph = kglite.load(saved, storage="mapped", topology=policy)
        assert kglite._backend_is_frozen(graph) is frozen, policy
        assert graph.graph_info()["storage_mode"] == "mapped"
        assert _answers(graph) == _answers(memory), policy
    # A graph loaded into memory storage never reads the sections.
    assert not kglite._backend_is_frozen(kglite.load(saved, storage="memory", topology="mapped"))


def test_a_bad_topology_value_is_an_argument_error(saved):
    with pytest.raises(kglite.ArgumentError, match="topology must be 'auto', 'mapped' or 'memory', got 'always'"):
        kglite.load(saved, topology="always")
    with pytest.raises(kglite.ArgumentError, match="topology must be"):
        kglite.open_session(saved, topology="always")


def test_open_session_takes_topology_and_a_durable_open_refuses_it(saved, tmp_path):
    session = kglite.open_session(saved, storage="mapped", topology="memory")
    assert session.cypher(PROBES[1]).to_list() == kglite.load(saved).cypher(PROBES[1]).to_list()
    with pytest.raises(ValueError, match="topology"):
        kglite.open_session(str(tmp_path / "d.kgl"), topology="memory", durable="off")


def test_graph_info_reports_mapped_heap_and_overlay_topology_bytes(saved):
    mapped = kglite.load(saved, storage="mapped")
    info = mapped.graph_info()
    assert info["topology_mapped_bytes"] > 0
    assert info["topology_heap_bytes"] < 64 * 1024, "only templates and sparse edge properties are heap"
    assert info["topology_overlay_entries"] == 0
    mapped.cypher("CREATE (:Doc {id: 900001, n: 1, title: 'new'})")
    after = mapped.graph_info()
    assert after["topology_overlay_entries"] > 0
    assert after["topology_mapped_bytes"] == info["topology_mapped_bytes"]

    heap = kglite.load(saved, storage="mapped", topology="memory").graph_info()
    assert heap["topology_mapped_bytes"] == 0
    assert heap["topology_heap_bytes"] >= 40 * NODES + 32 * EDGES, "the heap arrays are counted"
    assert heap["topology_overlay_entries"] == 0


def test_the_environment_kill_switch_puts_the_topology_on_the_heap(saved):
    probe = (
        "import sys, kglite; g = kglite.load(sys.argv[1], storage='mapped', topology='mapped');"
        "i = g.graph_info();"
        "print(kglite._backend_is_frozen(g), i['topology_mapped_bytes'], i['topology_heap_bytes'],"
        " g.cypher(\"MATCH (d:Doc) RETURN count(d) AS c\").to_list()[0]['c'])"
    )
    env = {**os.environ, "KGLITE_MMAP": "0"}
    out = subprocess.run(
        [sys.executable, "-c", probe, saved], capture_output=True, text=True, check=True, env=env
    ).stdout.split()
    frozen, mapped_bytes, heap_bytes, docs = out
    assert frozen == "True", "the sections still serve the graph, from heap copies"
    assert int(mapped_bytes) == 0
    assert int(heap_bytes) > 40 * NODES, "the copied sections are heap"
    assert int(docs) == NODES - 1
