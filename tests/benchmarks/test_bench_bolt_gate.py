"""Bolt server ratio guards for the Linux perf gate.

``kglite-bolt-server`` has no reference wheel to compare against and no
committed baseline, so each cell carries a **self-contained guard against a
control measured in the same process**: the loopback echo round trip, the
embedded engine, or the same server under no load. CI builds the server in
release mode and runs this file as a blocking step of the perf job. Locally,
release builds only::

    cargo build --release -p kglite-bolt-server
    KGLITE_BOLT_SERVER=target/release/kglite-bolt-server \
        pytest tests/benchmarks/test_bench_bolt_gate.py -m benchmark -v -s

What each guard catches:

* **Small exchange vs echo** -- a request/response over Bolt may cost at most
  ``MAX_EXCHANGE_VS_ECHO`` times a bare TCP echo round trip. Nagle's algorithm
  holding a small reply for the client's delayed ACK (``TCP_NODELAY`` missing,
  issue #201) turns ~20 us into ~44 ms on Linux; macOS loopback hides it.
* **Streaming vs embedded** -- 100k scalar rows and 100k ten-property map rows
  streamed to a raw reader may cost at most a multiple of materialising the
  same rows in the embedded engine. Per-record writes and flushes
  (``CoalescingWriter`` removed) cost 10x and more.
* **Peak memory** -- the server's peak resident memory while streaming 100k map
  rows. Each extra copy of the result adds hundreds of MB.
* **Head-of-line blocking** -- with two async workers and six slow queries
  running, a ``RETURN 1`` on another connection must not wait for a whole query.
* **Parameter decode** -- ``UNWIND $rows`` over 20k maps, message encoded once,
  against the same query over maps the server builds itself. A second cell
  sends twice the payload and must fail the same guard, so the cell proves it
  separates a 2x slower decoder from noise on every run.
* **Reader scaling** -- point lookups from four processes must outrun one
  process; a server-wide lock serialises them to 1.0x.

Statistic: ``min`` for the deterministic cells, ``p99`` for the tail cell, per
the performance protocol. Control values are printed with every verdict.
"""

from __future__ import annotations

import json
import multiprocessing as mp
import os
from pathlib import Path
import shutil
import socket
import subprocess
import sys
import threading
import time

import pandas as pd
import pytest

import kglite

try:
    from tests.bolt_raw import SIG_PULL, SIG_RECORD, SIG_RUN, SIG_SUCCESS, RawBolt, Struct, chunk, pack
except ImportError:  # CI copies this file and bolt_raw.py into one directory
    from bolt_raw import SIG_PULL, SIG_RECORD, SIG_RUN, SIG_SUCCESS, RawBolt, Struct, chunk, pack

pytestmark = [pytest.mark.benchmark]

#: Bolt `RETURN 1` over a raw client may cost this multiple of a bare TCP echo.
#: Measured 3.0x on macOS and 5.2x on a Linux runner (the Python client frames
#: more than the echo does); a Nagle stall is >1000x, so the ceiling is set for
#: the stall, not for drift.
MAX_EXCHANGE_VS_ECHO = 20.0
#: Streamed rows may cost this multiple of the embedded engine materialising them.
MAX_STREAM_VS_EMBEDDED = {"scalar": 3.0, "map10": 8.0}
#: Peak resident memory of the server while streaming 100k ten-property maps:
#: measured 843 MB on a Linux runner and 1040-1054 MB on macOS, so the ceiling
#: is 30% over each; an extra copy of the records adds ~450 MB.
MAX_PEAK_RSS_MB = 1100 if sys.platform.startswith("linux") else 1400
#: A `RETURN 1` during slow queries may wait this fraction of a slow query.
MAX_PROBE_WAIT_FRACTION = 0.25
#: Rows in the parameter-decode cell: enough that decoding, not the round
#: trip, is most of the exchange (the transport of the same bytes as one string
#: is 0.4 ms of ~32 ms).
PARAM_ROWS = 20_000
#: The same query run over parameter maps may cost this multiple of the same
#: query over maps the server builds itself (no payload to decode). Both sides
#: run in the one server process, interleaved, so runner speed cancels. The
#: embedded engine was the control before and was wrong for it: it converts
#: Python rows on the Python side, so its time moved with the interpreter
#: (9.1-15.3 ms across runners) while the server's did not (72-80 ms), and the
#: ratio read 5.3x to 7.9x with no code change.
#:
#: The ratio does not carry across operating systems, because the server-built
#: control costs 4.3 ms on macOS and 16.8 ms on a Linux runner while decoding
#: costs 32 and 79 ms. Measured: macOS 7.5-7.8x over six runs, Linux 4.7x
#: (decoding twice the payload reads 15.1-15.8x and 9.1x). Each ceiling sits
#: between the measured ratio and the 2x-payload ratio on its platform, about
#: 40% clear of both, so it catches a decoder twice as slow.
MAX_PARAMS_VS_SERVER_BUILT = 6.5 if sys.platform.startswith("linux") else 11.0
#: Four reader processes must reach this multiple of one reader's throughput.
MIN_READER_SCALING = 1.25

STREAM_QUERIES = {
    "scalar": "UNWIND range(1,100000) AS x RETURN x",
    "map10": (
        "UNWIND range(1,100000) AS x RETURN {a:x,b:x+1,c:'s'+toString(x),d:x*0.5,e:x%7,"
        "f:true,g:'text value',h:x*2,i:x%13,j:null} AS m"
    ),
}


def _server_binary() -> Path:
    """The release server. CI names it in ``KGLITE_BOLT_SERVER``, and a missing
    file there fails the run; without the variable a missing default build skips
    (``make bench`` runs this directory on machines that built only the wheel)."""
    given = os.environ.get("KGLITE_BOLT_SERVER")
    if given:
        binary = Path(given)
        if not binary.exists():
            pytest.fail(f"KGLITE_BOLT_SERVER={given} does not exist")
        return binary
    binary = Path(__file__).resolve().parents[2] / "target" / "release" / "kglite-bolt-server"
    if not binary.exists():
        pytest.skip(f"{binary} is not built; `cargo build --release -p kglite-bolt-server`")
    return binary


def _free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


class _Server:
    def __init__(self, graph: Path, env: dict[str, str] | None = None):
        binary = _server_binary()
        self.port = _free_port()
        self.proc = subprocess.Popen(
            [str(binary), "--graph", str(graph), "--bind", "127.0.0.1", "--port", str(self.port)],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.PIPE,
            env={**os.environ, **(env or {})},
        )
        deadline = time.time() + 30
        while True:
            try:
                socket.create_connection(("127.0.0.1", self.port), 0.2).close()
                return
            except OSError:
                if time.time() > deadline or self.proc.poll() is not None:
                    self.close()
                    pytest.fail("bolt server did not start")
                time.sleep(0.05)

    def peak_rss_mb(self) -> float:
        status = Path(f"/proc/{self.proc.pid}/status")
        if status.exists():
            for line in status.read_text(encoding="utf-8").splitlines():
                if line.startswith("VmHWM:"):
                    return int(line.split()[1]) / 1024
        out = subprocess.run(["ps", "-o", "rss=", "-p", str(self.proc.pid)], capture_output=True, text=True).stdout
        return int(out.strip() or 0) / 1024

    def close(self) -> None:
        self.proc.kill()
        self.proc.wait(10)
        if self.proc.stderr:
            self.proc.stderr.close()


@pytest.fixture(scope="module")
def graph_path(tmp_path_factory) -> Path:
    n = 10_000
    graph = kglite.KnowledgeGraph()
    graph.add_nodes(
        pd.DataFrame({"pid": range(n), "name": [f"P{i}" for i in range(n)], "age": [20 + i % 60 for i in range(n)]}),
        "Person",
        "pid",
        "name",
    )
    path = tmp_path_factory.mktemp("bolt-gate") / "gate.kgl"
    graph.save(str(path))
    return path


@pytest.fixture(scope="module")
def embedded(graph_path: Path):
    return kglite.load(str(graph_path))


@pytest.fixture
def server(graph_path: Path):
    srv = _Server(graph_path)
    yield srv
    srv.close()


def _min_seconds(fn, rounds: int, warmup: int = 20) -> float:
    for _ in range(warmup):
        fn()
    best = float("inf")
    for _ in range(rounds):
        started = time.perf_counter()
        fn()
        best = min(best, time.perf_counter() - started)
    return best


def _echo_rtt_seconds() -> float:
    """Minimum round trip of one byte through a loopback TCP echo."""
    listener = socket.socket()
    listener.bind(("127.0.0.1", 0))
    listener.listen(1)

    def serve():
        conn, _ = listener.accept()
        conn.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
        while data := conn.recv(64):
            conn.sendall(data)
        conn.close()

    thread = threading.Thread(target=serve, daemon=True)
    thread.start()
    client = socket.create_connection(listener.getsockname())
    client.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)

    def round_trip():
        client.sendall(b"x")
        client.recv(64)

    best = _min_seconds(round_trip, 2000, 200)
    client.close()
    thread.join(5)
    listener.close()
    return best


def _login(server: _Server) -> RawBolt:
    conn = RawBolt("127.0.0.1", server.port, timeout=60)
    conn.hello()
    conn.logon("u", "p")
    return conn


def _run_pull(conn: RawBolt, query: str, params: dict | None = None) -> None:
    """RUN and PULL in one write, then read to the end of the result."""
    conn.sock.sendall(chunk(pack(Struct(SIG_RUN, query, params or {}, {}))) + chunk(pack(Struct(SIG_PULL, {"n": -1}))))
    assert conn.recv()[0] == SIG_SUCCESS
    while conn.recv()[0] == SIG_RECORD:
        pass


def _controls(embedded) -> dict:
    return {
        "embedded_return1_us": _min_seconds(lambda: embedded.cypher("RETURN 1"), 2000, 200) * 1e6,
        "cpu_loop_ms": min(_cpu_loop() for _ in range(5)) * 1e3,
    }


def _cpu_loop() -> float:
    started = time.perf_counter()
    total = 0
    for i in range(2_000_000):
        total += i * i
    return time.perf_counter() - started


def _report(cell: str, **values) -> None:
    print(
        f"bolt-gate {cell}: " + json.dumps({k: round(v, 3) if isinstance(v, float) else v for k, v in values.items()})
    )


# --- small exchange ----------------------------------------------------------


def test_bolt_small_exchange_is_close_to_a_tcp_echo(server, embedded):
    echo = _echo_rtt_seconds()
    with _login(server) as conn:
        exchange = _min_seconds(lambda: _run_pull(conn, "RETURN 1"), 2000, 200)
    ratio = exchange / echo
    _report("exchange", exchange_us=exchange * 1e6, echo_us=echo * 1e6, ratio=ratio, **_controls(embedded))
    assert ratio <= MAX_EXCHANGE_VS_ECHO, (
        f"RETURN 1 over Bolt took {exchange * 1e6:.1f} us, {ratio:.1f}x a TCP echo ({echo * 1e6:.1f} us); "
        f"limit {MAX_EXCHANGE_VS_ECHO}x (TCP_NODELAY missing?)"
    )


# --- streaming and memory ----------------------------------------------------


@pytest.fixture(scope="module")
def rawclient(tmp_path_factory) -> Path:
    """The Rust raw reader: it decodes nothing, so it measures the server."""
    rustc = shutil.which("rustc")
    if rustc is None:
        pytest.fail("rustc is required to build the raw stream reader")
    source = Path(__file__).with_name("bolt_rawclient.rs")
    out = tmp_path_factory.mktemp("rawclient") / "rawclient"
    subprocess.run([rustc, "-O", str(source), "-o", str(out)], check=True, capture_output=True)
    return out


def _stream_ms(rawclient: Path, server: _Server, query: str, reps: int = 3) -> tuple[float, float]:
    out = subprocess.run(
        [str(rawclient), str(server.port), str(reps), query], capture_output=True, text=True, check=True
    ).stdout
    rows = [json.loads(line) for line in out.splitlines() if line.startswith("{")]
    assert rows and all(r["records"] == 100_000 for r in rows), out
    return min(r["run_success_ms"] for r in rows), min(r["stream_ms"] for r in rows)


@pytest.mark.parametrize("kind", ["scalar", "map10"])
def test_bolt_stream_stays_near_the_embedded_engine(kind, server, embedded, rawclient):
    query = STREAM_QUERIES[kind]
    embedded_ms = _min_seconds(lambda: len(embedded.cypher(query)), 3, 1) * 1e3
    run_ms, stream_ms = _stream_ms(rawclient, server, query)
    ratio = stream_ms / embedded_ms
    _report(
        f"stream-{kind}",
        run_ms=run_ms,
        stream_ms=stream_ms,
        embedded_ms=embedded_ms,
        ratio=ratio,
        **_controls(embedded),
    )
    assert ratio <= MAX_STREAM_VS_EMBEDDED[kind], (
        f"streaming 100k {kind} rows took {stream_ms:.0f} ms, {ratio:.1f}x the embedded engine "
        f"({embedded_ms:.0f} ms); limit {MAX_STREAM_VS_EMBEDDED[kind]}x"
    )


def test_bolt_peak_memory_of_a_map_stream(graph_path, rawclient):
    srv = _Server(graph_path)
    try:
        before = srv.peak_rss_mb()
        _stream_ms(rawclient, srv, STREAM_QUERIES["map10"], reps=1)
        peak = srv.peak_rss_mb()
    finally:
        srv.close()
    _report("peak-rss", before_mb=before, peak_mb=peak)
    assert peak <= MAX_PEAK_RSS_MB, f"server peak RSS {peak:.0f} MB streaming 100k map rows; limit {MAX_PEAK_RSS_MB} MB"


# --- head-of-line blocking ---------------------------------------------------


def test_bolt_slow_queries_do_not_stall_other_connections(graph_path, embedded):
    slow = "UNWIND range(1, 1500000) AS x RETURN count(x) AS c"
    # Two async workers and more slow queries than workers: with queries on the
    # workers, a small request waits for a whole query.
    srv = _Server(graph_path, env={"TOKIO_WORKER_THREADS": "2"})
    durations: list[float] = []
    clients = 6
    ready = threading.Barrier(clients + 1)

    def run_slow():
        with _login(srv) as conn:
            ready.wait()
            started = time.perf_counter()
            _run_pull(conn, slow)
            durations.append(time.perf_counter() - started)

    try:
        with _login(srv) as probe:
            idle = _min_seconds(lambda: _run_pull(probe, "RETURN 1"), 500, 50)
            threads = [threading.Thread(target=run_slow) for _ in range(clients)]
            for t in threads:
                t.start()
            ready.wait()
            time.sleep(0.05)
            waits = []
            while any(t.is_alive() for t in threads):
                started = time.perf_counter()
                _run_pull(probe, "RETURN 1")
                waits.append(time.perf_counter() - started)
                time.sleep(0.005)
            for t in threads:
                t.join()
    finally:
        srv.close()
    query_s = min(durations)
    p99 = sorted(waits)[min(len(waits) - 1, int(0.99 * len(waits)))]
    _report(
        "head-of-line",
        idle_us=idle * 1e6,
        probe_p99_ms=p99 * 1e3,
        probe_max_ms=max(waits) * 1e3,
        slow_query_ms=query_s * 1e3,
        probes=len(waits),
        **_controls(embedded),
    )
    assert query_s > 0.05, f"premise: the slow query must take real time, took {query_s * 1e3:.1f} ms"
    assert max(waits) <= query_s * MAX_PROBE_WAIT_FRACTION, (
        f"a RETURN 1 waited {max(waits) * 1e3:.0f} ms behind slow queries of {query_s * 1e3:.0f} ms"
    )


# --- parameter decode --------------------------------------------------------


def _param_rows(count: int) -> list[dict]:
    return [{"id": i, "name": f"name{i}", "age": 20 + i % 60, "score": i * 0.5, "ok": i % 2 == 0} for i in range(count)]


def _param_decode_ratio(server: _Server, embedded, decode_rows: int) -> dict:
    """Parameter-map query vs the same query over server-built maps.

    ``decode_rows`` is the payload size; the server-built control always makes
    ``PARAM_ROWS`` maps, so a payload of twice the rows models a decoder twice
    as slow (the red-proof mutation uses it)."""
    rows = _param_rows(decode_rows)
    query = "UNWIND $rows AS r RETURN count(r) AS c, sum(r.age) AS a"
    built = (
        f"UNWIND range(0, {PARAM_ROWS - 1}) AS i "
        "WITH {id: i, name: 'name' + toString(i), age: 20 + i % 60, score: i * 0.5, ok: i % 2 = 0} AS r "
        "RETURN count(r) AS c, sum(r.age) AS a"
    )
    # Encode once so the client's packing is not in the timed region.
    with_params = chunk(pack(Struct(SIG_RUN, query, {"rows": rows}, {}))) + chunk(pack(Struct(SIG_PULL, {"n": -1})))
    server_built = chunk(pack(Struct(SIG_RUN, built, {}, {}))) + chunk(pack(Struct(SIG_PULL, {"n": -1})))
    with _login(server) as conn:

        def exchange(message: bytes) -> None:
            conn.sock.sendall(message)
            assert conn.recv()[0] == SIG_SUCCESS
            while conn.recv()[0] == SIG_RECORD:
                pass

        for _ in range(3):
            exchange(with_params)
            exchange(server_built)
        bolt_s = built_s = float("inf")
        for _ in range(20):  # interleaved, so a slow stretch hits both sides
            started = time.perf_counter()
            exchange(with_params)
            bolt_s = min(bolt_s, time.perf_counter() - started)
            started = time.perf_counter()
            exchange(server_built)
            built_s = min(built_s, time.perf_counter() - started)
    embedded_s = _min_seconds(lambda: embedded.cypher(query, params={"rows": rows}), 5, 2)
    return {
        "bolt_ms": bolt_s * 1e3,
        "server_built_ms": built_s * 1e3,
        "ratio": bolt_s / built_s,
        "embedded_ms": embedded_s * 1e3,  # reported, not asserted: Python-bound, see the ceiling's note
    }


def _assert_param_ratio(result: dict) -> None:
    assert result["ratio"] <= MAX_PARAMS_VS_SERVER_BUILT, (
        f"UNWIND over {PARAM_ROWS} parameter maps took {result['bolt_ms']:.2f} ms over Bolt, "
        f"{result['ratio']:.1f}x the same query over server-built maps "
        f"({result['server_built_ms']:.2f} ms); limit {MAX_PARAMS_VS_SERVER_BUILT}x"
    )


def test_bolt_parameter_decode_stays_near_server_built_maps(server, embedded):
    result = _param_decode_ratio(server, embedded, PARAM_ROWS)
    _report("params", **result, **_controls(embedded))
    _assert_param_ratio(result)


def test_bolt_parameter_decode_guard_catches_a_slower_decoder(server, embedded):
    """Red proof, run in CI: twice the payload costs a decoder twice as slow,
    and the guard must reject it. A guard that stops separating a 2x decode
    regression from noise fails here instead of passing silently."""
    result = _param_decode_ratio(server, embedded, 2 * PARAM_ROWS)
    _report("params-2x-mutation", **result)
    with pytest.raises(AssertionError):
        _assert_param_ratio(result)


# --- reader scaling ----------------------------------------------------------


def _lookup_worker(port: int, seconds: float, index: int, results, start) -> None:
    import random

    rng = random.Random(index)
    with RawBolt("127.0.0.1", port, timeout=60) as conn:
        conn.hello()
        conn.logon("u", "p")
        start.wait()
        done = 0
        end = time.perf_counter() + seconds
        while time.perf_counter() < end:
            _run_pull(conn, "MATCH (n:Person {pid: $i}) RETURN n.name AS name", {"i": rng.randrange(10_000)})
            done += 1
    results.put(done)


def _lookups_per_second(port: int, clients: int, seconds: float = 3.0) -> float:
    ctx = mp.get_context("spawn")
    results = ctx.Queue()
    start = ctx.Event()
    procs = [ctx.Process(target=_lookup_worker, args=(port, seconds, i, results, start)) for i in range(clients)]
    for p in procs:
        p.start()
    time.sleep(1.5)
    start.set()
    total = sum(results.get() for _ in procs)
    for p in procs:
        p.join()
    return total / seconds


def test_bolt_readers_scale_across_connections(server, embedded):
    one = _lookups_per_second(server.port, 1)
    four = _lookups_per_second(server.port, 4)
    scaling = four / one
    _report(
        "reader-scaling",
        one_client_ops=one,
        four_client_ops=four,
        scaling=scaling,
        cpus=os.cpu_count(),
        **_controls(embedded),
    )
    assert scaling >= MIN_READER_SCALING, (
        f"four reader processes reached {scaling:.2f}x one reader ({four:.0f} vs {one:.0f} lookups/s); "
        f"floor {MIN_READER_SCALING}x (a server-wide lock?)"
    )


if __name__ == "__main__":
    sys.exit(pytest.main([__file__, "-m", "benchmark", "-s", "-v"]))
