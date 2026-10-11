"""Golden durable-log fixtures written by the published kglite 0.19.7 wheel (WAL format 11).

``tests/test_durable_golden.py`` pins format-10 logs. These are the logs the
pre-swap engine writes today, including the text-index build/drop op that format
11 added. They are committed binaries under ``tests/fixtures/durable_golden_v11/``
(see ``build_durable_golden_v11_fixtures.py`` for how they were produced); the
expectations are hand-derived literals, so a replay engine cannot pass by agreeing
with itself, and a fixture that stops loading is a finding, never a prompt to
regenerate.
"""

from __future__ import annotations

from pathlib import Path
import shutil
import subprocess
import sys
import textwrap

import pytest

import kglite
from tests.fixtures.build_durable_golden_v11_fixtures import EXPECTED, FIXTURE_DIR, SCENARIOS, compare, observe

WAL_HEADER = b"KWAL\x0b"  # magic + format 11
KGL_HEADER = b"RGF\x07\x02"
REPO_ROOT = Path(__file__).resolve().parents[1]
NAMES = sorted(SCENARIOS)


def _copy(name: str, tmp_path: Path) -> Path:
    """Copy a scenario: opening replays the log and checkpoints, consuming it."""
    target = tmp_path / name
    shutil.copytree(FIXTURE_DIR / name, target)
    return target / "app.kgl"


def _recover(path: Path, name: str) -> dict:
    graph = kglite.open(str(path), durable=SCENARIOS[name]["level"])
    try:
        return observe(name, graph)
    finally:
        del graph


def test_every_scenario_has_an_expectation_and_a_fixture():
    assert set(EXPECTED) == set(SCENARIOS) == {p.name for p in FIXTURE_DIR.iterdir() if p.is_dir()}
    assert "text_index_full" in SCENARIOS


@pytest.mark.parametrize("name", NAMES)
def test_reopen_replays_to_the_exact_expected_graph(name, tmp_path):
    assert compare(name, _recover(_copy(name, tmp_path), name)) == []


@pytest.mark.parametrize("name", NAMES)
def test_committed_files_carry_the_pinned_format_bytes(name):
    wal = (FIXTURE_DIR / name / "app.kgl-wal").read_bytes()
    assert wal[:5] == WAL_HEADER, "the WAL format version changed; 0.19.7 logs must stay readable"
    assert len(wal) > 32, "a log with no frames proves nothing"
    kgl = FIXTURE_DIR / name / "app.kgl"
    if kgl.exists():
        assert kgl.read_bytes()[:5] == KGL_HEADER


@pytest.mark.parametrize("name", NAMES)
def test_the_log_is_what_carries_the_graph(name, tmp_path):
    """Non-vacuity: without its log a scenario must not reproduce the expectation."""
    path = _copy(name, tmp_path)
    (path.parent / "app.kgl-wal").unlink()
    try:
        got = _recover(path, name)
    except Exception:
        return
    assert compare(name, got) != []


@pytest.mark.parametrize("name", NAMES)
def test_a_corrupted_frame_is_never_replayed_silently(name, tmp_path):
    """Non-vacuity: flipping one byte inside the first frame either refuses to open or changes the graph."""
    path = _copy(name, tmp_path)
    wal = path.parent / "app.kgl-wal"
    data = bytearray(wal.read_bytes())
    data[len(WAL_HEADER) + 12] ^= 0xFF
    wal.write_bytes(bytes(data))
    try:
        got = _recover(path, name)
    except Exception:
        return
    assert compare(name, got) != []


@pytest.mark.parametrize("name", NAMES)
def test_a_dropped_final_frame_is_never_replayed_silently(name, tmp_path):
    """Non-vacuity: a log missing its last op (torn tail) must differ from the expectation."""
    path = _copy(name, tmp_path)
    wal = path.parent / "app.kgl-wal"
    wal.write_bytes(wal.read_bytes()[:-1])
    try:
        got = _recover(path, name)
    except Exception:
        return
    assert compare(name, got) != []


def test_the_text_index_op_is_what_rebuilds_the_index(tmp_path):
    """The build/drop op is replayed, not recomputed: the body index exists and the dropped note index does not."""
    got = _recover(_copy("text_index_full", tmp_path), "text_index_full")
    assert got["api"] == {"body_indexed": True, "note_indexed": False}


@pytest.mark.parametrize("name", ["checkpoint_then_more_full", "second_crash_full", "text_index_full"])
def test_a_replayed_log_extends_and_replays_again_after_another_crash(name, tmp_path):
    """A 0.19.7 log that was replayed, extended by this engine, and killed again still recovers every frame."""
    path = _copy(name, tmp_path)
    script = textwrap.dedent(
        f"""
        import kglite, os
        g = kglite.open({str(path)!r}, durable={SCENARIOS[name]["level"]!r})
        g.cypher("CREATE (:Late {{id: 900, v: 'late'}})")
        os._exit(0)
        """
    )
    subprocess.run([sys.executable, "-c", script], check=True, cwd=REPO_ROOT)
    graph = kglite.open(str(path), durable=SCENARIOS[name]["level"])
    got = observe(name, graph)
    late = graph.cypher("MATCH (n:Late) RETURN n.id AS id, n.v AS v").to_list()
    del graph
    assert compare(name, got) == []
    assert late == [{"id": 900, "v": "late"}]
