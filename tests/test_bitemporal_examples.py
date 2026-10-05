"""The bitemporal examples print what the guide says they print.

`docs/python/guides/bitemporal.md` quotes the output of
`examples/bitemporal_org_chart.py` and `examples/bitemporal_snapshots.py`; each
script is run here and its whole stdout compared with the golden under
`tests/fixtures/example_output/`. A change to the engine that alters an answer
the guide shows, or a break in a script's close step, fails here.
"""

from __future__ import annotations

from pathlib import Path
import subprocess
import sys

import pytest

REPO = Path(__file__).resolve().parent.parent
GOLDEN = REPO / "tests" / "fixtures" / "example_output"


def run_example(name: str) -> subprocess.CompletedProcess:
    return subprocess.run(
        [sys.executable, str(REPO / "examples" / f"{name}.py")],
        capture_output=True,
        text=True,
        encoding="utf-8",
        timeout=100,
        check=False,
    )


@pytest.mark.parametrize("name", ["bitemporal_org_chart", "bitemporal_snapshots"])
def test_example_prints_its_golden_output(name):
    result = run_example(name)
    assert result.returncode == 0, result.stderr
    assert result.stdout == (GOLDEN / f"{name}.txt").read_text(encoding="utf-8")


def test_snapshots_example_closes_changed_and_vanished_images():
    """The rules the guide states, read off the printed history."""
    out = run_example("bitemporal_snapshots").stdout
    # Changed key: the old image is closed on the day the change was seen.
    assert "'id': 'm2@2024-01-31', 'role': 'eng', 'recorded_from': '2024-01-31', 'recorded_to': '2024-02-29'" in out
    assert "'id': 'm2@2024-02-29', 'role': 'senior', 'recorded_from': '2024-02-29', 'recorded_to': None" in out
    # Vanished key: closed, nothing added.
    assert "'id': 'm1@2024-02-29', 'role': 'lead', 'recorded_from': '2024-02-29', 'recorded_to': '2024-03-31'" in out
    # Re-delivery changes nothing; an older snapshot is refused.
    assert out.count("{'added': 0, 'closed': 0}") == 2
    assert "refused: snapshot 2024-02-29 is older than the latest applied (2024-03-31)" in out
    # The default-today trap: a membership with ended validity is on record
    # but invisible without FOR VALID_TIME ALL.
    assert "on record, valid today: ['m3']" in out
    assert "on record, all validity: ['m2', 'm3']" in out
