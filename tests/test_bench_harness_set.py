"""The CI perf gate pins the core benchmark harness's cell set to the tracked
baseline (``scripts/benchmark_provenance.py`` compares both wheels' result sets
against ``baselines/current.json``, which only a release captures). A cell
added to ``test_bench_core.py`` mid-cycle reddens that gate a full CI round
later; this test reddens it locally. New cells go in another benchmark file
until the next release captures them."""

from __future__ import annotations

import importlib.util
import json
from pathlib import Path
import subprocess
import sys

import pandas as pd
import pytest

from kglite import KnowledgeGraph

REPO = Path(__file__).resolve().parents[1]
HARNESS = REPO / "tests" / "benchmarks" / "test_bench_core.py"
BASELINE = REPO / "tests" / "benchmarks" / "baselines" / "current.json"


def _collected_cells() -> set[str]:
    out = subprocess.run(
        [
            sys.executable,
            "-m",
            "pytest",
            str(HARNESS),
            "-m",
            "benchmark",
            "--collect-only",
            "-q",
            "-p",
            "no:cacheprovider",
        ],
        capture_output=True,
        text=True,
        check=True,
        cwd=REPO,
    ).stdout
    return {line.rsplit("::", 1)[-1] for line in out.splitlines() if "::" in line}


def test_core_harness_cells_match_the_tracked_baseline():
    expected = {entry["name"] for entry in json.loads(BASELINE.read_text(encoding="utf-8"))["benchmarks"]}
    collected = _collected_cells()
    assert collected, "no benchmark cells collected from the core harness"
    assert collected == expected, (
        f"cells not in baselines/current.json: {sorted(collected - expected)}; "
        f"baseline cells missing from the harness: {sorted(expected - collected)}"
    )


def _load_bench_module(name: str):
    path = REPO / "tests" / "benchmarks" / f"{name}.py"
    spec = importlib.util.spec_from_file_location(f"_bench_guard_{name}", path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


@pytest.mark.parametrize("storage", [None, "mapped"])
@pytest.mark.parametrize("harness", ["test_bench_core", "test_bench_scan_program"])
def test_hop1_cells_run_an_unfused_expansion(harness, storage):
    """``count(*)`` over a typed edge plans as ``FusedCountTypedEdge``, an O(1)
    cached read; a hop1 cell on that shape times a lookup, so the perf gate
    cannot go red for an expansion regression. The cell query must stay on a
    shape that walks the edges."""
    query = _load_bench_module(harness).HOP1_QUERY
    graph = KnowledgeGraph() if storage is None else KnowledgeGraph(storage=storage)
    graph.add_nodes(pd.DataFrame({"pid": [0, 1, 2], "name": ["a", "b", "c"]}), "Person", "pid", "name")
    graph.add_connections(pd.DataFrame({"s": [0, 1], "d": [1, 2]}), "KNOWS", "Person", "s", "Person", "d")
    plan = [row["operation"] for row in graph.cypher("EXPLAIN " + query).to_list()]
    assert not any(op.startswith("Fused") for op in plan), f"{harness} hop1 query is fused: {plan}"
    assert graph.cypher(query).to_list() == [{"s": 3}]
