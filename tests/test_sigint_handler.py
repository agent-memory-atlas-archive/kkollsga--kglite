"""The SIGINT handler armed around engine calls (``crates/kglite-py/src/util.rs``).

The handler is armed with one ``sigaction`` per call and stays installed
between calls, forwarding SIGINT to the disposition it replaced. These tests
pin the contract that makes that safe: a Ctrl-C outside a query behaves as if
kglite were absent, a handler the user installs (before or between queries)
is the one that runs, and the cancel flag never leaks from one query into the
next. Cancellation of a running query is in ``test_cypher_cancellation.py``.
"""

import os
import signal
import sys
import threading
import time

import pytest

import kglite

pytestmark = pytest.mark.skipif(
    sys.platform == "win32",
    reason="SIGINT handler is #[cfg(unix)]-only; os.kill(SIGINT) would terminate the runner on Windows",
)

BETWEENNESS = "CALL betweenness() YIELD node, score RETURN count(*) AS c"


def _small():
    g = kglite.KnowledgeGraph()
    g.cypher("UNWIND range(1, 5) AS i CREATE (:N {id: i})")
    return g


def _algo_graph(n=12_000):
    g = kglite.KnowledgeGraph()
    g.cypher(f"UNWIND range(1, {n}) AS i CREATE (:N {{id: i}})")
    for d in (1, 7, 53, 211):
        g.cypher(f"MATCH (a:N) WITH a, a.id + {d} AS nb MATCH (b:N {{id: nb}}) CREATE (a)-[:R]->(b)")
    return g


@pytest.fixture
def restore_sigint():
    prior = signal.getsignal(signal.SIGINT)
    yield
    signal.signal(signal.SIGINT, prior)


def _deliver():
    os.kill(os.getpid(), signal.SIGINT)
    # The Python-level handler runs between bytecodes of the main thread.
    time.sleep(0.05)


def test_ctrl_c_between_queries_is_pythons_keyboard_interrupt(restore_sigint):
    g = _small()
    signal.signal(signal.SIGINT, signal.default_int_handler)
    g.cypher("RETURN 1")
    with pytest.raises(KeyboardInterrupt):
        _deliver()
    # ... and the engine still answers afterwards, with no stale cancel.
    assert g.cypher("MATCH (n:N) RETURN count(n) AS c").to_list() == [{"c": 5}]


def test_user_handler_installed_before_first_query_runs(restore_sigint):
    hits = []
    signal.signal(signal.SIGINT, lambda *_: hits.append("a"))
    g = _small()
    g.cypher("RETURN 1")
    _deliver()
    assert hits == ["a"]


def test_user_handler_replaced_between_queries_is_the_one_that_runs(restore_sigint):
    hits = []
    g = _small()
    signal.signal(signal.SIGINT, lambda *_: hits.append("a"))
    g.cypher("RETURN 1")
    _deliver()
    signal.signal(signal.SIGINT, lambda *_: hits.append("b"))
    g.cypher("RETURN 1")
    _deliver()
    signal.signal(signal.SIGINT, lambda *_: hits.append("c"))
    _deliver()  # replaced, then no query before the signal
    assert hits == ["a", "b", "c"]


def test_ignored_sigint_stays_ignored_between_queries(restore_sigint):
    g = _small()
    signal.signal(signal.SIGINT, signal.SIG_IGN)
    g.cypher("RETURN 1")
    _deliver()  # would terminate the runner if forwarded as SIG_DFL
    g.cypher("RETURN 1")


def test_cancel_flag_does_not_leak_into_the_next_query(restore_sigint):
    g = _algo_graph()
    signal.signal(signal.SIGINT, signal.default_int_handler)
    threading.Timer(0.3, lambda: os.kill(os.getpid(), signal.SIGINT)).start()
    with pytest.raises(KeyboardInterrupt):
        g.cypher(BETWEENNESS)
    assert g.cypher("MATCH (n:N) RETURN count(n) AS c").to_list() == [{"c": 12_000}]


def test_concurrent_calls_share_the_handler_and_release_it(restore_sigint):
    g = _small()
    hits = []
    signal.signal(signal.SIGINT, lambda *_: hits.append("user"))
    stop = threading.Event()

    def worker():
        while not stop.is_set():
            g.cypher("RETURN 1")

    threads = [threading.Thread(target=worker) for _ in range(4)]
    for t in threads:
        t.start()
    time.sleep(0.2)
    stop.set()
    for t in threads:
        t.join()
    _deliver()
    assert hits == ["user"]
