"""Round-trip latency of the smallest Bolt interactions (issue #201).

Without `TCP_NODELAY` on accepted connections, Linux holds each small reply in
Nagle's buffer until the client's delayed ACK: ~44 ms per exchange, ~132 ms per
explicit transaction (three exchanges). macOS loopback does not show it, so the
correctness suites never saw it; this test fails on Linux without the fix.
"""

import statistics
import time

import pytest

neo4j = pytest.importorskip("neo4j")

pytestmark = [pytest.mark.bolt]

SAMPLES = 20
# Healthy loopback is ~1-2 ms; the Nagle stall is >= 40 ms per exchange.
MAX_P50_MS = 20.0


def _p50_ms(fn) -> float:
    samples = []
    for _ in range(SAMPLES):
        t0 = time.perf_counter()
        fn()
        samples.append((time.perf_counter() - t0) * 1000)
    return statistics.median(samples)


def test_bolt_small_exchanges_are_not_nagle_delayed(bolt_server):
    with neo4j.GraphDatabase.driver(bolt_server, auth=("", "")) as driver:

        def autocommit():
            with driver.session() as s:
                s.run("RETURN 1 AS x").consume()

        def read_tx():
            with driver.session() as s:
                s.execute_read(lambda tx: tx.run("RETURN 1 AS x").consume())

        with driver.session() as open_session:

            def same_session():
                open_session.run("RETURN 1 AS x").consume()

            results = {
                "auto-commit RETURN 1": _p50_ms(autocommit),
                "explicit read transaction": _p50_ms(read_tx),
                "RETURN 1 on one open session": _p50_ms(same_session),
            }

    report = ", ".join(f"{label}: p50 {ms:.1f} ms" for label, ms in results.items())
    print(f"bolt latency -- {report}")
    slow = {label: ms for label, ms in results.items() if ms >= MAX_P50_MS}
    assert not slow, f"p50 over {MAX_P50_MS} ms (TCP_NODELAY missing?): {report}"


def test_slow_queries_do_not_stall_other_connections(tmp_path, bolt_binary_path):
    """Query execution runs off the async workers.

    With two workers and six slow queries running, a small request on another
    connection used to wait for a whole query: every worker was inside one.
    """
    import threading

    if not bolt_binary_path.exists():
        pytest.skip("bolt-server binary not built")
    from tests.bolt_raw import SIG_PULL, SIG_RUN, SIG_SUCCESS, RawBolt
    from tests.conftest import _build_bolt_fixture_graph, _spawn_bolt_server, _teardown_bolt_server

    graph = tmp_path / "hol.kgl"
    _build_bolt_fixture_graph(graph)
    proc, url = _spawn_bolt_server(graph, env={"TOKIO_WORKER_THREADS": "2"})
    host, port = url.removeprefix("bolt://").rsplit(":", 1)
    slow = "UNWIND range(1, 3000000) AS x RETURN count(x) AS c"
    durations: list[float] = []
    started = threading.Barrier(7)

    def run_slow():
        with RawBolt(host, int(port), timeout=120) as conn:
            conn.hello()
            conn.logon("u", "p")
            started.wait()
            t0 = time.perf_counter()
            assert conn.request(SIG_RUN, slow, {}, {}) == SIG_SUCCESS
            conn.request(SIG_PULL, {"n": -1})
            durations.append(time.perf_counter() - t0)

    try:
        with RawBolt(host, int(port)) as probe:
            probe.hello()
            probe.logon("u", "p")
            threads = [threading.Thread(target=run_slow) for _ in range(6)]
            for t in threads:
                t.start()
            started.wait()
            time.sleep(0.05)  # the slow queries are now executing
            waits = []
            while any(t.is_alive() for t in threads):
                t0 = time.perf_counter()
                assert probe.request(SIG_RUN, "RETURN 1 AS one", {}, {}) == SIG_SUCCESS
                probe.request(SIG_PULL, {"n": -1})
                waits.append(time.perf_counter() - t0)
                time.sleep(0.01)
            for t in threads:
                t.join()
    finally:
        _teardown_bolt_server(proc)

    assert waits, f"no probe completed while the slow queries ran ({durations})"
    assert min(durations) > 0.1, f"premise: the slow query must take real time, took {durations}"
    assert max(waits) < min(durations) / 4, (
        f"a RETURN 1 waited {max(waits) * 1000:.0f} ms behind queries that run {min(durations) * 1000:.0f} ms"
    )
