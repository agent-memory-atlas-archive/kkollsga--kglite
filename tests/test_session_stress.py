"""Adversarial concurrency stress harness for `Session` (opt-in: -m stress).

These run the Session concurrency scenarios under sustained heavy load — many
threads, thousands of iterations — and assert hard invariants every time. The
point is to surface *timing-dependent* races that the low-iteration functional
tests in test_session.py cannot: a single missed lock, a torn read, or a lost
update shows up here as a wrong count, not a clean pass.

Run: pytest tests/test_session_stress.py -m stress
Default suite excludes them (slow). Treat a failure here as a real concurrency
bug, not flakiness — the invariants are exact, not statistical.
"""

import json
import subprocess
import sys
import textwrap
import threading

import pandas as pd
import pytest

import kglite

THREADS = 16


def _counter_graph() -> kglite.KnowledgeGraph:
    g = kglite.KnowledgeGraph()
    g.add_nodes(pd.DataFrame({"id": [1], "title": ["ctr"], "n": [0]}), "C", "id", "title")
    return g


def _docs(n: int) -> kglite.KnowledgeGraph:
    g = kglite.KnowledgeGraph()
    g.add_nodes(
        pd.DataFrame(
            {
                "id": list(range(n)),
                "title": [f"d{i}" for i in range(n)],
                "team": ["A" if i % 2 == 0 else "B" for i in range(n)],
            }
        ),
        "Doc",
        "id",
        "title",
    )
    return g


@pytest.mark.stress
def test_write_compose_heavy_contention():
    """The strongest lost-update detector: THREADS writers each do PER
    read-modify-write increments of one shared counter through the Session.
    Every increment must land — final must equal THREADS*PER exactly."""
    per = 1500
    s = _counter_graph().session()

    def worker():
        for _ in range(per):
            s.execute("MATCH (n:C {id: 1}) SET n.n = n.n + 1")

    threads = [threading.Thread(target=worker) for _ in range(THREADS)]
    for t in threads:
        t.start()
    for t in threads:
        t.join()

    final = s.cypher("MATCH (n:C {id: 1}) RETURN n.n AS n").to_list()[0]["n"]
    assert final == THREADS * per, f"lost updates: {final} != {THREADS * per}"


@pytest.mark.stress
def test_mixed_read_write_invariants():
    """Writers append nodes while readers count concurrently. Readers must
    never see an impossible count (outside the monotonic range), never error,
    and the final count must be exact."""
    start_n = 50
    per_writer = 300
    writers = 8
    readers = 8
    s = _docs(start_n).session()

    errors: list[Exception] = []
    torn: list[int] = []
    done = threading.Event()

    def writer(base: int):
        for i in range(per_writer):
            try:
                s.execute(
                    "CREATE (n:Doc {id: $id, title: 'w'})",
                    params={"id": 10_000 + base * per_writer + i},
                )
            except Exception as e:  # noqa: BLE001
                errors.append(e)

    def reader():
        # read until all writers finish; counts only ever grow
        while not done.is_set():
            try:
                c = s.cypher("MATCH (n:Doc) RETURN count(n) AS c").to_list()[0]["c"]
                if not (start_n <= c <= start_n + writers * per_writer):
                    torn.append(c)
            except Exception as e:  # noqa: BLE001
                errors.append(e)

    wts = [threading.Thread(target=writer, args=(b,)) for b in range(writers)]
    rts = [threading.Thread(target=reader) for _ in range(readers)]
    for t in rts:
        t.start()
    for t in wts:
        t.start()
    for t in wts:
        t.join()
    done.set()
    for t in rts:
        t.join()

    assert errors == [], f"errors under mixed load: {errors[:3]}"
    assert torn == [], f"readers observed impossible counts: {torn[:3]}"
    final = s.cypher("MATCH (n:Doc) RETURN count(n) AS c").to_list()[0]["c"]
    assert final == start_n + writers * per_writer


@pytest.mark.stress
def test_snapshot_isolation_under_writes():
    """A held snapshot must NEVER observe writes that land after it was taken,
    no matter how much concurrent write pressure there is."""
    s = _docs(40).session()
    bad: list[int] = []
    errors: list[Exception] = []
    stop = threading.Event()

    def writer(base: int):
        i = 0
        while not stop.is_set():
            try:
                s.execute(
                    "CREATE (n:Doc {id: $id, title: 'w'})",
                    params={"id": 100_000 + base * 1_000_000 + i},
                )
                i += 1
            except Exception as e:  # noqa: BLE001
                errors.append(e)

    wts = [threading.Thread(target=writer, args=(b,)) for b in range(4)]
    for t in wts:
        t.start()
    try:
        # repeatedly snapshot and immediately re-read it many times while writes
        # rain down; each snapshot is frozen at its capture count.
        for _ in range(2000):
            fz = s.snapshot()
            c0 = fz.cypher("MATCH (n:Doc) RETURN count(n) AS c").to_list()[0]["c"]
            c1 = fz.cypher("MATCH (n:Doc) RETURN count(n) AS c").to_list()[0]["c"]
            if c0 != c1:
                bad.append(c1)
    finally:
        stop.set()
        for t in wts:
            t.join()

    assert errors == [], f"errors during snapshot isolation stress: {errors[:3]}"
    assert bad == [], f"a frozen snapshot changed under concurrent writes: {bad[:3]}"


@pytest.mark.stress
def test_per_thread_cursor_fluent_under_load():
    """THREADS threads each repeatedly take their own cursor off one shared
    Session and run a full fluent chain. Every chain must return the exact
    expected count, with zero errors — no single-owner borrow conflict."""
    n = 200  # 100 team A, 100 team B
    s = _docs(n).session()
    errors: list[Exception] = []
    bad: list[int] = []

    def worker():
        for _ in range(200):
            try:
                got = len(s.cursor().select("Doc").where({"team": "A"}).sort("id").to_df())
                if got != n // 2:
                    bad.append(got)
            except Exception as e:  # noqa: BLE001
                errors.append(e)

    threads = [threading.Thread(target=worker) for _ in range(THREADS)]
    for t in threads:
        t.start()
    for t in threads:
        t.join()

    assert errors == [], f"per-thread cursor errors: {errors[:3]}"
    assert bad == [], f"cursor fluent chains returned wrong counts: {bad[:3]}"


# --- durable graph -----------------------------------------------------------------------------
#
# Threaded readers, a Session reader, and writers against one durable handle, with and without an
# embedder callback that releases the GIL inside the write. The invariants hold on every engine that
# keeps the durable contract, so they survive the swap that lets writers queue instead of raising:
# a write either lands exactly once or raises the one allowed concurrency error, readers never see an
# impossible count, and after a hard exit the log carries exactly the writes that returned.

DURABLE_CHILD = textwrap.dedent(
    r"""
    import json, os, sys, threading, time
    import kglite

    path, with_callback = sys.argv[1], sys.argv[2] == "callback"
    WRITERS, PER, READERS = 4, 20, 3

    class Model:
        dimension = 2
        def embed(self, texts):
            time.sleep(0.002)  # releases the GIL while the write is in flight
            return [[1.0, 0.0] for _ in texts]

    g = kglite.open(path, durable="full")
    g.cypher("UNWIND range(1, 4) AS i CREATE (:Doc {id: i, hits: 0})")
    if with_callback:
        g.set_embedder(Model())
        g.cypher("MATCH (d:Doc) SET d.body = 'x'")
        g.embed_texts("Doc", "body", show_progress=False)
    session = g.session()
    done = threading.Event()
    errors, landed, impossible = [], [0] * WRITERS, []

    def write(w):
        q = ("MATCH (d:Doc {id: $id}) SET d.hits = d.hits + 1, d.s = text_score(d, 'body', 'q')"
             if with_callback else "MATCH (d:Doc {id: $id}) SET d.hits = d.hits + 1")
        for _ in range(PER):
            try:
                g.cypher(q, params={"id": w + 1})
                landed[w] += 1
            except RuntimeError as e:
                if "accessed concurrently" not in str(e):
                    errors.append(repr(e))
            except Exception as e:
                errors.append(repr(e))

    def read():
        while not done.is_set():
            try:
                c = session.cypher("MATCH (d:Doc) RETURN count(d) AS c").to_list()[0]["c"]
                if c != 4:
                    impossible.append(c)
                g.cypher("MATCH (d:Doc) RETURN count(d) AS c")
            except RuntimeError as e:
                if "accessed concurrently" not in str(e):
                    errors.append(repr(e))
            except Exception as e:
                errors.append(repr(e))

    readers = [threading.Thread(target=read) for _ in range(READERS)]
    writers = [threading.Thread(target=write, args=(w,)) for w in range(WRITERS)]
    for t in readers + writers:
        t.start()
    for t in writers:
        t.join()
    done.set()
    for t in readers:
        t.join()
    live = g.cypher("MATCH (d:Doc) RETURN d.id AS id, d.hits AS h ORDER BY id").to_list()
    print(json.dumps({"landed": landed, "live": live, "errors": errors, "impossible": impossible}), flush=True)
    os._exit(0)
    """
)


@pytest.mark.stress
@pytest.mark.parametrize("flavour", ["plain", "callback"])
def test_durable_threaded_readers_and_writers_lose_nothing_across_a_hard_exit(flavour, tmp_path):
    path = tmp_path / "app.kgl"
    done = subprocess.run(
        [sys.executable, "-c", DURABLE_CHILD, str(path), flavour],
        capture_output=True,
        text=True,
        timeout=90,  # a hang is a failure, well inside the 120 s ceiling
    )
    assert done.returncode == 0, done.stderr[-2000:]
    out = json.loads(done.stdout.strip().splitlines()[-1])
    assert out["errors"] == [], out["errors"][:3]
    assert out["impossible"] == [], "a Session reader saw a node count that never existed"
    # Each writer owns one document: its counter equals the writes that returned, live...
    assert [row["h"] for row in out["live"]] == out["landed"]
    # ...and after the hard exit.
    reopened = kglite.open(str(path), durable="full")
    assert [r["h"] for r in reopened.cypher("MATCH (d:Doc) RETURN d.id AS id, d.hits AS h ORDER BY id").to_list()] == (
        out["landed"]
    )
    assert sum(out["landed"]) > 0, "no write landed; the stress proved nothing"
