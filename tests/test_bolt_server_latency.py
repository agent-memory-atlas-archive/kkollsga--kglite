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
