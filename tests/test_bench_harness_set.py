"""The CI perf gate pins the core benchmark harness's cell set to the tracked
baseline (``scripts/benchmark_provenance.py`` compares both wheels' result sets
against ``baselines/current.json``, which only a release captures). A cell
added to ``test_bench_core.py`` mid-cycle reddens that gate a full CI round
later; this test reddens it locally. New cells go in another benchmark file
until the next release captures them."""

from __future__ import annotations

import json
from pathlib import Path
import subprocess
import sys

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
