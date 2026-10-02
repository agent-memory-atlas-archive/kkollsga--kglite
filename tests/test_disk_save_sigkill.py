"""A disk save killed at a random point leaves the graph at its pre- or post-save state.

A child process loads the graph, applies writes and saves; the parent kills it
with SIGKILL at a random moment of that save. Every reopen must read exactly the
state before the save or exactly the state after it, and the next save must
succeed and leave no staging directory behind.
"""

from __future__ import annotations

import random
import signal
import subprocess
import sys
import time

import pandas as pd
import pytest

import kglite

ITERATIONS = 8

CHILD = r"""
import sys, kglite
root, k = sys.argv[1], int(sys.argv[2])
g = kglite.load(root)
g.cypher("MATCH (e:Person {id: %d}) SET e.grade = %d" % (k + 1, 1000 + k))
g.cypher("CREATE (:Person {id: %d, name: 'k', grade: %d})" % (500000 + k, k))
g.cypher("MATCH (e:Person {id: %d}) DETACH DELETE e" % (3000 + k))
g.cypher("MATCH (o:Department {id: %d}) SET o.name = 'k%d'" % ((k % 20) + 1, k))
print("READY", flush=True)
g.save()
print("SAVED", flush=True)
"""


def _apply(g, k):
    g.cypher("MATCH (e:Person {id: $i}) SET e.grade = $v", params={"i": k + 1, "v": 1000 + k})
    g.cypher("CREATE (:Person {id: $i, name: 'k', grade: $k})", params={"i": 500000 + k, "k": k})
    g.cypher("MATCH (e:Person {id: $i}) DETACH DELETE e", params={"i": 3000 + k})
    g.cypher("MATCH (o:Department {id: $i}) SET o.name = $n", params={"i": (k % 20) + 1, "n": f"k{k}"})


def _state(g):
    nodes = g.cypher("MATCH (n) RETURN labels(n)[0] AS l, n.id AS id, n.title AS t, n.grade AS gr ORDER BY l, id")
    edges = g.cypher("MATCH (a)-[r]->(b) RETURN a.id AS a, b.id AS b, r.w AS w ORDER BY a, b, w")
    return nodes.to_list(), edges.to_list()


def _run_child(root, k):
    return subprocess.run([sys.executable, "-c", CHILD, str(root), str(k)], capture_output=True, text=True, timeout=120)


def _stages(root):
    return [p for p in (root / "generations").iterdir() if p.name.startswith(".stage-")]


@pytest.mark.parametrize("seed", [7])
def test_sigkill_during_save_leaves_pre_or_post_state(tmp_path, seed):
    root = tmp_path / "graph"
    n = 6000
    ids = range(1, n + 1)
    people = pd.DataFrame({"id": ids, "name": [f"p{i}" for i in ids], "grade": [i % 7 for i in ids]})
    depts = pd.DataFrame({"id": range(1, 21), "name": [f"d{i}" for i in range(1, 21)]})
    edges = pd.DataFrame({"e": ids, "d": [(i % 20) + 1 for i in ids], "w": ids})
    g = kglite.KnowledgeGraph(storage="disk", path=str(root))
    g.add_nodes(people, "Person", "id", "name")
    g.add_nodes(depts, "Department", "id", "name")
    g.add_relationships(edges, "WORKS_IN", "Person", "e", "Department", "d")
    g.save()
    del g

    started = time.time()
    done = _run_child(root, 0)
    assert "SAVED" in done.stdout, done.stderr
    full = time.time() - started

    rng = random.Random(seed)
    outcomes = {"pre": 0, "post": 0}
    for k in range(1, ITERATIONS + 1):
        pre = _state(kglite.load(str(root)))
        sim = kglite.load(str(root))
        _apply(sim, k)
        post = _state(sim)
        del sim

        child = subprocess.Popen([sys.executable, "-c", CHILD, str(root), str(k)], stdout=subprocess.PIPE, text=True)
        assert child.stdout.readline().startswith("READY")
        time.sleep(rng.uniform(0.0, full * 1.1))
        child.send_signal(signal.SIGKILL)
        child.wait()
        child.stdout.close()

        seen = _state(kglite.load(str(root)))
        assert seen in (pre, post), f"iteration {k}: state is neither the pre- nor the post-save state"
        outcomes["pre" if seen == pre else "post"] += 1

    final = _run_child(root, 99)
    assert "SAVED" in final.stdout, final.stderr
    assert _stages(root) == []
