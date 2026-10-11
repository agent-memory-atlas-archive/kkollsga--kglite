"""Handles taken before ``close()`` cannot reach the log afterwards.

``close()`` checkpoints, releases the writer lease and detaches the graph. A
``Transaction`` or ``Session`` taken earlier must not append to the log once
another process owns the path: two writers interleave LSNs and one process's
commits are silently skipped on replay.

Today's contract, pinned here so the shared-session swap cannot loosen it:

* an open ``Transaction`` refuses to commit after ``close()``;
* a retained ``Session`` and the closed graph itself keep working on their own
  detached in-memory copy, which no log, file or other process ever sees.

A second process then opens the path, writes, and its data is intact after both
a reopen and the first process's late attempts.
"""

from __future__ import annotations

import subprocess
import sys

import pytest

import kglite

COUNT = "MATCH (n) RETURN labels(n) AS l, n.id AS id ORDER BY l, id"


def _second_process_writes(path) -> None:
    script = (
        "import kglite, os\n"
        f"h = kglite.open({str(path)!r}, durable='full')\n"
        'h.cypher("CREATE (:Second {id: 100})")\n'
        "os._exit(0)\n"  # hard exit: the write lives only in the log
    )
    subprocess.run([sys.executable, "-c", script], check=True)


def _labels(path) -> list[tuple[str, int]]:
    g = kglite.open(str(path), durable="full")
    try:
        return [(r["l"][0], r["id"]) for r in g.cypher(COUNT).to_list()]
    finally:
        del g


@pytest.fixture
def closed(tmp_path):
    """A durable graph closed with a transaction, a read transaction and a session still held."""
    path = tmp_path / "app.kgl"
    g = kglite.open(str(path), durable="full")
    g.cypher("CREATE (:A {id: 1})")
    held = {"g": g, "session": g.session(), "tx": g.begin(), "tx_after_second": g.begin(), "read_tx": g.begin_read()}
    held["tx"].cypher("CREATE (:Tx {id: 2})")
    held["tx_after_second"].cypher("CREATE (:Tx2 {id: 3})")
    g.close()
    return path, held


def test_an_open_transaction_cannot_commit_after_close(closed):
    _, held = closed
    with pytest.raises(kglite.ArgumentError, match="persistence ownership has ended"):
        held["tx"].commit()


def test_a_second_process_can_open_write_and_keep_its_data_through_every_late_attempt(closed):
    path, held = closed
    _second_process_writes(path)

    with pytest.raises(kglite.ArgumentError, match="persistence ownership has ended"):
        held["tx_after_second"].commit()
    # The retained session and the closed handle write to their own detached copy only.
    held["session"].execute("CREATE (:LateSession {id: 4})")
    held["g"].cypher("CREATE (:LateGraph {id: 5})")

    assert _labels(path) == [("A", 1), ("Second", 100)]


def test_late_writes_on_a_detached_copy_never_reach_disk(closed):
    path, held = closed
    held["session"].execute("CREATE (:LateSession {id: 4})")
    held["g"].cypher("CREATE (:LateGraph {id: 5})")
    assert held["session"].cypher("MATCH (n:LateSession) RETURN count(n) AS c").scalar() == 1
    assert _labels(path) == [("A", 1)]


def test_a_read_transaction_is_unaffected_by_close(closed):
    _, held = closed
    assert held["read_tx"].cypher("MATCH (n:A) RETURN count(n) AS c").scalar() == 1
