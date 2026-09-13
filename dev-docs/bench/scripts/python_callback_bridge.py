#!/usr/bin/env python3
"""Release-only Python callback bridge benchmark.

The driver exercises the public bundled MCP entry point.  It creates its
fixture in a temporary directory and writes only the requested JSON result.
No model is downloaded.  ``hashlib`` supplies a synthetic native-concurrency
control: hashing buffers larger than 2047 bytes releases CPython's GIL.
"""

from __future__ import annotations

import argparse
import hashlib
import importlib
import json
import math
import os
from pathlib import Path
import platform
import selectors
import statistics
import subprocess
import sys
import tempfile
import time


def percentile(values: list[float], q: float) -> float:
    ordered = sorted(values)
    index = max(0, min(len(ordered) - 1, math.ceil(q * len(ordered)) - 1))
    return ordered[index]


def summary_ms(values: list[float]) -> dict[str, float]:
    return {
        "median": statistics.median(values),
        "p95": percentile(values, 0.95),
        "p99": percentile(values, 0.99),
        "mean": statistics.fmean(values),
        "min": min(values),
        "max": max(values),
    }


class Callback:
    dimension = 8

    def __init__(self, workload: str):
        self.workload = workload
        self.batch_lengths: list[int] = []

    def embed(self, texts: list[str]) -> list[list[float]]:
        self.batch_lengths.append(len(texts))
        telemetry = os.environ.get("CALLBACK_TELEMETRY")
        if telemetry:
            fd = os.open(telemetry, os.O_APPEND | os.O_CREAT | os.O_WRONLY, 0o600)
            try:
                os.write(fd, (json.dumps({"batch": len(texts)}) + "\n").encode())
            finally:
                os.close(fd)
        return [known_vector(text, self.workload) for text in texts]


def known_vector(text: str, workload: str) -> list[float]:
    payload = text.encode()
    rounds = 1 if workload == "cheap" else 256
    digest = b""
    for counter in range(rounds):
        # 16 KiB crosses hashlib's documented GIL-release threshold.
        digest = hashlib.sha256(payload.ljust(16_384, b"x") + counter.to_bytes(2, "little")).digest()
    return [float(byte) for byte in digest[: Callback.dimension]]


def cosine(left: list[float], right: list[float]) -> float:
    numerator = sum(a * b for a, b in zip(left, right, strict=True))
    left_norm = math.sqrt(sum(value * value for value in left))
    right_norm = math.sqrt(sum(value * value for value in right))
    return numerator / (left_norm * right_norm)


def expected_query_rows(query_name: str, workload: str) -> list[list[int | float]]:
    if query_name == "control":
        return [[120]]
    fixture_vectors = [known_vector(f"text-{node_id}", "cheap") for node_id in range(256)]
    if query_name == "embed_once":
        query_vector = known_vector("text-7", workload)
        ranked = sorted(
            ((node_id, cosine(vector, query_vector)) for node_id, vector in enumerate(fixture_vectors)),
            key=lambda item: (-item[1], item[0]),
        )
        return [[node_id, score] for node_id, score in ranked[:5]]
    if query_name == "embed_four":
        document_id = 7
        vector = fixture_vectors[document_id]
        scores = [cosine(vector, known_vector(f"text-{query_id}", workload)) for query_id in range(1, 5)]
        return [[document_id, *scores]]
    raise AssertionError(f"unknown query oracle {query_name!r}")


def result_payload(message: dict, request_id: int) -> dict:
    if message.get("id") != request_id:
        raise AssertionError(f"response ID mismatch for {request_id}: {message.get('id')!r}")
    result = message.get("result")
    if not isinstance(result, dict):
        raise AssertionError(f"request {request_id} has no result object: {message}")
    structured = result.get("structuredContent")
    if not isinstance(structured, dict):
        raise AssertionError(f"request {request_id} has no structured result: {result}")
    return structured


def validate_query_result(message: dict, request_id: int, query_name: str, expected: list[list[int | float]]) -> None:
    payload = result_payload(message, request_id)
    rows = payload.get("rows")
    columns = payload.get("columns")
    if not isinstance(rows, list) or not isinstance(columns, list):
        raise AssertionError(f"request {request_id} has invalid columns/rows: {payload}")
    if query_name == "control":
        if columns != ["total"] or rows != [[120]]:
            raise AssertionError(f"request {request_id} returned the wrong control result: {payload}")
        return
    if query_name == "embed_once":
        if columns != ["id", "score"] or len(rows) != 5:
            raise AssertionError(f"request {request_id} returned the wrong one-embedding shape: {payload}")
        for actual_row, expected_row in zip(rows, expected, strict=True):
            if not isinstance(actual_row, list) or len(actual_row) != 2:
                raise AssertionError(f"request {request_id} returned an invalid one-embedding row: {actual_row}")
            if actual_row[0] != expected_row[0] or not isinstance(actual_row[1], (int, float)):
                raise AssertionError(
                    f"request {request_id} returned the wrong ranked result: {rows}; expected {expected}"
                )
            if not math.isclose(actual_row[1], expected_row[1], rel_tol=1e-6, abs_tol=1e-7):
                raise AssertionError(
                    f"request {request_id} returned the wrong ranked score: {rows}; expected {expected}"
                )
        return
    if query_name == "embed_four":
        if columns != ["id", "a", "b", "c", "d"] or len(rows) != 1 or not isinstance(rows[0], list):
            raise AssertionError(f"request {request_id} returned the wrong four-embedding shape: {payload}")
        if len(rows[0]) != 5 or any(
            not isinstance(value, (int, float)) or not math.isfinite(float(value)) for value in rows[0]
        ):
            raise AssertionError(f"request {request_id} returned unusable four-embedding values: {rows[0]}")
        expected_row = expected[0]
        if rows[0][0] != expected_row[0] or any(
            not math.isclose(actual, wanted, rel_tol=1e-6, abs_tol=1e-7)
            for actual, wanted in zip(rows[0][1:], expected_row[1:], strict=True)
        ):
            raise AssertionError(
                f"request {request_id} returned wrong four-embedding values: {rows[0]}; expected {expected_row}"
            )
        return
    raise AssertionError(f"unknown query oracle {query_name!r}")


def oracle_self_check() -> list[str]:
    valid = {"id": 7, "result": {"structuredContent": {"columns": ["total"], "rows": [[120]]}}}
    validate_query_result(valid, 7, "control", expected_query_rows("control", "cheap"))
    ranked = expected_query_rows("embed_once", "cheap")
    four = expected_query_rows("embed_four", "cheap")[0]
    expected_failures = {
        "empty_result": ("control", {"id": 7, "result": {}}),
        "wrong_rows": ("control", {"id": 7, "result": {"structuredContent": {"columns": ["total"], "rows": [[0]]}}}),
        "wrong_id": ("control", {"id": 8, "result": {"structuredContent": {"columns": ["total"], "rows": [[120]]}}}),
        "wrong_embedding_score": (
            "embed_once",
            {
                "id": 7,
                "result": {
                    "structuredContent": {"columns": ["id", "score"], "rows": [*ranked[:-1], [ranked[-1][0], 0.5]]}
                },
            },
        ),
        "wrong_embedding_ranking": (
            "embed_once",
            {
                "id": 7,
                "result": {
                    "structuredContent": {"columns": ["id", "score"], "rows": [ranked[1], ranked[0], *ranked[2:]]}
                },
            },
        ),
        "wrong_four_embedding_values": (
            "embed_four",
            {
                "id": 7,
                "result": {"structuredContent": {"columns": ["id", "a", "b", "c", "d"], "rows": [[*four[:-1], 0.5]]}},
            },
        ),
    }
    observed = []
    for name, (query_name, message) in expected_failures.items():
        try:
            validate_query_result(message, 7, query_name, expected_query_rows(query_name, "cheap"))
        except AssertionError:
            observed.append(name)
        else:
            raise AssertionError(f"semantic oracle did not reject {name}")
    pending: dict[int, tuple[dict, int]] = {}
    seen: set[int] = set()
    store_response(pending, seen, {"id": 7}, 1)
    try:
        store_response(pending, seen, {"id": 7}, 2)
    except AssertionError:
        observed.append("duplicate_response_id")
    else:
        raise AssertionError("response-ID oracle did not reject a duplicate")
    return observed


def store_response(
    pending: dict[int, tuple[dict, int]], seen_response_ids: set[int], message: dict, received_ns: int
) -> None:
    response_id = message.get("id")
    if not isinstance(response_id, int):
        return
    if response_id in seen_response_ids:
        raise AssertionError(f"duplicate response ID {response_id}")
    seen_response_ids.add(response_id)
    pending[response_id] = (message, received_ns)


class Client:
    def __init__(self, proc: subprocess.Popen[bytes], timeout: float):
        self.proc = proc
        self.timeout = timeout
        self.buffer = bytearray()
        self.stderr = bytearray()
        self.pending: dict[int, tuple[dict, int]] = {}
        self.seen_response_ids: set[int] = set()
        self.selector = selectors.DefaultSelector()
        assert proc.stdout is not None
        assert proc.stderr is not None
        os.set_blocking(proc.stdout.fileno(), False)
        os.set_blocking(proc.stderr.fileno(), False)
        self.selector.register(proc.stdout, selectors.EVENT_READ, "stdout")
        self.selector.register(proc.stderr, selectors.EVENT_READ, "stderr")

    def send(self, payload: dict) -> None:
        assert self.proc.stdin is not None
        self.proc.stdin.write(json.dumps(payload, separators=(",", ":")).encode() + b"\n")
        self.proc.stdin.flush()

    def _parse_buffer(self, received_ns: int) -> None:
        while b"\n" in self.buffer:
            raw, _, rest = self.buffer.partition(b"\n")
            self.buffer = bytearray(rest)
            message = json.loads(raw)
            store_response(self.pending, self.seen_response_ids, message, received_ns)

    def receive_any(self, outstanding: set[int]) -> tuple[int, dict, int]:
        deadline = time.monotonic() + self.timeout
        while time.monotonic() < deadline:
            for response_id in self.pending:
                if response_id in outstanding:
                    message, received_ns = self.pending.pop(response_id)
                    return response_id, message, received_ns
            events = self.selector.select(max(0.0, deadline - time.monotonic()))
            if not events:
                break
            for key, _mask in events:
                chunk = os.read(key.fileobj.fileno(), 65_536)
                if key.data == "stderr":
                    if chunk:
                        self.stderr.extend(chunk)
                        del self.stderr[:-65_536]
                    else:
                        self.selector.unregister(key.fileobj)
                    continue
                if not chunk:
                    raise RuntimeError(f"server closed stdout: {self.stderr.decode(errors='replace')[-4000:]}")
                self.buffer.extend(chunk)
                self._parse_buffer(time.perf_counter_ns())
        raise TimeoutError(
            f"timeout waiting for responses {sorted(outstanding)}: {self.stderr.decode(errors='replace')[-4000:]}"
        )

    def receive(self, wanted: int) -> dict:
        _response_id, message, _received_ns = self.receive_any({wanted})
        return message


def request(request_id: int, query: str) -> dict:
    return {
        "jsonrpc": "2.0",
        "id": request_id,
        "method": "tools/call",
        "params": {"name": "cypher_query", "arguments": {"query": query}},
    }


def checked(client: Client, request_id: int) -> dict:
    message = client.receive(request_id)
    return checked_message(message, request_id)


def checked_message(message: dict, request_id: int) -> dict:
    result = message.get("result", {})
    if result.get("isError") or "error" in message:
        raise RuntimeError(f"request {request_id} failed: {message}")
    return message


def process_observation(pid: int) -> dict[str, int | None]:
    rss_kib = threads = None
    try:
        rss_kib = int(subprocess.check_output(["ps", "-o", "rss=", "-p", str(pid)], text=True).strip())
        threads = int(subprocess.check_output(["ps", "-M", "-p", str(pid)], text=True).count("\n") - 1)
    except (OSError, subprocess.SubprocessError, ValueError):
        pass
    return {"rss_kib": rss_kib, "threads": threads}


def direct_batch_cells(kglite, pandas, batch_sizes: list[int], workload: str) -> list[dict]:
    cells = []
    for batch_size in batch_sizes:
        graph = kglite.KnowledgeGraph()
        count = batch_size * 8
        graph.add_nodes(
            pandas.DataFrame(
                {
                    "id": range(count),
                    "title": [f"doc-{i}" for i in range(count)],
                    "body": [f"text-{i}" for i in range(count)],
                }
            ),
            "Doc",
            "id",
            "title",
            ["body"],
        )
        callback = Callback(workload)
        graph.set_embedder(callback)
        started = time.perf_counter_ns()
        result = graph.embed_texts("Doc", "body", batch_size=batch_size, show_progress=False)
        elapsed_ms = (time.perf_counter_ns() - started) / 1e6
        if result["embedded"] != count:
            raise AssertionError(result)
        cells.append(
            {
                "configured_batch_size": batch_size,
                "observed_callback_batches": callback.batch_lengths,
                "texts": count,
                "elapsed_ms": elapsed_ms,
                "texts_per_second": count / (elapsed_ms / 1000),
            }
        )
    return cells


def server_cell(
    python: str,
    launcher: Path,
    graph: Path,
    manifest: Path,
    workload: str,
    query_name: str,
    expected_rows: list[list[int | float]],
    query: str,
    depth: int,
    warmup: int,
    requests: int,
    timeout: float,
    telemetry_enabled: bool = False,
) -> dict:
    telemetry = graph.parent / f"telemetry-{workload}-{os.getpid()}-{time.time_ns()}.jsonl"
    env = {**os.environ, "CALLBACK_WORKLOAD": workload}
    if telemetry_enabled:
        env["CALLBACK_TELEMETRY"] = str(telemetry)
    start = time.perf_counter_ns()
    proc = subprocess.Popen(
        [python, str(launcher), "--graph", str(graph), "--mcp-config", str(manifest)],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        env=env,
    )
    client = Client(proc, timeout)
    try:
        client.send(
            {
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2024-11-05",
                    "capabilities": {},
                    "clientInfo": {"name": "callback-bench", "version": "1"},
                },
            }
        )
        checked(client, 1)
        startup_ms = (time.perf_counter_ns() - start) / 1e6
        client.send({"jsonrpc": "2.0", "method": "notifications/initialized"})
        for index in range(warmup):
            rid = 10 + index
            client.send(request(rid, query))
            checked(client, rid)

        latencies: list[float] = []
        responses: list[tuple[int, dict]] = []
        started_at: dict[int, int] = {}
        outstanding: set[int] = set()
        run_start = time.perf_counter_ns()
        sent = received = 0
        while received < requests:
            while sent < requests and sent - received < depth:
                rid = 10_000 + sent
                started_at[rid] = time.perf_counter_ns()
                client.send(request(rid, query))
                outstanding.add(rid)
                sent += 1
            rid, message, received_ns = client.receive_any(outstanding)
            outstanding.remove(rid)
            checked_message(message, rid)
            latencies.append((received_ns - started_at[rid]) / 1e6)
            responses.append((rid, message))
            received += 1
        elapsed_s = (received_ns - run_start) / 1e9
        for rid, message in responses:
            validate_query_result(message, rid, query_name, expected_rows)
        observation = process_observation(proc.pid)
        result = {
            "startup_ms": startup_ms,
            "depth": depth,
            "requests": requests,
            "throughput_rps": requests / elapsed_s,
            "latency_ms": summary_ms(latencies),
            **observation,
        }
        if telemetry_enabled:
            events = [json.loads(line) for line in telemetry.read_text().splitlines()] if telemetry.exists() else []
            result["observed_callback_batches"] = [event["batch"] for event in events if "batch" in event]
            result["observed_factory_invocations"] = sum(event.get("factory", 0) for event in events)
        return result
    finally:
        if proc.stdin:
            proc.stdin.close()
        try:
            proc.wait(timeout=timeout)
        except subprocess.TimeoutExpired:
            proc.terminate()
            try:
                proc.wait(timeout=2)
            except subprocess.TimeoutExpired:
                proc.kill()
                proc.wait(timeout=2)
        if proc.returncode != 0:
            stderr = proc.stderr.read().decode(errors="replace")[-4000:] if proc.stderr else ""
            raise RuntimeError(f"server exit {proc.returncode}: {stderr}")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--requests", type=int, default=200)
    parser.add_argument("--warmup", type=int, default=20)
    parser.add_argument("--depths", default="1,4,16")
    parser.add_argument("--batch-sizes", default="1,8,64")
    parser.add_argument("--timeout", type=float, default=15.0)
    args = parser.parse_args()
    os.environ.pop("CALLBACK_TELEMETRY", None)
    oracle_failures = oracle_self_check()

    kglite = importlib.import_module("kglite")
    pandas = importlib.import_module("pandas")
    module = importlib.import_module("kglite.kglite")
    extension = Path(module.__file__).resolve()
    if "site-packages" not in str(extension):
        raise RuntimeError(f"benchmark must use an isolated installed wheel, got {extension}")

    depths = [int(value) for value in args.depths.split(",")]
    batch_sizes = [int(value) for value in args.batch_sizes.split(",")]
    with tempfile.TemporaryDirectory(prefix="kglite-callback-bench-") as raw:
        temp = Path(raw)
        graph = kglite.KnowledgeGraph()
        count = 256
        graph.add_nodes(
            pandas.DataFrame(
                {
                    "id": range(count),
                    "title": [f"doc-{i}" for i in range(count)],
                    "body": [f"text-{i}" for i in range(count)],
                }
            ),
            "Doc",
            "id",
            "title",
            ["body"],
        )
        graph.set_embedder(Callback("cheap"))
        graph.embed_texts("Doc", "body", show_progress=False)
        graph_path = temp / "fixture.kgl"
        graph.save(str(graph_path))
        manifest = temp / "mcp.yaml"
        manifest.write_text(
            "name: callback-bench\n"
            "trust:\n  allow_embedder: true\n"
            "extensions:\n  embedder:\n    library: benchmark\n    model: benchmark\n",
            encoding="utf-8",
        )
        launcher = temp / "launch.py"
        launcher.write_text(
            "import json, os, sys\n"
            f"sys.path.insert(0, {str(Path(__file__).resolve().parent)!r})\n"
            "from python_callback_bridge import Callback\n"
            "import kglite\n"
            "def factory(_cfg):\n"
            "    telemetry = os.environ.get('CALLBACK_TELEMETRY')\n"
            "    if telemetry:\n"
            "        fd = os.open(telemetry, os.O_APPEND | os.O_CREAT | os.O_WRONLY, 0o600)\n"
            "        try:\n"
            "            os.write(fd, (json.dumps({'factory': 1}) + '\\n').encode())\n"
            "        finally:\n"
            "            os.close(fd)\n"
            "    return Callback(os.environ['CALLBACK_WORKLOAD'])\n"
            "kglite._run_mcp_server(sys.argv[1:], embedder_factory=factory)\n",
            encoding="utf-8",
        )
        queries = {
            "control": "MATCH (d:Doc) WHERE d.id < 16 RETURN sum(d.id) AS total",
            "embed_once": (
                "MATCH (d:Doc) RETURN d.id AS id, text_score(d,'body','text-7') AS score ORDER BY score DESC LIMIT 5"
            ),
            "embed_four": (
                "MATCH (d:Doc) WHERE d.id = 7 RETURN d.id AS id,text_score(d,'body','text-1') AS a,"
                "text_score(d,'body','text-2') AS b,text_score(d,'body','text-3') AS c,"
                "text_score(d,'body','text-4') AS d LIMIT 1"
            ),
        }
        cells = {}
        batch_preflight = {}
        expected_rows = {
            workload: {name: expected_query_rows(name, workload) for name in queries}
            for workload in ("cheap", "native_hash")
        }
        for workload in ("cheap", "native_hash"):
            cells[workload] = {}
            batch_preflight[workload] = {}
            for name, query in queries.items():
                preflight = server_cell(
                    sys.executable,
                    launcher,
                    graph_path,
                    manifest,
                    workload,
                    name,
                    expected_rows[workload][name],
                    query,
                    1,
                    0,
                    1,
                    args.timeout,
                    telemetry_enabled=True,
                )
                batch_preflight[workload][name] = {
                    "callback_batches": preflight["observed_callback_batches"],
                    "factory_invocations": preflight["observed_factory_invocations"],
                }
                cells[workload][name] = [
                    server_cell(
                        sys.executable,
                        launcher,
                        graph_path,
                        manifest,
                        workload,
                        name,
                        expected_rows[workload][name],
                        query,
                        depth,
                        args.warmup,
                        args.requests,
                        args.timeout,
                    )
                    for depth in depths
                ]
        direct = {
            workload: direct_batch_cells(kglite, pandas, batch_sizes, workload) for workload in ("cheap", "native_hash")
        }

    output = {
        "schema": 1,
        "captured_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "host": {
            "platform": platform.platform(),
            "python": sys.version,
            "cpu_count": os.cpu_count(),
            "loadavg": os.getloadavg() if hasattr(os, "getloadavg") else None,
        },
        "artifact": {
            "kglite_version": getattr(kglite, "__version__", None),
            "extension": str(extension),
            "sha256": hashlib.sha256(extension.read_bytes()).hexdigest(),
        },
        "parameters": {"requests": args.requests, "warmup": args.warmup, "depths": depths, "batch_sizes": batch_sizes},
        "semantic_oracle_expected_failures": oracle_failures,
        "cells": cells,
        "untimed_batch_preflight": batch_preflight,
        "direct_batch_cells": direct,
    }
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(output, indent=2) + "\n", encoding="utf-8")
    print(args.output)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
