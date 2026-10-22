"""Shared driver for the mapped write soak tests.

A mapped-loaded graph above the topology floors receives rounds of mixed
writes (node and relationship creates, property sets, deletes, committed and
rolled-back transactions) with a `save()` between rounds. The workload is a
seeded stream that depends only on its own bookkeeping, so the same stream
replays against a `memory` graph and the two must answer the same queries.
"""

from __future__ import annotations

import ctypes
import ctypes.util
import os
import random
import statistics
import struct
import sys
import time

import numpy as np
import pandas as pd

import kglite

PROBES = [
    "MATCH (d:Doc) RETURN count(d) AS c, sum(d.n) AS s, min(d.id) AS lo, max(d.id) AS hi",
    "MATCH ()-[r:LINKS]->() RETURN count(r) AS c",
    "MATCH (d:Doc {id: 1234}) RETURN d.n AS n, d.title AS t",
    "MATCH (a:Doc {id: 77})-[:LINKS]->(b) RETURN count(b) AS c, sum(b.n) AS s",
    "MATCH (a:Doc {id: 77})<-[:LINKS]-(b) RETURN count(b) AS c, min(b.id) AS lo",
    "MATCH (a:Doc)-[:LINKS]->(b:Doc) WHERE a.id < 300 RETURN count(b) AS c, sum(b.n) AS s",
    "MATCH (d:Doc) WHERE d.n = 7 RETURN count(d) AS c",
]


def build_base(nodes: int, edges: int) -> kglite.KnowledgeGraph:
    rng = np.random.default_rng(5)
    graph = kglite.KnowledgeGraph()
    graph.add_nodes(
        pd.DataFrame(
            {
                "id": np.arange(nodes),
                "title": [f"d{i}" for i in range(nodes)],
                "n": rng.integers(0, 100, nodes),
            }
        ),
        "Doc",
        "id",
        "title",
    )
    pairs = pd.DataFrame({"s": rng.integers(0, nodes, edges), "t": rng.integers(0, nodes, edges)})
    graph.add_relationships(pairs, "LINKS", "Doc", "s", "Doc", "t", distinct=True)
    return graph


class Workload:
    """A seeded stream of writes; its choices depend only on its own state."""

    def __init__(self, base_nodes: int, seed: int = 11) -> None:
        self.rng = random.Random(seed)
        self.live = list(range(base_nodes))
        self.next_id = base_nodes
        self.writes = 0

    def _pick(self) -> int:
        return self.live[self.rng.randrange(len(self.live))]

    def _create_node(self, run) -> int:
        i = self.next_id
        self.next_id += 1
        run("CREATE (:Doc {id: $i, title: $t, n: $n})", {"i": i, "t": f"w{i}", "n": i % 100})
        self.live.append(i)
        return i

    def _create_edge(self, run) -> None:
        run(
            "MATCH (a:Doc {id: $a}), (b:Doc {id: $b}) CREATE (a)-[:LINKS]->(b)",
            {"a": self._pick(), "b": self._pick()},
        )

    def _set(self, run) -> None:
        run("MATCH (d:Doc {id: $i}) SET d.n = $n", {"i": self._pick(), "n": self.rng.randrange(100)})

    def _delete(self, run) -> None:
        slot = self.rng.randrange(len(self.live))
        i = self.live[slot]
        self.live[slot] = self.live[-1]
        self.live.pop()
        run("MATCH (d:Doc {id: $i}) DETACH DELETE d", {"i": i})

    def op(self, graph, kind: str | None = None) -> None:
        """One write (a transaction counts as one)."""
        kind = (
            kind
            or self.rng.choices(
                ["node", "edge", "set", "delete", "tx_commit", "tx_rollback"],
                weights=[18, 30, 20, 8, 14, 10],
            )[0]
        )
        self.writes += 1
        if kind == "node":
            self._create_node(lambda q, p: graph.cypher(q, params=p))
        elif kind == "edge":
            self._create_edge(lambda q, p: graph.cypher(q, params=p))
        elif kind == "set":
            self._set(lambda q, p: graph.cypher(q, params=p))
        elif kind == "delete":
            self._delete(lambda q, p: graph.cypher(q, params=p))
        elif kind == "tx_commit":
            tx = graph.begin()
            self._create_node(lambda q, p: tx.cypher(q, params=p))
            self._create_edge(lambda q, p: tx.cypher(q, params=p))
            tx.commit()
        elif kind == "tx_rollback":
            # The rolled-back statements create only, so they remove nothing
            # the workload counts and leave no trace in its bookkeeping.
            tx = graph.begin()
            mark = (len(self.live), self.next_id)
            self._create_node(lambda q, p: tx.cypher(q, params=p))
            self._create_node(lambda q, p: tx.cypher(q, params=p))
            self._create_edge(lambda q, p: tx.cypher(q, params=p))
            tx.rollback()
            del self.live[mark[0] :]
            self.next_id = mark[1]
        else:
            raise ValueError(kind)

    def timed(self, graph, kind: str, count: int) -> float:
        """Median milliseconds of `count` ops of one kind."""
        samples = []
        for _ in range(count):
            started = time.perf_counter()
            self.op(graph, kind)
            samples.append((time.perf_counter() - started) * 1e3)
        return statistics.median(samples)

    def timed_read(self, graph, count: int) -> float:
        samples = []
        for _ in range(count):
            i = self._pick()
            started = time.perf_counter()
            graph.cypher("MATCH (a:Doc {id: $i})-[:LINKS]->(b) RETURN count(b) AS c", params={"i": i}).to_list()
            samples.append((time.perf_counter() - started) * 1e3)
        return statistics.median(samples)


def answers(graph: kglite.KnowledgeGraph) -> list:
    return [graph.cypher(q).to_list() for q in PROBES]


def footprint_bytes() -> int | None:
    """Memory this process is charged for; `None` where it cannot be read."""
    if sys.platform == "darwin":
        libc = ctypes.CDLL(ctypes.util.find_library("c"), use_errno=True)
        buf = ctypes.create_string_buffer(512)
        if libc.proc_pid_rusage(ctypes.c_int(os.getpid()), ctypes.c_int(0), ctypes.byref(buf)) != 0:
            return None
        return struct.unpack_from("=8Q", buf.raw, 16)[7]
    if sys.platform.startswith("linux"):
        with open("/proc/self/statm", encoding="ascii") as handle:
            return int(handle.read().split()[1]) * os.sysconf("SC_PAGE_SIZE")
    return None


def run_mapped_rounds(tmp, nodes, edges, rounds, ops_per_round, *, measure_footprint=False, log=None):
    """Drive the workload against a mapped-loaded graph and return one record per round.

    Round 0 is the fresh load. Each later round applies `ops_per_round` mixed
    writes, times probe ops, checks nothing itself (the caller asserts on the
    records), saves to a new file and records the state after the save.
    """
    base_path = str(tmp / "base.kgl")
    build_base(nodes, edges).save(base_path)
    graph = kglite.load(base_path, storage="mapped")
    work = Workload(nodes)
    records = []

    def cell_costs():
        return {
            "commit_ms": work.timed(graph, "tx_commit", 25),
            "edge_ms": work.timed(graph, "edge", 25),
            "read_ms": work.timed_read(graph, 25),
        }

    def snapshot(number, before_save):
        info = graph.graph_info()
        return {
            "round": number,
            "writes": work.writes,
            "status": info["topology_status"],
            # Integer columns kept delta-compressed are by design, not a loss.
            "fallbacks": [f for f in info["mapped_fallbacks"] if "delta-compressed" not in f["reason"]],
            "budget_used": info["write_budget_used_bytes"],
            "entries": info["topology_overlay_entries"],
            "before_save": before_save,
        }

    fresh = snapshot(0, False)
    fresh.update(cell_costs())
    if measure_footprint:
        kglite.trim_memory()
        fresh["footprint"] = footprint_bytes()
    records.append(fresh)
    previous = base_path
    for number in range(1, rounds + 1):
        writes_before = work.writes
        started = time.perf_counter()
        for _ in range(ops_per_round):
            work.op(graph)
        applied = time.perf_counter() - started
        record = {"round": number}
        record.update(cell_costs())
        dirty = snapshot(number, True)
        record["writes_in_round"] = work.writes - writes_before
        record["write_seconds"] = applied
        record["dirty"] = dirty
        # Rolled-back statements that delete nothing give their entries back.
        entries = graph.graph_info()["topology_overlay_entries"]
        for _ in range(20):
            work.op(graph, "tx_rollback")
        record["rollback_entries_delta"] = graph.graph_info()["topology_overlay_entries"] - entries
        path = str(tmp / f"round{number}.kgl")
        started = time.perf_counter()
        graph.save(path)
        record["save_seconds"] = time.perf_counter() - started
        if previous != base_path:
            os.remove(previous)
        previous = path
        record["saved"] = snapshot(number, False)
        record["answers"] = answers(graph)
        if measure_footprint:
            kglite.trim_memory()
            record["footprint"] = footprint_bytes()
        records.append(record)
        if log:
            log(record)
    final_path = previous
    reloaded = kglite.load(final_path, storage="mapped")
    records[-1]["reloaded_answers"] = answers(reloaded)
    records[-1]["reloaded_status"] = reloaded.graph_info()["topology_status"]
    return records, records[0]


def replay_in_memory(tmp, nodes, edges, rounds, ops_per_round):
    """The same stream against a memory graph: probe answers after each round."""
    base_path = str(tmp / "base.kgl")
    graph = kglite.load(base_path, storage="memory")
    work = Workload(nodes)
    out = []

    # The mapped run times 25 + 25 + 25 probes before the round's writes and
    # applies 20 rollbacks after them; mirror each draw in the same order.
    def cells():
        work.timed(graph, "tx_commit", 25)
        work.timed(graph, "edge", 25)
        work.timed_read(graph, 25)

    cells()
    for _ in range(rounds):
        for _ in range(ops_per_round):
            work.op(graph)
        cells()
        for _ in range(20):
            work.op(graph, "tx_rollback")
        out.append(answers(graph))
    return out
